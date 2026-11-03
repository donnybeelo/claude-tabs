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

    #[cfg(not(target_os = "macos"))]
    fn portal_light(s: &str) -> bool {
        s.contains("uint32 0") || s.contains("uint32 2")
    }

    #[cfg(not(target_os = "macos"))]
    pub fn is_light() -> bool {
        std::process::Command::new("gdbus")
            .args(["call", "--session", "--dest", "org.freedesktop.portal.Desktop",
                   "--object-path", "/org/freedesktop/portal/desktop",
                   "--method", "org.freedesktop.portal.Settings.ReadOne",
                   "org.freedesktop.appearance", "color-scheme"])
            .output()
            .map_or(false, |out| portal_light(&String::from_utf8_lossy(&out.stdout)))
    }

    #[cfg(not(target_os = "macos"))]
    pub fn watch_color_scheme(tx: Sender<AppEvent>) {
        use std::io::{BufRead, BufReader};
        use std::process::{Command, Stdio};

        std::thread::spawn(move || {
            let Ok(mut child) = Command::new("gdbus")
                .args(["monitor", "--session", "--dest", "org.freedesktop.portal.Desktop",
                       "--object-path", "/org/freedesktop/portal/desktop"])
                .stdout(Stdio::piped())
                .spawn() else { return };
            for line in BufReader::new(child.stdout.take().unwrap()).lines().map_while(Result::ok) {
                if line.contains("'color-scheme'") && tx.send(AppEvent::ColorScheme(portal_light(&line))).is_err() {
                    break;
                }
            }
        });
    }

    #[cfg(target_os = "macos")]
    pub fn is_light() -> bool {
        std::process::Command::new("defaults")
            .args(["read", "-g", "AppleInterfaceStyle"])
            .output()
            .map_or(true, |out| !String::from_utf8_lossy(&out.stdout).contains("Dark"))
    }

    #[cfg(target_os = "macos")]
    pub fn watch_color_scheme(tx: Sender<AppEvent>) {
        std::thread::spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_secs(2));
            if tx.send(AppEvent::ColorScheme(is_light())).is_err() {
                break;
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
    use windows_sys::Win32::System::Registry::{
        RegGetValueW, RegNotifyChangeKeyValue, RegOpenKeyExW, HKEY_CURRENT_USER, KEY_NOTIFY,
        REG_NOTIFY_CHANGE_LAST_SET, RRF_RT_REG_DWORD,
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

    const PERSONALIZE: &str = r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize";

    pub fn is_light() -> bool {
        let path: Vec<u16> = PERSONALIZE.encode_utf16().chain([0]).collect();
        let name: Vec<u16> = "AppsUseLightTheme".encode_utf16().chain([0]).collect();
        let mut value: u32 = 0;
        let mut size: u32 = 4;
        let status = unsafe {
            RegGetValueW(HKEY_CURRENT_USER, path.as_ptr(), name.as_ptr(), RRF_RT_REG_DWORD,
                std::ptr::null_mut(), &mut value as *mut u32 as *mut _, &mut size)
        };
        status == 0 && value == 1
    }

    pub fn watch_color_scheme(tx: Sender<AppEvent>) {
        std::thread::spawn(move || {
            let path: Vec<u16> = PERSONALIZE.encode_utf16().chain([0]).collect();
            let mut key = std::ptr::null_mut();
            if unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, path.as_ptr(), 0, KEY_NOTIFY, &mut key) } != 0 {
                return;
            }
            while unsafe { RegNotifyChangeKeyValue(key, 0, REG_NOTIFY_CHANGE_LAST_SET, std::ptr::null_mut(), 0) } == 0 {
                if tx.send(AppEvent::ColorScheme(is_light())).is_err() {
                    break;
                }
            }
        });
    }

    pub fn enable_vt() {
        let _ = crossterm::ansi_support::supports_ansi();
    }
}

pub use imp::*;
