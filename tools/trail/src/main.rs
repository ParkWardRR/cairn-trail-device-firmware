//! `trail`: the host tool for the Cairn trail logger.
//!
//!     trail console --cmd stat --seconds 3          talk to the board, capture its log
//!     trail console --session S0009 --out pulled/   pull a session, CRC-checked
//!     trail verify pulled/S0009 --stationary        check a pulled session
//!     trail prime --lat 47.6 --lon -122.3           give the receiver the time and a position
//!     trail assistnow identity|register|fetch|upload|install
//!     trail provision cert|test|write --mac 48:ca:43:5b:ed:60

mod assistnow;
mod config;
mod console;
mod prime;
mod provision;
mod serial;
mod verify;

use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "trail",
    about = "Host tool for the Cairn trail logger",
    version
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Capture the console, send commands, pull files off the card.
    Console(console::Args),
    /// Check a session pulled off the card.
    Verify(verify::Args),
    /// Prime the GNSS receiver with this computer's time and a position.
    Prime(prime::Args),
    /// u-blox AssistNow: register, download, load onto the card.
    #[command(subcommand)]
    Assistnow(assistnow::Cmd),
    /// Provision identity, Wi-Fi and server over USB.
    #[command(subcommand)]
    Provision(provision::Cmd),
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let r = match cli.cmd {
        Cmd::Console(a) => console::run(a),
        Cmd::Verify(a) => verify::run(a),
        Cmd::Prime(a) => prime::run(a).map(|_| true),
        Cmd::Assistnow(c) => assistnow::run(c).map(|_| true),
        Cmd::Provision(c) => provision::run_cmd(c).map(|_| true),
    };
    match r {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}
