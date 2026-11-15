use std::process::ExitCode;

fn main() -> ExitCode {
    ExitCode::from(agyline::run_cli_from_process())
}
