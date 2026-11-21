# Contracts

This page states the behavior `neotales-config` 0.0.0-alpha.0 promises. The core is in-memory and
caller-supplied sources. Process arguments, the process environment, and a dotenv file are
explicit, feature-gated providers. File formats, watching, reload, and vault resolution are not
part of this release.

## Sources

- A source returns a [`Value`](../src/value.rs) whose root is a `Value::Map`. Any other root fails
  `Builder::build` with `Error::is_root_not_map`.
- A source is loaded exactly once, at `Builder::build`, in registration order. `Config` never
  calls a source again.
- A source reports failure with `SourceError`, which carries `Unavailable` or `Invalid` and no
  message. The builder adds the registered source name and never forwards source text.
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
| `dotenv` | `DotenvFile` | `neotales-dotenv` |

No provider is compiled into the default build. `dotenv` is the only one with an optional
dependency; `cargo tree --no-default-features` shows no parser.

### Key mapping

All three providers map a name to nested maps through the same rules:

- The prefix is removed first. An empty prefix selects every name; a name that equals the prefix
  has an empty remainder and is invalid.
- The remainder is split on the explicit separator. With no explicit separator, the builder's
  delimiter is the separator. `flat()` disables splitting and keeps the remainder as one
  component.
- Every component must be non-empty. An empty separator is invalid.
- The provider builder methods pass the builder's delimiter to the provider. Registering a
  provider through `Builder::source` does not; that path keeps `.` unless the provider has an
  explicit separator or `flat()`.

### Values and collisions

- Environment and dotenv values stay strings. A numeric or boolean spelling is never parsed.
- Arguments values are strings for `--key=value` and booleans for `--key` and `--no-key`.
- A repeated scalar path keeps the later entry: argv order for `Arguments`, file order for
  `DotenvFile`, and original-name order for `Environment` (selected names are sorted first, so a
  case-fold collision resolves the same way whatever order the environment returned).
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

`DotenvFile` reads one caller-supplied path once, at build, with `neotales-dotenv`. `export`,
unquoted, single-quoted, double-quoted, and backtick values are accepted. A line whose first
non-whitespace character is `#` is a comment; an unquoted `#` ends a value and starts an inline
comment, while a `#` inside a quoted value stays literal. There is no interpolation and no
environment mutation. A read failure is `SourceError::Unavailable`; an invalid document, invalid
UTF-8, an empty key, or a mapping conflict is `SourceError::Invalid`.

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
| Environment mapping, case, and collisions | `tests/environment.rs` |
| Dotenv parsing, mapping, and errors | `tests/dotenv.rs` |
