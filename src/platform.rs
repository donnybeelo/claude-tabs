use std::sync::mpsc::Sender;

use crate::AppEvent;

#[cfg(unix)]
mod imp {
    use super::*;

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

    pub fn watch_hangup(tx: Sender<AppEvent>) {
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

    pub fn watch_color_scheme(tx: Sender<AppEvent>) {
        use std::io::{BufRead, BufReader};
        use std::process::{Command, Stdio};

        std::thread::spawn(move || {
            let is_light = |s: &str| !s.contains("prefer-dark");
            let key = ["org.gnome.desktop.interface", "color-scheme"];
            if let Ok(out) = Command::new("gsettings").arg("get").args(key).output() {
                let _ = tx.send(AppEvent::ColorScheme(is_light(&String::from_utf8_lossy(&out.stdout))));
            }
            let Ok(mut child) = Command::new("gsettings").arg("monitor").args(key).stdout(Stdio::piped()).spawn() else {
                return;
            };
            for line in BufReader::new(child.stdout.take().unwrap()).lines().map_while(Result::ok) {
                if tx.send(AppEvent::ColorScheme(is_light(&line))).is_err() {
                    break;
                }
            }
        });
    }

    pub fn process_alive(pid: u32) -> bool {
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }

    pub fn enable_vt() {}
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::sync::{Mutex, OnceLock};
    use std::time::Duration;
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, ERROR_PIPE_CONNECTED, INVALID_HANDLE_VALUE, WAIT_TIMEOUT,
    };
    use windows_sys::Win32::Storage::FileSystem::PIPE_ACCESS_INBOUND;
    use windows_sys::Win32::System::Console::{
        SetConsoleCtrlHandler, CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT,
    };
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_TYPE_BYTE, PIPE_WAIT,
    };
    use windows_sys::Win32::System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE};

    static SENDER: OnceLock<Mutex<Sender<AppEvent>>> = OnceLock::new();

    unsafe extern "system" fn on_console_event(kind: u32) -> i32 {
        if matches!(kind, CTRL_CLOSE_EVENT | CTRL_LOGOFF_EVENT | CTRL_SHUTDOWN_EVENT) {
            if let Some(tx) = SENDER.get() {
                let _ = tx.lock().map(|tx| tx.send(AppEvent::InputClosed));
            }
            std::thread::sleep(Duration::from_secs(4));
        }
        1
    }

    /// Saves and exits when the console closes, and opens a new tab when anything connects to the pipe
    pub fn watch_hangup(tx: Sender<AppEvent>) {
        let _ = SENDER.set(Mutex::new(tx.clone()));
        unsafe {
            SetConsoleCtrlHandler(Some(on_console_event), 1);
        }
        std::thread::spawn(move || {
            let name: Vec<u16> =
                format!(r"\\.\pipe\claude-tabs-{}", std::process::id()).encode_utf16().chain([0]).collect();
            loop {
                let pipe = unsafe {
                    CreateNamedPipeW(name.as_ptr(), PIPE_ACCESS_INBOUND, PIPE_TYPE_BYTE | PIPE_WAIT, 1, 0, 64, 0, std::ptr::null())
                };
                if pipe == INVALID_HANDLE_VALUE {
                    break;
                }
                let connected = unsafe { ConnectNamedPipe(pipe, std::ptr::null_mut()) != 0 || GetLastError() == ERROR_PIPE_CONNECTED };
                unsafe {
                    DisconnectNamedPipe(pipe);
                    CloseHandle(pipe);
                }
                if connected && tx.send(AppEvent::NewTab).is_err() {
                    break;
                }
            }
        });
    }

    pub fn process_alive(pid: u32) -> bool {
        unsafe {
            let process = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
            if process.is_null() {
                return false;
            }
            let alive = WaitForSingleObject(process, 0) == WAIT_TIMEOUT;
            CloseHandle(process);
            alive
        }
    }

    pub fn watch_color_scheme(_: Sender<AppEvent>) {}

    pub fn enable_vt() {
        let _ = crossterm::ansi_support::supports_ansi();
    }
}

pub use imp::*;
