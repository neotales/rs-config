//! Runs the loopback web sample.
//!
//! ```sh
//! cargo run -p neotales-config-example-web
//! cargo run -p neotales-config-example-web -- --demo.mode=arguments
//! APP_DEMO__MODE=environment cargo run -p neotales-config-example-web
//! ```
//!
//! The process runs until it is stopped (for example with Ctrl-C). Tests use
//! the library directly and call `Server::stop`, which is the explicit shutdown
//! path.

use std::{error::Error, process::ExitCode, thread, time::Duration};

use neotales_config_example_web::{
    POLL_INTERVAL, Server, build_reload, config_directory, loopback_address, process_arguments,
    process_environment,
};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            // Config errors are sanitized; start errors name a kind, not a value or a path.
            eprintln!("web sample: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let directory = config_directory();
    let reload = build_reload(&directory, process_arguments(), process_environment())?;
    let address = loopback_address(&reload.config())?;
    let server = Server::start(reload, address, POLL_INTERVAL)?;
    let address = server.local_addr();

    println!("neotales-config web sample (publish = false)");
    println!("listening on http://{address}/");
    println!("snapshot:   http://{address}/api/config");
    println!("health:     http://{address}/health");
    println!(
        "watcher:    polling every {} ms; edit site.toml or app.env and refresh",
        POLL_INTERVAL.as_millis()
    );
    println!("stop:       Ctrl-C");

    loop {
        thread::sleep(Duration::from_secs(3600));
    }
}
