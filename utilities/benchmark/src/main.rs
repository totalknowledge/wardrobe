#![deny(unsafe_code)]

use std::env;
use std::fs;
use std::io;
use std::process;
use wardrobe_benchmark::{BenchmarkConfig, ParseOutcome, print_help, run_benchmark};

fn main() {
    if let Err(error) = run() {
        eprintln!("wardrobe-benchmark: {error}");
        process::exit(1);
    }
}

fn run() -> io::Result<()> {
    let config = match BenchmarkConfig::from_args(env::args().skip(1))? {
        ParseOutcome::Help => {
            print_help();
            return Ok(());
        }
        ParseOutcome::Run(config) => config,
    };

    let output_path = config.output_path.clone();
    let report = run_benchmark(config)?;
    let markdown = report.to_markdown();

    if let Some(path) = output_path {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, &markdown)?;
    }

    println!("{markdown}");
    Ok(())
}
