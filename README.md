# rs-config

`neotales-config` merges ordered configuration sources into one immutable, typed snapshot: later
sources win per key, nested maps merge, and every node remembers the source that wrote it.

By [x0craft](https://github.com/x0craft).

[![Neotales](https://github.com/neotales/brand/raw/main/assets/logo.png)](https://github.com/neotales/rs-config)

This repository is a virtual Cargo workspace. There is no root package: every crate is a top-level
directory, and a crate that shares this group's version gets its own directory rather than a place
under `crates/`.

| Crate | Directory | README |
| --- | --- | --- |
| `neotales-config` | `config/` | [config/README.md](config/README.md) |

A member exists only when it ships, so there are no placeholder crates. The shared toolchain,
formatting, lint, audit, and release configuration live at the root and apply to every member.

## Licence

[MIT](LICENSE.md)

The crate's own documentation is in [`config/docs/`](config/docs/).
