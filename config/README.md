# neotales-config

Merge ordered configuration sources into one immutable, typed snapshot.

[![Neotales](https://github.com/neotales/brand/raw/main/assets/logo.png)](https://github.com/neotales/rs-config)

<!-- badges: crates.io, docs.rs, CI. Add once the crate name and first version exist. -->

## Install

```sh
cargo add neotales-config@0.0.0-alpha.0
```

No feature is on by default, and the core has no optional provider dependency. This alpha reads
in-memory values and caller-supplied sources.

## What you can do ⚙️

**Merge defaults and an overlay.** Later sources win per key, and nested maps merge.

```rust
use neotales_config::{Builder, Value};

let config = Builder::new()
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
    .memory(
        "overlay",
        Value::map([("http", Value::map([("port", Value::from(9090_i64))]))]),
    )
    .build()
    .expect("configuration builds");

assert_eq!(config.get::<u16>("http.port").unwrap(), 9090);
assert_eq!(config.get::<String>("http.host").unwrap(), "localhost");
```

**Read a typed document.** `get` deserializes one value, and `load` deserializes a focused
subtree. A missing path and a wrong type are different errors. Add `serde` with the `derive`
feature for the struct.

```rust
use neotales_config::{Builder, Value};
use serde::Deserialize;

#[derive(Debug, Deserialize, PartialEq)]
struct Http {
    port: u16,
    host: String,
}

let config = Builder::new()
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
    .expect("configuration builds");

let http = config.child("http").unwrap().load::<Http>().unwrap();
assert_eq!(http.host, "localhost");
```

**Ask where a value came from.** Provenance is recorded per node. A later source that only merges
children does not claim the parent map, and replacing a subtree drops the provenance below it.

```rust
use neotales_config::{Builder, Value};

let config = Builder::new()
    .memory(
        "defaults",
        Value::map([("http", Value::map([("port", Value::from(8080_i64))]))]),
    )
    .memory(
        "overlay",
        Value::map([("http", Value::map([("port", Value::from(9090_i64))]))]),
    )
    .build()
    .expect("configuration builds");

assert_eq!(config.origin("http.port").unwrap(), "overlay");
assert_eq!(config.origin("http").unwrap(), "defaults");
```

**Choose the path delimiter.** The default is `.`; `:`, `/`, `,`, and `|` are also supported. With
`:`, `http:port` is two components and `http.port` is one component whose name contains a dot. Any
other character is refused at `build()` with an invalid-delimiter error, before a source is read.

```rust
use neotales_config::{Builder, Value};

let config = Builder::new()
    .delimiter(':')
    .memory(
        "defaults",
        Value::map([
            ("http", Value::map([("port", Value::from(8080_i64))])),
            ("http.port", Value::from(9090_i64)),
        ]),
    )
    .build()
    .expect("configuration builds");

assert_eq!(config.get::<i64>("http:port").unwrap(), 8080);
assert_eq!(config.get::<i64>("http.port").unwrap(), 9090);
```

**Implement a source.** A source returns a map root or a `SourceError` that carries a kind and no
message. A closure implements `Source`, so a provider in another crate cannot leak a value through
this one.

```rust
use neotales_config::{Builder, SourceError, Value};

let config = Builder::new()
    .source("computed", || {
        Ok(Value::map([("workers", Value::from(4_i64))]))
    })
    .build()
    .expect("configuration builds");

assert_eq!(config.get::<u8>("workers").unwrap(), 4);
```

Arrays replace; they never concatenate. An empty path, a leading or trailing delimiter, or two
adjacent delimiters is an invalid-path error, not a silent miss. A string is never parsed as a
number, an integer is never read as a float, and a float is never read as an integer, even when the
number has no fraction. `get_or` returns its default only for absence, so a present `null`,
`false`, `0`, or `""` is returned as-is. A nested type mismatch names the source that wrote the
failing leaf, not the source of the map around it.

## Licence

[MIT](LICENSE.md)
