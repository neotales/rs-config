//! Behaviour tests for the web sample, including bounded hot-reload waits.

use std::{
    fs,
    io::{Read, Write as _},
    net::{SocketAddr, TcpStream},
    path::Path,
    thread,
    time::{Duration, Instant},
};

use neotales_config::Arguments;
use neotales_config_example_web::{Server, build_reload, environment_from_pairs, loopback_address};
use serde_json::Value as JsonValue;

const WAIT: Duration = Duration::from_secs(5);

fn write_defaults(directory: &Path, port: u16) {
    fs::write(
        directory.join("defaults.json"),
        format!(
            r#"{{
  "server": {{ "bind": "127.0.0.1", "port": {port} }},
  "demo": {{ "greeting": "hello from defaults.json", "mode": "defaults", "banner": false }}
}}
"#
        ),
    )
    .expect("write defaults.json");
}

fn write_atomic(path: &Path, contents: &str) {
    let replacement = path.with_extension("tmp");
    fs::write(&replacement, contents).expect("write replacement");
    if fs::rename(&replacement, path).is_err() {
        // Windows refuses to rename over an existing file. Removing first is
        // not atomic, but the watcher treats the gap as an optional-file
        // change and retries, which is exactly the behavior under test.
        fs::remove_file(path).expect("remove the previous file");
        fs::rename(&replacement, path).expect("rename replacement over target");
    }
}

fn no_arguments() -> Arguments {
    Arguments::new(Vec::new())
}

fn no_environment() -> neotales_config::Environment {
    environment_from_pairs(Vec::<(&str, &str)>::new())
}

fn start(directory: &Path) -> Server {
    let reload = build_reload(directory, no_arguments(), no_environment())
        .expect("the sample configuration loads");
    let address = loopback_address(&reload.config()).expect("the sample address is valid");
    Server::start(reload, address, Duration::from_millis(25)).expect("the sample server starts")
}

fn get(address: SocketAddr, path: &str) -> String {
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(2))
        .expect("connect to the sample server");
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("set a read timeout");
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n"
    )
    .expect("write the request");
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .expect("read the response");
    response
}

fn body(response: &str) -> &str {
    response
        .split_once("\r\n\r\n")
        .map_or("", |(_headers, body)| body)
}

fn wait_for(
    address: SocketAddr,
    path: &str,
    description: &str,
    predicate: impl Fn(&str) -> bool,
) -> String {
    let deadline = Instant::now() + WAIT;
    loop {
        let response = get(address, path);
        let body = body(&response).to_owned();
        if predicate(&body) {
            return body;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {description}; last body: {body}"
        );
        thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn http_views_expose_only_safe_keys_and_provenance() {
    let directory = tempfile::tempdir().expect("temporary directory");
    write_defaults(directory.path(), 0);
    fs::write(
        directory.path().join("site.toml"),
        "[demo]\ngreeting = \"hello from site.toml\"\nmode = \"toml\"\n",
    )
    .expect("write site.toml");
    fs::write(
        directory.path().join("app.env"),
        "APP_demo__greeting=\"hello from app.env for ${DEMO_NAME}\"\n\
         APP_demo__mode=dotenv\n\
         APP_demo__literal='${DEMO_NAME}'\n",
    )
    .expect("write app.env");

    let reload = build_reload(
        directory.path(),
        no_arguments(),
        environment_from_pairs([("TOP_SECRET", "canary-value-that-must-not-appear")]),
    )
    .expect("the sample configuration loads");
    let address = loopback_address(&reload.config()).expect("the sample address is valid");
    let server = Server::start(reload, address, Duration::from_millis(25))
        .expect("the sample server starts");
    let address = server.local_addr();

    assert!(address.ip().is_loopback());

    let response = get(address, "/api/config");
    assert!(response.starts_with("HTTP/1.1 200 OK"));
    assert!(response.contains("Cache-Control: no-store"));
    assert!(!response.contains("canary-value-that-must-not-appear"));
    let document: JsonValue = serde_json::from_str(body(&response)).expect("valid JSON");
    assert_eq!(
        document["keys"]["server.port"]["value"],
        JsonValue::from(0_u16)
    );
    assert_eq!(
        document["keys"]["server.port"]["origin"],
        JsonValue::from("defaults.json")
    );
    assert_eq!(
        document["keys"]["demo.mode"]["value"],
        JsonValue::from("dotenv")
    );
    assert_eq!(
        document["keys"]["demo.mode"]["origin"],
        JsonValue::from("app.env")
    );
    assert_eq!(
        document["keys"]["demo.literal"]["value"],
        JsonValue::from("${DEMO_NAME}")
    );
    assert_eq!(document["last_error"], JsonValue::Null);

    let html = get(address, "/");
    assert!(html.starts_with("HTTP/1.1 200 OK"));
    assert!(html.contains("Content-Security-Policy:"));
    assert!(html.contains("demo.mode"));
    assert!(html.contains("hello from app.env"));
    assert!(!html.contains("canary-value-that-must-not-appear"));

    let health = get(address, "/health");
    assert_eq!(body(&health), "ok\n");

    let missing = get(address, "/not-found");
    assert!(missing.starts_with("HTTP/1.1 404 Not Found"));

    server.stop();
}

#[test]
fn optional_files_appear_change_and_disappear_with_hot_reload() {
    let directory = tempfile::tempdir().expect("temporary directory");
    write_defaults(directory.path(), 0);
    let server = start(directory.path());
    let address = server.local_addr();
    let path = directory.path().join("site.toml");

    let initial = get(address, "/api/config");
    assert_eq!(
        serde_json::from_str::<JsonValue>(body(&initial)).unwrap()["keys"]["demo.mode"]["value"],
        JsonValue::from("defaults")
    );

    write_atomic(
        &path,
        "[demo]\ngreeting = \"first live edit\"\nmode = \"toml\"\n",
    );
    let first = wait_for(address, "/api/config", "the first TOML edit", |body| {
        body.contains("first live edit")
    });
    let first: JsonValue = serde_json::from_str(&first).expect("valid JSON");
    assert_eq!(
        first["keys"]["demo.greeting"]["origin"],
        JsonValue::from("site.toml")
    );

    write_atomic(
        &path,
        "[demo]\ngreeting = \"second live edit\"\nmode = \"toml\"\n",
    );
    wait_for(address, "/api/config", "the second TOML edit", |body| {
        body.contains("second live edit")
    });

    fs::remove_file(&path).expect("remove site.toml");
    let removed = wait_for(address, "/api/config", "the TOML removal", |body| {
        serde_json::from_str::<JsonValue>(body).is_ok_and(|document| {
            document["keys"]["demo.mode"]["value"] == "defaults"
                && document["keys"]["demo.greeting"]["origin"] == "defaults.json"
        })
    });
    let removed: JsonValue = serde_json::from_str(&removed).expect("valid JSON");
    assert_eq!(
        removed["keys"]["demo.greeting"]["value"],
        JsonValue::from("hello from defaults.json")
    );

    write_atomic(
        &directory.path().join("app.env"),
        "APP_demo__literal='${DEMO_NAME}'\n",
    );
    wait_for(
        address,
        "/api/config",
        "the dotenv file appearing",
        |body| body.contains("${DEMO_NAME}"),
    );

    server.stop();
}

#[test]
fn a_failed_reload_keeps_the_last_good_snapshot_and_reports_a_safe_error() {
    let directory = tempfile::tempdir().expect("temporary directory");
    write_defaults(directory.path(), 0);
    fs::write(
        directory.path().join("site.toml"),
        "[demo]\ngreeting = \"stable value\"\nmode = \"toml\"\n",
    )
    .expect("write site.toml");

    let server = start(directory.path());
    let address = server.local_addr();
    wait_for(address, "/api/config", "the initial TOML value", |body| {
        body.contains("stable value")
    });

    write_atomic(&directory.path().join("site.toml"), "[demo\n");
    let failed = wait_for(address, "/api/config", "the parse error", |body| {
        !body.contains("\"last_error\": null")
    });
    let failed: JsonValue = serde_json::from_str(&failed).expect("valid JSON");
    let last_error = failed["last_error"].as_str().expect("a string error");
    assert!(last_error.contains("site.toml"));
    assert!(last_error.contains("rejected the document"));
    assert!(!last_error.contains(&directory.path().display().to_string()));
    assert_eq!(
        failed["keys"]["demo.greeting"]["value"],
        JsonValue::from("stable value")
    );

    write_atomic(
        &directory.path().join("site.toml"),
        "[demo]\ngreeting = \"recovered value\"\nmode = \"toml\"\n",
    );
    let recovered = wait_for(address, "/api/config", "the recovered value", |body| {
        body.contains("recovered value")
    });
    let recovered: JsonValue = serde_json::from_str(&recovered).expect("valid JSON");
    assert_eq!(recovered["last_error"], JsonValue::Null);

    server.stop();
}

#[test]
fn a_non_loopback_address_is_refused() {
    let directory = tempfile::tempdir().expect("temporary directory");
    write_defaults(directory.path(), 0);
    let reload = build_reload(directory.path(), no_arguments(), no_environment())
        .expect("the sample configuration loads");

    let error = Server::start(
        reload,
        SocketAddr::from(([0, 0, 0, 0], 0)),
        Duration::from_millis(25),
    )
    .expect_err("a non-loopback bind is refused");

    assert!(error.to_string().contains("non-loopback"));
}

#[test]
fn a_missing_required_defaults_file_is_a_sanitized_error() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let error = build_reload(directory.path(), no_arguments(), no_environment())
        .expect_err("defaults.json is required");

    assert!(error.is_source_failure());
    let text = error.to_string();
    assert!(!text.contains(&directory.path().display().to_string()));
}
