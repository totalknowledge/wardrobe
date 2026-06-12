use std::io;
use wardrobe_cli::{CliConfig, run_cli_logic};

fn main() -> io::Result<()> {
    let config = CliConfig::from_args(std::env::args().skip(1))?;
    run_cli_logic(config)
}
