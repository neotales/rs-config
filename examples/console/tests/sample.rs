//! Behaviour tests for the console sample.

use std::{ffi::OsString, fs, path::Path, process::Command};

use neotales_config_example_console::{DOTENV_LOOKUP_VALUE, build, environment_from_pairs, report};

fn write_defaults(directory: &Path) {
    fs::write(
        directory.join("defaults.json"),
        r#"{
  "server": { "bind": "127.0.0.1", "port": 18080 },
  "demo": { "greeting": "hello from defaults.json", "mode": "defaults", "banner": false }
}
"#,
    )
    .expect("write defaults.json");
}

fn write_overlays(directory: &Path) {
    fs::write(
        directory.join("site.toml"),
        "[server]\nport = 18081\n\n[demo]\ngreeting = \"hello from site.toml\"\nmode = \"toml\"\n",
    )
    .expect("write site.toml");
    fs::write(
        directory.join("app.env"),
        "APP_demo__greeting=\"hello from app.env for ${DEMO_NAME}\"\n\
         APP_demo__mode=dotenv\n\
         APP_demo__literal='${DEMO_NAME}'\n",
    )
    .expect("write app.env");
}

fn no_arguments() -> neotales_config::Arguments {
    neotales_config::Arguments::new(Vec::new())
}

fn no_environment() -> neotales_config::Environment {
    environment_from_pairs(Vec::<(&str, &str)>::new())
}

#[test]
fn absent_optional_files_keep_the_defaults() {
    let directory = tempfile::tempdir().expect("temporary directory");
    write_defaults(directory.path());

    let config = build(directory.path(), no_arguments(), no_environment())
        .expect("the required defaults file loads");

    assert_eq!(config.get::<String>("demo.mode").unwrap(), "defaults");
    assert_eq!(
        config.get::<String>("demo.greeting").unwrap(),
        "hello from defaults.json"
    );
    assert_eq!(config.get::<u16>("server.port").unwrap(), 18080);
    assert!(!config.get::<bool>("demo.banner").unwrap());
    assert!(config.get::<String>("demo.literal").is_err());
    assert_eq!(config.origin("demo.mode").unwrap(), "defaults.json");
}

#[test]
fn optional_overlays_win_over_defaults_in_registration_order() {
    let directory = tempfile::tempdir().expect("temporary directory");
    write_defaults(directory.path());
    write_overlays(directory.path());

    let config =
        build(directory.path(), no_arguments(), no_environment()).expect("the overlays load");

    assert_eq!(config.get::<u16>("server.port").unwrap(), 18081);
    assert_eq!(config.origin("server.port").unwrap(), "site.toml");
    assert_eq!(config.get::<String>("demo.mode").unwrap(), "dotenv");
    assert_eq!(
        config.get::<String>("demo.greeting").unwrap(),
        format!("hello from app.env for {DOTENV_LOOKUP_VALUE}")
    );
    assert_eq!(
        config.get::<String>("demo.literal").unwrap(),
        "${DEMO_NAME}"
    );
    assert_eq!(config.origin("demo.greeting").unwrap(), "app.env");
    assert_eq!(config.origin("demo.literal").unwrap(), "app.env");
}

#[test]
fn environment_overrides_dotenv_and_ignores_unselected_names() {
    let directory = tempfile::tempdir().expect("temporary directory");
    write_defaults(directory.path());
    write_overlays(directory.path());

    let environment = environment_from_pairs([
        ("APP_DEMO__MODE", "environment"),
        ("TOP_SECRET", "must-not-be-read"),
    ]);
    let config =
        build(directory.path(), no_arguments(), environment).expect("the environment source loads");

    assert_eq!(config.get::<String>("demo.mode").unwrap(), "environment");
    assert_eq!(config.origin("demo.mode").unwrap(), "environment");
    assert!(!report(&config, directory.path()).contains("must-not-be-read"));
}

#[test]
fn arguments_win_over_the_environment_and_accept_flags() {
    let directory = tempfile::tempdir().expect("temporary directory");
    write_defaults(directory.path());
    write_overlays(directory.path());

    let environment = environment_from_pairs([("APP_DEMO__MODE", "environment")]);
    let arguments = neotales_config::Arguments::new([
        OsString::from("--demo.mode=arguments"),
        OsString::from("--demo.banner"),
    ]);
    let config =
        build(directory.path(), arguments, environment).expect("the argument source loads");

    assert_eq!(config.get::<String>("demo.mode").unwrap(), "arguments");
    assert_eq!(config.origin("demo.mode").unwrap(), "arguments");
    assert!(config.get::<bool>("demo.banner").unwrap());
    assert_eq!(config.origin("demo.banner").unwrap(), "arguments");
}

#[test]
fn a_missing_required_defaults_file_is_a_sanitized_error() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let error = build(directory.path(), no_arguments(), no_environment())
        .expect_err("defaults.json is required");

    assert!(error.is_source_failure());
    let text = error.to_string();
    assert!(!text.contains(&directory.path().display().to_string()));
    assert!(text.contains("defaults.json"));
}

#[test]
fn report_names_absent_optional_files_and_only_safe_keys() {
    let directory = tempfile::tempdir().expect("temporary directory");
    write_defaults(directory.path());
    let config =
        build(directory.path(), no_arguments(), no_environment()).expect("the defaults file loads");

    let text = report(&config, directory.path());

    assert!(text.contains("site.toml (optional, absent)"));
    assert!(text.contains("app.env (optional, absent)"));
    assert!(text.contains("demo.greeting"));
    assert!(text.contains("defaults.json"));
    assert!(!text.contains("TOP_SECRET"));
}

#[test]
fn the_binary_does_not_print_an_unselected_environment_value() {
    let directory = tempfile::tempdir().expect("temporary directory");
    write_defaults(directory.path());

    let output = Command::new(env!("CARGO_BIN_EXE_neotales-config-example-console"))
        .env("DEMO_CONFIG_DIR", directory.path())
        .env("TOP_SECRET", "canary-value-that-must-not-appear")
        .env("APP_DEMO__MODE", "environment")
        .output()
        .expect("run the console sample");

    assert!(
        output.status.success(),
        "sample exited with {}",
        output.status
    );
    let stdout = String::from_utf8(output.stdout).expect("sample stdout is UTF-8");
    assert!(stdout.contains("environment"));
    assert!(stdout.contains("demo.mode"));
    assert!(!stdout.contains("canary-value-that-must-not-appear"));
    assert!(!stdout.contains(&directory.path().display().to_string()));
}
