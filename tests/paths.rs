//! Strict path validation, cursors, and the builder-configurable delimiter.

use neotales_config::{Builder, Config, Value};
use serde::Deserialize;

fn fixture() -> Config {
    Builder::new()
        .memory(
            "defaults",
            Value::map([
                (
                    "http",
                    Value::map([
                        ("port", Value::from(8080_i64)),
                        ("host", Value::from("localhost")),
                    ]),
                ),
                ("http.port", Value::from("literal")),
                ("null", Value::Null),
            ]),
        )
        .build()
        .expect("the fixture builds")
}

#[test]
fn every_invalid_path_form_is_an_error_not_a_miss() {
    for path in ["", ".", "a.", ".a", "a..b", ".."] {
        let error = fixture().get::<i64>(path).expect_err("the path is invalid");
        assert!(error.is_invalid_path(), "{path:?}: {error}");
        assert_eq!(error.path(), Some(path));

        assert!(fixture().has(path).expect_err("invalid").is_invalid_path());
        assert!(
            fixture()
                .child(path)
                .expect_err("invalid")
                .is_invalid_path()
        );
        assert!(
            fixture()
                .origin(path)
                .expect_err("invalid")
                .is_invalid_path()
        );
        assert!(
            fixture()
                .get_or(path, 0_i64)
                .expect_err("invalid")
                .is_invalid_path()
        );
    }
}

#[test]
fn a_valid_path_is_one_or_more_non_empty_components() {
    let config = fixture();
    assert!(config.has("http").unwrap());
    assert!(config.has("http.port").unwrap());
    assert!(!config.has("http.absent").unwrap());
    assert_eq!(config.get::<i64>("http.port").unwrap(), 8080);
}

#[test]
fn child_paths_over_the_default_delimiter_agree() {
    let config = fixture();
    let by_parts = config.child("http").unwrap().child("port").unwrap();
    let by_path = config.child("http.port").unwrap();
    assert_eq!(by_parts.value().unwrap(), &Value::from(8080_i64));
    assert_eq!(by_path.value().unwrap(), &Value::from(8080_i64));
    assert_eq!(
        config.get::<i64>("http.port").unwrap(),
        by_parts.load::<i64>().unwrap()
    );
}

#[test]
fn a_child_is_relative_to_its_cursor() {
    let config = fixture();
    let http = config.child("http").unwrap();
    assert_eq!(http.get::<i64>("port").unwrap(), 8080);
    assert_eq!(http.get::<String>("host").unwrap(), "localhost");
    assert!(!http.has("http").unwrap());

    let error = http.get::<i64>("absent").expect_err("absent");
    assert!(error.is_missing(), "{error}");
    assert_eq!(error.path(), Some("http.absent"));
}

#[test]
fn a_missing_child_is_missing_not_an_empty_document() {
    let error = fixture().child("absent").expect_err("absent");
    assert!(error.is_missing(), "{error}");
    assert_eq!(error.path(), Some("absent"));
}

#[test]
fn load_reads_the_focused_document() {
    #[derive(Debug, Deserialize, PartialEq)]
    struct Root {
        http: Http,
    }

    #[derive(Debug, Deserialize, PartialEq)]
    struct Http {
        port: u16,
        host: String,
    }

    let config = fixture();
    assert_eq!(
        config.load::<Root>().unwrap(),
        Root {
            http: Http {
                port: 8080,
                host: "localhost".to_owned(),
            },
        }
    );
}

#[test]
fn a_custom_delimiter_splits_only_on_that_delimiter() {
    let config = Builder::new()
        .delimiter(':')
        .memory(
            "defaults",
            Value::map([
                ("http", Value::map([("port", Value::from(8080_i64))])),
                ("http.port", Value::from(9090_i64)),
            ]),
        )
        .build()
        .expect("the fixture builds");

    assert_eq!(config.get::<i64>("http:port").unwrap(), 8080);
    assert_eq!(config.get::<i64>("http.port").unwrap(), 9090);
    assert_eq!(
        config
            .child("http")
            .unwrap()
            .child("port")
            .unwrap()
            .load::<i64>()
            .unwrap(),
        8080
    );

    for path in ["", ":", ":http", "http:", "http::port"] {
        assert!(
            config
                .get::<i64>(path)
                .expect_err("invalid")
                .is_invalid_path(),
            "{path:?}"
        );
    }

    let error = config.get::<i64>("http:absent").expect_err("absent");
    assert!(error.is_missing(), "{error}");
    assert_eq!(error.path(), Some("http:absent"));
}

#[test]
fn a_literal_dotted_key_is_not_selected_by_a_dotted_path() {
    let config = fixture();
    // The dotted path walks `http` then `port`, and never matches the literal key.
    assert_eq!(
        config.child("http.port").unwrap().load::<i64>().unwrap(),
        8080
    );
    // The literal key is reachable by reading the whole document.
    let root = config.value().unwrap().as_map().unwrap();
    assert_eq!(root["http.port"], Value::from("literal"));
}

#[test]
fn present_null_is_not_absent() {
    let config = fixture();
    assert!(config.has("null").unwrap());
    assert_eq!(config.get::<Option<String>>("null").unwrap(), None);
    assert!(
        config
            .get::<Option<i64>>("absent")
            .expect_err("absent")
            .is_missing()
    );
}
