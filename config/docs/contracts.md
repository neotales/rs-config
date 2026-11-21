# Contracts

This page states the behavior `neotales-config` 0.0.0-alpha.0 promises. The core is in-memory and
caller-supplied sources. Process arguments, the process environment, a dotenv file, and six file
formats are explicit, feature-gated providers. The optional `reload` feature adds a cloneable
snapshot holder, manual refresh, change subscriptions, and an explicit polling watcher. Vault
resolution is not part of this release.

## Sources

- A source returns a [`Value`](../src/value.rs) whose root is a `Value::Map`. A caller-supplied
  source that returns any other root fails `Builder::build` with `Error::is_root_not_map`. A
  format provider rejects a document whose root is not a map as `SourceError::Invalid`.
- A source is loaded once per `Builder::build`, in registration order. `Config` never calls a
  source again. A format source reads its path on every `Source::load`, so loading it again sees a
  replaced file; `Builder::build` performs exactly one load. `Builder::build_reloadable` performs
  one initial load and loads every source again on each `Reload::refresh`.
- `Source::watch_paths` reports stable local files whose contents decide `Source::load`. Its default
  is empty. Every file provider returns its configured path, including `DotenvFile`. Paths are used
  only for change detection and never reach an error or a public `Debug` output.
- A source reports failure with `SourceError`, which carries `Unavailable`, `Invalid`, or
  `Unrepresentable` and no message. The builder adds the registered source name and never forwards
  source text. `Unrepresentable` becomes `Error::is_unrepresentable`; the other two become a
  source failure with the same kind.
- Source names are unique and non-empty. A repeated name fails with
  `Error::is_duplicate_source`; an empty name fails with `Error::is_empty_source_name`. Both are
  checked before any source is loaded.
- A published tree contains only finite floats. A non-finite float fails with
  `Error::is_unrepresentable`.

## Providers

Each provider is opt-in and is a normal `Source`:

| Feature | Source | External dependency |
| --- | --- | --- |
| `args` | `Arguments` | none |
| `environment` | `Environment` | none |
| `dotenv` | `DotenvFile` | `neotales-dotenv`, `neotales-env` |
| `json` | `JsonFile` | `serde_json` |
| `json5` | `Json5File` | `json5` |
| `jsonc` | `JsoncFile` | `jsonc-parser` |
| `toml` | `TomlFile` | `toml` |
| `yaml` | `YamlFile` | `saphyr`, `saphyr-parser` |
| `ini` | `IniFile` | none |

No provider is compiled into the default build. Every parser and the dotenv expander are optional
dependencies of exactly one feature; `cargo tree --no-default-features` shows no parser and no
expander.

### Key mapping

`Arguments`, `Environment`, and `DotenvFile` map a name to nested maps through the same rules:

- The prefix is removed first. An empty prefix selects every name; a name that equals the prefix
  has an empty remainder and is invalid.
- The remainder is split on the explicit separator. With no explicit separator, the builder's
  delimiter is the separator. `flat()` disables splitting and keeps the remainder as one
  component.
- Every component must be non-empty. An empty separator is invalid.
- The provider builder methods pass the builder's final delimiter to the provider. The delimiter is
  applied at build, so registration order relative to `Builder::delimiter` cannot change the
  split. Registering a provider through `Builder::source` does not pass it; that path keeps `.`
  unless the provider has an explicit separator or `flat()`.

### Values and collisions

- Environment and dotenv values stay strings, including after dotenv expansion. A numeric or
  boolean spelling is never parsed.
- Arguments values are strings for `--key=value` and booleans for `--key` and `--no-key`.
- A repeated scalar path keeps the later entry: argv order for `Arguments`, file order for
  `DotenvFile`, and original-name order for `Environment` (selected names are sorted first, so a
  case-fold collision resolves the same way whatever order the environment returned). Two names
  with the identical original spelling reject the source, even when the prefix selects neither,
  because the winner would depend on the order the pairs were returned in.
- A path that would have to be both a scalar and a map rejects the source. It is never resolved by
  iteration order.
- A non-Unicode environment name or value, a non-Unicode argv token, and a non-UTF-8 dotenv file
  reject the source.
- A dotenv line that is not an assignment rejects the whole file; a later assignment to the same
  key wins.

### Arguments

Accepted tokens are exactly:

- `--key=value`: a string, possibly empty and possibly containing `=` or spaces.
- `--key`: boolean `true`.
- `--no-key`: boolean `false`; the `no-` form applies only without `=`.

Every other token is invalid: a positional, a short switch, a bare `-`, the empty token, `--`, an
empty key, an empty path component, and a non-Unicode token. There is no `--` terminator and no
separate value token. `Arguments::from_process` skips the program path; `Arguments::new` takes the
arguments only. Neither mutates the process environment or argv.

### Process environment

`Environment::process` reads `std::env::vars_os()` once, at build. `Environment::from_pairs`
supplies a fixed set instead. The provider never calls `set_var`, `remove_var`, or any other
mutation. A prefix and a `Case` policy are explicit; there is no implicit discovery and no
automatic uppercase or lowercase.

### Dotenv

`DotenvFile` reads one caller-supplied path once per load, at build, with `neotales-dotenv`.
`export`, unquoted, single-quoted, double-quoted, and backtick values are accepted. A line whose
first non-whitespace character is `#` is a comment; an unquoted `#` ends a value and starts an
inline comment, while a `#` inside a quoted value stays literal. A read failure is
`SourceError::Unavailable`; an invalid document, invalid UTF-8, an empty key, a mapping conflict,
or an expansion failure is `SourceError::Invalid`.

Interpolation is on by default and uses `neotales-env`:

- Unquoted and double-quoted values expand; single-quoted and backtick values do not.
- Expansion is sequential in source order. The per-load local scope starts empty. Every
  assignment's expanded value enters the scope under its exact source key before the prefix
  selects it, so an unselected assignment is visible to a later one.
- A lookup checks the local scope first, then the process environment. `with_environment` replaces
  the process lookup with a fixed set for a test or a captured environment. A repeated fixed name
  keeps its last value, and fixed names are matched exactly, including on Windows, where a process
  lookup is case-insensitive. It never selects an assignment.
- `${NAME:=text}` writes only the local scope. The process environment and the fixed set are never
  written. The operand is stored as written: `neotales-env` does not expand a `$` inside it before
  storage, so a later assignment sees the operand literally.
- Accepted forms are `$NAME`, `${NAME}`, `${NAME:-text}`, `${NAME:text}`, `${NAME:=text}`, and
  `${NAME:?text}`. An unbraced `$NAME` takes the longest run of letters, digits, and underscores,
  so `${NAME}` is required before another name character. A looked-up value is inserted as-is; a
  `$` inside it is not expanded a second time. A missing or empty name is empty; the
  default forms supply `text`, which may itself contain references that `neotales-env` expands;
  the required form rejects the source. `$$` and `\$` are a literal `$`. Command substitution, Windows `%NAME%` references, arithmetic,
  word splitting, globbing, positional parameters, arrays, and other Bash parameter forms are not
  evaluated. `${NAME-default}` without the colon is malformed;
  a colon introduces only the default, assign, or required form. A process name whose value is not
  Unicode is treated as unset.
- A duplicate assignment sees the earlier expanded value and then replaces it. A forward reference
  reads the environment, not a later assignment.
- Expansion runs for every assignment, selected or not. A malformed reference in an unselected
  assignment rejects the whole source.
- No expansion error is forwarded. In particular, the `text` after `:?` is discarded and never
  reaches an error or a `Debug` output.
- `DotenvFile::interpolate(false)` keeps the parser's decoded values unchanged and does not read
  the process environment.
- The dotenv parser decodes its own escapes before expansion. A single-quoted value is not
  expanded; a backtick value is not expanded even though its escapes are decoded like a
  double-quoted value's. `\$` survives parsing and is the reliable literal-`$` escape in an
  unquoted or double-quoted value.

### File formats

Every format has its own constructor (`JsonFile::new`, `Json5File::new`, `JsoncFile::new`,
`TomlFile::new`, `YamlFile::new`, `IniFile::new`) and its own builder method. Nothing is inferred
from a file extension, and no provider falls back to another format.

A format source reads its path again on every load, and reports that path through
`Source::watch_paths`. A build always performs exactly one read; the `reload` feature is what
rebuilds the snapshot from the same sources.

File-format keys are literal and hierarchical exactly as the document wrote them. The builder's
delimiter is not applied as a key separator: a dotted key in a document is one key, not a nested
path.

| Feature | Dialect | Root | Duplicate names | Numbers |
| --- | --- | --- | --- | --- |
| `json` | RFC 8259 JSON, UTF-8 | object | later member wins | exact integer to `i128`, finite float |
| `json5` | JSON5 1.0, UTF-8 | object | later member wins | exact integer to `i128`, finite float |
| `jsonc` | JSON plus `//` and `/* */` comments | object | later member wins | exact integer to `i128`, finite float |
| `toml` | TOML as implemented by `toml` 0.9 (spec 1.1.0) | table | rejected | `i64` integer, finite float |
| `yaml` | YAML 1.2 core schema | mapping | rejected | exact integer to `i128`, finite float |
| `ini` | the dialect below | map | later assignment wins | every value is a string |

A document that holds a value the model cannot represent rejects the source with
`SourceError::Unrepresentable`; it is never rounded. That covers an integer outside `i128` for
JSON, JSON5, JSONC, and YAML, a non-finite float for those formats and TOML, and an empty or
unrepresentable number token for any of them.

#### JSON

A byte-order mark, a comment, a trailing comma, a single-quoted string, an unquoted name, a
hexadecimal number, `Infinity`, and `NaN` are not JSON and reject the source. A quoted number
stays a string.

#### JSON5

JSON5 1.0 accepts comments, trailing commas, single-quoted strings, unquoted member names,
hexadecimal numbers, a unary plus, a bare leading or trailing decimal point, `Infinity`, `NaN`,
multi-line strings, and the dialect's Unicode whitespace. That whitespace set is wider than ASCII:
U+00A0, U+2028, U+2029, and U+FEFF are whitespace too, so `{ port:\u{00A0}8080 }` is valid.
`Infinity`, `-Infinity`, `NaN`, and a decimal literal that overflows to infinity are
unrepresentable. Negative zero keeps its float kind.

#### JSONC

JSONC is standard JSON plus `//` line comments and `/* */` block comments. It is not JSON5:
trailing commas, single-quoted strings, unquoted names, hexadecimal numbers, a unary plus, a bare
decimal point, `Infinity`, `NaN`, and JSON5 string escapes reject the source. The parser runs with
every extension except comments disabled.

#### TOML

TOML keys and tables are used as the parser resolved them. A duplicate key or table is a TOML
error. A date, time, or offset date-time becomes `Value::DateTime` with its canonical RFC 3339
text; a typed read uses `DateTime`, never `String`. `nan`, `inf`, and `-inf` are unrepresentable.

#### YAML

The dialect is YAML 1.2 core schema. Rejected: an anchor, an alias, an explicit tag, a mapping key
that is not a string, a plain `<<` merge key, a duplicate mapping key, an empty document, and more
than one document. A merge key is rejected whether its value is an alias, an inline mapping
(`<<: { a: 1 }`), or any other node; a quoted key spelling `<<` is a literal string key. `yes` and
`no` are strings under YAML 1.2; a quoted scalar is always a string, so `''` is an empty string. An
empty plain scalar, as in `enabled:`, is null. A plain integer literal is read exactly into `i128`,
including the `0x` and `0o` forms, and a value outside `i128` is unrepresentable rather than
rounded.

#### INI

The INI dialect is this crate's own:

- A line whose first non-whitespace character is `#` or `;` is a comment. There is no inline
  comment: a `#` or `;` after content is part of the value.
- `[name]` starts a section. The name is trimmed and must not be empty. It is one literal key, so
  `[a.b]` is the key `a.b`, not a nested map.
- `key = value` assigns a string. The first `=` separates the key and the value; both are trimmed,
  the key must not be empty, and the value may be empty. One pair of matching wrapping single or
  double quotes is removed. No escape and no interpolation is interpreted.
- An assignment before the first section is a root key.
- Every other line rejects the document, including a bare word and an unterminated section header.

A later assignment to the same key wins. A repeated section header continues the existing section
and later assignments merge into it; a root scalar and a section with the same name reject the
document. Every value stays a string, including a numeric or boolean spelling.

## Reload and watching (`reload`)

The `reload` feature is code only: it adds no dependency and starts no thread by itself.

- `Builder::build_reloadable` returns `Reload`, a cloneable holder. Cloning is one `Arc` reference
  count; the sources, published snapshot, subscribers, and watch baseline are shared.
- The holder captures the watched-file baseline **before** the initial load. A change made while
  the initial load is running is seen by the next poll instead of being absorbed into the baseline.
- `Reload::refresh` loads every source in registration order and validates every tree before
  merging any of them. The merged value tree and its provenance are one immutable `Snapshot`, and
  the new `Arc` is published in one write. A reader therefore never sees a new value with old
  provenance, or the reverse.
- A failed read, parse, validation, non-map root, or unrepresentable value leaves the previous
  snapshot published. `RefreshOutcome::Failed` and `Reload::last_error` report the same sanitized
  error the builder would return. The error never carries a value, a document, parser text, or a
  path, and its source chain stays empty.
- `Reload::refresh_if_changed` fingerprints the watched paths before the load. Fingerprints are a
  digest of file contents, not a timestamp, so a replacement that preserves size or modification
  time is still detected. Missing, unreadable, and recreated paths are distinct from a present
  file, so deletion and recovery are observed. A change that produces byte-identical contents is
  not a change, because the snapshot would be identical.
- The observed fingerprint advances only when a load succeeds. A failed load leaves it unchanged and
  marks the reload pending, so the next poll retries even when the watched bytes and metadata did
  not change. That covers a torn read and another source that was transiently unavailable. Retry is
  bounded by the polling interval, not a spin loop, and the last good snapshot stays published. A
  successful load clears the stored error even when the merged tree equals the still-published
  snapshot.
- Refreshes are serialized by one lock. Concurrent manual refreshes, a manual refresh racing a
  poll, and overlapping polls run one at a time. A successful refresh that produces the same value
  tree and provenance returns `Unchanged` and publishes and notifies nothing.
- `Reload::subscribe` returns a `Subscription` with its own generation. Every subscriber observes
  the newest snapshot independently; notifications coalesce, so a lagging subscriber sees the
  latest change once rather than every intermediate one. A subscriber reads its generation and its
  snapshot as one atomic pair, so a publication that interleaves with the read cannot make one
  snapshot appear under two generations. Dropping a subscriber unregisters it. When the last holder
  and watcher are gone, a receiver reports disconnection.
- `Reload::start_watching` is the only call that creates a thread. It is explicit, takes a
  non-zero interval, and requires at least one source that reports a path. `Watcher::stop` and
  `Watcher::drop` set the stop flag, wake the thread, and join it. A failed poll records its error
  and keeps polling; a pending failure is retried on the next tick.
- Polling is not an OS notification API. Detection latency is up to the interval plus refresh
  time. Every poll streams every watched file in fixed-size chunks to fingerprint it: memory is
  bounded by a single chunk, not the file size, but the whole file is still read, so disk I/O and
  cost follow total watched size. The fingerprint is independent of any provider's read limit;
  that limit applies when the source loads.
- Change detection covers only the paths a source reports through `Source::watch_paths`. An input
  that is not one of those files (for example a credential store) is invisible to polling; call
  `Reload::refresh` after it changes.
- A partially written file safely fails and is retried on the next poll even without a content
  change; an atomic rename is still the recommended write pattern.
- A published snapshot holds merged values in plaintext. Only the newest snapshot is held by the
  holder, but an older snapshot lives while a `Config` clone references it. A failed load's
  temporary tree is dropped before the outcome returns. Fingerprints store a digest, never file
  bytes. Do not log a `Config` tree that contains a secret.

## Merge

Sources register in ascending precedence. For every key:

- Two maps merge recursively.
- A map and a scalar, array, or null replace one another.
- Two arrays replace; they never concatenate.
- Two scalars replace, including two scalars of different types.

There are no profiles and no alternative strategies.

Provenance follows the same walk:

- Every node records the source that introduced or last replaced it.
- A later source that merges children into an existing map keeps the earlier source on the parent
  map and on untouched children.
- A replacement owns the whole new subtree. Provenance for every node below the replaced path is
  dropped before the new subtree is recorded, so a removed key never answers with a stale source.
- Provenance identity is a sequence of path components, not a joined string. A key that contains a
  delimiter therefore has its own identity and never shares an entry with a nested path of the same
  spelling.

## Paths

A path is one or more non-empty components separated by the builder's delimiter. The default
delimiter is `.`. Invalid forms are rejected before lookup:

- the empty path,
- a leading delimiter (`".a"`),
- a trailing delimiter (`"a."`),
- two adjacent delimiters (`"a..b"`).

An invalid path returns `Error::is_invalid_path`. It is never treated as an absent key.

### Delimiters

The supported delimiters are `.` (the default), `:`, `/`, `,`, and `|`. The set is fixed rather
than caller-defined. Each entry is one ASCII byte that is neither alphanumeric, whitespace, nor a
control character, so a split is predictable and a path reads the same in a message and a log line.

`Builder::delimiter` accepts any `char` and keeps it; `Builder::build` checks it before source
names and before any source is loaded. A character outside the set fails with
`Error::is_invalid_delimiter`. The rejected character is not stored on the error, so it cannot
reach a log line. Extending the set is a deliberate change to the documented list, not a
per-caller option.

A custom delimiter only changes how a supplied path is split. With `:`, `http:port` is the nested
path `http` then `port`, while `http.port` is one component named `http.port`. Under the default
`.`, the latter spelling is the nested path, and a literal key of that name is not selected by it.

## Typed reads

- `has` reports presence without deserializing. Null, false, zero, and empty are present. It
  returns `Result<bool, Error>` so an invalid path is an error rather than a false answer.
- `get` fails with `Error::is_missing` for an absent path and `Error::is_type_mismatch` for a
  present value of the wrong shape.
- `get_or` returns its default only for absence. A present wrong type is still a mismatch.
- `load` deserializes the focused subtree.
- `value` returns the focused node without deserializing it.
- Numeric kinds are strict. A `Value::Integer` reads as an integer type, and only when the target
  holds it: `u64::MAX` reads as `u64` and not as `i64`. A `Value::Float` reads as `f64`, or as
  `f32` only when the narrowing is exact. An integer is never read as a float and a float is never
  read as an integer, even when the number has no fraction, so `1` does not satisfy an `f64` read
  and `1.0` does not satisfy an `i64` read. A string is never parsed as a number.
- A `Value::DateTime` reads only as [`DateTime`](../src/value.rs). It is never read as a `String`
  or a map, and a string or a map is never read as a `DateTime`. The date-time is its own serde
  kind, not a one-field map, so a map that happens to contain a `text` key cannot satisfy a
  `DateTime` read. Only the `toml` feature produces this value.
- A deserialization error carries a fixed shape label. A caller-supplied serde message, a custom
  expectation, and a value are all discarded.
- A type mismatch names the source that wrote the failing leaf when the snapshot records one,
  otherwise the nearest recorded ancestor. A failure below a struct field is attributed to that
  field's source, not to the source of the map that contains it. A leaf inside an array has no
  entry of its own, so the array's source answers for it. When nothing is recorded, no source is
  named rather than a guessed one.

## Immutability and cost

`Config` holds an `Arc` to an immutable snapshot and an `Arc` cursor. Cloning increments two
reference counts and copies no tree. `child` validates, extends the cursor, and shares the same
snapshot. A build allocates one snapshot; a failed build returns no `Config`.

## Errors

`Error` names a path, a source name, a fixed shape label, or a fixed reason. It never stores or
prints a value, a secret, a connection string, a file body, a rejected delimiter, or foreign error
text. `Error::source()` is always `None`, so no nested error can reintroduce text.

## Test map

| Contract | Test |
| --- | --- |
| Precedence, nested merge, replacement | `tests/sources.rs` |
| Provenance and stale-entry removal | `tests/sources.rs` |
| Unique and non-empty source names | `tests/sources.rs` |
| Map root enforcement | `tests/sources.rs` |
| Invalid path forms and custom delimiter | `tests/paths.rs` |
| Supported and rejected delimiters | `tests/delimiters.rs` |
| Literal dotted key versus nested path | `tests/paths.rs`, `tests/typed.rs` |
| Presence, absence, mismatch, `get_or` | `tests/typed.rs` |
| Kind-strict numbers and no string coercion | `tests/typed.rs`, `src/de.rs` |
| Leaf attribution on a nested mismatch | `tests/typed.rs`, `src/de.rs` |
| Sanitized errors | `tests/errors.rs`, `tests/delimiters.rs` |
| Snapshot clone and value-free `Debug` | `tests/immutability.rs` |
| Core with every provider feature off | `tests/providers_off.rs` |
| Argument switches, collisions, and errors | `tests/arguments.rs` |
| Environment mapping, case, duplicates, and collisions | `tests/environment.rs` |
| Dotenv parsing, interpolation, mapping, and errors | `tests/dotenv.rs` |
| JSON, JSON5, JSONC, TOML, YAML, and INI end-to-end reads | `tests/formats.rs`, `src/source/*.rs` |
| Each file source rereads its path | `tests/formats.rs` |
| Duplicate-name rules per dialect | `src/source/json.rs`, `src/source/json5.rs`, `src/source/jsonc.rs`, `src/source/toml_file.rs`, `src/source/yaml.rs`, `src/source/ini.rs` |
| Unrepresentable numbers are rejected, never rounded | `tests/formats.rs`, `src/source/*.rs` |
| File path, document text, and parser text never reach an error | `tests/format_security.rs` |
| TOML date-time is typed, not a string | `tests/formats.rs`, `src/source/toml_file.rs` |
| Manual refresh, no-change, and old-snapshot independence | `tests/reload.rs` |
| A provenance-only change still publishes | `tests/reload.rs` |
| Independent coalesced subscriptions and disconnection | `tests/reload.rs` |
| Generation and snapshot are read as one pair | `src/reload.rs` |
| Serialized concurrent refreshes | `tests/reload.rs` |
| Baseline before the initial load | `tests/reload.rs` |
| Failed reload keeps value and provenance, then recovers | `tests/reload.rs` |
| Pending failure retries without a watched-file change | `tests/reload.rs` |
| Deletion, recreation, and same-size atomic replacement | `tests/reload.rs` |
| Explicit watcher publication, failure recovery, and shutdown | `tests/reload.rs` |
| Dotenv path is watchable | `tests/reload.rs` |
