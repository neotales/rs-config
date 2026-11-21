//! Typed reads: presence, absence, type mismatch, and exact numeric conversion.

use std::collections::BTreeMap;

use neotales_config::{Builder, Config, Value};
use serde::Deserialize;

#[derive(Debug, Deserialize, PartialEq)]
struct Http {
    port: u16,
    host: String,
}

#[derive(Debug, Deserialize, PartialEq)]
struct Endpoint {
    port: u16,
}

#[derive(Debug, Deserialize, PartialEq)]
enum Connection {
    Pair(u8, u8),
    Struct(Endpoint),
}

#[derive(Debug, Deserialize, PartialEq)]
enum Shape {
    Unit,
    New(u16),
    Tuple(u16, u16),
    Struct { port: u16 },
}

/// The value a variant payload is given when it must fail to deserialize.
const INVALID: &str = "fixture-secret-9f3a2c";

fn fixture() -> Config {
    Builder::new()
        .memory(
            "defaults",
            Value::map([
                ("null", Value::Null),
                ("flag", Value::Bool(false)),
                ("zero", Value::from(0_i64)),
                ("empty", Value::from("")),
                ("text", Value::from("8080")),
                (
                    "http",
                    Value::map([
                        ("port", Value::from(8080_i64)),
                        ("host", Value::from("localhost")),
                    ]),
                ),
                (
                    "pair",
                    Value::map([(
                        "Pair",
                        Value::array([Value::from(1_i64), Value::from(2_i64)]),
                    )]),
                ),
                (
                    "struct",
                    Value::map([("Struct", Value::map([("port", Value::from(8080_i64))]))]),
                ),
                (
                    "long_pair",
                    Value::array([Value::from(1_i64), Value::from(2_i64), Value::from(3_i64)]),
                ),
                (
                    "labels",
                    Value::map([("a", Value::from("x")), ("b", Value::from("y"))]),
                ),
            ]),
        )
        .build()
        .expect("the fixture builds")
}

#[test]
fn present_null_false_zero_and_empty_are_not_missing() {
    let config = fixture();
    assert!(config.has("null").unwrap());
    assert!(config.has("flag").unwrap());
    assert!(config.has("zero").unwrap());
    assert!(config.has("empty").unwrap());
    assert_eq!(config.get::<Option<String>>("null").unwrap(), None);
    assert!(!config.get::<bool>("flag").unwrap());
    assert_eq!(config.get::<i64>("zero").unwrap(), 0);
    assert_eq!(config.get::<String>("empty").unwrap(), "");
}

#[test]
fn a_malformed_value_is_a_type_mismatch_and_an_absent_path_is_missing() {
    let config = fixture();
    let error = config
        .get::<i64>("text")
        .expect_err("a string is not an integer");
    assert!(error.is_type_mismatch(), "{error}");
    assert!(!error.is_missing(), "{error}");

    let error = config.get::<i64>("absent").expect_err("absent");
    assert!(error.is_missing(), "{error}");
    // An optional read does not turn a missing path into `None`.
    assert!(
        config
            .get::<Option<i64>>("absent")
            .expect_err("absent")
            .is_missing()
    );
}

#[test]
fn get_or_returns_the_default_only_for_absence() {
    let config = fixture();
    assert_eq!(config.get_or("absent", 7_i64).unwrap(), 7);
    assert_eq!(config.get_or("zero", 7_i64).unwrap(), 0);
    assert_eq!(
        config.get_or("empty", String::from("fallback")).unwrap(),
        ""
    );
    // A present null is not an absence, and a present null is not an integer.
    assert!(
        config
            .get_or("null", 7_i64)
            .expect_err("null")
            .is_type_mismatch()
    );
}

#[test]
fn reads_structs_and_arrays() {
    let config = fixture();
    assert_eq!(
        config.get::<Http>("http").unwrap(),
        Http {
            port: 8080,
            host: "localhost".to_owned(),
        }
    );
    assert_eq!(config.get::<Vec<i64>>("pair.Pair").unwrap(), vec![1, 2]);
}

#[test]
fn reads_maps() {
    let config = fixture();
    let labels = config
        .get::<BTreeMap<String, String>>("labels")
        .expect("a map reads as a map");
    assert_eq!(labels["a"], "x");
    assert_eq!(labels["b"], "y");
}

#[test]
fn reads_externally_tagged_tuple_and_struct_variants() {
    let config = fixture();
    assert_eq!(
        config.get::<Connection>("pair").unwrap(),
        Connection::Pair(1, 2)
    );
    assert_eq!(
        config.get::<Connection>("struct").unwrap(),
        Connection::Struct(Endpoint { port: 8080 })
    );
}

#[test]
fn a_tuple_read_rejects_a_sequence_of_another_length() {
    let config = fixture();
    assert!(
        config
            .get::<(u8, u8)>("long_pair")
            .unwrap_err()
            .is_type_mismatch()
    );
}

#[test]
fn a_string_is_never_parsed_as_a_number() {
    let config = fixture();
    for error in [
        config.get::<i64>("text").unwrap_err(),
        config.get::<u16>("text").unwrap_err(),
        config.get::<f64>("text").unwrap_err(),
        config.get::<bool>("text").unwrap_err(),
    ] {
        assert!(error.is_type_mismatch(), "{error}");
    }
    assert!(config.get::<String>("zero").unwrap_err().is_type_mismatch());
}

#[test]
fn numeric_reads_are_exact_within_a_kind_and_never_across_kinds() {
    let config = Builder::new()
        .memory(
            "numbers",
            Value::map([
                ("biggest", Value::Integer(i128::from(u64::MAX))),
                ("whole_float", Value::Float(8080.0)),
                ("fraction", Value::Float(8080.5)),
                ("narrow_float", Value::Float(0.1)),
                ("max_i64", Value::Integer(i128::from(i64::MAX))),
            ]),
        )
        .build()
        .expect("the fixture builds");

    // Within the integer kinds, a value reads as a narrower type only when it fits.
    assert_eq!(config.get::<u64>("biggest").unwrap(), u64::MAX);
    assert!(config.get::<i64>("biggest").unwrap_err().is_type_mismatch());

    // Within the float kinds, a value reads only as a float that holds it exactly.
    assert_eq!(config.get::<f64>("whole_float").unwrap(), 8080.0);
    assert_eq!(config.get::<f32>("whole_float").unwrap(), 8080.0_f32);
    assert!(
        config
            .get::<f32>("narrow_float")
            .unwrap_err()
            .is_type_mismatch()
    );

    // An integer is never read as a float and a float is never read as an integer, even when the
    // number has no fraction.
    for kind in ["max_i64", "biggest"] {
        assert!(
            config.get::<f64>(kind).unwrap_err().is_type_mismatch(),
            "{kind} as f64"
        );
        assert!(
            config.get::<f32>(kind).unwrap_err().is_type_mismatch(),
            "{kind} as f32"
        );
    }
    for kind in ["whole_float", "fraction"] {
        assert!(
            config.get::<i64>(kind).unwrap_err().is_type_mismatch(),
            "{kind} as i64"
        );
        assert!(
            config.get::<u16>(kind).unwrap_err().is_type_mismatch(),
            "{kind} as u16"
        );
    }
}

#[test]
fn a_nested_type_mismatch_names_the_source_of_the_failing_leaf() {
    let config = Builder::new()
        .memory(
            "defaults",
            Value::map([(
                "net",
                Value::map([(
                    "server",
                    Value::map([
                        ("port", Value::from(8080_i64)),
                        ("host", Value::from("localhost")),
                    ]),
                )]),
            )]),
        )
        .memory(
            "overlay",
            Value::map([(
                "net",
                Value::map([("server", Value::map([("port", Value::from("oops"))]))]),
            )]),
        )
        .build()
        .expect("the fixture builds");

    // The nested map was introduced by `defaults`; only the failing leaf came from `overlay`.
    assert_eq!(config.origin("net.server").unwrap(), "defaults");
    assert_eq!(config.origin("net.server.port").unwrap(), "overlay");

    let error = config
        .get::<Http>("net.server")
        .expect_err("the leaf is a string");
    assert!(error.is_type_mismatch(), "{error}");
    assert_eq!(error.path(), Some("net.server"));
    assert_eq!(error.source_name(), Some("overlay"), "{error}");

    // The same holds when a child cursor loads its focused value.
    let error = config
        .child("net.server")
        .unwrap()
        .load::<Http>()
        .expect_err("the leaf is a string");
    assert!(error.is_type_mismatch(), "{error}");
    assert_eq!(error.source_name(), Some("overlay"), "{error}");
}

#[test]
fn a_nested_mismatch_inside_an_array_names_the_array_source() {
    let config = Builder::new()
        .memory(
            "defaults",
            Value::map([("ports", Value::array([Value::from(80_i64)]))]),
        )
        .memory(
            "overlay",
            Value::map([(
                "ports",
                Value::array([Value::from(80_i64), Value::from("oops")]),
            )]),
        )
        .build()
        .expect("the fixture builds");

    // An array has no provenance of its own; the array node's source answers for its elements.
    let error = config
        .get::<Vec<u16>>("ports")
        .expect_err("the second element is a string");
    assert!(error.is_type_mismatch(), "{error}");
    assert_eq!(error.source_name(), Some("overlay"), "{error}");
}

#[test]
fn an_enum_payload_mismatch_names_the_source_of_the_failing_leaf() {
    // Every variant shape is introduced by `defaults` and its failing payload node is written by
    // `overlay`, so attributing the enum node itself reports the wrong source.
    let config = Builder::new()
        .memory(
            "defaults",
            Value::map([
                ("new", Value::map([("New", Value::from(1_i64))])),
                (
                    "tuple",
                    Value::map([(
                        "Tuple",
                        Value::array([Value::from(1_i64), Value::from(2_i64)]),
                    )]),
                ),
                (
                    "struct",
                    Value::map([("Struct", Value::map([("port", Value::from(8080_i64))]))]),
                ),
            ]),
        )
        .memory(
            "overlay",
            Value::map([
                ("new", Value::map([("New", Value::from(INVALID))])),
                (
                    "tuple",
                    Value::map([(
                        "Tuple",
                        Value::array([Value::from(1_i64), Value::from(INVALID)]),
                    )]),
                ),
                (
                    "struct",
                    Value::map([("Struct", Value::map([("port", Value::from(INVALID))]))]),
                ),
            ]),
        )
        .build()
        .expect("the fixture builds");

    // The fixtures are what make the attribution observable: the enum map and the struct variant
    // map come from `defaults`, and only the failing payload leaf comes from `overlay`.
    assert_eq!(config.origin("new").unwrap(), "defaults");
    assert_eq!(config.origin("new.New").unwrap(), "overlay");
    assert_eq!(config.origin("tuple").unwrap(), "defaults");
    assert_eq!(config.origin("tuple.Tuple").unwrap(), "overlay");
    assert_eq!(config.origin("struct").unwrap(), "defaults");
    assert_eq!(config.origin("struct.Struct").unwrap(), "defaults");
    assert_eq!(config.origin("struct.Struct.port").unwrap(), "overlay");

    for (path, expected) in [
        ("new", "an unsigned 16-bit integer"),
        ("tuple", "an unsigned 16-bit integer"),
        ("struct", "an unsigned 16-bit integer"),
    ] {
        let error = config
            .get::<Shape>(path)
            .expect_err("the payload is a string, not an integer");
        assert!(error.is_type_mismatch(), "{error}");
        // The reported path is the path that was read, not the internal leaf.
        assert_eq!(error.path(), Some(path), "{error}");
        assert_eq!(error.expected(), Some(expected), "{error}");
        assert_eq!(
            error.source_name(),
            Some("overlay"),
            "the failing payload leaf from `overlay` must answer for `{path}`: {error}"
        );
        for text in [format!("{error}"), format!("{error:?}")] {
            assert!(
                !text.contains(INVALID),
                "the payload reached the error: {text}"
            );
        }
    }
}

#[test]
fn an_enum_payload_of_the_wrong_shape_names_the_payload_source() {
    // Here the payload node itself is the one that fails, and only `overlay` wrote it. The variant
    // key must be part of the failing path, or attribution falls back to the enum map from
    // `defaults`.
    let config = Builder::new()
        .memory(
            "defaults",
            Value::map([
                ("unit", Value::map([("Unit", Value::Null)])),
                (
                    "tuple",
                    Value::map([(
                        "Tuple",
                        Value::array([Value::from(1_i64), Value::from(2_i64)]),
                    )]),
                ),
                (
                    "struct",
                    Value::map([("Struct", Value::map([("port", Value::from(8080_i64))]))]),
                ),
            ]),
        )
        .memory(
            "overlay",
            Value::map([
                ("unit", Value::map([("Unit", Value::from(1_i64))])),
                ("tuple", Value::map([("Tuple", Value::from(INVALID))])),
                ("struct", Value::map([("Struct", Value::from(INVALID))])),
            ]),
        )
        .build()
        .expect("the fixture builds");

    assert_eq!(config.origin("unit").unwrap(), "defaults");
    assert_eq!(config.origin("unit.Unit").unwrap(), "overlay");
    assert_eq!(config.origin("tuple").unwrap(), "defaults");
    assert_eq!(config.origin("tuple.Tuple").unwrap(), "overlay");
    assert_eq!(config.origin("struct").unwrap(), "defaults");
    assert_eq!(config.origin("struct.Struct").unwrap(), "overlay");

    for (path, expected) in [
        ("unit", "a unit variant"),
        ("tuple", "a sequence of the expected length"),
        ("struct", "a struct variant"),
    ] {
        let error = config
            .get::<Shape>(path)
            .expect_err("the payload has the wrong shape");
        assert!(error.is_type_mismatch(), "{error}");
        assert_eq!(error.path(), Some(path), "{error}");
        assert_eq!(error.expected(), Some(expected), "{error}");
        assert_eq!(
            error.source_name(),
            Some("overlay"),
            "the payload node from `overlay` must answer for `{path}`: {error}"
        );
        for text in [format!("{error}"), format!("{error:?}")] {
            assert!(
                !text.contains(INVALID),
                "the payload reached the error: {text}"
            );
        }
    }
}

#[test]
fn a_child_can_load_its_focused_value() {
    let config = fixture();
    let http = config.child("http").expect("http exists");
    assert_eq!(
        http.load::<Http>().unwrap(),
        Http {
            port: 8080,
            host: "localhost".to_owned(),
        }
    );
    assert_eq!(http.value().unwrap().type_name(), "map");
}

#[test]
fn a_mismatch_on_the_root_names_no_source() {
    // The root map was never written by a source, so there is nothing to attribute. Omitting the
    // source is deliberate: a guessed name is worse than no name.
    let config = Builder::new().build().expect("an empty build succeeds");
    let error = config.load::<i64>().expect_err("the root is a map");
    assert!(error.is_type_mismatch(), "{error}");
    assert_eq!(error.source_name(), None, "{error}");
}
