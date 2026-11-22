# Runnable samples

These packages are workspace members because CI builds and tests them with the rest of the
repository, but they are **not published crates**. Each package sets `publish = false`.

| Sample | Directory | Run |
| --- | --- | --- |
| Console | [`console/`](console/) | `cargo run -p neotales-config-example-console` |
| Web | [`web/`](web/) | `cargo run -p neotales-config-example-web` |

Both use the same documented source order: a required `defaults.json`, optional `site.toml`,
optional `app.env`, the `APP_` environment, and `--key=value` arguments. The web sample adds a
`reload`-based watcher and a loopback-only HTTP view.

The fixtures contain no secrets. The samples print only an allowlist of keys, never a whole
environment or snapshot.
