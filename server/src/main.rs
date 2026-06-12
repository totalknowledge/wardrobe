use std::io;
use wardrobe_server::{ServerConfig, run};

fn main() -> io::Result<()> {
    let config = ServerConfig::from_args(std::env::args().skip(1))?;
    run(config)
}
