//! The single `kaisa` executable. All behaviour lives in the library crates.

use std::io;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match kaisa_cli::run(&args, &mut io::stdout().lock(), &mut io::stderr().lock()) {
        Ok(status) => ExitCode::from(status),
        // Writing to stdout/stderr failed (for example a closed pipe); nothing more can be said.
        Err(_) => ExitCode::FAILURE,
    }
}
