//! Source precedence, recursive merge, source names, and per-node attribution.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use neotales_config::{Builder, Config, Source, SourceError, Value};

fn config() -> Config {
    Builder::new()
        .memory(
            "defaults",
            Value::map([
                (
                    "http",
                    Value::map([("port", Value::from(8080_i64)), ("host", Value::from("a"))]),
                ),
                (
                    "list",
                    Value::array([Value::from(1_i64), Value::from(2_i64)]),
                ),
                ("map", Value::map([("inner", Value::from(true))])),
                ("scalar", Value::from("first")),
            ]),
        )
        .memory(
            "overlay",
            Value::map([
                ("http", Value::map([("port", Value::from(9090_i64))])),
                ("list", Value::array([Value::from(3_i64)])),
                ("map", Value::from(7_i64)),
                ("scalar", Value::map([("inner", Value::from(false))])),
            ]),
        )
        .build()
        .expect("the fixture builds")
}

#[test]
fn a_later_source_wins_per_key_and_nested_maps_merge() {
    let config = config();
    assert_eq!(config.get::<i64>("http.port").unwrap(), 9090);
    assert_eq!(config.get::<String>("http.host").unwrap(), "a");
}

#[test]
fn arrays_replace_and_are_never_concatenated() {
    let config = config();
    assert_eq!(config.get::<Vec<i64>>("list").unwrap(), vec![3]);
}

#[test]
fn a_scalar_replaces_a_map_and_a_map_replaces_a_scalar() {
    let config = config();
    assert_eq!(config.get::<i64>("map").unwrap(), 7);
    assert!(!config.has("map.inner").unwrap());
    assert!(!config.get::<bool>("scalar.inner").unwrap());
}

#[test]
fn a_node_keeps_the_source_that_last_wrote_it() {
    let config = config();
    assert_eq!(config.origin("http.port").unwrap(), "overlay");
    assert_eq!(config.origin("http.host").unwrap(), "defaults");
    // The map itself came from the earlier source; the later one only merged a child into it.
    assert_eq!(config.origin("http").unwrap(), "defaults");
    assert_eq!(config.origin("map").unwrap(), "overlay");
    assert_eq!(config.origin("scalar").unwrap(), "overlay");
    assert_eq!(config.origin("list").unwrap(), "overlay");
}

#[test]
fn replacing_a_subtree_drops_the_stale_provenance_below_it() {
    let config = Builder::new()
        .memory(
            "first",
            Value::map([(
                "branch",
                Value::map([("leaf", Value::from(1_i64)), ("other", Value::from(2_i64))]),
            )]),
        )
        .memory("second", Value::map([("branch", Value::from(9_i64))]))
        .memory(
            "third",
            Value::map([("branch", Value::map([("fresh", Value::from(4_i64))]))]),
        )
        .build()
        .expect("the fixture builds");

    assert_eq!(config.get::<i64>("branch.fresh").unwrap(), 4);
    assert!(!config.has("branch.leaf").unwrap());
    assert!(!config.has("branch.other").unwrap());
    assert!(config.origin("branch.leaf").unwrap_err().is_missing());
    assert_eq!(config.origin("branch.fresh").unwrap(), "third");
    assert_eq!(config.origin("branch").unwrap(), "third");
}

#[test]
fn a_literal_dotted_key_does_not_share_identity_with_a_nested_path() {
    let config = Builder::new()
        .memory(
            "nested",
            Value::map([("http", Value::map([("port", Value::from(8080_i64))]))]),
        )
        .memory(
            "literal",
            Value::map([("http.port", Value::from(9090_i64))]),
        )
        .build()
        .expect("the fixture builds");

    assert_eq!(config.get::<i64>("http.port").unwrap(), 8080);
    assert_eq!(config.origin("http.port").unwrap(), "nested");
    assert_eq!(
        config.value().unwrap().as_map().unwrap()["http.port"],
        Value::from(9090_i64),
        "the literal key is still in the tree"
    );
}

#[test]
fn with_another_delimiter_a_dotted_key_is_one_component() {
    let config = Builder::new()
        .delimiter(':')
        .memory(
            "nested",
            Value::map([("http", Value::map([("port", Value::from(8080_i64))]))]),
        )
        .memory(
            "literal",
            Value::map([("http.port", Value::from(9090_i64))]),
        )
        .build()
        .expect("the fixture builds");

    assert_eq!(config.get::<i64>("http:port").unwrap(), 8080);
    assert_eq!(config.origin("http:port").unwrap(), "nested");
    assert_eq!(config.get::<i64>("http.port").unwrap(), 9090);
    assert_eq!(config.origin("http.port").unwrap(), "literal");
}

#[test]
fn source_names_must_be_unique() {
    let error = Builder::new()
        .memory("same", Value::map([("a", Value::from(1_i64))]))
        .memory("same", Value::map([("b", Value::from(2_i64))]))
        .build()
        .expect_err("a repeated source name is refused");

    assert!(error.is_duplicate_source(), "{error}");
    assert_eq!(error.source_name(), Some("same"));
}

#[test]
fn source_names_must_not_be_empty() {
    let error = Builder::new()
        .memory("", Value::map([("a", Value::from(1_i64))]))
        .build()
        .expect_err("an empty source name is refused");

    assert!(error.is_empty_source_name(), "{error}");
}

#[test]
fn a_duplicate_name_is_refused_before_any_source_is_loaded() {
    let loaded = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&loaded);
    let error = Builder::new()
        .source("same", move || {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(Value::map([("a", Value::from(1_i64))]))
        })
        .memory("same", Value::map([("b", Value::from(2_i64))]))
        .build()
        .expect_err("a repeated source name is refused");

    assert!(error.is_duplicate_source(), "{error}");
    assert_eq!(
        loaded.load(Ordering::SeqCst),
        0,
        "the name check runs before a source is loaded"
    );
}

#[test]
fn a_source_root_must_be_a_map() {
    let error = Builder::new()
        .memory("scalar", Value::from(1_i64))
        .build()
        .expect_err("a scalar root is refused");

    assert!(error.is_root_not_map(), "{error}");
    assert_eq!(error.source_name(), Some("scalar"));
}

#[test]
fn an_empty_source_list_builds_an_empty_map() {
    let config = Builder::new().build().expect("an empty build succeeds");
    assert_eq!(config.value().unwrap().as_map().unwrap().len(), 0);
    assert!(!config.has("anything").unwrap());
    assert!(config.get::<i64>("anything").unwrap_err().is_missing());
}

#[test]
fn a_caller_supplied_source_joins_the_order() {
    let config = Builder::new()
        .memory("defaults", Value::map([("key", Value::from("first"))]))
        .source("remote", || {
            Ok(Value::map([("key", Value::from("second"))]))
        })
        .build()
        .expect("the fixture builds");

    assert_eq!(config.get::<String>("key").unwrap(), "second");
    assert_eq!(config.origin("key").unwrap(), "remote");
}

#[test]
fn a_source_is_loaded_once_at_build() {
    let loaded = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&loaded);
    let config = Builder::new()
        .source("computed", move || {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(Value::map([("key", Value::from(1_i64))]))
        })
        .build()
        .expect("the fixture builds");

    assert_eq!(config.get::<i64>("key").unwrap(), 1);
    assert_eq!(config.origin("key").unwrap(), "computed");
    assert_eq!(
        loaded.load(Ordering::SeqCst),
        1,
        "a read never loads a source again"
    );
}

#[test]
fn a_source_failure_keeps_its_kind_and_the_registered_name() {
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

    assert!(error.is_source_failure(), "{error}");
    assert_eq!(error.source_name(), Some("remote"));
    assert_eq!(
        error.source_kind(),
        Some(neotales_config::SourceErrorKind::Invalid)
    );
}

#[test]
fn a_non_finite_float_fails_before_the_tree_is_merged() {
    let error = Builder::new()
        .memory("defaults", Value::map([("ratio", Value::Float(f64::NAN))]))
        .build()
        .expect_err("a non-finite float is refused");

    assert!(error.is_unrepresentable(), "{error}");
    assert_eq!(error.source_name(), Some("defaults"));
}
