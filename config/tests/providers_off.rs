//! The core stays memory-only when every provider feature is off.

#![cfg(not(any(feature = "args", feature = "environment", feature = "dotenv")))]

use neotales_config::{Builder, SourceError, Value};

#[test]
fn a_memory_source_still_builds_without_a_provider() {
    let config = Builder::new()
        .memory(
            "defaults",
            Value::map([("http", Value::map([("port", Value::from(8080_i64))]))]),
        )
        .build()
        .expect("a memory-only build succeeds");

    assert_eq!(config.get::<u16>("http.port").unwrap(), 8080);
}

#[test]
fn a_caller_source_still_builds_without_a_provider() {
    let config = Builder::new()
        .source("computed", || {
            Ok::<_, SourceError>(Value::map([("workers", Value::from(4_i64))]))
        })
        .build()
        .expect("a caller-supplied source builds");

    assert_eq!(config.get::<u8>("workers").unwrap(), 4);
}
