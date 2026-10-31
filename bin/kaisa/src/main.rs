//! The single `kaisa` executable. All behaviour lives in the library crates.

use std::io::{self, Write};
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match kaisa_cli::run(&args, &mut io::stdout().lock(), &mut io::stderr().lock()) {
        Ok(status) => ExitCode::from(status),
        // An I/O error that reached here must not vanish. Saying so can itself fail (for
        // example on a closed stderr), and then nothing more can be said.
        Err(error) => {
            let _ = writeln!(io::stderr(), "kaisa: {error}");
            ExitCode::FAILURE
        }
    }
}
