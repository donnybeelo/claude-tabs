mod fx;
mod keys;
mod render;
mod store;
mod tab;

use alacritty_terminal::event::{Event as TermEvent, WindowSize};
use alacritty_terminal::grid::Scroll;
use alacritty_terminal::term::TermMode;
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, ModifierKeyCode, MouseButton, MouseEvent, MouseEventKind,
};
use std::io::Write;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use fx::Fx;
use render::{Region, Screen, Target};
use store::{SavedTab, SavedWindow, Store};
use tab::{new_session_id, Size, Tab};

pub enum AppEvent {
    Input(Event),
    InputClosed,
    NewTab,
    Term(u64, TermEvent),
}

const SETUP: &[u8] = b"\x1b[?1049h\x1b[?25l\x1b[?1003h\x1b[?1006h\x1b[?2004h\x1b[?1004h\x1b[>15u";
const TEARDOWN: &[u8] = b"\x1b[<u\x1b[?1004l\x1b[?2004l\x1b[?1006l\x1b[?1003l\x1b[0m\x1b[?25h\x1b[?1049l";

fn restore_terminal() {
    let mut out = std::io::stdout();
    let _ = out.write_all(TEARDOWN);
    let _ = out.flush();
    let _ = crossterm::terminal::disable_raw_mode();
}

static HANGUP_PIPE: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);

extern "C" fn on_hangup(_: libc::c_int) {
    let fd = HANGUP_PIPE.load(std::sync::atomic::Ordering::Relaxed);
    unsafe {
        libc::write(fd, b"x".as_ptr().cast(), 1);
    }
}

extern "C" fn on_new_tab(_: libc::c_int) {
    let fd = HANGUP_PIPE.load(std::sync::atomic::Ordering::Relaxed);
    unsafe {
        libc::write(fd, b"n".as_ptr().cast(), 1);
    }
}

fn watch_hangup(tx: Sender<AppEvent>) {
    let mut fds = [0; 2];
    unsafe {
        libc::pipe(fds.as_mut_ptr());
    }
    HANGUP_PIPE.store(fds[1], std::sync::atomic::Ordering::Relaxed);
    unsafe {
        libc::signal(libc::SIGHUP, on_hangup as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, on_hangup as *const () as libc::sighandler_t);
        libc::signal(libc::SIGUSR1, on_new_tab as *const () as libc::sighandler_t);
    }
    std::thread::spawn(move || {
        let mut byte = 0u8;
        while unsafe { libc::read(fds[0], (&mut byte as *mut u8).cast(), 1) } == 1 {
            if byte == b'n' {
                let _ = tx.send(AppEvent::NewTab);
            } else {
                let _ = tx.send(AppEvent::InputClosed);
                break;
            }
        }
    });
}

fn base64(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, b)| n | (*b as u32) << (16 - 8 * i));
        for i in 0..4 {
            out.push(if i <= chunk.len() { TABLE[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

struct App {
    tabs: Vec<Tab>,
    active: usize,
    next_id: u64,
    tx: Sender<AppEvent>,
    store: Store,
    screen: Screen,
    size: Size,
    hover: Option<u16>,
    regions: Vec<Region>,
    dirty: bool,
    quit: bool,
    closed: Vec<(usize, SavedTab)>,
    cleanups: Vec<std::thread::JoinHandle<()>>,
    mru: Vec<u64>,
    cycling: Option<usize>,
    dragging: bool,
    animating: bool,
}

impl App {
    fn content_size(cols: u16, rows: u16) -> Size {
        Size { cols: cols.max(2), rows: rows.saturating_sub(1).max(1) }
    }

    fn add_tab(&mut self, session: String, cwd: std::path::PathBuf, title: String) -> usize {
        self.next_id += 1;
        self.tabs.push(Tab { id: self.next_id, session, cwd, title, running: None, busy: false, fx: Fx::default() });
        self.tabs.len() - 1
    }

    fn activate(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        self.active = index;
        if self.cycling.is_none() {
            self.touch_mru();
        }
        let tab = &mut self.tabs[index];
        tab.fx.seen(Instant::now());
        if tab.running.is_none() {
            let _ = tab.start(self.size, &self.tx);
        }
        self.screen.invalidate();
        self.dirty = true;
        self.save();
    }

    fn touch_mru(&mut self) {
        let id = self.tabs[self.active].id;
        self.mru.retain(|&other| other != id);
        self.mru.insert(0, id);
    }

    /// Steps through tabs in most recently used order while Ctrl is held
    fn cycle(&mut self, step: isize) {
        let ids: Vec<u64> = self.tabs.iter().map(|t| t.id).collect();
        self.mru.retain(|id| ids.contains(id));
        for id in ids {
            if !self.mru.contains(&id) {
                self.mru.push(id);
            }
        }
        if self.mru.len() < 2 {
            return;
        }
        let position = (self.cycling.unwrap_or(0) as isize + step).rem_euclid(self.mru.len() as isize) as usize;
        self.cycling = Some(position);
        let id = self.mru[position];
        if let Some(index) = self.tabs.iter().position(|t| t.id == id) {
            self.activate(index);
        }
    }

    fn end_cycle(&mut self) {
        if self.cycling.take().is_some() && self.active < self.tabs.len() {
            self.touch_mru();
        }
    }

    fn new_tab(&mut self) {
        let cwd = self.tabs.get(self.active).map(|t| t.cwd.clone()).unwrap_or_else(store::home);
        let index = self.add_tab(new_session_id(), cwd, "Claude Code".into());
        self.tabs[index].fx = Fx::opening();
        self.activate(index);
    }

    fn close_tab(&mut self, index: usize) {
        self.end_cycle();
        if index >= self.tabs.len() {
            return;
        }
        let mut tab = self.tabs.remove(index);
        tab.sync_session();
        tab.kill();
        let pid = tab.running.as_ref().map(|r| r.pid as i32);
        let session = tab.session.clone();
        self.cleanups.push(std::thread::spawn(move || {
            for _ in 0..100 {
                if pid.is_none_or(|pid| unsafe { libc::kill(pid, 0) } != 0) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            store::drop_if_boring(&session);
        }));
        self.screen.close_tab(index, tab.title.clone(), std::mem::take(&mut tab.fx));
        self.closed.push((index, SavedTab { session: tab.session, cwd: tab.cwd, title: tab.title }));
        if self.closed.len() > 50 {
            self.closed.remove(0);
        }
        if self.tabs.is_empty() {
            self.store.remove();
            self.quit = true;
            return;
        }
        if index < self.active || self.active >= self.tabs.len() {
            self.active = self.active.saturating_sub(1);
        }
        let active = self.active;
        self.activate(active);
    }

    fn restore_tab(&mut self) {
        let Some((index, tab)) = self.closed.pop() else {
            return;
        };
        self.next_id += 1;
        let index = index.min(self.tabs.len());
        let restored = Tab {
            id: self.next_id,
            session: tab.session,
            cwd: tab.cwd,
            title: tab.title,
            running: None,
            busy: false,
            fx: Fx::opening(),
        };
        self.tabs.insert(index, restored);
        self.activate(index);
    }

    fn save(&self) {
        let tabs = self
            .tabs
            .iter()
            .map(|t| SavedTab { session: t.session.clone(), cwd: t.cwd.clone(), title: t.title.clone() })
            .collect();
        self.store.save(&SavedWindow { tabs, active: self.active, closed: self.closed.clone() });
    }

    fn sync_sessions(&mut self) {
        let mut changed = false;
        let now = Instant::now();
        for (i, tab) in self.tabs.iter_mut().enumerate() {
            changed |= tab.sync_session();
            self.dirty |= tab.fx.set_busy(tab.busy, i == self.active, now);
        }
        if changed {
            self.save();
        }
    }

    fn active_mode(&self) -> Option<TermMode> {
        let running = self.tabs.get(self.active)?.running.as_ref()?;
        Some(*running.term.lock().mode())
    }

    fn write_active(&self, bytes: Vec<u8>) {
        if let Some(tab) = self.tabs.get(self.active) {
            tab.write(bytes);
        }
    }

    fn scroll_active(&mut self, scroll: Scroll) {
        if let Some(running) = self.tabs.get(self.active).and_then(|t| t.running.as_ref()) {
            let mut term = running.term.lock();
            if term.grid().display_offset() != 0 || !matches!(scroll, Scroll::Bottom) {
                term.scroll_display(scroll);
                self.dirty = true;
            }
        }
    }

    fn handle_key(&mut self, mut key: KeyEvent) {
        if let KeyCode::Modifier(modifier) = key.code {
            if key.kind == KeyEventKind::Release
                && matches!(modifier, ModifierKeyCode::LeftControl | ModifierKeyCode::RightControl)
            {
                self.end_cycle();
            }
            return;
        }
        if key.kind == KeyEventKind::Release {
            return;
        }
        if let KeyCode::Char(c) = key.code {
            if c.is_uppercase() && key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
                key.modifiers |= KeyModifiers::SHIFT;
            }
        }
        let m = key.modifiers;
        let ctrl = m == KeyModifiers::CONTROL;
        let ctrl_shift = m == KeyModifiers::CONTROL | KeyModifiers::SHIFT;
        match key.code {
            KeyCode::Tab if ctrl => return self.cycle(1),
            KeyCode::BackTab | KeyCode::Tab if ctrl_shift => return self.cycle(-1),
            _ => self.end_cycle(),
        }
        let count = self.tabs.len();
        match key.code {
            KeyCode::Char('t') if ctrl => return self.new_tab(),
            KeyCode::Char('t' | 'T') if ctrl_shift => return self.restore_tab(),
            KeyCode::Char('w') if ctrl => return self.close_tab(self.active),
            KeyCode::PageDown if ctrl => return self.activate((self.active + 1) % count),
            KeyCode::PageUp if ctrl => return self.activate((self.active + count - 1) % count),
            KeyCode::Char('9') if ctrl => return self.activate(count - 1),
            KeyCode::Char(c @ '1'..='8') if ctrl => return self.activate(c as usize - '1' as usize),
            _ => {},
        }
        let Some(mode) = self.active_mode() else {
            return;
        };
        if let Some(bytes) = keys::encode_key(&key, mode) {
            self.scroll_active(Scroll::Bottom);
            self.write_active(bytes);
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) {
        if matches!(mouse.kind, MouseEventKind::Down(_)) {
            self.end_cycle();
        }
        if self.dragging {
            match mouse.kind {
                MouseEventKind::Drag(MouseButton::Left) if mouse.row == 0 => self.drag_to(mouse.column),
                MouseEventKind::Up(MouseButton::Left) => {
                    self.dragging = false;
                    self.save();
                },
                _ => {},
            }
            return;
        }
        let hover = (mouse.row == 0).then_some(mouse.column);
        if hover != self.hover {
            self.hover = hover;
            self.dirty = true;
        }
        if mouse.row == 0 {
            let target = self.regions.iter().find(|r| mouse.column >= r.start && mouse.column < r.end);
            let count = self.tabs.len();
            match (mouse.kind, target.map(|r| &r.target)) {
                (MouseEventKind::Down(MouseButton::Left), Some(Target::Tab(i))) => {
                    self.activate(*i);
                    self.dragging = true;
                },
                (MouseEventKind::Down(MouseButton::Left), Some(Target::Close(i))) => self.close_tab(*i),
                (MouseEventKind::Down(MouseButton::Middle), Some(Target::Tab(i) | Target::Close(i))) => {
                    self.close_tab(*i)
                },
                (MouseEventKind::Down(MouseButton::Left), Some(Target::New)) => self.new_tab(),
                (MouseEventKind::ScrollDown, _) => self.activate((self.active + 1) % count),
                (MouseEventKind::ScrollUp, _) => self.activate((self.active + count - 1) % count),
                _ => {},
            }
            return;
        }
        let Some(mode) = self.active_mode() else {
            return;
        };
        if let Some(bytes) = keys::encode_mouse(&mouse, mode, mouse.row - 1) {
            self.write_active(bytes);
            return;
        }
        match mouse.kind {
            MouseEventKind::ScrollUp => self.scroll_active(Scroll::Delta(3)),
            MouseEventKind::ScrollDown => self.scroll_active(Scroll::Delta(-3)),
            _ => {},
        }
    }

    /// Moves the dragged tab once the pointer passes the middle of a neighbouring tab
    fn drag_to(&mut self, column: u16) {
        let span = |index: usize| {
            let parts = self.regions.iter().filter(|r| matches!(r.target, Target::Tab(i) | Target::Close(i) if i == index));
            parts.fold(None, |span: Option<(u16, u16)>, r| {
                Some(span.map_or((r.start, r.end), |(start, end)| (start.min(r.start), end.max(r.end))))
            })
        };
        let Some(target) = (0..self.tabs.len()).find(|&i| span(i).is_some_and(|(start, end)| column >= start && column < end))
        else {
            return;
        };
        let Some((start, end)) = span(target) else {
            return;
        };
        let middle = (start + end) / 2;
        if (target > self.active && column >= middle) || (target < self.active && column < middle) {
            let tab = self.tabs.remove(self.active);
            self.tabs.insert(target, tab);
            self.active = target;
            self.render();
        }
    }

    fn handle_input(&mut self, event: Event) {
        match event {
            Event::Key(key) => self.handle_key(key),
            Event::Mouse(mouse) => self.handle_mouse(mouse),
            Event::Paste(text) => {
                let Some(mode) = self.active_mode() else {
                    return;
                };
                let bytes = if mode.contains(TermMode::BRACKETED_PASTE) {
                    format!("\x1b[200~{}\x1b[201~", text.replace('\x1b', ""))
                } else {
                    text.replace("\r\n", "\r").replace('\n', "\r")
                };
                self.write_active(bytes.into_bytes());
            },
            Event::FocusGained | Event::FocusLost => {
                if self.active_mode().is_some_and(|m| m.contains(TermMode::FOCUS_IN_OUT)) {
                    let code = if matches!(event, Event::FocusGained) { "\x1b[I" } else { "\x1b[O" };
                    self.write_active(code.into());
                }
            },
            Event::Resize(cols, rows) => {
                self.size = Self::content_size(cols, rows);
                self.screen = Screen::new(cols, rows);
                for tab in &self.tabs {
                    tab.resize(self.size);
                }
                self.dirty = true;
            },
        }
    }

    fn handle_term(&mut self, id: u64, event: TermEvent) {
        let Some(index) = self.tabs.iter().position(|t| t.id == id) else {
            return;
        };
        match event {
            TermEvent::Wakeup => self.dirty |= index == self.active,
            TermEvent::Title(title) => {
                self.tabs[index].title = title;
                self.tabs[index].sync_session();
                self.dirty = true;
                self.save();
            },
            TermEvent::ResetTitle => {
                self.tabs[index].title = "Claude Code".into();
                self.dirty = true;
            },
            TermEvent::PtyWrite(text) => self.tabs[index].write(text.into_bytes()),
            TermEvent::TextAreaSizeRequest(format) => {
                let size = WindowSize { num_lines: self.size.rows, num_cols: self.size.cols, cell_width: 1, cell_height: 1 };
                self.tabs[index].write(format(size).into_bytes());
            },
            TermEvent::ClipboardStore(_, text) => {
                let mut out = std::io::stdout();
                let _ = write!(out, "\x1b]52;c;{}\x07", base64(text.as_bytes()));
                let _ = out.flush();
            },
            TermEvent::Bell => {
                let _ = std::io::stdout().write_all(b"\x07");
            },
            TermEvent::ChildExit(_) => self.close_tab(index),
            _ => {},
        }
    }

    fn render(&mut self) {
        self.dirty = false;
        let mut items: Vec<(&str, &mut Fx)> = self.tabs.iter_mut().map(|t| (t.title.as_str(), &mut t.fx)).collect();
        (self.regions, self.animating) = self.screen.draw_tabbar(&mut items, self.active, self.hover, Instant::now());
        drop(items);
        let cursor = match self.tabs.get(self.active).and_then(|t| t.running.as_ref()) {
            Some(running) => self.screen.draw_term(&running.term.lock()),
            None => {
                self.screen.draw_message("Could not start claude");
                None
            },
        };
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(self.screen.flush(cursor));
        let _ = out.flush();
    }

    fn run(&mut self, rx: Receiver<AppEvent>) {
        let mut last_sync = Instant::now();
        while !self.quit {
            let timeout = if self.animating { Duration::from_millis(33) } else { Duration::from_secs(1) };
            match rx.recv_timeout(timeout) {
                Ok(event) => self.dispatch(event),
                Err(RecvTimeoutError::Timeout) => {},
                Err(RecvTimeoutError::Disconnected) => break,
            }
            while let Ok(event) = rx.try_recv() {
                self.dispatch(event);
            }
            if last_sync.elapsed() >= Duration::from_secs(1) {
                self.sync_sessions();
                last_sync = Instant::now();
            }
            if (self.dirty || self.animating) && !self.quit {
                self.render();
            }
        }
    }

    fn dispatch(&mut self, event: AppEvent) {
        match event {
            AppEvent::Input(event) => self.handle_input(event),
            AppEvent::Term(id, event) => self.handle_term(id, event),
            AppEvent::NewTab => self.new_tab(),
            AppEvent::InputClosed => {
                self.sync_sessions();
                for tab in &self.tabs {
                    tab.kill();
                }
                self.store.touch();
                std::process::exit(0);
            },
        }
    }
}

fn main() {
    let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
    let _ = crossterm::terminal::enable_raw_mode();
    let mut out = std::io::stdout();
    let _ = out.write_all(SETUP);
    let _ = out.flush();
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        default_hook(info);
    }));

    let (tx, rx) = mpsc::channel();
    watch_hangup(tx.clone());
    let (store, saved) = Store::open();
    let mut app = App {
        tabs: Vec::new(),
        active: 0,
        next_id: 0,
        tx: tx.clone(),
        store,
        screen: Screen::new(cols, rows),
        size: App::content_size(cols, rows),
        hover: None,
        regions: Vec::new(),
        dirty: true,
        quit: false,
        closed: Vec::new(),
        cleanups: Vec::new(),
        mru: Vec::new(),
        cycling: None,
        dragging: false,
        animating: false,
    };
    app.closed = saved.closed;
    for saved_tab in saved.tabs {
        app.add_tab(saved_tab.session, saved_tab.cwd, saved_tab.title);
    }
    if app.tabs.is_empty() {
        app.new_tab();
    } else {
        app.activate(saved.active);
    }

    std::thread::spawn(move || loop {
        match crossterm::event::read() {
            Ok(event) => {
                if tx.send(AppEvent::Input(event)).is_err() {
                    break;
                }
            },
            Err(_) => {
                let _ = tx.send(AppEvent::InputClosed);
                break;
            },
        }
    });

    app.render();
    app.run(rx);
    restore_terminal();
    for cleanup in app.cleanups {
        let _ = cleanup.join();
    }
}
