//! Reload one TOML file manually and through the explicit polling watcher.
//!
//! Run with:
//!
//! ```sh
//! cargo run -p neotales-config --features reload,toml --example reload
//! ```
//!
//! The example writes a temporary file, starts a watcher, replaces the file by renaming a second
//! file over it, waits for the new snapshot with a bounded timeout, and stops the watcher. It
//! prints only ports, never the file path or a whole value tree.

use std::{error::Error, fs, time::Duration};

use neotales_config::{Builder, TomlFile};

fn main() -> Result<(), Box<dyn Error>> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("app.toml");
    fs::write(&path, "[http]\nport = 8080\n")?;

    let reload = Builder::new()
        .toml_file("app", TomlFile::new(&path))
        .build_reloadable()?;
    println!("initial port: {}", reload.config().get::<u16>("http.port")?);

    // Manual refresh; no thread and no watcher involved.
    println!("manual refresh: {:?}", reload.refresh());

    let subscription = reload.subscribe();
    let watcher = reload.start_watching(Duration::from_millis(50))?;

    // Replace the file atomically so the poller never reads a half-written document.
    let replacement = directory.path().join("next.toml");
    fs::write(&replacement, "[http]\nport = 9090\n")?;
    fs::rename(&replacement, &path)?;

    match subscription.recv_timeout(Duration::from_secs(5)) {
        Ok(config) => println!("reloaded port: {}", config.get::<u16>("http.port")?),
        Err(error) => println!("no reload observed: {error}"),
    }

    watcher.stop();
    Ok(())
}
