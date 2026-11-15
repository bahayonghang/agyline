//! Binary shim: every mode decision (pipe render, configurator, subcommands, hidden workers) lives
//! in `cli::run_cli`. The returned `u8` is the exit code: 0 success, 1 failure, 2 bad arguments.

use std::process::ExitCode;

fn main() -> ExitCode {
    ExitCode::from(agyline::run_cli_from_process())
}
