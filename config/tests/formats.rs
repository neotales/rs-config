//! End-to-end reads for each format feature, against the checked-in fixtures.
#![cfg_attr(
    not(any(
        feature = "json",
        feature = "json5",
        feature = "jsonc",
        feature = "toml",
        feature = "yaml",
        feature = "ini"
    )),
    allow(unused_imports, dead_code)
)]

use std::path::{Path, PathBuf};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/formats")
        .join(name)
}

#[cfg(any(
    feature = "json",
    feature = "json5",
    feature = "jsonc",
    feature = "toml",
    feature = "yaml"
))]
fn assert_gateway(config: &neotales_config::Config) {
    use neotales_config::Value;

    assert_eq!(config.get::<String>("service.name").unwrap(), "gateway");
    assert_eq!(config.get::<u8>("service.workers").unwrap(), 4);
    assert!(!config.get::<bool>("service.debug").unwrap());
    assert_eq!(config.get::<String>("http.host").unwrap(), "127.0.0.1");
    assert_eq!(config.get::<u16>("http.port").unwrap(), 8080);
    assert_eq!(config.get::<u64>("http.timeouts.read_ms").unwrap(), 5000);
    assert_eq!(config.get::<u64>("http.timeouts.write_ms").unwrap(), 2500);
    assert_eq!(
        config.get::<String>("database.url").unwrap(),
        "postgres://app@db.internal/app"
    );
    assert_eq!(config.get::<u8>("database.pool.max").unwrap(), 20);
    assert_eq!(config.get::<String>("logging.level").unwrap(), "info");
    assert_eq!(
        config.get::<Vec<String>>("logging.targets").unwrap(),
        vec!["stdout".to_owned(), "file".to_owned()]
    );
    assert!((config.get::<f64>("ratio").unwrap() - 0.25).abs() < f64::EPSILON);
    assert_eq!(config.origin("http.port").unwrap(), "gateway");
    assert_eq!(config.value().map(Value::type_name), Some("map"));
}

/// A number above `i64::MAX`; TOML cannot spell one, so its fixture has a different check.
#[cfg(any(
    feature = "json",
    feature = "json5",
    feature = "jsonc",
    feature = "yaml"
))]
fn assert_big(config: &neotales_config::Config) {
    assert_eq!(
        config.get::<i128>("big").unwrap(),
        18_446_744_073_709_551_617
    );
}

#[cfg(feature = "json")]
#[test]
fn json_reads_the_fixture_and_merges_as_a_later_source() {
    use neotales_config::{Builder, JsonFile, Value};

    let config = Builder::new()
        .memory(
            "defaults",
            Value::map([("http", Value::map([("port", Value::from(1_i64))]))]),
        )
        .json_file("gateway", JsonFile::new(fixture("defaults.json")))
        .build()
        .expect("the JSON fixture parses");
    assert_gateway(&config);
    assert_big(&config);
    assert_eq!(config.origin("http.port").unwrap(), "gateway");
    assert_eq!(config.origin("http").unwrap(), "defaults");
}

#[cfg(feature = "json5")]
#[test]
fn json5_reads_the_fixture_and_its_extensions() {
    use neotales_config::{Builder, Json5File};

    let config = Builder::new()
        .json5_file("gateway", Json5File::new(fixture("defaults.json5")))
        .build()
        .expect("the JSON5 fixture parses");
    assert_gateway(&config);
    assert_big(&config);
}

#[cfg(feature = "jsonc")]
#[test]
fn jsonc_reads_the_fixture_comments_and_all() {
    use neotales_config::{Builder, JsoncFile};

    let config = Builder::new()
        .jsonc_file("gateway", JsoncFile::new(fixture("defaults.jsonc")))
        .build()
        .expect("the JSONC fixture parses");
    assert_gateway(&config);
    assert_big(&config);
}

#[cfg(feature = "toml")]
#[test]
fn toml_reads_the_fixture_and_keeps_a_date_time_typed() {
    use neotales_config::{Builder, DateTime, TomlFile};

    let config = Builder::new()
        .toml_file("gateway", TomlFile::new(fixture("defaults.toml")))
        .build()
        .expect("the TOML fixture parses");
    assert_gateway(&config);
    assert_eq!(config.get::<i128>("large").unwrap(), i128::from(i64::MAX));
    assert_eq!(
        config.get::<DateTime>("service.started").unwrap().as_str(),
        "1979-05-27T07:32:00"
    );
    assert!(
        config
            .get::<String>("service.started")
            .unwrap_err()
            .is_type_mismatch()
    );
}

#[cfg(feature = "yaml")]
#[test]
fn yaml_reads_the_fixture() {
    use neotales_config::{Builder, YamlFile};

    let config = Builder::new()
        .yaml_file("gateway", YamlFile::new(fixture("defaults.yaml")))
        .build()
        .expect("the YAML fixture parses");
    assert_gateway(&config);
    assert_big(&config);
}

#[cfg(feature = "ini")]
#[test]
fn ini_reads_the_fixture_and_keeps_every_value_a_string() {
    use neotales_config::{Builder, IniFile};

    let config = Builder::new()
        .ini_file("gateway", IniFile::new(fixture("defaults.ini")))
        .build()
        .expect("the INI fixture parses");
    assert_eq!(config.get::<String>("root_name").unwrap(), "gateway");
    assert_eq!(config.get::<String>("service.name").unwrap(), "gateway");
    assert_eq!(config.get::<String>("service.workers").unwrap(), "4");
    assert_eq!(config.get::<String>("http.port").unwrap(), "8080");
    assert_eq!(
        config.get::<String>("logging.targets").unwrap(),
        "stdout, file"
    );
    assert!(
        config
            .get::<u16>("http.port")
            .unwrap_err()
            .is_type_mismatch()
    );

    // A dotted section name is one literal key, never a nested path.
    let root = config.value().unwrap().as_map().unwrap();
    let timeouts = root["http.timeouts"].as_map().unwrap();
    assert_eq!(timeouts["read_ms"], neotales_config::Value::from("5000"));
    assert!(!config.has("http.timeouts.read_ms").unwrap());
}

#[cfg(feature = "json")]
#[test]
fn json_rereads_the_path_on_every_load() {
    use neotales_config::{JsonFile, Source, Value};

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("gateway.json");
    let source = JsonFile::new(&path);
    std::fs::write(&path, r#"{"port": 8080}"#).unwrap();
    assert_eq!(
        source.load().unwrap().as_map().unwrap()["port"],
        Value::Integer(8080)
    );
    std::fs::write(&path, r#"{"port": 9090}"#).unwrap();
    assert_eq!(
        source.load().unwrap().as_map().unwrap()["port"],
        Value::Integer(9090)
    );
}

#[cfg(feature = "json5")]
#[test]
fn json5_rereads_the_path_on_every_load() {
    use neotales_config::{Json5File, Source, Value};

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("gateway.json5");
    let source = Json5File::new(&path);
    std::fs::write(&path, "{ port: 8080 }").unwrap();
    assert_eq!(
        source.load().unwrap().as_map().unwrap()["port"],
        Value::Integer(8080)
    );
    std::fs::write(&path, "{ port: 9090 }").unwrap();
    assert_eq!(
        source.load().unwrap().as_map().unwrap()["port"],
        Value::Integer(9090)
    );
}

#[cfg(feature = "jsonc")]
#[test]
fn jsonc_rereads_the_path_on_every_load() {
    use neotales_config::{JsoncFile, Source, Value};

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("gateway.jsonc");
    let source = JsoncFile::new(&path);
    std::fs::write(&path, r#"{"port": 8080} // live"#).unwrap();
    assert_eq!(
        source.load().unwrap().as_map().unwrap()["port"],
        Value::Integer(8080)
    );
    std::fs::write(&path, r#"{"port": 9090} /* live */"#).unwrap();
    assert_eq!(
        source.load().unwrap().as_map().unwrap()["port"],
        Value::Integer(9090)
    );
}

#[cfg(feature = "toml")]
#[test]
fn toml_rereads_the_path_on_every_load() {
    use neotales_config::{Source, TomlFile, Value};

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("gateway.toml");
    let source = TomlFile::new(&path);
    std::fs::write(&path, "port = 8080\n").unwrap();
    assert_eq!(
        source.load().unwrap().as_map().unwrap()["port"],
        Value::Integer(8080)
    );
    std::fs::write(&path, "port = 9090\n").unwrap();
    assert_eq!(
        source.load().unwrap().as_map().unwrap()["port"],
        Value::Integer(9090)
    );
}

#[cfg(feature = "yaml")]
#[test]
fn yaml_rereads_the_path_on_every_load() {
    use neotales_config::{Source, Value, YamlFile};

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("gateway.yaml");
    let source = YamlFile::new(&path);
    std::fs::write(&path, "port: 8080\n").unwrap();
    assert_eq!(
        source.load().unwrap().as_map().unwrap()["port"],
        Value::Integer(8080)
    );
    std::fs::write(&path, "port: 9090\n").unwrap();
    assert_eq!(
        source.load().unwrap().as_map().unwrap()["port"],
        Value::Integer(9090)
    );
}

#[cfg(feature = "ini")]
#[test]
fn ini_rereads_the_path_on_every_load() {
    use neotales_config::{IniFile, Source, Value};

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("gateway.ini");
    let source = IniFile::new(&path);
    std::fs::write(&path, "port = 8080\n").unwrap();
    assert_eq!(
        source.load().unwrap().as_map().unwrap()["port"],
        Value::from("8080")
    );
    std::fs::write(&path, "port = 9090\n").unwrap();
    assert_eq!(
        source.load().unwrap().as_map().unwrap()["port"],
        Value::from("9090")
    );
}

#[cfg(any(
    feature = "json",
    feature = "json5",
    feature = "jsonc",
    feature = "toml",
    feature = "yaml"
))]
#[test]
fn a_number_the_model_cannot_hold_rejects_the_build() {
    use neotales_config::{Builder, SourceError};

    let directory = tempfile::tempdir().unwrap();

    #[cfg(feature = "json")]
    {
        let path = directory.path().join("big.json");
        std::fs::write(
            &path,
            r#"{"value": 170141183460469231731687303715884105728}"#,
        )
        .unwrap();
        let error = Builder::new()
            .json_file("json", neotales_config::JsonFile::new(&path))
            .build()
            .expect_err("the integer cannot be held");
        assert!(error.is_unrepresentable(), "{error}");
        assert!(!format!("{error}").contains("170141183460469231731687303715884105728"));
    }

    #[cfg(feature = "json5")]
    {
        let path = directory.path().join("big.json5");
        std::fs::write(&path, "{ value: 170141183460469231731687303715884105728 }").unwrap();
        let error = Builder::new()
            .json5_file("json5", neotales_config::Json5File::new(&path))
            .build()
            .expect_err("the integer cannot be held");
        assert!(error.is_unrepresentable(), "{error}");
    }

    #[cfg(feature = "jsonc")]
    {
        let path = directory.path().join("big.jsonc");
        std::fs::write(
            &path,
            r#"{"value": 170141183460469231731687303715884105728}"#,
        )
        .unwrap();
        let error = Builder::new()
            .jsonc_file("jsonc", neotales_config::JsoncFile::new(&path))
            .build()
            .expect_err("the integer cannot be held");
        assert!(error.is_unrepresentable(), "{error}");
    }

    #[cfg(feature = "yaml")]
    {
        let path = directory.path().join("big.yaml");
        std::fs::write(&path, "value: 170141183460469231731687303715884105728\n").unwrap();
        let error = Builder::new()
            .yaml_file("yaml", neotales_config::YamlFile::new(&path))
            .build()
            .expect_err("the integer cannot be held");
        assert!(error.is_unrepresentable(), "{error}");
    }

    #[cfg(feature = "toml")]
    {
        let path = directory.path().join("big.toml");
        std::fs::write(&path, "value = nan\n").unwrap();
        let error = Builder::new()
            .toml_file("toml", neotales_config::TomlFile::new(&path))
            .build()
            .expect_err("the float cannot be held");
        assert!(error.is_unrepresentable(), "{error}");
    }

    // The import is used by the format-specific branches only.
    let _ = SourceError::invalid();
}
