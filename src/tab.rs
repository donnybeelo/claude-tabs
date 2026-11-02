use alacritty_terminal::event::{Event, EventListener, Notify, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, Msg, Notifier};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::tty::{self, Shell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::Arc;

use crate::fx::Fx;
use crate::store::{find_conversation, home};
use crate::AppEvent;

#[derive(Clone)]
pub struct Listener {
    tab: u64,
    tx: Sender<AppEvent>,
}

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        let _ = self.tx.send(AppEvent::Term(self.tab, event));
    }
}

#[derive(Clone, Copy)]
pub struct Size {
    pub cols: u16,
    pub rows: u16,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.rows as usize
    }
    fn screen_lines(&self) -> usize {
        self.rows as usize
    }
    fn columns(&self) -> usize {
        self.cols as usize
    }
}

impl Size {
    fn window(self) -> WindowSize {
        WindowSize { num_lines: self.rows, num_cols: self.cols, cell_width: 1, cell_height: 1 }
    }
}

pub struct Running {
    pub term: Arc<FairMutex<Term<Listener>>>,
    notifier: Notifier,
    pub pid: u32,
}

pub struct Tab {
    pub id: u64,
    pub session: String,
    pub cwd: PathBuf,
    pub title: String,
    pub running: Option<Running>,
    pub busy: bool,
    pub fx: Fx,
}

pub fn new_session_id() -> String {
    let mut b = [0u8; 16];
    let _ = getrandom::fill(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

impl Tab {
    pub fn start(&mut self, size: Size, tx: &Sender<AppEvent>) -> std::io::Result<()> {
        let flag = match find_conversation(&self.session) {
            Some(cwd) => {
                if !cwd.as_os_str().is_empty() {
                    self.cwd = cwd;
                }
                "--resume"
            },
            None => {
                self.session = new_session_id();
                "--session-id"
            },
        };
        let options = tty::Options {
            shell: Some(Shell::new("claude".into(), vec![flag.into(), self.session.clone()])),
            working_directory: Some(self.cwd.clone()),
            drain_on_exit: false,
            env: HashMap::from([("CLAUDE_TABS_PID".into(), std::process::id().to_string())]),
            #[cfg(windows)]
            escape_args: true,
        };
        let pty = tty::new(&options, size.window(), self.id)?;
        #[cfg(unix)]
        let pid = pty.child().id();
        #[cfg(windows)]
        let pid = pty.child_watcher().pid().map_or(0, |pid| pid.get());
        let listener = Listener { tab: self.id, tx: tx.clone() };
        let config = Config { kitty_keyboard: true, scrolling_history: 10_000, ..Default::default() };
        let term = Arc::new(FairMutex::new(Term::new(config, &size, listener.clone())));
        let event_loop = EventLoop::new(term.clone(), listener, pty, false, false)?;
        let notifier = Notifier(event_loop.channel());
        event_loop.spawn();
        self.running = Some(Running { term, notifier, pid });
        Ok(())
    }

    pub fn write(&self, bytes: Vec<u8>) {
        if let Some(running) = &self.running {
            running.notifier.notify(bytes);
        }
    }

    pub fn resize(&self, size: Size) {
        if let Some(running) = &self.running {
            let _ = running.notifier.0.send(Msg::Resize(size.window()));
            running.term.lock().resize(size);
        }
    }

    pub fn kill(&self) {
        if let Some(running) = &self.running {
            #[cfg(unix)]
            unsafe {
                libc::kill(running.pid as i32, libc::SIGHUP);
            }
            #[cfg(windows)]
            let _ = running.notifier.0.send(Msg::Shutdown);
        }
    }

    /// Picks up conversation changes made inside Claude, such as /resume or /clear
    pub fn sync_session(&mut self) -> bool {
        let Some(running) = &self.running else {
            return false;
        };
        let path = home().join(format!(".claude/sessions/{}.json", running.pid));
        let Some(info) = std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        else {
            return false;
        };
        self.busy = info["status"] == "busy";
        let Some(session) = info["sessionId"].as_str() else {
            return false;
        };
        let cwd = info["cwd"].as_str().map(PathBuf::from).unwrap_or_else(|| self.cwd.clone());
        if session == self.session && cwd == self.cwd {
            return false;
        }
        self.session = session.to_string();
        self.cwd = cwd;
        true
    }
}
