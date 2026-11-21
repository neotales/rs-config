//! The opt-in dotenv file provider.

#![cfg(feature = "dotenv")]

use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use neotales_config::{Builder, DotenvFile, SourceErrorKind};

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
fn values_are_not_interpolated() {
    let file = Fixture::new(
        b"NAME=\"$OTHER\"\nRAW=`$OTHER`\nSINGLE='$OTHER'\nINLINE=value # literal\nQUOTED=\"value # literal\"\n",
    );
    let config = Builder::new()
        .dotenv_file("dotenv", DotenvFile::new(file.path()))
        .build()
        .expect("the dotenv source builds");

    assert_eq!(config.get::<String>("NAME").unwrap(), "$OTHER");
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
