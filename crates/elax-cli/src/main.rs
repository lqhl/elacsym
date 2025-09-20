//! Command-line tooling for elacsym administration.

use anyhow::Result;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(author, version, about)]
struct Cli {}

fn main() -> Result<()> {
    let _ = Cli::parse();
    println!("elax-cli placeholder");
    Ok(())
}
