use std::io;
use std::thread;
use std::time::Duration;
use wardrobe_core::WardrobeEngine;

#[derive(Debug)]
struct ServerConfig {
    data_dir: String,
    check_only: bool,
}

impl ServerConfig {
    fn from_args<I>(args: I) -> io::Result<Self>
    where
        I: IntoIterator<Item = String>,
    {
        let mut data_dir = String::from("./wardrobe");
        let mut check_only = false;
        let mut args = args.into_iter();

        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--data-dir" => {
                    data_dir = args.next().ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "--data-dir requires a directory path",
                        )
                    })?;
                }
                "--check" => check_only = true,
                "--help" | "-h" => {
                    print_help();
                    std::process::exit(0);
                }
                unknown => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("Unknown server argument: {unknown}"),
                    ));
                }
            }
        }

        Ok(Self {
            data_dir,
            check_only,
        })
    }
}

fn print_help() {
    println!("wardrobe-server");
    println!("  --data-dir <path>  Storage directory for the Wardrobe database");
    println!("  --check            Initialize the daemon and exit without blocking");
}

fn main() -> io::Result<()> {
    let config = ServerConfig::from_args(std::env::args().skip(1))?;
    let _engine = WardrobeEngine::open(&config.data_dir)?;

    println!(
        "Wardrobe daemon initialized with storage directory: {}",
        config.data_dir
    );

    if config.check_only {
        println!("Wardrobe daemon check completed.");
        return Ok(());
    }

    println!("Wardrobe daemon is running. Protocol transport is not enabled yet.");
    loop {
        thread::sleep(Duration::from_secs(60));
    }
}
