# neotales-config

Merge ordered configuration sources into one immutable, typed snapshot.

[![Neotales](https://github.com/neotales/brand/raw/main/assets/logo.png)](https://github.com/neotales/rs-config)

<!-- badges: crates.io, docs.rs, CI. Add once the crate name and first version exist. -->

## Install

```sh
cargo add neotales-config@0.0.0-alpha.0
```

No feature is on by default, and the core has no optional provider dependency. Add only what you
read:

```sh
cargo add neotales-config@0.0.0-alpha.0 --features args
cargo add neotales-config@0.0.0-alpha.0 --features environment
cargo add neotales-config@0.0.0-alpha.0 --features dotenv
```

The `dotenv` feature adds `neotales-dotenv`, the document parser. `args` and `environment` add code
and no crate.

| Feature | Provider | Reads |
| --- | --- | --- |
| none | `Builder::memory`, `Builder::source` | caller-supplied values and closures |
| `args` | `Builder::arguments`, `Arguments` | already-tokenized `OsString` argv |
| `environment` | `Builder::environment`, `Environment` | the process environment or fixed pairs |
| `dotenv` | `Builder::dotenv_file`, `DotenvFile` | one caller-supplied dotenv file |

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
The provider builder methods pass this delimiter to the provider as its default key separator, so
an environment name or switch written with the same character maps the same way. An explicit
`separator` overrides it, and `flat()` disables splitting.

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

**Read already-tokenized arguments.** The `args` feature adds `Arguments`, which consumes
`OsString` tokens. It never joins them into a command-line string, never evaluates quotes or
escapes, and never writes to the process environment. `--key=value` is a string, `--key` is
`true`, and `--no-key` is `false`; a value is never parsed as a number or a boolean.

```rust
use std::ffi::OsString;

use neotales_config::{Arguments, Builder};

let config = Builder::new()
    .arguments(
        "arguments",
        Arguments::new([
            OsString::from("--http.port=8080"),
            OsString::from("--verbose"),
            OsString::from("--no-color"),
        ]),
    )
    .build()
    .expect("configuration builds");

assert_eq!(config.get::<String>("http.port").unwrap(), "8080");
assert!(config.get::<bool>("verbose").unwrap());
assert!(!config.get::<bool>("color").unwrap());
```

A token that is not `--<key>=<value>`, `--<key>`, or `--no-<key>` rejects the source: positionals,
short switches, `--`, an empty key, a non-Unicode token, and an empty path component all fail.
There is no `--` terminator, and `--key value` is not accepted; use `--key=value`. A later switch
wins at the same path. `Arguments::from_process()` reads `std::env::args_os()` and skips the
program path; `Arguments::new` takes the arguments only.

**Read the environment.** The `environment` feature adds `Environment`. A value stays a string;
`APP_HTTP_PORT=8080` yields `"8080"`, and reading it as an integer is a type mismatch.

```rust
use neotales_config::{Builder, Case, Environment};

let config = Builder::new()
    .environment(
        "environment",
        Environment::from_pairs([("APP_HTTP_PORT", "8080")])
            .prefix("APP_")
            .separator("_")
            .case(Case::Lower),
    )
    .build()
    .expect("configuration builds");

assert_eq!(config.get::<String>("http.port").unwrap(), "8080");
```

`Environment::process()` reads the process environment once, at build. `Environment::from_pairs`
keeps a test or a snapshot out of the process, and no provider writes to it. The prefix is removed
before the key is split. With no explicit separator, the builder's delimiter is the separator;
`flat()` keeps the remainder as one component, and an explicit `separator` overrides the delimiter.
Selected names are sorted by original spelling, so `APP_A` and `APP_a` with `Case::Lower` resolve
the same way whatever order the environment returned; the later name wins at one path. Two names
that would need one key to be both a scalar and a map reject the source. An empty key, an empty
path component, an empty separator, and a non-Unicode name or value are invalid. An error names
the source and the kind, never a variable or a value.

**Read a dotenv file.** The `dotenv` feature adds `DotenvFile`, which parses one caller-supplied
file with `neotales-dotenv`. It never interpolates a value and never changes the process
environment.

```rust,no_run
use neotales_config::{Builder, DotenvFile};

let config = Builder::new()
    .dotenv_file("dotenv", DotenvFile::new("config/.env").prefix("APP_"))
    .build()
    .expect("configuration builds");

assert_eq!(config.get::<String>("http.port").unwrap(), "8080");
```

`export KEY=value`, unquoted, single-quoted, double-quoted, and backtick values are accepted, and
every value stays a string. A line whose first non-whitespace character is `#` is a comment; an
unquoted `#` ends a value and starts an inline comment, while a `#` inside a quoted value stays
literal. With a file line `APP_http.port=8080`, the builder's default delimiter `.` produces the
nested map `http` then `port`; an explicit `separator` overrides it, and `flat()` keeps the key as
one component. A later assignment wins at the same path. An invalid line, an empty key, an empty
path component, a non-UTF-8 file, and a scalar/map conflict reject the source. The error carries
no path, line, or value. Reading the file is the only I/O; the process environment is never read
or written.

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
