# neotales-config console sample

A runnable, **non-publishable** console sample. The package sets `publish = false`; it is run from
this repository with `cargo run` and is never uploaded to a registry.

The sample registers five sources in one fixed precedence order and prints a small, explicitly
chosen report with each value's origin.

## Run

```sh
cargo run -p neotales-config-example-console
```

The default output is built from `examples/console/fixtures/`:

```text
neotales-config console sample (publish = false)
sources, lowest to highest precedence: defaults.json (required, present), site.toml (optional, present), app.env (optional, present), APP_* environment, --key=value arguments
dotenv expansion: ${NAME} expands in unquoted and double-quoted values; single-quoted values stay literal

key                value                                          origin
server.bind        127.0.0.1                                      defaults.json
server.port        18081                                          site.toml
demo.greeting      "hello from app.env for fixed dotenv lookup"  app.env
demo.mode          "dotenv"                                       app.env
demo.banner        false                                          defaults.json
demo.literal       "${DEMO_NAME}"                                 app.env
```

## Sources and precedence

Later sources win per key; nested maps merge. This is the exact registration order in
[`src/lib.rs`](src/lib.rs).

| Order | Source | File or input | Required |
| --- | --- | --- | --- |
| 1 | JSON | `defaults.json` | yes |
| 2 | TOML | `site.toml` | no |
| 3 | dotenv | `app.env` | no |
| 4 | environment | names starting with `APP_` | no |
| 5 | arguments | `--key=value`, `--key`, `--no-key` | no |

`site.toml` and `app.env` call `.optional(true)`. Only an actual not-found is an empty source; a
directory, a permission failure, invalid UTF-8, and a parse failure still fail the load.

The environment source uses `APP_` as its prefix and `__` as its separator. `APP_DEMO__MODE`
therefore selects `demo.mode`:

```sh
APP_DEMO__MODE=environment cargo run -p neotales-config-example-console
```

Arguments are already-tokenized `--key=value` switches. There is no `--key value` form and no `--`
terminator:

```sh
cargo run -p neotales-config-example-console -- --demo.mode=arguments --demo.banner
```

The port stays an integer in the sample: `defaults.json` and `site.toml` provide it. A value from
`app.env`, the environment, or an argument is a string, and the console report prints that string
as-is. The web sample reads `server.port` as a `u16`, so setting `APP_SERVER__PORT` or
`--server.port=19000` there is a type mismatch. This is the crate's documented strict typing, not a
special case in the sample.

## Dotenv expansion scope

`app.env` is read once per load. Expansion is sequential and applies only to unquoted and
double-quoted values:

- `${DEMO_NAME}` in a double-quoted value expands.
- `'${DEMO_NAME}'` in a single-quoted value stays literal; the report prints it as `"${DEMO_NAME}"`.
- The sample passes a fixed `DEMO_NAME` lookup with `with_environment(...)`. That replaces the
  process-environment part of the lookup, so a `DEMO_NAME` in the shell does not change the report.
- Expansion never writes the process environment. A `${NAME:=text}` assignment would live only in
  the per-load scope, and this sample does not use one.

## Optional files and no-file behavior

Point the sample at a directory that has only `defaults.json`:

```sh
tmp=$(mktemp -d)
cp examples/console/fixtures/defaults.json "$tmp/"
DEMO_CONFIG_DIR="$tmp" cargo run -p neotales-config-example-console
rm -rf "$tmp"
```

The report says `site.toml (optional, absent)` and `app.env (optional, absent)`. `server.port` is
then `18080` and `demo.mode` is `defaults`. `DEMO_CONFIG_DIR` is an application switch, not a
configuration source, and the sample never prints it.

## Output and secrets

The report reads only the six keys in `SAFE_KEYS`. It never dumps the process environment, an
unselected value, a whole snapshot, or a path. Configuration errors are the crate's sanitized
errors: a source name and a shape label, no value, file body, or path. The sample's own test runs
the binary with an unrelated `TOP_SECRET` variable and asserts it does not appear.

Do not put a secret in `demo.greeting`, `demo.mode`, or any other printed key; the sample is
demonstrating display, not redaction.

## Test

```sh
cargo test -p neotales-config-example-console
```

The tests cover absent optional files, overlay precedence, prefix selection, argument flags,
sanitized errors, and binary output that must not echo an unselected environment value.
