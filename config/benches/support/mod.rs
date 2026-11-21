//! Generated benchmark fixtures and the in-memory model they represent.
//!
//! Every document is produced from fixed integer formulas, never from a random
//! source and never from a file outside the temporary directory. The same
//! logical configuration is rendered into each dialect, so a comparison between
//! formats is a comparison of the dialect's parser and not of its fixture.
//!
//! The values are synthetic and deliberately non-secret: loopback hosts,
//! `fixture`-style names, and a database URL with a user but no password. No
//! document here is a real configuration file.

#![allow(
    dead_code,
    reason = "helpers used only when the matching provider feature is enabled"
)]

use std::{
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
};

use neotales_config::Value;
use tempfile::TempDir;

/// The document size used by the benchmark groups.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum Size {
    /// One service: the shape of a small hand-written file.
    Small,
    /// Many services: the shape of a generated or fleet-wide file.
    Realistic,
}

impl Size {
    /// Both sizes, smallest first.
    pub(crate) const BOTH: [Self; 2] = [Self::Small, Self::Realistic];

    /// The label used in a benchmark id and in a fixture filename.
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Small => "small",
            Self::Realistic => "realistic",
        }
    }

    /// The number of service tables in the document.
    pub(crate) const fn services(self) -> usize {
        match self {
            Self::Small => 1,
            Self::Realistic => 96,
        }
    }
}

/// The file dialects the benchmark renders.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum Format {
    /// RFC 8259 JSON.
    Json,
    /// JSON5 1.0.
    Json5,
    /// JSON with comments.
    Jsonc,
    /// TOML 1.1 as implemented by the `toml` crate.
    Toml,
    /// YAML 1.2 core schema.
    Yaml,
    /// The INI dialect this crate defines.
    Ini,
    /// A dotenv document.
    Dotenv,
}

impl Format {
    /// Every dialect, in the order the documentation lists them.
    pub(crate) const ALL: [Self; 7] = [
        Self::Json,
        Self::Json5,
        Self::Jsonc,
        Self::Toml,
        Self::Yaml,
        Self::Ini,
        Self::Dotenv,
    ];

    /// The fixture filename extension.
    const fn extension(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Json5 => "json5",
            Self::Jsonc => "jsonc",
            Self::Toml => "toml",
            Self::Yaml => "yaml",
            Self::Ini => "ini",
            Self::Dotenv => "env",
        }
    }
}

/// One generated fixture.
struct Entry {
    size: Size,
    format: Format,
    path: PathBuf,
    bytes: u64,
}

/// A temporary directory holding every fixture, alive for the benchmark run.
pub(crate) struct Fixtures {
    // Kept for its destructor: dropping it removes every generated file.
    _directory: TempDir,
    entries: Vec<Entry>,
}

impl Fixtures {
    /// Writes every dialect at every size into a fresh temporary directory.
    ///
    /// The files are generated even when the matching feature is off, because
    /// the generator has no parser dependency. This keeps the fixture identical
    /// whatever subset of features the benchmark is built with.
    pub(crate) fn new() -> Self {
        let directory = TempDir::new().expect("temporary directory");
        let mut entries = Vec::new();
        for size in Size::BOTH {
            for format in Format::ALL {
                let text = render(format, size);
                let path =
                    directory
                        .path()
                        .join(format!("{}.{}", size.label(), format.extension()));
                fs::write(&path, text.as_bytes()).expect("write fixture");
                entries.push(Entry {
                    size,
                    format,
                    path,
                    bytes: u64::try_from(text.len()).expect("fixture fits u64"),
                });
            }
        }
        Self {
            _directory: directory,
            entries,
        }
    }

    /// Returns the path of one fixture.
    pub(crate) fn path(&self, format: Format, size: Size) -> &Path {
        &self.entry(format, size).path
    }

    /// Returns the byte length of one fixture.
    pub(crate) fn bytes(&self, format: Format, size: Size) -> u64 {
        self.entry(format, size).bytes
    }

    fn entry(&self, format: Format, size: Size) -> &Entry {
        self.entries
            .iter()
            .find(|entry| entry.format == format && entry.size == size)
            .expect("the fixture was generated")
    }
}

/// Returns a small overlay that a later source would supply in a real build.
pub(crate) fn overlay() -> Value {
    Value::map([
        ("http", Value::map([("port", Value::from(9090_i64))])),
        ("logging", Value::map([("level", Value::from("debug"))])),
    ])
}

/// Returns the in-memory form of the generated configuration.
///
/// The memory baseline builds from this tree. It matches the JSON family's
/// shape: numbers stay [`Value::Integer`] and `targets` stays an array. INI and
/// dotenv documents can only express strings and cannot express the array, so
/// their benchmark measures the same document volume through their own value
/// shape.
pub(crate) fn value(size: Size) -> Value {
    let mut services = std::collections::BTreeMap::new();
    for index in 0..size.services() {
        let service = Service::new(index);
        services.insert(
            service.name.clone(),
            Value::map([
                ("name", Value::from(service.name.clone())),
                ("workers", Value::from(service.workers)),
                ("debug", Value::from(service.debug)),
                ("host", Value::from("127.0.0.1")),
                ("port", Value::from(service.port)),
                ("read_ms", Value::from(service.read_ms)),
                ("write_ms", Value::from(service.write_ms)),
                ("url", Value::from(service.url.clone())),
                ("pool_min", Value::from(service.pool_min)),
                ("pool_max", Value::from(service.pool_max)),
                ("level", Value::from("info")),
                (
                    "targets",
                    Value::array([Value::from("stdout"), Value::from("file")]),
                ),
            ]),
        );
    }

    Value::map([
        ("root_name", Value::from("gateway")),
        ("ratio", Value::from(0.25_f64)),
        ("large", Value::from(9_223_372_036_854_775_807_i64)),
        (
            "service",
            Value::map([
                ("name", Value::from("gateway")),
                ("workers", Value::from(4_i64)),
                ("debug", Value::from(false)),
            ]),
        ),
        (
            "http",
            Value::map([
                ("host", Value::from("127.0.0.1")),
                ("port", Value::from(8080_i64)),
                (
                    "timeouts",
                    Value::map([
                        ("read_ms", Value::from(5000_i64)),
                        ("write_ms", Value::from(2500_i64)),
                    ]),
                ),
            ]),
        ),
        (
            "database",
            Value::map([
                ("url", Value::from("postgres://app@db0.internal/app")),
                (
                    "pool",
                    Value::map([("min", Value::from(2_i64)), ("max", Value::from(20_i64))]),
                ),
            ]),
        ),
        (
            "logging",
            Value::map([
                ("level", Value::from("info")),
                (
                    "targets",
                    Value::array([Value::from("stdout"), Value::from("file")]),
                ),
            ]),
        ),
        ("services", Value::Map(services)),
    ])
}

/// One deterministic service row.
struct Service {
    name: String,
    workers: i64,
    debug: bool,
    port: i64,
    read_ms: i64,
    write_ms: i64,
    url: String,
    pool_min: i64,
    pool_max: i64,
}

impl Service {
    fn new(index: usize) -> Self {
        let index = i64::try_from(index).expect("the service index fits i64");
        Self {
            name: format!("service_{index:02}"),
            workers: 4 + index % 8,
            debug: index % 2 == 0,
            port: 8080 + index,
            read_ms: 5000 + index % 5 * 100,
            write_ms: 2500 + index % 3 * 100,
            url: format!("postgres://app@db{}.internal/app", index % 4),
            pool_min: 2 + index % 4,
            pool_max: 20 + index % 8,
        }
    }
}

/// Renders one dialect at one size.
fn render(format: Format, size: Size) -> String {
    match format {
        Format::Json => json(size),
        Format::Json5 => json5(size),
        Format::Jsonc => jsonc(size),
        Format::Toml => toml(size),
        Format::Yaml => yaml(size),
        Format::Ini => ini(size),
        Format::Dotenv => dotenv(size),
    }
}

fn json(size: Size) -> String {
    let mut out = String::from("{\n");
    out.push_str("  \"root_name\": \"gateway\",\n");
    out.push_str("  \"ratio\": 0.25,\n");
    out.push_str("  \"large\": 9223372036854775807,\n");
    out.push_str("  \"service\": {\"name\": \"gateway\", \"workers\": 4, \"debug\": false},\n");
    out.push_str(
        "  \"http\": {\"host\": \"127.0.0.1\", \"port\": 8080, \"timeouts\": \
         {\"read_ms\": 5000, \"write_ms\": 2500}},\n",
    );
    out.push_str(
        "  \"database\": {\"url\": \"postgres://app@db0.internal/app\", \"pool\": \
         {\"min\": 2, \"max\": 20}},\n",
    );
    out.push_str("  \"logging\": {\"level\": \"info\", \"targets\": [\"stdout\", \"file\"]},\n");
    out.push_str("  \"services\": {");
    for index in 0..size.services() {
        let service = Service::new(index);
        if index > 0 {
            out.push(',');
        }
        write!(
            out,
            "\n    \"{}\": {{\"name\": \"{}\", \"workers\": {}, \"debug\": {}, \
             \"host\": \"127.0.0.1\", \"port\": {}, \"read_ms\": {}, \"write_ms\": {}, \
             \"url\": \"{}\", \"pool_min\": {}, \"pool_max\": {}, \"level\": \"info\", \
             \"targets\": [\"stdout\", \"file\"]}}",
            service.name,
            service.name,
            service.workers,
            service.debug,
            service.port,
            service.read_ms,
            service.write_ms,
            service.url,
            service.pool_min,
            service.pool_max,
        )
        .expect("writing to a String cannot fail");
    }
    out.push_str("\n  }\n}\n");
    out
}

fn json5(size: Size) -> String {
    let mut out = String::from("{\n");
    out.push_str("  // Synthetic benchmark fixture; no value is a real credential.\n");
    out.push_str("  root_name: 'gateway',\n");
    out.push_str("  ratio: .25,\n");
    out.push_str("  large: 9223372036854775807,\n");
    out.push_str("  service: { name: 'gateway', workers: 4, debug: false },\n");
    out.push_str(
        "  http: { host: '127.0.0.1', port: 0x1F90, timeouts: \
         { read_ms: 5000, write_ms: 2500 } },\n",
    );
    out.push_str(
        "  database: { url: 'postgres://app@db0.internal/app', pool: \
         { min: 2, max: 20 } },\n",
    );
    out.push_str("  logging: { level: 'info', targets: ['stdout', 'file'] },\n");
    out.push_str("  services: {");
    for index in 0..size.services() {
        let service = Service::new(index);
        if index > 0 {
            out.push(',');
        }
        write!(
            out,
            "\n    {}: {{ name: '{}', workers: {}, debug: {}, host: '127.0.0.1', \
             port: 0x{:X}, read_ms: {}, write_ms: {}, url: '{}', pool_min: {}, \
             pool_max: {}, level: 'info', targets: ['stdout', 'file'] }}",
            service.name,
            service.name,
            service.workers,
            service.debug,
            service.port,
            service.read_ms,
            service.write_ms,
            service.url,
            service.pool_min,
            service.pool_max,
        )
        .expect("writing to a String cannot fail");
    }
    out.push_str("\n  },\n}\n");
    out
}

fn jsonc(size: Size) -> String {
    let mut out = String::from("{\n");
    out.push_str("  // Synthetic benchmark fixture; editors keep this comment.\n");
    out.push_str("  \"root_name\": \"gateway\",\n");
    out.push_str("  \"ratio\": 0.25,\n");
    out.push_str("  \"large\": 9223372036854775807,\n");
    out.push_str("  \"service\": {\"name\": \"gateway\", \"workers\": 4, \"debug\": false},\n");
    out.push_str(
        "  \"http\": {\"host\": \"127.0.0.1\", \"port\": 8080, \"timeouts\": \
         {\"read_ms\": 5000, \"write_ms\": 2500}},\n",
    );
    out.push_str(
        "  \"database\": {\"url\": \"postgres://app@db0.internal/app\", \"pool\": \
         {\"min\": 2, \"max\": 20}},\n",
    );
    out.push_str("  \"logging\": {\"level\": \"info\", \"targets\": [\"stdout\", \"file\"]},\n");
    out.push_str("  \"services\": {");
    for index in 0..size.services() {
        let service = Service::new(index);
        if index > 0 {
            out.push(',');
        }
        write!(
            out,
            "\n    /* generated row */\n    \"{}\": {{\"name\": \"{}\", \"workers\": {}, \
             \"debug\": {}, \"host\": \"127.0.0.1\", \"port\": {}, \"read_ms\": {}, \
             \"write_ms\": {}, \"url\": \"{}\", \"pool_min\": {}, \"pool_max\": {}, \
             \"level\": \"info\", \"targets\": [\"stdout\", \"file\"]}}",
            service.name,
            service.name,
            service.workers,
            service.debug,
            service.port,
            service.read_ms,
            service.write_ms,
            service.url,
            service.pool_min,
            service.pool_max,
        )
        .expect("writing to a String cannot fail");
    }
    out.push_str("\n  }\n}\n");
    out
}

fn toml(size: Size) -> String {
    let mut out = String::from("# Synthetic benchmark fixture; no value is a real credential.\n");
    out.push_str("root_name = \"gateway\"\n");
    out.push_str("ratio = 0.25\n");
    out.push_str("large = 9223372036854775807\n\n");
    out.push_str("[service]\nname = \"gateway\"\nworkers = 4\ndebug = false\n\n");
    out.push_str("[http]\nhost = \"127.0.0.1\"\nport = 8080\n\n");
    out.push_str("[http.timeouts]\nread_ms = 5000\nwrite_ms = 2500\n\n");
    out.push_str("[database]\nurl = \"postgres://app@db0.internal/app\"\n\n");
    out.push_str("[database.pool]\nmin = 2\nmax = 20\n\n");
    out.push_str("[logging]\nlevel = \"info\"\ntargets = [\"stdout\", \"file\"]\n");
    for index in 0..size.services() {
        let service = Service::new(index);
        write!(
            out,
            "\n[services.{}]\nname = \"{}\"\nworkers = {}\ndebug = {}\nhost = \"127.0.0.1\"\n\
             port = {}\nread_ms = {}\nwrite_ms = {}\nurl = \"{}\"\npool_min = {}\n\
             pool_max = {}\nlevel = \"info\"\ntargets = [\"stdout\", \"file\"]\n",
            service.name,
            service.name,
            service.workers,
            service.debug,
            service.port,
            service.read_ms,
            service.write_ms,
            service.url,
            service.pool_min,
            service.pool_max,
        )
        .expect("writing to a String cannot fail");
    }
    out
}

fn yaml(size: Size) -> String {
    let mut out = String::from("# Synthetic benchmark fixture; no value is a real credential.\n");
    out.push_str("root_name: gateway\nratio: 0.25\nlarge: 9223372036854775807\n");
    out.push_str("service:\n  name: gateway\n  workers: 4\n  debug: false\n");
    out.push_str("http:\n  host: 127.0.0.1\n  port: 8080\n");
    out.push_str("  timeouts:\n    read_ms: 5000\n    write_ms: 2500\n");
    out.push_str("database:\n  url: postgres://app@db0.internal/app\n");
    out.push_str("  pool:\n    min: 2\n    max: 20\n");
    out.push_str("logging:\n  level: info\n  targets:\n    - stdout\n    - file\n");
    out.push_str("services:\n");
    for index in 0..size.services() {
        let service = Service::new(index);
        write!(
            out,
            "  {}:\n    name: {}\n    workers: {}\n    debug: {}\n    host: 127.0.0.1\n    \
             port: {}\n    read_ms: {}\n    write_ms: {}\n    \
             url: {}\n    pool_min: {}\n    pool_max: {}\n    level: info\n    \
             targets:\n      - stdout\n      - file\n",
            service.name,
            service.name,
            service.workers,
            service.debug,
            service.port,
            service.read_ms,
            service.write_ms,
            service.url,
            service.pool_min,
            service.pool_max,
        )
        .expect("writing to a String cannot fail");
    }
    out
}

fn ini(size: Size) -> String {
    let mut out = String::from("; Synthetic benchmark fixture; no value is a real credential.\n");
    out.push_str("root_name = gateway\nratio = 0.25\nlarge = 9223372036854775807\n\n");
    out.push_str("[service]\nname = gateway\nworkers = 4\ndebug = false\n\n");
    out.push_str("[http]\nhost = 127.0.0.1\nport = 8080\n\n");
    out.push_str("[http.timeouts]\nread_ms = 5000\nwrite_ms = 2500\n\n");
    out.push_str("[database]\nurl = postgres://app@db0.internal/app\n\n");
    out.push_str("[database.pool]\nmin = 2\nmax = 20\n\n");
    out.push_str("[logging]\nlevel = info\ntargets = stdout, file\n");
    for index in 0..size.services() {
        let service = Service::new(index);
        write!(
            out,
            "\n[services.{}]\nname = {}\nworkers = {}\ndebug = {}\nhost = 127.0.0.1\n\
             port = {}\nread_ms = {}\nwrite_ms = {}\nurl = {}\npool_min = {}\n\
             pool_max = {}\nlevel = info\ntargets = stdout, file\n",
            service.name,
            service.name,
            service.workers,
            service.debug,
            service.port,
            service.read_ms,
            service.write_ms,
            service.url,
            service.pool_min,
            service.pool_max,
        )
        .expect("writing to a String cannot fail");
    }
    out
}

fn dotenv(size: Size) -> String {
    let mut out = String::from("# Synthetic benchmark fixture; no value is a real credential.\n");
    out.push_str("APP_root_name=gateway\nAPP_ratio=0.25\nAPP_large=9223372036854775807\n");
    out.push_str("APP_service.name=gateway\nAPP_service.workers=4\nAPP_service.debug=false\n");
    out.push_str("APP_http.host=127.0.0.1\nAPP_http.port=8080\n");
    out.push_str("APP_http.timeouts.read_ms=5000\nAPP_http.timeouts.write_ms=2500\n");
    out.push_str("APP_database.url=postgres://app@db0.internal/app\n");
    out.push_str("APP_database.pool.min=2\nAPP_database.pool.max=20\n");
    out.push_str("APP_logging.level=info\nAPP_logging.targets=stdout,file\n");
    for index in 0..size.services() {
        let service = Service::new(index);
        let prefix = format!("APP_services.{}", service.name);
        write!(
            out,
            "{prefix}.name={name}\n{prefix}.workers={workers}\n{prefix}.debug={debug}\n\
             {prefix}.host=127.0.0.1\n{prefix}.port={port}\n{prefix}.read_ms={read_ms}\n\
             {prefix}.write_ms={write_ms}\n{prefix}.url={url}\n\
             {prefix}.pool_min={pool_min}\n{prefix}.pool_max={pool_max}\n\
             {prefix}.level=info\n{prefix}.targets=stdout,file\n",
            name = service.name,
            workers = service.workers,
            debug = service.debug,
            port = service.port,
            read_ms = service.read_ms,
            write_ms = service.write_ms,
            url = service.url,
            pool_min = service.pool_min,
            pool_max = service.pool_max,
        )
        .expect("writing to a String cannot fail");
    }
    out
}
