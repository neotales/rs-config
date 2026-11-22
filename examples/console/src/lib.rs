//! The console sample's configuration wiring and safe report.
//!
//! This package is a runnable sample, not a published crate. It registers the same
//! sources in the same precedence order as the web sample:
//!
//! 1. `defaults.json`, required
//! 2. `site.toml`, optional
//! 3. `app.env`, optional and expanded
//! 4. the process environment under `APP_`, with `__` as the key separator
//! 5. `--key=value` arguments
//!
//! The report prints only a fixed allowlist of keys. It never dumps the whole
//! snapshot, the environment, or a path, because a process environment can hold
//! a secret that this sample has no reason to display.

use std::{
    fmt::Write as _,
    path::{Path, PathBuf},
};

use neotales_config::{
    Arguments, Builder, Case, Config, DotenvFile, Environment, Error, JsonFile, TomlFile,
};

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
/// The name this sample gives the dotenv `${DEMO_NAME}` lookup.
pub const DOTENV_LOOKUP_NAME: &str = "DEMO_NAME";
/// The fixed value this sample gives the dotenv `${DEMO_NAME}` lookup.
pub const DOTENV_LOOKUP_VALUE: &str = "fixed dotenv lookup";

/// The only keys the sample prints.
pub const SAFE_KEYS: [&str; 6] = [
    "server.bind",
    "server.port",
    "demo.greeting",
    "demo.mode",
    "demo.banner",
    "demo.literal",
];

/// Loads the sample configuration from `directory`.
///
/// Sources are registered lowest precedence first, so `arguments` wins over the
/// environment, which wins over `app.env`, which wins over `site.toml`, which
/// wins over `defaults.json`. Both overlays are optional: an absent file is an
/// empty source, and every value still comes from the files that do exist.
///
/// # Errors
///
/// Returns a sanitized [`Error`] when a required file is missing, a file cannot
/// be read or parsed, a provider rejects its input, or a source root is not a
/// map. The error never contains a value, a file body, or a path.
pub fn build(
    directory: &Path,
    arguments: Arguments,
    environment: Environment,
) -> Result<Config, Error> {
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
        .build()
}

/// Reads the sample's environment policy over a fixed set of pairs.
///
/// Tests use this instead of the process environment so the result does not
/// depend on the shell that runs the suite. The policy is the same one
/// [`process_environment`] applies: select `APP_`, split on `__`, and lower-case
/// each component.
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

/// Builds the console report from the safe key allowlist.
///
/// Only [`SAFE_KEYS`] are read. The report names the fixed source files but
/// never prints `directory`, an arbitrary environment value, or a whole value
/// tree. A missing optional value is reported as absent.
#[must_use]
pub fn report(config: &Config, directory: &Path) -> String {
    let mut output = String::new();
    let _ = writeln!(output, "neotales-config console sample (publish = false)");
    let _ = writeln!(
        output,
        "sources, lowest to highest precedence: defaults.json (required, {}), \
         site.toml (optional, {}), app.env (optional, {}), \
         APP_* environment, --key=value arguments",
        file_state(directory, DEFAULTS_FILE),
        file_state(directory, TOML_FILE),
        file_state(directory, DOTENV_FILE),
    );
    let _ = writeln!(
        output,
        "dotenv expansion: ${{NAME}} expands in unquoted and double-quoted values; \
         single-quoted values stay literal"
    );
    let _ = writeln!(output);
    let _ = writeln!(output, "{:<18} {:<46} origin", "key", "value");

    for key in SAFE_KEYS {
        let _ = writeln!(
            output,
            "{key:<18} {:<46} {}",
            display_value(config, key),
            display_origin(config, key),
        );
    }

    output
}

/// Returns the sample's fixture directory, or `DEMO_CONFIG_DIR` when set.
///
/// The environment variable is an application switch, not a configuration
/// source. It is never printed, so an operator can point the sample at a
/// private directory without leaking that path into the report.
#[must_use]
pub fn config_directory() -> PathBuf {
    std::env::var_os("DEMO_CONFIG_DIR").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures"),
        PathBuf::from,
    )
}

fn file_state(directory: &Path, name: &str) -> &'static str {
    let path = directory.join(name);
    if path.is_file() {
        "present"
    } else if path.exists() {
        "not a regular file"
    } else {
        "absent"
    }
}

fn display_value(config: &Config, key: &str) -> String {
    match config.get::<serde_json::Value>(key) {
        Ok(serde_json::Value::String(value)) => format!("{value:?}"),
        Ok(value) => value.to_string(),
        Err(error) if error.is_missing() => String::from("(absent)"),
        Err(error) => format!("(unavailable: {error})"),
    }
}

fn display_origin(config: &Config, key: &str) -> String {
    match config.origin(key) {
        Ok(origin) => origin.to_owned(),
        Err(error) if error.is_missing() => String::from("(absent)"),
        Err(error) => format!("(unavailable: {error})"),
    }
}
