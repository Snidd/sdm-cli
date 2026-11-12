use std::process::ExitCode;

fn main() -> ExitCode {
    match sdm_cli::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            sdm_cli::output::print_error(&err);
            ExitCode::from(err.exit_code())
        }
    }
}
