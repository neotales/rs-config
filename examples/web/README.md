# neotales-config web sample

A runnable, **non-publishable** loopback web sample. The package sets `publish = false`; it is run
from this repository with `cargo run` and is never uploaded to a registry.

It serves a config snapshot and per-key provenance, and a polling watcher hot-reloads the files while
the process is running. The HTTP server uses only `std`; `serde_json` is used to render the JSON
view.

## Run

```sh
cargo run -p neotales-config-example-web
```

The default fixtures put the server on `http://127.0.0.1:18081/`:

```text
neotales-config web sample (publish = false)
listening on http://127.0.0.1:18081/
snapshot:   http://127.0.0.1:18081/api/config
health:     http://127.0.0.1:18081/health
watcher:    polling every 100 ms; edit site.toml or app.env and refresh
stop:       Ctrl-C
```

The port is `18081` because the optional `site.toml` overrides `server.port` from the required
`defaults.json` value of `18080`.

Read both views:

```sh
curl -i http://127.0.0.1:18081/
curl -s http://127.0.0.1:18081/api/config
curl -s http://127.0.0.1:18081/health
```

`/` is an HTML table of the allowlisted keys and their origins. `/api/config` is the same snapshot as
JSON, with the generation, the watcher's poll interval and watched-path **count**, and the last
sanitized refresh error. Neither view contains a path, a whole environment, or an unselected key.

Other switches work the same way as the console sample:

```sh
cargo run -p neotales-config-example-web -- --demo.mode=arguments
APP_DEMO__MODE=environment cargo run -p neotales-config-example-web
```

The argument syntax is `--key=value`, `--key` for `true`, and `--no-key` for `false`; there is no
`--key value` form and no `--` terminator. `server.port` must stay an integer from JSON or TOML. A
value from `app.env`, the environment, or an argument is a string, so setting the port through one
of those sources is a type mismatch rather than an implicit parse.

## Sources and precedence

Later sources win per key; nested maps merge. This is the exact registration order in
[`src/lib.rs`](src/lib.rs).

| Order | Source | File or input | Required | Watched |
| --- | --- | --- | --- | --- |
| 1 | JSON | `defaults.json` | yes | yes |
| 2 | TOML | `site.toml` | no | yes |
| 3 | dotenv | `app.env` | no | yes |
| 4 | environment | names starting with `APP_`, split on `__` | no | no |
| 5 | arguments | `--key=value`, `--key`, `--no-key` | no | no |

An optional file that appears, changes, or disappears is a reload. A changed file reloads **every**
source, not only the file that changed. The watcher's baseline is taken before the initial load, so
an edit during startup is not lost.

## Live change and refresh

Run a disposable copy so the repository fixtures stay untouched:

```sh
tmp=$(mktemp -d)
cp examples/web/fixtures/* "$tmp"/
DEMO_CONFIG_DIR="$tmp" cargo run -p neotales-config-example-web
```

In a second terminal, read the snapshot, replace `app.env` atomically, and read it again:

```sh
curl -s http://127.0.0.1:18081/api/config | grep -A2 'demo.greeting'
printf 'APP_demo__greeting="changed live at %s"\nAPP_demo__mode=dotenv\n' "$(date +%s)" \
  > "$tmp/app.env.next"
mv "$tmp/app.env.next" "$tmp/app.env"
sleep 0.3
curl -s http://127.0.0.1:18081/api/config | grep -A2 'demo.greeting'
```

The second read shows the new value and origin `app.env`. Refresh the browser page for the same
change in the HTML view. The write-and-rename pattern avoids a torn read; a direct write still fails
safely and is retried, but it can briefly show an error in the JSON view.

`app.env` wins over `site.toml` for `demo.greeting`, so a `site.toml` edit is hidden while it is
present. To watch TOML change instead, remove the optional dotenv file and edit TOML:

```sh
rm "$tmp/app.env"
printf '[server]\nport = 18081\n\n[demo]\ngreeting = "edited site.toml"\nmode = "toml"\n' \
  > "$tmp/site.toml.next"
mv "$tmp/site.toml.next" "$tmp/site.toml"
sleep 0.3
curl -s http://127.0.0.1:18081/api/config | grep -A2 'demo.greeting'
```

Clean up the sample process and the disposable directory you created:

```sh
# Ctrl-C in the terminal running cargo run
rm -rf "$tmp"
```

## Optional files and no-file behavior

A directory with only `defaults.json` is valid. Both overlays are optional:

```sh
tmp=$(mktemp -d)
cp examples/web/fixtures/defaults.json "$tmp/"
DEMO_CONFIG_DIR="$tmp" cargo run -p neotales-config-example-web
```

That starts on `http://127.0.0.1:18080/`, with `demo.mode` from `defaults.json` and no
`demo.literal`. Creating `site.toml` or `app.env` in `$tmp` while it runs is picked up by the
watcher, and removing one drops the values it contributed. `DEMO_CONFIG_DIR` is an application
switch, not a configuration source, and it is never printed.

## Dotenv expansion scope

`app.env` uses the same rules as the console sample:

- Unquoted and double-quoted values expand `${NAME}`.
- Single-quoted values stay literal; the fixture's `demo.literal` is the literal text
  `${DEMO_NAME}`.
- The sample supplies a fixed `DEMO_NAME` with `with_environment(...)`, so a process `DEMO_NAME`
  does not change the result.
- Expansion reads earlier assignments and then the fixed lookup. It never writes the process
  environment, and a `${NAME:=text}` assignment would live only in that load's local scope.

## Watcher lifetime and errors

`Server` owns the `Watcher` handle. `Server::stop` and `Drop` stop the watcher first and then join the
HTTP thread; no polling thread outlives the handle. A failed parse or read keeps the last good
snapshot published, records a sanitized error on `Reload::last_error`, and is retried on the next
poll. The JSON view's `last_error` and the HTML footer show that error without a value, file body, or
path. A missing required `defaults.json` still fails a refresh and keeps the previous snapshot.

## Loopback and output safety

- The server refuses a non-loopback address before binding. The fixture uses `127.0.0.1`.
- It serves three GET paths and returns `404` otherwise; a non-GET method is `405` with `Allow: GET`.
- It has no directory listing, no static file path, no script, and no form.
- HTML responses carry `Content-Security-Policy: default-src 'none'; style-src 'unsafe-inline'` and
  `X-Content-Type-Options: nosniff`; every dynamic value is HTML-escaped.
- Only the six keys in `SAFE_KEYS` are read from the snapshot. Do not put a secret in one of those
  keys; the sample demonstrates display and provenance, not redaction.

## Test

```sh
cargo test -p neotales-config-example-web
```

The tests start a server on an ephemeral loopback port and use bounded waits. They cover the HTML and
JSON views, provenance, a TOML file appearing, changing, and disappearing, a dotenv file appearing,
a parse failure that keeps the last good snapshot and reports a safe error, recovery, non-loopback
refusal, and a missing required defaults file. Every test stops the server explicitly; the `Drop`
implementation also joins both handles.
