//! Errors must not echo a value, a connection string, or caller-supplied deserializer text.

use std::{error::Error as StdError, sync::Arc};

use neotales_config::{Builder, Error, Source, SourceError, Value};
use serde::Deserialize;

const SECRET: &str = "fixture-secret-9f3a2c";
const CONNECTION: &str = "postgres://user:fixture-secret-9f3a2c@localhost/db";

/// Asserts the fixtures are absent from every observable form of the error.
#[track_caller]
fn assert_sanitized(error: &Error) {
    let shown = format!("{error}");
    let debugged = format!("{error:?}");
    for text in [&shown, &debugged] {
        assert!(
            !text.contains(SECRET),
            "the secret reached the error: {text}"
        );
        assert!(
            !text.contains("postgres://"),
            "the connection string reached the error: {text}"
        );
        assert!(
            !text.contains("fixture-secret"),
            "a value reached the error: {text}"
        );
    }
    assert!(error.source().is_none(), "the error chain must stay empty");
}

#[test]
fn a_missing_path_error_names_the_path_only() {
    let error = Builder::new()
        .memory("defaults", Value::map([("present", Value::from(1_i64))]))
        .build()
        .expect("the fixture builds")
        .get::<i64>("absent")
        .expect_err("absent");
    assert!(error.is_missing());
    assert_eq!(error.path(), Some("absent"));
    assert_sanitized(&error);
}

#[test]
fn a_source_failure_does_not_forward_text() {
    struct Refusing;

    impl Source for Refusing {
        fn load(&self) -> Result<Value, SourceError> {
            Err(SourceError::invalid())
        }
    }

    let error = Builder::new()
        .source("remote", Refusing)
        .build()
        .expect_err("the source refuses");
    assert!(error.is_source_failure());
    assert!(format!("{error}").contains("remote"));
    assert_sanitized(&error);
}

#[test]
fn a_non_finite_float_is_rejected_without_the_number() {
    let error = Builder::new()
        .memory(
            "defaults",
            Value::map([("ratio", Value::Float(f64::INFINITY))]),
        )
        .build()
        .expect_err("an infinite float cannot be represented");
    assert!(error.is_unrepresentable());
    let shown = format!("{error}");
    assert!(!shown.contains("inf"), "{shown}");
    assert_sanitized(&error);
}

#[test]
fn a_type_mismatch_names_the_path_and_shape_never_the_value() {
    let config = Builder::new()
        .memory(
            "defaults",
            Value::map([
                ("url", Value::from(CONNECTION)),
                ("token", Value::from(SECRET)),
            ]),
        )
        .build()
        .expect("the fixture builds");

    let error = config
        .get::<i64>("token")
        .expect_err("a string is not an integer");
    assert!(error.is_type_mismatch());
    assert_eq!(error.path(), Some("token"));
    assert_eq!(error.source_name(), Some("defaults"));
    assert_eq!(error.expected(), Some("a signed 64-bit integer"));
    assert_sanitized(&error);

    let error = config
        .get::<u16>("url")
        .expect_err("a string is not an integer");
    assert_sanitized(&error);
}

#[test]
fn a_custom_serde_expectation_does_not_reach_the_error() {
    #[derive(Debug)]
    struct Finicky;

    impl<'de> Deserialize<'de> for Finicky {
        fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            let value = <String as Deserialize>::deserialize(deserializer)?;
            Err(serde::de::Error::invalid_value(
                serde::de::Unexpected::Str(&value),
                &"fixture-secret-review",
            ))
        }
    }

    let config = Builder::new()
        .memory("defaults", Value::map([("key", Value::from(SECRET))]))
        .build()
        .expect("the fixture builds");
    let error = config
        .get::<Finicky>("key")
        .expect_err("the deserializer refuses the value");
    assert!(error.is_type_mismatch(), "{error}");
    assert_sanitized(&error);
    for text in [format!("{error}"), format!("{error:?}")] {
        assert!(
            !text.contains("fixture-secret-review"),
            "the custom expectation reached the error: {text}"
        );
    }
}

#[test]
fn builder_errors_name_only_their_inputs() {
    let duplicate = Builder::new()
        .memory("same", Value::map([("a", Value::from(1_i64))]))
        .memory("same", Value::map([("b", Value::from(2_i64))]))
        .build()
        .expect_err("a repeated name is refused");
    assert_sanitized(&duplicate);

    let root = Builder::new()
        .memory("root", Value::from(SECRET))
        .build()
        .expect_err("a scalar root is refused");
    assert_sanitized(&root);

    let empty = Builder::new()
        .memory("", Value::map([("a", Value::from(1_i64))]))
        .build()
        .expect_err("an empty name is refused");
    assert_sanitized(&empty);
}

#[test]
fn config_debug_reports_no_value() {
    let config = Builder::new()
        .memory(
            "defaults",
            Value::map([
                ("token", Value::from(SECRET)),
                ("http", Value::map([("port", Value::from(1_i64))])),
            ]),
        )
        .build()
        .expect("the fixture builds");
    let shown = format!("{config:?}");
    assert!(!shown.contains(SECRET), "{shown}");
    assert!(shown.contains("map"), "{shown}");
}

#[test]
fn builder_debug_reports_no_value() {
    let builder = Builder::new()
        .memory("defaults", Value::map([("token", Value::from(SECRET))]))
        .memory("overlay", Value::map([("token", Value::from(SECRET))]));
    let shown = format!("{builder:?}");
    assert!(!shown.contains(SECRET), "{shown}");
    assert!(shown.contains("sources"), "{shown}");
}

#[test]
fn a_stateful_source_can_be_registered() {
    #[derive(Debug)]
    struct Counting(Arc<str>);

    impl Source for Counting {
        fn load(&self) -> Result<Value, SourceError> {
            Ok(Value::map([("name", Value::from(&*self.0))]))
        }
    }

    let config = Builder::new()
        .source("counting", Counting(Arc::from("fixture")))
        .build()
        .expect("the fixture builds");
    assert_eq!(config.get::<String>("name").unwrap(), "fixture");
}
