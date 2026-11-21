//! A snapshot is immutable, and a clone and a child share it.

use neotales_config::{Builder, Value};

fn config() -> neotales_config::Config {
    Builder::new()
        .memory(
            "defaults",
            Value::map([(
                "http",
                Value::map([
                    ("port", Value::from(8080_i64)),
                    ("host", Value::from("localhost")),
                ]),
            )]),
        )
        .build()
        .expect("the fixture builds")
}

#[test]
fn a_clone_reads_the_same_snapshot() {
    let config = config();
    let clone = config.clone();
    assert_eq!(clone.get::<i64>("http.port").unwrap(), 8080);
    assert_eq!(config.origin("http.port").unwrap(), "defaults");
    assert_eq!(clone.origin("http.port").unwrap(), "defaults");
}

#[test]
fn a_child_and_its_parent_share_the_snapshot() {
    let config = config();
    let http = config.child("http").expect("http exists");
    assert_eq!(http.get::<u16>("port").unwrap(), 8080);
    assert_eq!(config.get::<u16>("http.port").unwrap(), 8080);
    assert_eq!(http.origin("port").unwrap(), "defaults");
    assert_eq!(config.origin("http.port").unwrap(), "defaults");
}

#[test]
fn config_debug_reports_the_cursor_path_and_type() {
    let config = config();
    let root = format!("{config:?}");
    assert!(root.contains("path"), "{root}");
    assert!(root.contains("map"), "{root}");

    let child = format!("{:?}", config.child("http").unwrap());
    assert!(child.contains("http"), "{child}");
    assert!(child.contains("map"), "{child}");
}
