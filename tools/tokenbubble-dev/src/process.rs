//! Interrupt handling and child shutdown for the dev runner.

use std::process::Child;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// How long a politely closed app gets before it is killed.
const CLOSE_GRACE: Duration = Duration::from_secs(3);

static INTERRUPTED: AtomicBool = AtomicBool::new(false);
#[cfg(windows)]
static SHUTDOWN_DONE: AtomicBool = AtomicBool::new(false);

pub fn interrupted() -> bool {
    INTERRUPTED.load(Ordering::Acquire)
}

/// Tell a console-close handler that cleanup finished and the process may end.
pub fn shutdown_done() {
    #[cfg(windows)]
    SHUTDOWN_DONE.store(true, Ordering::Release);
}

/// Ask the app to close so it can remove its tray icon, then make sure it is gone.
pub fn stop_child(child: &mut Child) {
    if child.try_wait().ok().flatten().is_none()
        && !(request_close(child.id()) && wait_child(child, CLOSE_GRACE))
    {
        let _ = child.kill();
    }
    let _ = child.wait();
}

fn wait_child(child: &mut Child, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if child.try_wait().ok().flatten().is_some() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Post `WM_CLOSE` to every top-level window of `pid`, including hidden ones such as a
/// tray icon's message window. Returns whether any window was asked.
#[cfg(windows)]
fn request_close(pid: u32) -> bool {
    use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowThreadProcessId, PostMessageW, WM_CLOSE,
    };
    use windows::core::BOOL;

    struct Search {
        pid: u32,
        found: Vec<HWND>,
    }
    unsafe extern "system" fn visit(window: HWND, search: LPARAM) -> BOOL {
        let search = unsafe { &mut *(search.0 as *mut Search) };
        let mut owner = 0;
        unsafe { GetWindowThreadProcessId(window, Some(&mut owner)) };
        if owner == search.pid {
            search.found.push(window);
        }
        true.into()
    }
    let mut search = Search {
        pid,
        found: Vec::new(),
    };
    let _ = unsafe { EnumWindows(Some(visit), LPARAM(&mut search as *mut Search as isize)) };
    for window in &search.found {
        let _ = unsafe { PostMessageW(Some(*window), WM_CLOSE, WPARAM(0), LPARAM(0)) };
    }
    !search.found.is_empty()
}

#[cfg(unix)]
fn request_close(pid: u32) -> bool {
    unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) == 0 }
}

#[cfg(windows)]
pub fn install_interrupt_handler() {
    use windows::Win32::System::Console::{CTRL_C_EVENT, SetConsoleCtrlHandler};
    use windows::core::BOOL;

    unsafe extern "system" fn on_console_event(event: u32) -> BOOL {
        INTERRUPTED.store(true, Ordering::Release);
        if event != CTRL_C_EVENT {
            // Ctrl+Break, console close, logoff: Windows ends the process when this
            // returns, so give the main loop a moment to stop the app.
            let deadline = Instant::now() + Duration::from_millis(4500);
            while !SHUTDOWN_DONE.load(Ordering::Acquire) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(25));
            }
        }
        true.into()
    }

    let _ = unsafe { SetConsoleCtrlHandler(Some(on_console_event), true) };
}

#[cfg(unix)]
pub fn install_interrupt_handler() {
    extern "C" fn on_signal(_: libc::c_int) {
        INTERRUPTED.store(true, Ordering::Release);
    }
    let handler = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
    unsafe {
        libc::signal(libc::SIGINT, handler);
        libc::signal(libc::SIGTERM, handler);
        libc::signal(libc::SIGHUP, handler);
    }
}
