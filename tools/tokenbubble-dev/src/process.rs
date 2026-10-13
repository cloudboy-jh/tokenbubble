//! Interrupt handling and child shutdown for the dev runner.

use std::process::Child;
use std::sync::atomic::{AtomicBool, Ordering};

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

/// End a child the runner started. The app keeps no state that needs a graceful close.
pub fn stop_child(child: &mut Child) {
    if child.try_wait().ok().flatten().is_none() {
        let _ = child.kill();
    }
    let _ = child.wait();
}

#[cfg(windows)]
pub fn install_interrupt_handler() {
    use std::time::{Duration, Instant};
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
