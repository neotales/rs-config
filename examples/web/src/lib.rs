//! The web sample's configuration wiring, safe loopback HTTP server, and watcher.
//!
//! This package is a runnable sample, not a published crate. It exists to show
//! four things at once:
//!
//! - sources registered in one documented precedence order;
//! - optional files that may appear, change, or disappear while the process is
//!   running;
//! - a polling watcher with an explicit handle whose `stop`/`Drop` joins its
//!   thread;
//! - a snapshot view that prints only an allowlist of keys and their origins.
//!
//! The HTTP server is built on `std` and binds only a loopback address. It never
//! serves a directory, never evaluates a script, and never prints an arbitrary
//! environment value. A value that is not on [`SAFE_KEYS`] is not in a response.

use std::{
    error::Error as StdError,
    fmt::{self, Write as _},
    io::{self, BufRead, BufReader, Write as _},
    net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use neotales_config::{
    Arguments, Builder, Case, Config, DotenvFile, Environment, Error as ConfigError, JsonFile,
    Reload, TomlFile, WatchError, Watcher,
};
use serde_json::{Map, Value as JsonValue, json};

/// The required defaults document in the sample directory.
pub const DEFAULTS_FILE: &str = "defaults.json";
/// The optional TOML overlay in the sample directory.
pub const TOML_FILE: &str = "site.toml";
/// The optional dotenv overlay in the sample directory.
pub const DOTENV_FILE: &str = "app.env";
/// The environment prefix that selects a configuration variable.
pub const ENVIRONMENT_PREFIX: &str = "APP_";
/// The separator that turns one environment name into nested keys.
pub const ENVIRONMENT_SEPARATOR: &str = "__";
/// The fixed name this sample gives the dotenv `${DEMO_NAME}` lookup.
pub const DOTENV_LOOKUP_NAME: &str = "DEMO_NAME";
/// The fixed value this sample gives the dotenv `${DEMO_NAME}` lookup.
pub const DOTENV_LOOKUP_VALUE: &str = "fixed dotenv lookup";
/// The watcher interval used by the sample.
pub const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// The only keys the HTML and JSON views expose.
pub const SAFE_KEYS: [&str; 6] = [
    "server.bind",
    "server.port",
    "demo.greeting",
    "demo.mode",
    "demo.banner",
    "demo.literal",
];

/// Loads the sample configuration and keeps its sources for reloading.
///
/// Sources are registered lowest precedence first, so `arguments` wins over the
/// environment, which wins over `app.env`, which wins over `site.toml`, which
/// wins over `defaults.json`. `site.toml` and `app.env` are optional.
///
/// # Errors
///
/// Returns a sanitized [`ConfigError`] when a required file is missing, a file
/// cannot be read or parsed, a provider rejects its input, or a source root is
/// not a map. No value, file body, or path is included.
pub fn build_reload(
    directory: &Path,
    arguments: Arguments,
    environment: Environment,
) -> Result<Reload, ConfigError> {
    let defaults = JsonFile::new(directory.join(DEFAULTS_FILE));
    let overlay = TomlFile::new(directory.join(TOML_FILE)).optional(true);
    let dotenv = DotenvFile::new(directory.join(DOTENV_FILE))
        .optional(true)
        .prefix(ENVIRONMENT_PREFIX)
        .separator(ENVIRONMENT_SEPARATOR)
        // A fixed lookup keeps the sample offline and reproducible. It affects
        // `${NAME}` expansion only; assignment selection still uses the prefix.
        .with_environment([(DOTENV_LOOKUP_NAME, DOTENV_LOOKUP_VALUE)]);

    Builder::new()
        .json_file("defaults.json", defaults)
        .toml_file("site.toml", overlay)
        .dotenv_file("app.env", dotenv)
        .environment("environment", environment)
        .arguments("arguments", arguments)
        .build_reloadable()
}

/// Reads the sample's environment policy over a fixed set of pairs.
///
/// Tests use this instead of the process environment so a snapshot does not
/// depend on the shell that runs the suite.
#[must_use]
pub fn environment_from_pairs<K, V>(pairs: impl IntoIterator<Item = (K, V)>) -> Environment
where
    K: Into<String>,
    V: Into<String>,
{
    configured_environment(Environment::from_pairs(pairs))
}

/// Reads the process environment with the sample's prefix and separator.
///
/// The provider reads the process once per load and never writes to it.
#[must_use]
pub fn process_environment() -> Environment {
    configured_environment(Environment::process())
}

fn configured_environment(environment: Environment) -> Environment {
    environment
        .prefix(ENVIRONMENT_PREFIX)
        .separator(ENVIRONMENT_SEPARATOR)
        .case(Case::Lower)
}

/// Reads the process arguments as the highest-precedence source.
///
/// The accepted syntax is `--key=value`, `--key` for `true`, and `--no-key` for
/// `false`. There is no `--key value` form and no `--` terminator.
#[must_use]
pub fn process_arguments() -> Arguments {
    Arguments::from_process()
}

/// Returns the sample's fixture directory, or `DEMO_CONFIG_DIR` when set.
///
/// The environment variable is an application switch, not a configuration
/// source. It is never printed.
#[must_use]
pub fn config_directory() -> PathBuf {
    std::env::var_os("DEMO_CONFIG_DIR").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures"),
        PathBuf::from,
    )
}

/// Why the sample server could not start or bind.
#[derive(Debug)]
pub enum StartError {
    /// A key needed for the listener was missing or had the wrong type.
    Config(ConfigError),
    /// `server.bind` is not an IPv4 address.
    InvalidBind,
    /// The requested address is not a loopback address.
    NotLoopback(SocketAddr),
    /// The listener could not be created or configured.
    Bind(io::Error),
    /// The polling watcher could not be started.
    Watch(WatchError),
    /// The HTTP thread could not be spawned.
    Thread(io::Error),
}

impl fmt::Display for StartError {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(error) => write!(output, "invalid sample configuration: {error}"),
            Self::InvalidBind => output.write_str("server.bind must be an IPv4 address"),
            Self::NotLoopback(address) => {
                write!(output, "refusing to bind non-loopback address {address}")
            }
            Self::Bind(error) => {
                write!(
                    output,
                    "the loopback listener could not be created: {error}"
                )
            }
            Self::Watch(error) => write!(output, "the polling watcher could not start: {error}"),
            Self::Thread(_) => output.write_str("the HTTP thread could not be started"),
        }
    }
}

impl StdError for StartError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Config(error) => Some(error),
            Self::Bind(error) | Self::Thread(error) => Some(error),
            Self::Watch(error) => Some(error),
            Self::InvalidBind | Self::NotLoopback(_) => None,
        }
    }
}

/// A running loopback HTTP server and its explicit watcher handle.
///
/// The HTTP thread and the polling thread are both owned here. [`Server::stop`]
/// or dropping the value stops the watcher first and then joins the HTTP
/// thread, so neither outlives its handle. The server accepts one request at a
/// time and is intended for a local sample, not production traffic.
#[derive(Debug)]
pub struct Server {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    watcher: Option<Watcher>,
}

struct Shared {
    reload: Reload,
    poll_interval: Duration,
}

impl Server {
    /// Starts the HTTP server and the polling watcher.
    ///
    /// `address` must be a loopback address. The listener is bound before the
    /// watcher starts, so a port conflict fails without leaving a polling thread
    /// behind.
    ///
    /// # Errors
    ///
    /// Returns [`StartError::NotLoopback`] for a non-loopback address,
    /// [`StartError::Bind`] when the listener cannot bind, and
    /// [`StartError::Watch`] or [`StartError::Thread`] when a thread cannot
    /// start.
    pub fn start(
        reload: Reload,
        address: SocketAddr,
        interval: Duration,
    ) -> Result<Self, StartError> {
        if !address.ip().is_loopback() {
            return Err(StartError::NotLoopback(address));
        }

        let listener = TcpListener::bind(address).map_err(StartError::Bind)?;
        listener.set_nonblocking(true).map_err(StartError::Bind)?;
        let address = listener.local_addr().map_err(StartError::Bind)?;
        let watcher = reload.start_watching(interval).map_err(StartError::Watch)?;

        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let shared = Arc::new(Shared {
            reload,
            poll_interval: interval,
        });
        let thread_shared = Arc::clone(&shared);
        let spawned = thread::Builder::new()
            .name(String::from("neotales-config-example-web"))
            .spawn(move || serve(&listener, &thread_shared, &thread_stop));

        match spawned {
            Ok(thread) => Ok(Self {
                address,
                stop,
                thread: Some(thread),
                watcher: Some(watcher),
            }),
            Err(error) => {
                watcher.stop();
                Err(StartError::Thread(error))
            }
        }
    }

    /// Returns the bound address, including the port chosen for `0`.
    #[must_use]
    pub const fn local_addr(&self) -> SocketAddr {
        self.address
    }

    /// Stops the watcher and joins the HTTP thread.
    ///
    /// This is equivalent to dropping the value and makes the shutdown point
    /// explicit in a caller.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        if let Some(watcher) = self.watcher.take() {
            watcher.stop();
        }
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            // The thread has no useful result after shutdown; a panic in a
            // handler is reported by the panic hook and must not be re-raised
            // from `Drop`.
            let _ = thread.join();
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Resolves the `server.bind` and `server.port` keys into a loopback address.
///
/// The port is read as an integer, so a value that arrived from a string
/// provider (`app.env`, the environment, or arguments) is a type mismatch, not
/// silently parsed. `server.port` stays an integer in `defaults.json` and
/// `site.toml` in this sample.
///
/// # Errors
///
/// Returns [`StartError::InvalidBind`] when the bind value is not an IPv4
/// address, or the sanitized config error when a key is missing or has the
/// wrong type.
pub fn loopback_address(config: &Config) -> Result<SocketAddr, StartError> {
    let bind = config
        .get::<String>("server.bind")
        .map_err(StartError::Config)?;
    let ip: Ipv4Addr = bind.parse().map_err(|_| StartError::InvalidBind)?;
    let port = config
        .get::<u16>("server.port")
        .map_err(StartError::Config)?;
    Ok(SocketAddr::from((ip, port)))
}

/// Builds the JSON document served at `/api/config`.
///
/// The document contains only [`SAFE_KEYS`], each with its value and origin.
/// The watcher is described by its poll interval and watched-path **count**, not
/// by a path, and [`Reload::last_error`] is already sanitized.
#[must_use]
pub fn json_snapshot(reload: &Reload, poll_interval: Duration) -> String {
    let config = reload.config();
    let entries = safe_entries(&config);

    let mut keys = Map::new();
    for entry in entries {
        let mut object = Map::new();
        match entry.value {
            Some(value) => {
                object.insert(String::from("value"), value);
            }
            None => {
                object.insert(String::from("value"), JsonValue::Null);
            }
        }
        if let Some(origin) = entry.origin {
            object.insert(String::from("origin"), JsonValue::String(origin));
        }
        if let Some(error) = entry.error {
            object.insert(String::from("error"), JsonValue::String(error));
        }
        keys.insert(entry.key.to_owned(), JsonValue::Object(object));
    }

    let document = json!({
        "sample": "neotales-config web",
        "publishable": false,
        "generation": reload.generation(),
        "watch": {
            "poll_millis": poll_interval.as_millis(),
            "path_count": reload.watched_path_count(),
        },
        "last_error": reload.last_error().map(|error| error.to_string()),
        "keys": keys,
    });

    serde_json::to_string_pretty(&document).unwrap_or_else(|_| {
        String::from("{\"error\":\"the snapshot could not be serialized as JSON\"}")
    })
}

/// Builds the HTML page served at `/`.
///
/// Every dynamic value is escaped. The page loads no script and the response
/// carries a restrictive `Content-Security-Policy`.
#[must_use]
pub fn html_snapshot(reload: &Reload, poll_interval: Duration) -> String {
    let config = reload.config();
    let entries = safe_entries(&config);
    let mut rows = String::new();
    for entry in entries {
        let value = entry.value.as_ref().map_or_else(
            || String::from("(absent)"),
            |value| match value {
                JsonValue::String(text) => text.clone(),
                other => other.to_string(),
            },
        );
        let origin = entry.origin.unwrap_or_else(|| String::from("(absent)"));
        let error = entry
            .error
            .map_or_default(|error| format!(" (unavailable: {error})"));
        let _ = write!(
            rows,
            "<tr><th>{}</th><td>{}{}</td><td>{}</td></tr>",
            escape_html(entry.key),
            escape_html(&value),
            escape_html(&error),
            escape_html(&origin),
        );
    }

    let last_error = reload.last_error().map_or_else(
        || String::from("none"),
        |error| escape_html(&error.to_string()),
    );

    format!(
        "<!doctype html>\n\
         <html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>neotales-config web sample</title>\
         <style>\
         body{{font:16px/1.45 system-ui,sans-serif;margin:2rem;max-width:60rem}}\
         table{{border-collapse:collapse;width:100%;margin:1rem 0}}\
         th,td{{border-bottom:1px solid #ccc;padding:.4rem .6rem;text-align:left;vertical-align:top}}\
         th{{white-space:nowrap}}code{{font-family:ui-monospace,monospace}}\
         .note{{color:#555}}\
         </style></head><body>\
         <h1>neotales-config web sample</h1>\
         <p class=\"note\">Non-publishable sample. Only an allowlisted set of keys is shown; \
         do not put a secret in one of these keys.</p>\
         <p>generation <code>{}</code> &middot; watcher poll <code>{} ms</code> over \
         <code>{}</code> paths &middot; last refresh error: <code>{}</code></p>\
         <table><thead><tr><th>key</th><th>value</th><th>origin</th></tr></thead>\
         <tbody>{rows}</tbody></table>\
         <p class=\"note\">Edit <code>site.toml</code> or <code>app.env</code> in the sample \
         directory and refresh this page. A missing optional file is an empty source.</p>\
         </body></html>",
        reload.generation(),
        poll_interval.as_millis(),
        reload.watched_path_count(),
        last_error,
    )
}

struct SafeEntry {
    key: &'static str,
    value: Option<JsonValue>,
    origin: Option<String>,
    error: Option<String>,
}

fn safe_entries(config: &Config) -> Vec<SafeEntry> {
    SAFE_KEYS
        .into_iter()
        .map(|key| match config.get::<JsonValue>(key) {
            Ok(value) => SafeEntry {
                key,
                value: Some(value),
                origin: config.origin(key).ok().map(str::to_owned),
                error: None,
            },
            Err(error) if error.is_missing() => SafeEntry {
                key,
                value: None,
                origin: None,
                error: None,
            },
            Err(error) => SafeEntry {
                key,
                value: None,
                origin: None,
                error: Some(error.to_string()),
            },
        })
        .collect()
}

fn escape_html(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            other => escaped.push(other),
        }
    }
    escaped
}

fn serve(listener: &TcpListener, shared: &Shared, stop: &AtomicBool) {
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _peer)) => handle_connection(stream, shared),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(20));
            }
            Err(_) => thread::sleep(Duration::from_millis(50)),
        }
    }
}

fn handle_connection(mut stream: TcpStream, shared: &Shared) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));

    let (method, path) = {
        let mut reader = BufReader::new(&stream);
        let mut request_line = String::new();
        if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
            return;
        }

        // Consume the headers without keeping their values. The server reads no
        // body and has no use for a cookie or an authorization header.
        let mut header = String::new();
        for _ in 0..64 {
            header.clear();
            match reader.read_line(&mut header) {
                Ok(0) | Err(_) => break,
                Ok(_) if header == "\r\n" || header == "\n" => break,
                Ok(_) => {}
            }
        }

        let mut parts = request_line.split_whitespace();
        let method = parts.next().unwrap_or_default().to_owned();
        let target = parts.next().unwrap_or_default();
        let path = target.split('?').next().unwrap_or("/").to_owned();
        (method, path)
    };

    if method != "GET" {
        write_response(
            &mut stream,
            "405 Method Not Allowed",
            "text/plain; charset=utf-8",
            "only GET is supported\n",
            true,
        );
        return;
    }

    match path.as_str() {
        "/" => write_response(
            &mut stream,
            "200 OK",
            "text/html; charset=utf-8",
            &html_snapshot(&shared.reload, shared.poll_interval),
            false,
        ),
        "/api/config" => write_response(
            &mut stream,
            "200 OK",
            "application/json; charset=utf-8",
            &json_snapshot(&shared.reload, shared.poll_interval),
            false,
        ),
        "/health" => write_response(
            &mut stream,
            "200 OK",
            "text/plain; charset=utf-8",
            "ok\n",
            false,
        ),
        _ => write_response(
            &mut stream,
            "404 Not Found",
            "text/plain; charset=utf-8",
            "not found\n",
            false,
        ),
    }
}

fn write_response(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &str,
    allow_get: bool,
) {
    let allow = if allow_get { "Allow: GET\r\n" } else { "" };
    let content_security_policy = if content_type.starts_with("text/html") {
        "Content-Security-Policy: default-src 'none'; style-src 'unsafe-inline'; \
         base-uri 'none'; form-action 'none'\r\n"
    } else {
        ""
    };

    let head = format!(
        "HTTP/1.1 {status}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         Connection: close\r\n\
         X-Content-Type-Options: nosniff\r\n\
         {allow}{content_security_policy}\r\n",
        body.len(),
    );

    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body.as_bytes());
    let _ = stream.flush();
}
