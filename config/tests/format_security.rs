//! A format source must never put a path, a document line, a value, or parser text on an error.
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

/// Every observable form of an error, plus its source chain.
#[track_caller]
fn assert_sanitized(error: &neotales_config::Error, forbidden: &[&str]) {
    let shown = format!("{error}");
    let debugged = format!("{error:?}");
    for text in [&shown, &debugged] {
        for needle in forbidden {
            assert!(
                !text.contains(needle),
                "`{needle}` reached the error: {text}"
            );
        }
    }
    assert!(std::error::Error::source(error).is_none());
}

#[track_caller]
fn assert_rejected(name: &str, error: &neotales_config::Error) {
    assert!(error.is_source_failure(), "{name}: {error}");
    assert_sanitized(
        error,
        &[
            "fixture-secret",
            "fixtures/formats",
            "invalid.json",
            "invalid.toml",
            "invalid.yaml",
            "invalid.ini",
        ],
    );
}

#[cfg(any(
    feature = "json",
    feature = "json5",
    feature = "jsonc",
    feature = "toml",
    feature = "yaml",
    feature = "ini"
))]
#[test]
fn a_malformed_document_is_rejected_without_its_text() {
    use neotales_config::Builder;

    let directory = tempfile::tempdir().unwrap();
    let missing = directory.path().join("does-not-exist");

    #[cfg(feature = "json")]
    {
        let error = Builder::new()
            .json_file(
                "json",
                neotales_config::JsonFile::new(fixture("invalid.json")),
            )
            .build()
            .expect_err("the fixture is malformed");
        assert_rejected("json", &error);

        let error = Builder::new()
            .json_file("json", neotales_config::JsonFile::new(&missing))
            .build()
            .expect_err("the file is absent");
        assert_rejected("json missing", &error);
    }

    #[cfg(feature = "json5")]
    {
        let error = Builder::new()
            .json5_file(
                "json5",
                neotales_config::Json5File::new(fixture("invalid.json5")),
            )
            .build()
            .expect_err("the fixture is malformed");
        assert_rejected("json5", &error);

        let error = Builder::new()
            .json5_file("json5", neotales_config::Json5File::new(&missing))
            .build()
            .expect_err("the file is absent");
        assert_rejected("json5 missing", &error);
    }

    #[cfg(feature = "jsonc")]
    {
        let error = Builder::new()
            .jsonc_file(
                "jsonc",
                neotales_config::JsoncFile::new(fixture("invalid.jsonc")),
            )
            .build()
            .expect_err("the fixture is malformed");
        assert_rejected("jsonc", &error);

        let error = Builder::new()
            .jsonc_file("jsonc", neotales_config::JsoncFile::new(&missing))
            .build()
            .expect_err("the file is absent");
        assert_rejected("jsonc missing", &error);
    }

    #[cfg(feature = "toml")]
    {
        let error = Builder::new()
            .toml_file(
                "toml",
                neotales_config::TomlFile::new(fixture("invalid.toml")),
            )
            .build()
            .expect_err("the fixture is malformed");
        assert_rejected("toml", &error);

        let error = Builder::new()
            .toml_file("toml", neotales_config::TomlFile::new(&missing))
            .build()
            .expect_err("the file is absent");
        assert_rejected("toml missing", &error);
    }

    #[cfg(feature = "yaml")]
    {
        let error = Builder::new()
            .yaml_file(
                "yaml",
                neotales_config::YamlFile::new(fixture("invalid.yaml")),
            )
            .build()
            .expect_err("the fixture is malformed");
        assert_rejected("yaml", &error);

        let error = Builder::new()
            .yaml_file("yaml", neotales_config::YamlFile::new(&missing))
            .build()
            .expect_err("the file is absent");
        assert_rejected("yaml missing", &error);
    }

    #[cfg(feature = "ini")]
    {
        let error = Builder::new()
            .ini_file("ini", neotales_config::IniFile::new(fixture("invalid.ini")))
            .build()
            .expect_err("the fixture is malformed");
        assert_rejected("ini", &error);

        let error = Builder::new()
            .ini_file("ini", neotales_config::IniFile::new(&missing))
            .build()
            .expect_err("the file is absent");
        assert_rejected("ini missing", &error);
    }
}

#[cfg(any(
    feature = "json",
    feature = "json5",
    feature = "jsonc",
    feature = "toml",
    feature = "yaml",
    feature = "ini"
))]
#[test]
fn a_provider_debug_never_prints_its_path() {
    let path = fixture("does-not-exist-with-a-host-name");

    #[cfg(feature = "json")]
    {
        let debugged = format!("{:?}", neotales_config::JsonFile::new(&path));
        assert!(!debugged.contains("does-not-exist"), "{debugged}");
    }
    #[cfg(feature = "json5")]
    {
        let debugged = format!("{:?}", neotales_config::Json5File::new(&path));
        assert!(!debugged.contains("does-not-exist"), "{debugged}");
    }
    #[cfg(feature = "jsonc")]
    {
        let debugged = format!("{:?}", neotales_config::JsoncFile::new(&path));
        assert!(!debugged.contains("does-not-exist"), "{debugged}");
    }
    #[cfg(feature = "toml")]
    {
        let debugged = format!("{:?}", neotales_config::TomlFile::new(&path));
        assert!(!debugged.contains("does-not-exist"), "{debugged}");
    }
    #[cfg(feature = "yaml")]
    {
        let debugged = format!("{:?}", neotales_config::YamlFile::new(&path));
        assert!(!debugged.contains("does-not-exist"), "{debugged}");
    }
    #[cfg(feature = "ini")]
    {
        let debugged = format!("{:?}", neotales_config::IniFile::new(&path));
        assert!(!debugged.contains("does-not-exist"), "{debugged}");
    }
}

#[cfg(any(
    feature = "json",
    feature = "json5",
    feature = "jsonc",
    feature = "yaml"
))]
#[test]
fn a_document_root_that_is_not_a_map_is_rejected() {
    use neotales_config::Builder;

    let directory = tempfile::tempdir().unwrap();

    #[cfg(feature = "json")]
    {
        let path = directory.path().join("array.json");
        std::fs::write(&path, "[1, 2]").unwrap();
        let error = Builder::new()
            .json_file("json", neotales_config::JsonFile::new(&path))
            .build()
            .expect_err("an array is not a configuration object");
        assert!(error.is_source_failure(), "{error}");
    }

    #[cfg(feature = "json5")]
    {
        let path = directory.path().join("array.json5");
        std::fs::write(&path, "[1, 2]").unwrap();
        let error = Builder::new()
            .json5_file("json5", neotales_config::Json5File::new(&path))
            .build()
            .expect_err("an array is not a configuration object");
        assert!(error.is_source_failure(), "{error}");
    }

    #[cfg(feature = "jsonc")]
    {
        let path = directory.path().join("array.jsonc");
        std::fs::write(&path, "[1, 2] // comment").unwrap();
        let error = Builder::new()
            .jsonc_file("jsonc", neotales_config::JsoncFile::new(&path))
            .build()
            .expect_err("an array is not a configuration object");
        assert!(error.is_source_failure(), "{error}");
    }

    #[cfg(feature = "yaml")]
    {
        let path = directory.path().join("array.yaml");
        std::fs::write(&path, "- one\n- two\n").unwrap();
        let error = Builder::new()
            .yaml_file("yaml", neotales_config::YamlFile::new(&path))
            .build()
            .expect_err("a sequence is not a configuration map");
        assert!(error.is_source_failure(), "{error}");
    }
}

#[cfg(any(
    feature = "json",
    feature = "json5",
    feature = "jsonc",
    feature = "toml",
    feature = "yaml",
    feature = "ini"
))]
#[test]
fn a_non_utf8_file_is_invalid_without_its_bytes() {
    use neotales_config::Builder;

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("binary.dat");
    std::fs::write(&path, b"fixture-secret-9f3a2c\xff\xfe").unwrap();

    #[cfg(feature = "json")]
    {
        let error = Builder::new()
            .json_file("json", neotales_config::JsonFile::new(&path))
            .build()
            .expect_err("the file is not UTF-8");
        assert_rejected("json", &error);
    }
    #[cfg(feature = "json5")]
    {
        let error = Builder::new()
            .json5_file("json5", neotales_config::Json5File::new(&path))
            .build()
            .expect_err("the file is not UTF-8");
        assert_rejected("json5", &error);
    }
    #[cfg(feature = "jsonc")]
    {
        let error = Builder::new()
            .jsonc_file("jsonc", neotales_config::JsoncFile::new(&path))
            .build()
            .expect_err("the file is not UTF-8");
        assert_rejected("jsonc", &error);
    }
    #[cfg(feature = "toml")]
    {
        let error = Builder::new()
            .toml_file("toml", neotales_config::TomlFile::new(&path))
            .build()
            .expect_err("the file is not UTF-8");
        assert_rejected("toml", &error);
    }
    #[cfg(feature = "yaml")]
    {
        let error = Builder::new()
            .yaml_file("yaml", neotales_config::YamlFile::new(&path))
            .build()
            .expect_err("the file is not UTF-8");
        assert_rejected("yaml", &error);
    }
    #[cfg(feature = "ini")]
    {
        let error = Builder::new()
            .ini_file("ini", neotales_config::IniFile::new(&path))
            .build()
            .expect_err("the file is not UTF-8");
        assert_rejected("ini", &error);
    }
}
