//! The core stays memory-only when every provider feature is off.

#![cfg(not(any(
    feature = "args",
    feature = "environment",
    feature = "dotenv",
    feature = "json",
    feature = "json5",
    feature = "jsonc",
    feature = "toml",
    feature = "yaml",
    feature = "ini"
)))]

use neotales_config::{Builder, SourceError, Value};

const ABSENT: [&str; 8] = [
    "neotales-dotenv",
    "serde_json",
    "json5",
    "jsonc-parser",
    "toml",
    "saphyr",
    "tempfile",
    "winnow",
];

#[test]
fn a_memory_source_still_builds_without_a_provider() {
    let config = Builder::new()
        .memory(
            "defaults",
            Value::map([("http", Value::map([("port", Value::from(8080_i64))]))]),
        )
        .build()
        .expect("a memory-only build succeeds");

    assert_eq!(config.get::<u16>("http.port").unwrap(), 8080);
}

#[test]
fn a_caller_source_still_builds_without_a_provider() {
    let config = Builder::new()
        .source("computed", || {
            Ok::<_, SourceError>(Value::map([("workers", Value::from(4_i64))]))
        })
        .build()
        .expect("a caller-supplied source builds");

    assert_eq!(config.get::<u8>("workers").unwrap(), 4);
}

#[test]
fn the_default_dependency_graph_has_no_parser() {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest
        .parent()
        .expect("the crate directory is inside the workspace root");
    // A separate target directory keeps this invocation from waiting on the lock the running
    // `cargo test` holds.
    let target = tempfile::tempdir().expect("a temporary target directory");
    let output = std::process::Command::new(env!("CARGO"))
        .args([
            "tree",
            "-p",
            "neotales-config",
            "--no-default-features",
            "--prefix",
            "none",
            "--edges",
            "normal",
        ])
        .current_dir(workspace)
        .env("CARGO_TARGET_DIR", target.path())
        .output()
        .expect("cargo tree runs");

    assert!(
        output.status.success(),
        "cargo tree failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let graph = String::from_utf8(output.stdout).expect("cargo tree prints UTF-8");
    for dependency in ABSENT {
        assert!(
            !graph.contains(dependency),
            "{dependency} is in the default graph:\n{graph}"
        );
    }
    // Serialization support itself is always present; only the format parsers are optional.
    assert!(graph.contains("serde"), "{graph}");
}
