use std::io;
use wardrobe_server::{ServerConfig, run};

fn main() -> io::Result<()> {
    let config = ServerConfig::from_args(std::env::args().skip(1))?;
    install_shutdown_handler()?;
    run(config)
}

fn install_shutdown_handler() -> io::Result<()> {
    shutdown_handler::install()
}

#[cfg(windows)]
mod shutdown_handler {
    use std::io::{self, Write};
    use std::sync::atomic::{AtomicBool, Ordering};

    const CTRL_C_EVENT: u32 = 0;
    const CTRL_BREAK_EVENT: u32 = 1;
    const HANDLED: i32 = 1;
    const UNHANDLED: i32 = 0;

    static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);

    type HandlerRoutine = Option<unsafe extern "system" fn(u32) -> i32>;

    #[link(name = "Kernel32")]
    unsafe extern "system" {
        #[link_name = "SetConsoleCtrlHandler"]
        fn set_console_ctrl_handler(handler: HandlerRoutine, add: i32) -> i32;
    }

    pub fn install() -> io::Result<()> {
        let installed = unsafe { set_console_ctrl_handler(Some(handle_console_event), HANDLED) };
        if installed == UNHANDLED {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    unsafe extern "system" fn handle_console_event(event: u32) -> i32 {
        match event {
            CTRL_C_EVENT | CTRL_BREAK_EVENT => {
                if SHUTDOWN_REQUESTED.swap(true, Ordering::SeqCst) {
                    std::process::exit(0);
                }

                let mut stdout = io::stdout().lock();
                let _ = writeln!(stdout, "\nWardrobe daemon shutdown requested.");
                let _ = stdout.flush();
                std::process::exit(0);
            }
            _ => UNHANDLED,
        }
    }
}

#[cfg(not(windows))]
mod shutdown_handler {
    use std::io;

    pub fn install() -> io::Result<()> {
        Ok(())
    }
}
