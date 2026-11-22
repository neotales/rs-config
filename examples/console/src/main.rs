//! Runs the console sample once and prints a safe report.
//!
//! ```sh
//! cargo run -p neotales-config-example-console
//! cargo run -p neotales-config-example-console -- --demo.mode=arguments
//! APP_DEMO__MODE=environment cargo run -p neotales-config-example-console
//! ```

use std::{error::Error, process::ExitCode};

use neotales_config_example_console::{
    build, config_directory, process_arguments, process_environment, report,
};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            // Config errors are sanitized: a source name and a shape label, no value or path.
            eprintln!("console sample: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let directory = config_directory();
    let config = build(&directory, process_arguments(), process_environment())?;
    print!("{}", report(&config, &directory));
    Ok(())
}
