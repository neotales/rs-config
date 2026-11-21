//! The builder accepts a small, documented set of path delimiters and rejects the rest before any
//! source is loaded.

use std::{
    error::Error as StdError,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use neotales_config::{Builder, Value};

/// The supported set, written out here so a change to the policy fails this test on purpose.
const SUPPORTED: [char; 5] = ['.', ':', '/', ',', '|'];

#[test]
fn every_supported_delimiter_splits_a_two_component_path() {
    for delimiter in SUPPORTED {
        let nested = format!("http{delimiter}port");
        let config = Builder::new()
            .delimiter(delimiter)
            .memory(
                "defaults",
                Value::map([("http", Value::map([("port", Value::from(8080_i64))]))]),
            )
            .build()
            .expect("a supported delimiter builds");

        assert_eq!(config.get::<u16>(&nested).unwrap(), 8080, "{delimiter:?}");
        assert_eq!(config.origin(&nested).unwrap(), "defaults", "{delimiter:?}");
        assert!(config.has(&nested).unwrap(), "{delimiter:?}");
        assert_eq!(
            config.child("http").unwrap().get::<u16>("port").unwrap(),
            8080,
            "{delimiter:?}"
        );
    }
}

#[test]
fn an_unsupported_delimiter_is_its_own_error_category() {
    for delimiter in [
        '\0', '\u{1b}', '\n', '\t', ' ', 'a', 'A', '1', 'é', '😀', '\\', '-', '_', '+',
    ] {
        let error = Builder::new()
            .delimiter(delimiter)
            .build()
            .expect_err("an unsupported delimiter is refused");

        assert!(error.is_invalid_delimiter(), "{delimiter:?}: {error}");
        assert!(!error.is_invalid_path(), "{delimiter:?}: {error}");
        assert!(!error.is_source_failure(), "{delimiter:?}: {error}");
        assert_eq!(error.path(), None, "{delimiter:?}");
        assert_eq!(error.source_name(), None, "{delimiter:?}");
        assert_eq!(error.expected(), None, "{delimiter:?}");
        assert!(error.source().is_none(), "{delimiter:?}");
    }
}

#[test]
fn a_rejected_delimiter_is_not_echoed() {
    // Each of these is distinctive enough that a leak would show up in the text, and none of them
    // appears in the fixed message.
    for delimiter in ['\0', '\u{1b}', 'é', '😀'] {
        let error = Builder::new()
            .delimiter(delimiter)
            .build()
            .expect_err("an unsupported delimiter is refused");
        for text in [format!("{error}"), format!("{error:?}")] {
            assert!(
                !text.contains(delimiter),
                "{delimiter:?} reached the error: {text}"
            );
        }
    }
}

#[test]
fn a_rejected_delimiter_fails_before_any_source_is_loaded() {
    let loaded = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&loaded);
    let error = Builder::new()
        .delimiter('a')
        .source("computed", move || {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(Value::map([("key", Value::from(1_i64))]))
        })
        .build()
        .expect_err("an unsupported delimiter is refused");

    assert!(error.is_invalid_delimiter(), "{error}");
    assert_eq!(
        loaded.load(Ordering::SeqCst),
        0,
        "the delimiter is checked before a source is loaded"
    );
}
