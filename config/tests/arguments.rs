//! The opt-in tokenized argv provider.

#![cfg(feature = "args")]

use std::ffi::OsString;

use neotales_config::{Arguments, Builder, Source, SourceErrorKind, Value};

/// Distinctive enough that a leak into an error is visible.
const SECRET: &str = "fixture-secret-9f3a2c";

fn tokens(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

#[test]
fn accepted_switches_map_to_strings_and_booleans() {
    let config = Builder::new()
        .arguments(
            "arguments",
            Arguments::new(tokens(&[
                "--http.port=8080",
                "--verbose",
                "--no-color",
                "--empty=",
                "--equation=a=b",
            ])),
        )
        .build()
        .expect("the arguments source builds");

    assert_eq!(config.get::<String>("http.port").unwrap(), "8080");
    assert!(config.get::<bool>("verbose").unwrap());
    assert!(!config.get::<bool>("color").unwrap());
    assert_eq!(config.get::<String>("empty").unwrap(), "");
    assert_eq!(config.get::<String>("equation").unwrap(), "a=b");
    assert!(
        config
            .get::<u16>("http.port")
            .unwrap_err()
            .is_type_mismatch(),
        "a valued switch is never parsed"
    );
}

#[test]
fn a_no_prefix_with_an_equals_sign_is_a_literal_key() {
    let config = Builder::new()
        .arguments("arguments", Arguments::new(tokens(&["--no-cache=1"])))
        .build()
        .expect("the arguments source builds");

    assert_eq!(config.get::<String>("no-cache").unwrap(), "1");
}

#[test]
fn the_later_switch_wins_at_the_same_path() {
    let config = Builder::new()
        .arguments(
            "arguments",
            Arguments::new(tokens(&["--port=1", "--port=2", "--flag", "--no-flag"])),
        )
        .build()
        .expect("the arguments source builds");

    assert_eq!(config.get::<String>("port").unwrap(), "2");
    assert!(!config.get::<bool>("flag").unwrap());
}

#[test]
fn a_custom_builder_delimiter_is_the_default_key_separator() {
    let config = Builder::new()
        .delimiter(':')
        .arguments(
            "arguments",
            Arguments::new(tokens(&["--http:port=8080", "--http.port=9090"])),
        )
        .build()
        .expect("the arguments source builds");

    assert_eq!(config.get::<String>("http:port").unwrap(), "8080");
    assert_eq!(config.get::<String>("http.port").unwrap(), "9090");
}

#[test]
fn an_explicit_separator_overrides_the_builder_delimiter() {
    let config = Builder::new()
        .delimiter(':')
        .arguments(
            "arguments",
            Arguments::new(tokens(&["--http__port=8080"])).separator("__"),
        )
        .build()
        .expect("the arguments source builds");

    assert_eq!(config.get::<String>("http:port").unwrap(), "8080");
}

#[test]
fn flat_keeps_the_key_as_one_component() {
    let config = Builder::new()
        .arguments(
            "arguments",
            Arguments::new(tokens(&["--http:port=8080"])).flat(),
        )
        .build()
        .expect("the arguments source builds");

    assert_eq!(config.get::<String>("http:port").unwrap(), "8080");
}

#[test]
fn unsupported_tokens_reject_the_source() {
    for token in [
        "positional",
        "-x",
        "-",
        "",
        "--",
        "--=value",
        "--no-",
        "--a..b=1",
    ] {
        let error = Builder::new()
            .arguments("arguments", Arguments::new(tokens(&[token])))
            .build()
            .expect_err("the token is not an accepted switch");
        assert_eq!(
            error.source_kind(),
            Some(SourceErrorKind::Invalid),
            "{token:?}"
        );
    }
}

#[test]
fn a_separate_value_token_is_not_accepted() {
    let error = Builder::new()
        .arguments(
            "arguments",
            Arguments::new(tokens(&["--http.port", "8080"])),
        )
        .build()
        .expect_err("the value token is a positional argument");

    assert_eq!(error.source_kind(), Some(SourceErrorKind::Invalid));
}

#[test]
fn a_scalar_and_a_map_on_one_key_do_not_echo_a_value() {
    let scalar = format!("--http={SECRET}");
    let error = Builder::new()
        .arguments(
            "arguments",
            Arguments::new([OsString::from(scalar), OsString::from("--http.port=1")]),
        )
        .build()
        .expect_err("one key cannot be a scalar and a map");

    assert!(error.is_source_failure());
    assert_eq!(error.source_kind(), Some(SourceErrorKind::Invalid));
    for text in [format!("{error}"), format!("{error:?}")] {
        assert!(
            !text.contains(SECRET),
            "the value reached the error: {text}"
        );
    }
}

#[test]
fn a_rejected_token_is_not_echoed() {
    let error = Builder::new()
        .arguments("arguments", Arguments::new(tokens(&[SECRET])))
        .build()
        .expect_err("a positional token is rejected");

    for text in [format!("{error}"), format!("{error:?}")] {
        assert!(
            !text.contains(SECRET),
            "the token reached the error: {text}"
        );
    }
}

#[cfg(unix)]
#[test]
fn a_non_unicode_token_does_not_echo_its_bytes() {
    use std::os::unix::ffi::OsStringExt;

    let bad = OsString::from_vec(b"--fixture-secret-\xff=1".to_vec());
    let error = Builder::new()
        .arguments("arguments", Arguments::new([bad]))
        .build()
        .expect_err("a non-Unicode token is rejected");

    assert_eq!(error.source_kind(), Some(SourceErrorKind::Invalid));
    let shown = format!("{error}");
    assert!(!shown.contains("fixture-secret"), "{shown}");
}

#[test]
fn the_source_value_is_a_map() {
    let arguments = Arguments::new(Vec::<OsString>::new());
    let value = arguments.load().expect("an empty vector loads");
    assert_eq!(value, Value::map(std::iter::empty::<(String, Value)>()));
}
