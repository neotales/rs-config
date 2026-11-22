//! Per-provider behavior of the `optional` and `reload_on_change` file flags.
//!
//! Every suite runs against a real path in a temporary directory. The optional case must accept
//! only a genuine not-found; a directory, a non-UTF-8 document, and a parse failure must all still
//! reject the source. The watch case asserts the public [`Source::watch_paths`] result directly,
//! so it does not depend on the `reload` feature.

#![cfg(any(
    feature = "dotenv",
    feature = "json",
    feature = "json5",
    feature = "jsonc",
    feature = "toml",
    feature = "yaml",
    feature = "ini"
))]

use neotales_config::{Builder, Source, SourceErrorKind, Value};

const SENTINEL: &str = "fixture-secret-9f3a2c";

/// Generates the flag suite for one file provider.
macro_rules! provider_suite {
    ($type:ident, $builder:ident, $file:literal, $valid:literal, $invalid:literal) => {
        #[test]
        fn a_required_file_is_the_default_and_a_missing_one_fails() {
            let directory = tempfile::tempdir().expect("a temporary directory");
            let path = directory.path().join($file);

            let error = Builder::new()
                .$builder("file", neotales_config::$type::new(&path))
                .build()
                .expect_err("a required file that is absent fails the build");

            assert!(error.is_source_failure(), "{error}");
            assert_eq!(error.source_kind(), Some(SourceErrorKind::Unavailable));
            assert_eq!(error.source_name(), Some("file"));
        }

        #[test]
        fn an_optional_missing_file_contributes_an_empty_map() {
            let directory = tempfile::tempdir().expect("a temporary directory");
            let path = directory.path().join($file);

            let config = Builder::new()
                .memory("defaults", Value::map([("fallback", Value::from(1_i64))]))
                .$builder("file", neotales_config::$type::new(&path).optional(true))
                .build()
                .expect("an absent optional file is not an error");

            assert_eq!(config.get::<i64>("fallback").unwrap(), 1);
            assert_eq!(config.origin("fallback").unwrap(), "defaults");
            assert_eq!(
                config.value().unwrap().as_map().unwrap().len(),
                1,
                "an absent optional file must not contribute a key"
            );
        }

        #[test]
        fn an_optional_present_file_reads_normally() {
            let directory = tempfile::tempdir().expect("a temporary directory");
            let path = directory.path().join($file);
            std::fs::write(&path, $valid).expect("the fixture is writable");

            let config = Builder::new()
                .$builder("file", neotales_config::$type::new(&path).optional(true))
                .build()
                .expect("a present optional file still reads");

            assert!(!config.value().unwrap().as_map().unwrap().is_empty());
        }

        #[test]
        fn an_optional_directory_is_still_unavailable() {
            let directory = tempfile::tempdir().expect("a temporary directory");

            let error = Builder::new()
                .$builder(
                    "file",
                    neotales_config::$type::new(directory.path()).optional(true),
                )
                .build()
                .expect_err("a directory is not an absent file");

            assert!(error.is_source_failure(), "{error}");
            assert_eq!(error.source_kind(), Some(SourceErrorKind::Unavailable));
        }

        #[test]
        fn an_optional_invalid_document_still_fails_without_its_text() {
            let directory = tempfile::tempdir().expect("a temporary directory");
            let path = directory.path().join($file);
            std::fs::write(&path, $invalid).expect("the fixture is writable");

            let error = Builder::new()
                .$builder("file", neotales_config::$type::new(&path).optional(true))
                .build()
                .expect_err("a malformed document is not an absent file");

            assert!(error.is_source_failure(), "{error}");
            assert_eq!(error.source_kind(), Some(SourceErrorKind::Invalid));
            for text in [format!("{error}"), format!("{error:?}")] {
                assert!(!text.contains(SENTINEL), "{text}");
            }
        }

        #[test]
        fn an_optional_non_utf8_document_still_fails() {
            let directory = tempfile::tempdir().expect("a temporary directory");
            let path = directory.path().join($file);
            std::fs::write(&path, [0xff_u8, 0xfe, 0xfd]).expect("the fixture is writable");

            let error = Builder::new()
                .$builder("file", neotales_config::$type::new(&path).optional(true))
                .build()
                .expect_err("a non-UTF-8 document is not an absent file");

            assert!(error.is_source_failure(), "{error}");
            assert_eq!(error.source_kind(), Some(SourceErrorKind::Invalid));
        }

        #[test]
        fn the_change_watch_flag_controls_the_reported_path() {
            let directory = tempfile::tempdir().expect("a temporary directory");
            let path = directory.path().join($file);

            let default = neotales_config::$type::new(&path);
            assert_eq!(default.watch_paths(), vec![path.clone()]);

            let disabled = neotales_config::$type::new(&path).reload_on_change(false);
            assert!(
                disabled.watch_paths().is_empty(),
                "a disabled change watch must report no path"
            );

            let enabled = neotales_config::$type::new(&path).reload_on_change(true);
            assert_eq!(enabled.watch_paths(), vec![path.clone()]);
        }

        #[test]
        fn the_configured_path_is_unchanged_by_the_flags() {
            let directory = tempfile::tempdir().expect("a temporary directory");
            let path = directory.path().join($file);

            let source = neotales_config::$type::new(&path)
                .optional(true)
                .reload_on_change(false);
            assert_eq!(source.path(), path);
        }
    };
}

#[cfg(feature = "json")]
mod json {
    use super::*;

    provider_suite!(
        JsonFile,
        json_file,
        "app.json",
        r#"{"probe": 8080}"#,
        r#"{"probe": fixture-secret-9f3a2c}"#
    );

    #[test]
    fn an_optional_empty_file_is_present_and_still_rejected() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = directory.path().join("empty.json");
        std::fs::write(&path, b"").expect("the fixture is writable");

        let error = Builder::new()
            .json_file("file", neotales_config::JsonFile::new(&path).optional(true))
            .build()
            .expect_err("an empty document is present, not absent");

        assert_eq!(error.source_kind(), Some(SourceErrorKind::Invalid));
    }
}

#[cfg(feature = "json5")]
mod json5 {
    use super::*;

    provider_suite!(
        Json5File,
        json5_file,
        "app.json5",
        "{ probe: 0x1F90 }",
        "{ probe: fixture-secret-9f3a2c }"
    );
}

#[cfg(feature = "jsonc")]
mod jsonc {
    use super::*;

    provider_suite!(
        JsoncFile,
        jsonc_file,
        "app.jsonc",
        "{\"probe\": 8080} // a comment",
        "{\"probe\": fixture-secret-9f3a2c}"
    );
}

#[cfg(feature = "toml")]
mod toml_file {
    use super::*;

    provider_suite!(
        TomlFile,
        toml_file,
        "app.toml",
        "probe = 8080\n",
        "probe = fixture-secret-9f3a2c\n"
    );
}

#[cfg(feature = "yaml")]
mod yaml {
    use super::*;

    provider_suite!(
        YamlFile,
        yaml_file,
        "app.yaml",
        "probe: 8080\n",
        "probe: [fixture-secret-9f3a2c\n"
    );
}

#[cfg(feature = "ini")]
mod ini {
    use super::*;

    provider_suite!(
        IniFile,
        ini_file,
        "app.ini",
        "probe = 8080\n",
        "fixture-secret-9f3a2c\n"
    );
}

#[cfg(feature = "dotenv")]
mod dotenv {
    use super::*;

    provider_suite!(
        DotenvFile,
        dotenv_file,
        ".env",
        "probe=8080\n",
        "fixture-secret-9f3a2c\n"
    );

    #[test]
    fn an_absent_optional_file_still_validates_the_key_policy() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = directory.path().join(".env");

        let error = Builder::new()
            .dotenv_file(
                "file",
                neotales_config::DotenvFile::new(&path)
                    .separator("")
                    .optional(true),
            )
            .build()
            .expect_err("an empty separator is a policy error, not an absence");

        assert!(error.is_source_failure(), "{error}");
        assert_eq!(error.source_kind(), Some(SourceErrorKind::Invalid));
    }
}
