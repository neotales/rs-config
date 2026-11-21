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
cargo add neotales-config@0.0.0-alpha.0 --features json
cargo add neotales-config@0.0.0-alpha.0 --features json5
cargo add neotales-config@0.0.0-alpha.0 --features jsonc
cargo add neotales-config@0.0.0-alpha.0 --features toml
cargo add neotales-config@0.0.0-alpha.0 --features yaml
cargo add neotales-config@0.0.0-alpha.0 --features ini
cargo add neotales-config@0.0.0-alpha.0 --features reload
```

The `dotenv`, `json`, `json5`, `jsonc`, `toml`, and `yaml` features add their parser crate;
`dotenv` also adds `neotales-env` for interpolation. `args`, `environment`, and `ini` add code and
no crate, because the INI parser is part of this crate.

| Feature | Provider | Reads | Adds |
| --- | --- | --- | --- |
| none | `Builder::memory`, `Builder::source` | caller-supplied values and closures | nothing |
| `args` | `Builder::arguments`, `Arguments` | already-tokenized `OsString` argv | code |
| `environment` | `Builder::environment`, `Environment` | the process environment or fixed pairs | code |
| `dotenv` | `Builder::dotenv_file`, `DotenvFile` | one caller-supplied dotenv file | `neotales-dotenv`, `neotales-env` |
| `json` | `Builder::json_file`, `JsonFile` | RFC 8259 JSON | `serde_json` |
| `json5` | `Builder::json5_file`, `Json5File` | JSON5 1.0 | `json5` |
| `jsonc` | `Builder::jsonc_file`, `JsoncFile` | JSON plus comments | `jsonc-parser` |
| `toml` | `Builder::toml_file`, `TomlFile` | TOML 1.1 | `toml` |
| `yaml` | `Builder::yaml_file`, `YamlFile` | YAML 1.2 core schema | `saphyr`, `saphyr-parser` |
| `ini` | `Builder::ini_file`, `IniFile` | the documented INI dialect | code |
| `reload` | `Builder::build_reloadable`, `Reload` | source reload, subscriptions, opt-in polling | code, no thread until started |

The formats are separate on purpose. Enabling `json` compiles no YAML parser, and a build with no
feature has no parser or dotenv expander in its dependency graph.

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
an environment name or switch written with the same character maps the same way. The delimiter is
applied when `build()` runs, not when the provider is registered: `.environment(..).delimiter(':')`
and `.delimiter(':').environment(..)` map identically. An explicit `separator` overrides it, and
`flat()` disables splitting. A file-format provider never takes the delimiter; a document already
expresses its own structure and every key is literal.

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
before the key is split. With no explicit separator, the builder's final delimiter is the separator,
whatever order `delimiter()` and `environment()` were called in; `flat()` keeps the remainder as one
component, and an explicit `separator` overrides the delimiter. Selected names are sorted by original
spelling, so `APP_A` and `APP_a` with `Case::Lower` resolve the same way whatever order the
environment returned; the later name wins at one path. Two pairs with the identical original name
reject the source instead, even when the prefix selects neither copy, because which value won would
depend on the order they were returned in. Two names that would need one key to be both a scalar and
a map reject the source. An empty key, an empty path component, an empty separator, and a non-Unicode
name or value are invalid. An error names the source and the kind, never a variable or a value.

**Read a dotenv file.** The `dotenv` feature adds `DotenvFile`, which parses one caller-supplied
file with `neotales-dotenv` and expands `${NAME}` references with `neotales-env`. The file is read,
and interpolation falls back to the process environment unless `with_environment` supplies a fixed
set. The process environment is never written.

```rust,no_run
use neotales_config::{Builder, DotenvFile};

let config = Builder::new()
    .dotenv_file(
        "dotenv",
        DotenvFile::new("config/.env")
            .prefix("APP_")
            .with_environment([("HOME", "example-home")]),
    )
    .build()
    .expect("configuration builds");

assert_eq!(config.get::<String>("http.port").unwrap(), "8080");
```

`export KEY=value`, unquoted, single-quoted, double-quoted, and backtick values are accepted, and
every value stays a string after expansion. A line whose first non-whitespace character is `#` is a
comment; an unquoted `#` ends a value and starts an inline comment, while a `#` inside a quoted
value stays literal. With a file line `APP_http.port=8080`, the builder's default delimiter `.`
produces the nested map `http` then `port`; an explicit `separator` overrides it, and `flat()` keeps
the key as one component. A later assignment wins at the same path. An invalid line, an empty key,
an empty path component, a non-UTF-8 file, and a scalar/map conflict reject the source. The error
carries no path, line, variable, or value.

Interpolation is on by default and is sequential:

- An unquoted or double-quoted value expands. A single-quoted or backtick value stays literal.
- The lookup checks the document first: an earlier assignment's expanded value, including one the
  prefix does not select and one written by `${NAME:=text}`, then the process environment.
- `${NAME:=text}` stores `text` in a per-load local scope for later assignments. It never writes
  the process environment or the fixed set supplied by `with_environment`, and the local scope is
  not part of the snapshot. A reload starts a new local scope, so the write does not survive it.
- A duplicate assignment sees the earlier expanded value before it replaces it. A forward
  reference reads the environment, because the later assignment has not run yet.
- A missing or empty name expands to an empty string. `${NAME:-text}` and `${NAME:text}` use
  `text`, and a reference inside that default is expanded; `${NAME:?text}` rejects the source and
  discards `text`, so it never reaches an error.
- A reference may nest inside the `text` of a default, assign, or required operand, and the outer
  operator is the first `:` outside a nested `${…}`. Nesting is bounded: more than
  `DotenvFile::MAX_NESTING_DEPTH` (16) references open at once rejects the source as invalid before
  expansion starts. A token whose braces are literal, or whose reference is escaped, is not a
  nested reference and is handed to `neotales-env` unchanged.
- An unbraced `$NAME` takes the longest run of letters, digits, and underscores, so `$HOMEtail`
  is the name `HOMEtail`; use `${HOME}tail` before another name character.
- A looked-up value is inserted as-is. A `$` inside an earlier assignment's value or an environment
  value is not expanded a second time.
- `$$` and `\$` produce a literal `$`. Command substitution (`$(...)`), Windows `%NAME%`
  references, arithmetic, word splitting, globbing, positional parameters, and arrays are not
  evaluated. `${NAME-default}` without the colon is malformed; a colon introduces only the
  default, assign, or required form above.
- Expansion runs in source order for every assignment, selected or not. A malformed reference in
  any assignment rejects the whole source, even when the prefix selects none of it.
- `DotenvFile::interpolate(false)` keeps every value literal and does not read the process
  environment. It is the opt-out for a document that is not a template.

The parser runs before expansion, so it removes the outer quotes and decodes its own escapes
first. In an unquoted or double-quoted value, `\$` is the reliable way to write a literal `$`; a
single-quoted value is not expanded at all, and a backtick value is not expanded even though the
parser decodes its escapes like a double-quoted value's. `${NAME:=text}` stores the operand as
written: `neotales-env` does not expand a `$` inside that operand before storing it, so use a
literal default when a later assignment reads the stored name.

`with_environment` replaces the process lookup with a fixed set for a test or a captured
environment. It affects interpolation only; it never selects an assignment, and a repeated name
keeps its last value. The accepted grammar is a subset of a shell, not Bash evaluation.

**Read a configuration file.** Each format is a separate opt-in feature and an explicit
constructor. Nothing is inferred from a file extension, and no source tries the next format when
one fails.

```rust,no_run
use neotales_config::{Builder, IniFile, JsonFile, TomlFile, YamlFile};

let config = Builder::new()
    .json_file("defaults", JsonFile::new("config/defaults.json"))
    .toml_file("site", TomlFile::new("config/site.toml"))
    .yaml_file("overlay", YamlFile::new("config/overlay.yaml"))
    .ini_file("local", IniFile::new("config/local.ini"))
    .build()
    .expect("configuration builds");
```

The JSON5 and JSONC constructors are the same shape: `Json5File::new(path)` and
`JsoncFile::new(path)`. A provider reads its path again on every load, so building again or using
the `reload` holder sees a replaced file.

| Feature | Dialect | Root | Duplicate names | Number rule |
| --- | --- | --- | --- | --- |
| `json` | RFC 8259 JSON | object | later member wins | integer exact to `i128`, finite float |
| `json5` | JSON5 1.0 | object | later member wins | integer exact to `i128`, finite float |
| `jsonc` | JSON plus `//` and `/* */` comments | object | later member wins | integer exact to `i128`, finite float |
| `toml` | TOML 1.1 | always a table | rejected | `i64` integer, finite float |
| `yaml` | YAML 1.2 core schema | mapping | rejected | integer exact to `i128`, finite float |
| `ini` | the dialect below | always a map | later assignment wins | every value is a string |

A value the model cannot hold rejects the source with an unrepresentable-value error; it is never
rounded. For JSON, JSON5, JSONC, and YAML that includes an integer outside `i128` and a float that
is not finite. For TOML it includes `nan`, `inf`, and `-inf`. TOML dates and times stay typed as
`DateTime`; JSON, JSON5, JSONC, YAML, and INI have no date-time kind. A `DateTime` reads only as
`DateTime`: it is not a string and not a map, and neither a string nor a map is read as one.

JSONC is not JSON5. It accepts standard JSON plus `//` line comments and `/* */` block comments; a
trailing comma, a single-quoted string, an unquoted name, a hexadecimal number, `Infinity`, and
`NaN` reject it. JSON5 accepts all of those extensions.

YAML accepts the YAML 1.2 core schema and rejects anchors, aliases, explicit tags, non-string keys,
a plain `<<` merge key, duplicate keys, an empty document, and more than one document. A merge key
is rejected even when its value is an inline mapping; a quoted key spelling `<<` is a literal
string key. An empty plain scalar such as `enabled:` is null, while `''` is an empty string. `yes`
and `no` are strings under YAML 1.2; use `true` and `false` for booleans.

INI uses the plain dialect this crate defines. A `[section]` header is one literal key, so
`[a.b]` is the key `a.b`, not a nested map. An assignment before the first section is a root key.
A line whose first non-whitespace character is `#` or `;` is a comment, and an inline `#`/`;` is
part of the value. One pair of matching wrapping quotes is removed and no escape or interpolation
is interpreted. A repeated key keeps its last value; a repeated section header continues the
existing section; a root scalar and a section with the same name reject the file.

**Reload and watch.** The `reload` feature keeps the resolved sources beside the current immutable
snapshot. `build_reloadable()` returns a cloneable `Reload`; `refresh()` reloads every source and
publishes a new snapshot only when the merged values or their provenance differ. A failed read,
parse, or validation leaves the previous good snapshot in place and reports a sanitized error.

```rust,no_run
use std::time::Duration;

use neotales_config::{Builder, TomlFile};

let reload = Builder::new()
    .toml_file("app", TomlFile::new("config/app.toml"))
    .build_reloadable()
    .expect("the initial load succeeds");

// Manual refresh; no thread and no watcher.
let outcome = reload.refresh();
println!("{outcome:?}");

// An explicit polling watcher. Dropping the handle stops and joins its thread.
let subscription = reload.subscribe();
let watcher = reload
    .start_watching(Duration::from_millis(250))
    .expect("a file source is watchable");
if let Ok(config) = subscription.recv_timeout(Duration::from_secs(5)) {
    println!("http.port = {}", config.get::<u16>("http.port").unwrap());
}
watcher.stop();
```

Every file provider, including `DotenvFile`, reports its path through `Source::watch_paths`; a
custom source can report its own stable files or return an empty list. Polling fingerprints each
watched file by streaming it in fixed-size chunks, so its memory use is bounded by a chunk rather
than the file size. It still reads every byte, so a short interval over a large file costs real
I/O; the fingerprint does not impose a parser's read limit. Detection latency is the interval plus
the refresh time. A partially written file fails safely and is retried on the next poll even
without a content change; write a temporary file and rename it to avoid a torn read. A pending
failure, including one from another source, is retried at the polling interval and the last good
snapshot stays published. The baseline is captured before the initial load, so a change during
startup is not lost. Refreshes are serialized, and subscribers coalesce independently to the newest
snapshot. Only the reported paths are observed: a source whose input also includes something else,
such as a credential store, is invisible to polling, so call `refresh()` after that input changes.
There is no async runtime, and no thread exists until `start_watching()` is called. A published
snapshot holds plaintext, so do not log a `Config` that contains a secret. See the crate
documentation for the full lifecycle, shutdown, error, and lifetime contract.

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
