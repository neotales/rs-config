//! The opt-in dotenv file provider.

#![cfg(feature = "dotenv")]

use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use neotales_config::{Builder, DotenvFile, SourceErrorKind, Value};

/// Distinctive enough that a leak into an error is visible.
const SECRET: &str = "fixture-secret-9f3a2c";

static NEXT: AtomicU64 = AtomicU64::new(0);

/// A temporary dotenv file removed when the test ends.
struct Fixture {
    path: PathBuf,
}

impl Fixture {
    fn new(contents: impl AsRef<[u8]>) -> Self {
        let path = std::env::temp_dir().join(format!(
            "neotales-config-dotenv-{}-{}.env",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::write(&path, contents).expect("the fixture file is writable");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[test]
fn assignments_stay_strings_and_nest_on_the_builder_delimiter() {
    let file = Fixture::new(
        b"# retained\nAPP_http.port=8080\nexport APP_http.host=localhost\nIGNORED=1\n",
    );
    let config = Builder::new()
        .dotenv_file("dotenv", DotenvFile::new(file.path()).prefix("APP_"))
        .build()
        .expect("the dotenv source builds");

    assert_eq!(config.get::<String>("http.port").unwrap(), "8080");
    assert_eq!(config.get::<String>("http.host").unwrap(), "localhost");
    assert!(
        config
            .get::<u16>("http.port")
            .unwrap_err()
            .is_type_mismatch(),
        "a dotenv value is never parsed"
    );
    assert!(!config.has("IGNORED").unwrap());
    assert_eq!(config.origin("http.port").unwrap(), "dotenv");
}

#[test]
fn literal_mode_keeps_every_value_unexpanded() {
    let file = Fixture::new(
        b"NAME=\"$OTHER\"\nRAW=`$OTHER`\nSINGLE='$OTHER'\nINLINE=value # literal\nQUOTED=\"value # literal\"\nBROKEN=${UNTERMINATED\n",
    );
    let config = Builder::new()
        .dotenv_file("dotenv", DotenvFile::new(file.path()).interpolate(false))
        .build()
        .expect("the dotenv source builds");

    assert_eq!(config.get::<String>("NAME").unwrap(), "$OTHER");
    assert_eq!(config.get::<String>("BROKEN").unwrap(), "${UNTERMINATED");
    assert_eq!(config.get::<String>("RAW").unwrap(), "$OTHER");
    assert_eq!(config.get::<String>("SINGLE").unwrap(), "$OTHER");
    assert_eq!(config.get::<String>("INLINE").unwrap(), "value");
    assert_eq!(config.get::<String>("QUOTED").unwrap(), "value # literal");
}

#[test]
fn a_custom_builder_delimiter_is_the_default_key_separator() {
    let file = Fixture::new(b"APP_http:port=8080\n");
    let config = Builder::new()
        .delimiter(':')
        .dotenv_file("dotenv", DotenvFile::new(file.path()).prefix("APP_"))
        .build()
        .expect("the dotenv source builds");

    assert_eq!(config.get::<String>("http:port").unwrap(), "8080");
}

#[test]
fn the_final_builder_delimiter_applies_in_either_registration_order() {
    // The implicit separator is the delimiter the builder ends up with, not the one it had when
    // the source was registered.
    let file = Fixture::new(b"APP_http:port=8080\nAPP_http.port=9090\n");
    for config in [
        Builder::new()
            .dotenv_file("dotenv", DotenvFile::new(file.path()).prefix("APP_"))
            .delimiter(':')
            .build(),
        Builder::new()
            .delimiter(':')
            .dotenv_file("dotenv", DotenvFile::new(file.path()).prefix("APP_"))
            .build(),
    ] {
        let config = config.expect("the dotenv source builds");
        assert_eq!(config.get::<String>("http:port").unwrap(), "8080");
        assert_eq!(config.get::<String>("http.port").unwrap(), "9090");
    }
}

#[test]
fn an_explicit_separator_survives_a_builder_delimiter_in_either_order() {
    let file = Fixture::new(b"APP_http__port=8080\n");
    for config in [
        Builder::new()
            .dotenv_file(
                "dotenv",
                DotenvFile::new(file.path()).prefix("APP_").separator("__"),
            )
            .delimiter(':')
            .build(),
        Builder::new()
            .delimiter(':')
            .dotenv_file(
                "dotenv",
                DotenvFile::new(file.path()).prefix("APP_").separator("__"),
            )
            .build(),
    ] {
        assert_eq!(
            config
                .expect("the dotenv source builds")
                .get::<String>("http:port")
                .unwrap(),
            "8080"
        );
    }
}

#[test]
fn flat_survives_a_builder_delimiter_in_either_order() {
    let file = Fixture::new(b"APP_http:port=8080\n");
    for config in [
        Builder::new()
            .dotenv_file("dotenv", DotenvFile::new(file.path()).prefix("APP_").flat())
            .delimiter(':')
            .build(),
        Builder::new()
            .delimiter(':')
            .dotenv_file("dotenv", DotenvFile::new(file.path()).prefix("APP_").flat())
            .build(),
    ] {
        let config = config.expect("the dotenv source builds");
        let root = config.value().expect("the root is present");
        assert_eq!(
            root.as_map().expect("the root is a map").get("http:port"),
            Some(&Value::from("8080"))
        );
    }
}

#[test]
fn an_empty_separator_is_invalid_even_when_nothing_is_selected() {
    for contents in [&b""[..], &b"IGNORED=1\n"[..]] {
        let file = Fixture::new(contents);
        let error = Builder::new()
            .dotenv_file(
                "dotenv",
                DotenvFile::new(file.path()).prefix("APP_").separator(""),
            )
            .build()
            .expect_err("an empty separator is a caller error");

        assert_eq!(error.source_kind(), Some(SourceErrorKind::Invalid));
    }
}

#[test]
fn an_explicit_separator_overrides_the_builder_delimiter() {
    let file = Fixture::new(b"APP_http__port=8080\n");
    let config = Builder::new()
        .delimiter(':')
        .dotenv_file(
            "dotenv",
            DotenvFile::new(file.path()).prefix("APP_").separator("__"),
        )
        .build()
        .expect("the dotenv source builds");

    assert_eq!(config.get::<String>("http:port").unwrap(), "8080");
}

#[test]
fn flat_keeps_the_key_as_one_component() {
    let file = Fixture::new(b"APP_http:port=8080\n");
    let config = Builder::new()
        .dotenv_file("dotenv", DotenvFile::new(file.path()).prefix("APP_").flat())
        .build()
        .expect("the dotenv source builds");

    assert_eq!(config.get::<String>("http:port").unwrap(), "8080");
}

#[test]
fn a_later_assignment_wins_at_the_same_path() {
    let file = Fixture::new(b"APP_PORT=1\nAPP_PORT=2\n");
    let config = Builder::new()
        .dotenv_file("dotenv", DotenvFile::new(file.path()).prefix("APP_"))
        .build()
        .expect("the dotenv source builds");

    assert_eq!(config.get::<String>("PORT").unwrap(), "2");
}

#[test]
fn an_invalid_line_rejects_the_source_without_the_value() {
    let file = Fixture::new(format!("APP_PORT={SECRET}\nbroken\n"));
    let error = Builder::new()
        .dotenv_file("dotenv", DotenvFile::new(file.path()).prefix("APP_"))
        .build()
        .expect_err("an invalid line rejects the document");

    assert_eq!(error.source_kind(), Some(SourceErrorKind::Invalid));
    for text in [format!("{error}"), format!("{error:?}")] {
        assert!(
            !text.contains(SECRET),
            "the value reached the error: {text}"
        );
    }
}

#[test]
fn a_scalar_and_a_map_on_one_key_do_not_echo_a_value() {
    let file = Fixture::new(format!("APP_http={SECRET}\nAPP_http.port=1\n"));
    let error = Builder::new()
        .dotenv_file("dotenv", DotenvFile::new(file.path()).prefix("APP_"))
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
fn an_empty_key_or_an_empty_remainder_is_invalid() {
    for contents in [&b"=value\n"[..], &b"APP_=value\n"[..]] {
        let file = Fixture::new(contents);
        let error = Builder::new()
            .dotenv_file("dotenv", DotenvFile::new(file.path()).prefix("APP_"))
            .build()
            .expect_err("the assignment cannot become a path");
        assert_eq!(error.source_kind(), Some(SourceErrorKind::Invalid));
    }
}

#[test]
fn a_non_utf8_file_is_invalid_without_its_bytes() {
    let file = Fixture::new(b"APP_PORT=fixture-secret-9f3a2c-\xff\n");
    let error = Builder::new()
        .dotenv_file("dotenv", DotenvFile::new(file.path()).prefix("APP_"))
        .build()
        .expect_err("a non-UTF-8 file is rejected");

    assert_eq!(error.source_kind(), Some(SourceErrorKind::Invalid));
    for text in [format!("{error}"), format!("{error:?}")] {
        assert!(!text.contains(SECRET), "the file reached the error: {text}");
    }
}

#[test]
fn a_missing_file_is_unavailable_without_its_path() {
    let file = Fixture::new(b"APP_PORT=1\n");
    let path = file.path().to_owned();
    fs::remove_file(&path).expect("the fixture file was created");
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .expect("the fixture name is Unicode")
        .to_owned();

    let error = Builder::new()
        .dotenv_file("dotenv", DotenvFile::new(&path).prefix("APP_"))
        .build()
        .expect_err("a missing file is unavailable");

    assert_eq!(error.source_kind(), Some(SourceErrorKind::Unavailable));
    assert_eq!(error.source_name(), Some("dotenv"));
    for text in [format!("{error}"), format!("{error:?}")] {
        assert!(!text.contains(&name), "the path reached the error: {text}");
    }
}

#[test]
fn the_configured_path_is_available_to_the_caller() {
    let file = Fixture::new(b"APP_PORT=1\n");
    let source = DotenvFile::new(file.path());
    assert_eq!(source.path(), file.path());
    assert!(
        !format!("{source:?}").contains(
            file.path()
                .file_name()
                .and_then(|name| name.to_str())
                .expect("the fixture name is Unicode")
        )
    );
}

#[test]
fn the_environment_is_the_fallback_for_missing_document_values() {
    let file = Fixture::new(b"APP_HOME=$HOME/bin\nAPP_BRACED=${HOME}/lib\n");
    let config = Builder::new()
        .dotenv_file(
            "dotenv",
            DotenvFile::new(file.path())
                .prefix("APP_")
                .with_environment([("HOME", "fixture/home")]),
        )
        .build()
        .expect("the dotenv source builds");

    assert_eq!(config.get::<String>("HOME").unwrap(), "fixture/home/bin");
    assert_eq!(config.get::<String>("BRACED").unwrap(), "fixture/home/lib");
}

#[test]
fn an_unselected_assignment_is_visible_to_a_later_selected_value() {
    let file = Fixture::new(b"OTHER=/from-document\nAPP_VALUE=$OTHER\n");
    let config = Builder::new()
        .dotenv_file(
            "dotenv",
            DotenvFile::new(file.path())
                .prefix("APP_")
                .with_environment([("OTHER", "/from-environment")]),
        )
        .build()
        .expect("the dotenv source builds");

    assert_eq!(config.get::<String>("VALUE").unwrap(), "/from-document");
    assert!(!config.has("OTHER").unwrap());
}

#[test]
fn expansion_is_sequential_for_duplicates_and_forward_references() {
    let file = Fixture::new(b"APP_A=first\nAPP_A=$APP_A-second\nAPP_B=$APP_C\nAPP_C=later\n");
    let config = Builder::new()
        .dotenv_file(
            "dotenv",
            DotenvFile::new(file.path())
                .prefix("APP_")
                .with_environment([("APP_C", "from-environment")]),
        )
        .build()
        .expect("the dotenv source builds");

    assert_eq!(config.get::<String>("A").unwrap(), "first-second");
    assert_eq!(config.get::<String>("B").unwrap(), "from-environment");
    assert_eq!(config.get::<String>("C").unwrap(), "later");
}

#[test]
fn a_local_assignment_is_visible_to_later_assignments_only() {
    let file = Fixture::new(
        b"APP_FIRST=${LOCAL:=fallback}\nAPP_SECOND=$LOCAL\nAPP_PAIR=${PAIR:=a}-${PAIR}\n",
    );
    let config = Builder::new()
        .dotenv_file(
            "dotenv",
            DotenvFile::new(file.path())
                .prefix("APP_")
                .with_environment([("HOME", "fixture/home")]),
        )
        .build()
        .expect("the dotenv source builds");

    assert_eq!(config.get::<String>("FIRST").unwrap(), "fallback");
    assert_eq!(config.get::<String>("SECOND").unwrap(), "fallback");
    assert_eq!(config.get::<String>("PAIR").unwrap(), "a-a");
    assert!(!config.has("LOCAL").unwrap());
}

#[test]
fn unquoted_and_double_quoted_expand_while_single_and_raw_stay_literal() {
    let file = Fixture::new(
        b"APP_UNQUOTED=$HOME/unquoted\nAPP_DOUBLE=\"$HOME/double\"\nAPP_SINGLE='$HOME/single'\nAPP_RAW=`$HOME/raw`\nAPP_COMMENT=value # $HOME not part of the value\nAPP_QUOTED_HASH=\"value # $HOME\"\n",
    );
    let config = Builder::new()
        .dotenv_file(
            "dotenv",
            DotenvFile::new(file.path())
                .prefix("APP_")
                .with_environment([("HOME", "fixture/home")]),
        )
        .build()
        .expect("the dotenv source builds");

    assert_eq!(
        config.get::<String>("UNQUOTED").unwrap(),
        "fixture/home/unquoted"
    );
    assert_eq!(
        config.get::<String>("DOUBLE").unwrap(),
        "fixture/home/double"
    );
    assert_eq!(config.get::<String>("SINGLE").unwrap(), "$HOME/single");
    assert_eq!(config.get::<String>("RAW").unwrap(), "$HOME/raw");
    assert_eq!(config.get::<String>("COMMENT").unwrap(), "value");
    assert_eq!(
        config.get::<String>("QUOTED_HASH").unwrap(),
        "value # fixture/home"
    );
}

#[test]
fn a_backslash_or_a_second_dollar_keeps_a_dollar_literal() {
    let file =
        Fixture::new(b"APP_BACKSLASH=\\$HOME\nAPP_DOUBLE_DOLLAR=$$HOME\nAPP_DOUBLE=\"\\$HOME\"\n");
    let config = Builder::new()
        .dotenv_file(
            "dotenv",
            DotenvFile::new(file.path())
                .prefix("APP_")
                .with_environment([("HOME", "fixture/home")]),
        )
        .build()
        .expect("the dotenv source builds");

    assert_eq!(config.get::<String>("BACKSLASH").unwrap(), "$HOME");
    assert_eq!(config.get::<String>("DOUBLE_DOLLAR").unwrap(), "$HOME");
    assert_eq!(config.get::<String>("DOUBLE").unwrap(), "$HOME");
}

#[test]
fn command_substitution_and_windows_references_stay_literal() {
    let file = Fixture::new(b"APP_COMMAND=$(printf expanded)\nAPP_WINDOWS=%HOME%\n");
    let config = Builder::new()
        .dotenv_file(
            "dotenv",
            DotenvFile::new(file.path())
                .prefix("APP_")
                .with_environment([("HOME", "fixture/home")]),
        )
        .build()
        .expect("the dotenv source builds");

    assert_eq!(
        config.get::<String>("COMMAND").unwrap(),
        "$(printf expanded)"
    );
    assert_eq!(config.get::<String>("WINDOWS").unwrap(), "%HOME%");
}

#[test]
fn a_missing_name_is_empty_and_a_default_can_replace_it() {
    let file = Fixture::new(
        b"APP_EMPTY=$NEOTALES_CONFIG_DOTENV_MISSING_9F3A2C\nAPP_DEFAULT=${NEOTALES_CONFIG_DOTENV_MISSING_9F3A2C:-fallback}\nAPP_COLON_DEFAULT=${NEOTALES_CONFIG_DOTENV_MISSING_9F3A2C:fallback}\nAPP_NESTED=${NEOTALES_CONFIG_DOTENV_MISSING_9F3A2C:-${HOME}/nested}\n",
    );
    let config = Builder::new()
        .dotenv_file(
            "dotenv",
            DotenvFile::new(file.path())
                .prefix("APP_")
                .with_environment([("HOME", "fixture/home")]),
        )
        .build()
        .expect("the dotenv source builds");

    assert_eq!(config.get::<String>("EMPTY").unwrap(), "");
    assert_eq!(config.get::<String>("DEFAULT").unwrap(), "fallback");
    assert_eq!(config.get::<String>("COLON_DEFAULT").unwrap(), "fallback");
    assert_eq!(
        config.get::<String>("NESTED").unwrap(),
        "fixture/home/nested"
    );
}

#[test]
fn a_required_reference_rejects_without_its_message() {
    let file = Fixture::new(format!(
        "APP_VALUE=${{NEOTALES_CONFIG_DOTENV_MISSING_9F3A2C:?{SECRET}}}\n"
    ));
    let error = Builder::new()
        .dotenv_file(
            "dotenv",
            DotenvFile::new(file.path())
                .prefix("APP_")
                .with_environment([("UNRELATED", "value")]),
        )
        .build()
        .expect_err("a required reference with no value rejects the source");

    assert_eq!(error.source_kind(), Some(SourceErrorKind::Invalid));
    for text in [format!("{error}"), format!("{error:?}")] {
        assert!(
            !text.contains(SECRET),
            "the required message reached the error: {text}"
        );
    }
}

#[test]
fn an_unselected_malformed_reference_rejects_the_source() {
    let file = Fixture::new(format!(
        "IGNORED=${{NEOTALES_CONFIG_DOTENV_MISSING_9F3A2C:?{SECRET}}}\nAPP_OK=1\n"
    ));
    let error = Builder::new()
        .dotenv_file(
            "dotenv",
            DotenvFile::new(file.path())
                .prefix("APP_")
                .with_environment([("UNRELATED", "value")]),
        )
        .build()
        .expect_err("expansion runs before prefix selection");

    assert_eq!(error.source_kind(), Some(SourceErrorKind::Invalid));
    for text in [format!("{error}"), format!("{error:?}")] {
        assert!(
            !text.contains(SECRET),
            "the required message reached the error: {text}"
        );
    }
}

#[test]
fn a_malformed_reference_rejects_the_source() {
    for contents in [
        &b"APP_VALUE=${MISSING\n"[..],
        &b"APP_VALUE=${}\n"[..],
        &b"APP_VALUE=${MISSING-default}\n"[..],
    ] {
        let file = Fixture::new(contents);
        let error = Builder::new()
            .dotenv_file(
                "dotenv",
                DotenvFile::new(file.path())
                    .prefix("APP_")
                    .with_environment([("UNRELATED", "value")]),
            )
            .build()
            .expect_err("a malformed reference rejects the source");
        assert_eq!(error.source_kind(), Some(SourceErrorKind::Invalid));
    }
}

#[test]
fn a_local_assignment_never_writes_the_process_environment() {
    const NAME: &str = "NEOTALES_CONFIG_DOTENV_LOCAL_ONLY_9F3A2C";
    assert!(
        std::env::var_os(NAME).is_none(),
        "the test name is already present in the environment"
    );

    let file = Fixture::new(format!("APP_VALUE=${{{NAME}:=local}}\nAPP_LATER=${NAME}\n"));
    let config = Builder::new()
        .dotenv_file("dotenv", DotenvFile::new(file.path()).prefix("APP_"))
        .build()
        .expect("the dotenv source builds");

    assert_eq!(config.get::<String>("VALUE").unwrap(), "local");
    assert_eq!(config.get::<String>("LATER").unwrap(), "local");
    assert!(
        std::env::var_os(NAME).is_none(),
        "the setter touched the process environment"
    );
}

#[cfg(feature = "reload")]
#[test]
fn expanded_values_are_rebuilt_on_reload() {
    use neotales_config::RefreshOutcome;

    let file = Fixture::new(b"APP_VALUE=$HOME/one\n");
    let reload = Builder::new()
        .dotenv_file(
            "dotenv",
            DotenvFile::new(file.path())
                .prefix("APP_")
                .with_environment([("HOME", "fixture/home")]),
        )
        .build_reloadable()
        .expect("the dotenv source reloads");
    assert_eq!(
        reload.config().get::<String>("VALUE").unwrap(),
        "fixture/home/one"
    );

    fs::write(file.path(), b"APP_VALUE=$HOME/two\n").expect("the fixture file is writable");
    assert_eq!(reload.refresh_if_changed(), RefreshOutcome::Reloaded);
    assert_eq!(
        reload.config().get::<String>("VALUE").unwrap(),
        "fixture/home/two"
    );
}

#[test]
fn the_default_lookup_reads_the_process_environment() {
    // PATH is a non-secret value every supported runner sets. The tests above inject a fixed
    // environment so their values are synthetic; this one exercises the default callback, which
    // must read the process rather than a fixed empty map.
    let path = std::env::var("PATH").expect("the process environment has a PATH");
    let file = Fixture::new(b"APP_PATH=$PATH\n");
    let config = Builder::new()
        .dotenv_file("dotenv", DotenvFile::new(file.path()).prefix("APP_"))
        .build()
        .expect("the dotenv source builds");

    assert_eq!(config.get::<String>("PATH").unwrap(), path);
}

#[test]
fn the_fixed_environment_supplies_lookups_but_not_configuration_keys() {
    let file = Fixture::new(b"APP_VALUE=$ONLY_LOOKUP\n");
    let config = Builder::new()
        .dotenv_file(
            "dotenv",
            DotenvFile::new(file.path())
                .prefix("APP_")
                .with_environment([("ONLY_LOOKUP", "from-environment")]),
        )
        .build()
        .expect("the dotenv source builds");

    assert_eq!(config.get::<String>("VALUE").unwrap(), "from-environment");
    assert!(!config.has("ONLY_LOOKUP").unwrap());
}

#[test]
fn an_assigned_operand_is_stored_as_written() {
    let file = Fixture::new(b"APP_FIRST=${ASSIGNED:=$HOME}\nAPP_LATER=$ASSIGNED\n");
    let config = Builder::new()
        .dotenv_file(
            "dotenv",
            DotenvFile::new(file.path())
                .prefix("APP_")
                .with_environment([("HOME", "fixture/home")]),
        )
        .build()
        .expect("the dotenv source builds");

    assert_eq!(config.get::<String>("FIRST").unwrap(), "$HOME");
    assert_eq!(config.get::<String>("LATER").unwrap(), "$HOME");
}

#[test]
fn a_looked_up_value_is_not_expanded_again() {
    let file = Fixture::new(b"APP_VALUE=$WRAPPED\n");
    let config = Builder::new()
        .dotenv_file(
            "dotenv",
            DotenvFile::new(file.path())
                .prefix("APP_")
                .with_environment([("WRAPPED", "$HOME"), ("HOME", "fixture/home")]),
        )
        .build()
        .expect("the dotenv source builds");

    assert_eq!(config.get::<String>("VALUE").unwrap(), "$HOME");
}
