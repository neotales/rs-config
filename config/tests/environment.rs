//! The opt-in process environment provider.

#![cfg(feature = "environment")]

use neotales_config::{Builder, Case, Environment, SourceErrorKind, Value};

/// Distinctive enough that a leak into an error is visible.
const SECRET: &str = "fixture-secret-9f3a2c";

#[test]
fn values_stay_strings_and_nest_on_the_builder_delimiter() {
    let config = Builder::new()
        .environment(
            "environment",
            Environment::from_pairs([("APP_http.port", "8080"), ("IGNORED", "x")]).prefix("APP_"),
        )
        .build()
        .expect("the environment source builds");

    assert_eq!(config.get::<String>("http.port").unwrap(), "8080");
    assert!(
        config
            .get::<u16>("http.port")
            .unwrap_err()
            .is_type_mismatch(),
        "an environment value is never parsed"
    );
    assert!(!config.has("IGNORED").unwrap());
}

#[test]
fn prefix_and_case_apply_before_the_key_is_mapped() {
    let config = Builder::new()
        .environment(
            "environment",
            Environment::from_pairs([("APP_HTTP_PORT", "8080")])
                .prefix("APP_")
                .separator("_")
                .case(Case::Lower),
        )
        .build()
        .expect("the environment source builds");

    assert_eq!(config.get::<String>("http.port").unwrap(), "8080");
    assert_eq!(config.origin("http.port").unwrap(), "environment");
}

#[test]
fn a_custom_builder_delimiter_is_the_default_key_separator() {
    let config = Builder::new()
        .delimiter(':')
        .environment(
            "environment",
            Environment::from_pairs([("APP_http:port", "8080")]).prefix("APP_"),
        )
        .build()
        .expect("the environment source builds");

    assert_eq!(config.get::<String>("http:port").unwrap(), "8080");
}

#[test]
fn a_key_with_the_default_delimiter_stays_literal() {
    let config = Builder::new()
        .delimiter(':')
        .environment(
            "environment",
            Environment::from_pairs([("APP_http.port", "8080")]).prefix("APP_"),
        )
        .build()
        .expect("the environment source builds");

    assert_eq!(config.get::<String>("http.port").unwrap(), "8080");
}

#[test]
fn the_final_builder_delimiter_applies_in_either_registration_order() {
    // The implicit separator is the delimiter the builder ends up with, not the one it had when
    // the source was registered.
    for config in [
        Builder::new()
            .environment(
                "environment",
                Environment::from_pairs([("APP_http:port", "8080"), ("APP_http.port", "9090")])
                    .prefix("APP_"),
            )
            .delimiter(':')
            .build(),
        Builder::new()
            .delimiter(':')
            .environment(
                "environment",
                Environment::from_pairs([("APP_http:port", "8080"), ("APP_http.port", "9090")])
                    .prefix("APP_"),
            )
            .build(),
    ] {
        let config = config.expect("the environment source builds");
        assert_eq!(config.get::<String>("http:port").unwrap(), "8080");
        assert_eq!(config.get::<String>("http.port").unwrap(), "9090");
    }
}

#[test]
fn an_explicit_separator_survives_a_builder_delimiter_in_either_order() {
    for config in [
        Builder::new()
            .environment(
                "environment",
                Environment::from_pairs([("APP_http__port", "8080")])
                    .prefix("APP_")
                    .separator("__"),
            )
            .delimiter(':')
            .build(),
        Builder::new()
            .delimiter(':')
            .environment(
                "environment",
                Environment::from_pairs([("APP_http__port", "8080")])
                    .prefix("APP_")
                    .separator("__"),
            )
            .build(),
    ] {
        assert_eq!(
            config
                .expect("the environment source builds")
                .get::<String>("http:port")
                .unwrap(),
            "8080"
        );
    }
}

#[test]
fn flat_survives_a_builder_delimiter_in_either_order() {
    for config in [
        Builder::new()
            .environment(
                "environment",
                Environment::from_pairs([("APP_http:port", "8080")])
                    .prefix("APP_")
                    .flat(),
            )
            .delimiter(':')
            .build(),
        Builder::new()
            .delimiter(':')
            .environment(
                "environment",
                Environment::from_pairs([("APP_http:port", "8080")])
                    .prefix("APP_")
                    .flat(),
            )
            .build(),
    ] {
        let config = config.expect("the environment source builds");
        let root = config.value().expect("the root is present");
        assert_eq!(
            root.as_map().expect("the root is a map").get("http:port"),
            Some(&Value::from("8080"))
        );
    }
}

#[test]
fn an_explicit_separator_overrides_the_builder_delimiter() {
    let config = Builder::new()
        .delimiter(':')
        .environment(
            "environment",
            Environment::from_pairs([("APP_http__port", "8080")])
                .prefix("APP_")
                .separator("__"),
        )
        .build()
        .expect("the environment source builds");

    assert_eq!(config.get::<String>("http:port").unwrap(), "8080");
}

#[test]
fn flat_keeps_the_remainder_as_one_component() {
    let config = Builder::new()
        .environment(
            "environment",
            Environment::from_pairs([("APP_http:port", "8080")])
                .prefix("APP_")
                .flat(),
        )
        .build()
        .expect("the environment source builds");

    assert_eq!(config.get::<String>("http:port").unwrap(), "8080");
}

#[test]
fn a_collision_is_resolved_by_sorted_name_in_either_input_order() {
    for pairs in [
        [("APP_A", "first"), ("APP_a", "second")],
        [("APP_a", "second"), ("APP_A", "first")],
    ] {
        let config = Builder::new()
            .environment(
                "environment",
                Environment::from_pairs(pairs)
                    .prefix("APP_")
                    .case(Case::Lower),
            )
            .build()
            .expect("the environment source builds");

        assert_eq!(config.get::<String>("a").unwrap(), "second");
    }
}

#[test]
fn a_duplicate_name_rejects_the_source_in_either_order_without_a_value() {
    // Two pairs with the same name leave the winner to the caller's iteration order, so the set
    // is refused instead of resolved by that order.
    for pairs in [
        [("APP_A", SECRET), ("APP_A", "second")],
        [("APP_A", "second"), ("APP_A", SECRET)],
        [("APP_A", SECRET), ("APP_A", SECRET)],
    ] {
        let error = Builder::new()
            .environment("environment", Environment::from_pairs(pairs).prefix("APP_"))
            .build()
            .expect_err("a duplicate name is ambiguous");

        assert_eq!(error.source_kind(), Some(SourceErrorKind::Invalid));
        for text in [format!("{error}"), format!("{error:?}")] {
            assert!(
                !text.contains(SECRET),
                "the value reached the error: {text}"
            );
        }
    }
}

#[test]
fn a_duplicate_name_rejects_the_source_even_when_the_prefix_selects_none() {
    for pairs in [
        [("OTHER_A", "first"), ("OTHER_A", "second")],
        [("OTHER_A", "second"), ("OTHER_A", "first")],
    ] {
        let error = Builder::new()
            .environment("environment", Environment::from_pairs(pairs).prefix("APP_"))
            .build()
            .expect_err("a duplicate name is ambiguous wherever it sits");

        assert_eq!(error.source_kind(), Some(SourceErrorKind::Invalid));
    }
}

#[test]
fn a_scalar_and_a_map_on_one_key_reject_the_source_without_the_value() {
    let error = Builder::new()
        .environment(
            "environment",
            Environment::from_pairs([("APP_HTTP", SECRET), ("APP_HTTP_PORT", "1")])
                .prefix("APP_")
                .separator("_"),
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
fn an_empty_name_remainder_or_separator_is_invalid() {
    for environment in [
        Environment::from_pairs([("", "value")]),
        Environment::from_pairs([("APP_", "value")]).prefix("APP_"),
        Environment::from_pairs([("APP_A", "value")])
            .prefix("APP_")
            .separator(""),
    ] {
        let error = Builder::new()
            .environment("environment", environment)
            .build()
            .expect_err("the environment cannot be mapped");
        assert_eq!(error.source_kind(), Some(SourceErrorKind::Invalid));
    }
}

#[test]
fn an_empty_separator_is_invalid_even_when_no_name_is_selected() {
    for environment in [
        Environment::from_pairs(Vec::<(String, String)>::new()).separator(""),
        Environment::from_pairs([("OTHER_A", "value")])
            .prefix("APP_")
            .separator(""),
        Environment::from_pairs([("APP_A", "value")])
            .prefix("APP_")
            .flat()
            .separator(""),
    ] {
        let error = Builder::new()
            .environment("environment", environment)
            .build()
            .expect_err("an empty separator is a caller error");
        assert_eq!(error.source_kind(), Some(SourceErrorKind::Invalid));
    }
}
