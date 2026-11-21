# neotales-config-keepass

An opt-in KeePass KDBX source for `neotales-config`: explicit entry fields, explicit configuration
paths, explicit credentials, explicit read limits.

By [x0craft](https://github.com/x0craft).

[![Neotales](https://github.com/neotales/brand/raw/main/assets/logo.png)](https://github.com/neotales/rs-config)

## Install

The examples below import three crates: the provider, the core builder, and the KDBX codec whose
types appear in the provider's API. Add all three directly; neither dependency is re-exported.

```sh
cargo add neotales-config@0.0.0-alpha.0
cargo add neotales-keepass@0.0.0-alpha.1
cargo add neotales-config-keepass@0.0.0-alpha.0
```

The provider is a normal `neotales-config` source. The core crate does not depend on this crate or
on `neotales-keepass`; a build that never registers the provider compiles no KDBX codec and no
cryptography in the core.

The `reload` feature forwards `neotales-config/reload`, so one feature name enables the snapshot
holder and the explicit polling watcher for the whole path:

```sh
cargo add neotales-config-keepass@0.0.0-alpha.0 --features reload
```

Cargo unifies features, so this also enables `neotales-config/reload` through the provider's
dependency; the direct `neotales-config` line above does not need the feature repeated.

The first version is not on crates.io yet. It depends on `neotales-config` at the same group
version, so it can be uploaded only after that core version is on crates.io. See the repository
`RELEASE.md` for the order; the CI and release changes here do not claim either crate is published.

## What it does

`KeePassFile` opens one caller-supplied KDBX file, asks a caller-supplied credential factory for
one credential value, and copies the fields the caller mapped into an ordinary configuration
snapshot.

```rust,no_run
use neotales_config::{Builder, SourceError};
use neotales_config_keepass::{KeePassFile, Mapping};
use neotales_keepass::{KdbxCredentials, KdbxReadLimits, KdbxUuid};

let entry = KdbxUuid::from_base64("EREREREREREREREREREREQ==").expect("fixed UUID");

let vault = KeePassFile::new(
    "secrets/application.kdbx",
    KdbxReadLimits::default(),
    || KdbxCredentials::password(b"from-the-caller").map_err(|_| SourceError::unavailable()),
)
.mapping(Mapping::new(entry, "UserName", "database.user"))
.mapping(Mapping::new(entry, "Password", "database.password"));

let config = Builder::new()
    .source("vault", vault)
    .build()
    .expect("the vault is read");

assert_eq!(config.get::<String>("database.user").unwrap(), "from-the-vault");
```

There is no implicit discovery. The path is explicit, the entry is named by its stable UUID, the
field is named exactly, and the destination path is explicit. Nothing is inferred from an entry
title, a group name, a file extension, or the process environment.

### Mapping rules

- A field key is matched exactly and case-sensitively against the entry's ordered field list.
  Presence comes from that list, not from the codec's entry properties: an absent field rejects the
  source, and a present but empty field reads as `""`. The provider never substitutes a codec
  default.
- `Title` is the one standard field the published codec does not keep presence for. In KDBX the
  title is the `<Name>` element, and `neotales-keepass` 0.0.0-alpha.1 replaces a missing `<Name>`
  with the literal `"Entry"`, so an absent title cannot be distinguished from an explicit `"Entry"`
  title. The provider therefore does not read `Entry::title`. A `Title` mapping succeeds only when
  the vault carries a literal `Title` field (some writers, including keepass-rs, store one) and
  otherwise fails closed. Reading the standard `<Name>` title needs an upstream codec change that
  preserves its presence.
- Every value becomes a `Value::String`. A numeric or boolean spelling is never parsed, and a
  protected value is revealed to UTF-8 text. A value that is not UTF-8 rejects the source.
- A missing entry, a missing field, and a mapped field that cannot be read all reject the whole
  source. Nothing is skipped, so there is no partial tree.
- Mappings are validated before the vault is opened: an empty field key, an empty path or path
  component, the same entry and field mapped twice, the same path mapped twice, and a path that is
  a prefix of another path are all rejected. So is an empty mapping list.
- Paths are split on the provider's delimiter, `.` by default. `:`, `/`, `,`, and `|` are also
  accepted. Configure the core builder with the same delimiter to read the path back as a string.

```rust,no_run
# use neotales_config::Builder;
# use neotales_config_keepass::{KeePassFile, Mapping};
# use neotales_keepass::{KdbxCredentials, KdbxReadLimits, KdbxUuid};
# let entry = KdbxUuid::from_bytes([0x11; 16]);
let vault = KeePassFile::new(
    "secrets/application.kdbx",
    KdbxReadLimits::default(),
    || Ok(KdbxCredentials::password(b"from-the-caller").unwrap()),
)
.delimiter(':')
.mapping(Mapping::new(entry, "Password", "database:password"));

let config = Builder::new()
    .delimiter(':')
    .source("vault", vault)
    .build()
    .expect("the vault is read");

assert_eq!(config.get::<String>("database:password").unwrap(), "from-the-vault");
```

A path component that contains a different supported delimiter is a literal key, because only the
configured delimiter splits.

### Credentials

`CredentialFactory` is the only credential path. Implement it, or pass a closure of type
`Fn() -> Result<KdbxCredentials, SourceError> + Send + Sync`. The provider calls it once per
**load**: the initial build, every `Reload::refresh`, and a watcher pass that actually loads because
a watched file changed or an earlier load failed. A watcher pass over unchanged files does not load
and does not call the factory. The returned credentials are dropped after that load.

The watcher fingerprints only the KDBX path. If only the credential source changes, the file
fingerprint is unchanged and the watcher does not reload; call `Reload::refresh()` yourself after
rotating a credential.

The factory runs on whichever thread performs the load, including the watcher thread. It must not
prompt, and it must be safe to call from more than one thread. The error type is `SourceError`,
which carries a kind and no message: return `SourceError::unavailable()` for a credential-store
failure, and never put a value in the error.

The provider never reads a password, never opens a key file, never looks for a `.kdbx` file, and
never asks a terminal or a desktop for input.

### Read limits

`KeePassFile::new` requires `KdbxReadLimits`. The provider passes the value unchanged to every
load and adds no hidden bound. Choose the input, header, KDF, block, decompression, XML, and binary
budgets for the trust boundary of the vault; `KdbxReadLimits::default()` is the codec's
conservative baseline, not a policy for hostile input. At load the file is handed to the codec as
an open stream, so the provider itself never holds a whole-vault buffer.

### Errors

An unreadable file is `SourceError::Unavailable`. Wrong credentials, a malformed, truncated, or
over-limit document, and a mapping that does not resolve are `SourceError::Invalid`. A credential
factory error keeps the kind the factory chose.

The provider never forwards the `neotales-keepass` error message. The message is not printed, not
stored, and not attached as a source error. `neotales_config::Error` then names the registered
source and the fixed kind.

### Reload and watching

The source reports the KDBX path through `Source::watch_paths`, including when the file does not
exist yet. With the core `reload` feature, `Builder::build_reloadable` captures that path and
`Reload::start_watching` polls it:

```rust,no_run
use std::time::Duration;

use neotales_config::{Builder, SourceError};
use neotales_config_keepass::{KeePassFile, Mapping};
use neotales_keepass::{KdbxCredentials, KdbxReadLimits, KdbxUuid};

let entry = KdbxUuid::from_base64("EREREREREREREREREREREQ==").expect("fixed UUID");
let reload = Builder::new()
    .source(
        "vault",
        KeePassFile::new(
            "secrets/application.kdbx",
            KdbxReadLimits::default(),
            || KdbxCredentials::password(b"from-the-caller").map_err(|_| SourceError::unavailable()),
        )
        .mapping(Mapping::new(entry, "Password", "database.password")),
    )
    .build_reloadable()
    .expect("the initial load succeeds");

let subscription = reload.subscribe();
let watcher = reload.start_watching(Duration::from_secs(1)).expect("a watcher starts");

// Replace the vault atomically; the watcher notices the new bytes.
let updated = subscription.recv_timeout(Duration::from_secs(30)).expect("a change arrives");
assert!(updated.has("database.password").unwrap());

watcher.stop();
```

A failed reload, from a torn write, a replaced file, wrong credentials, or an exceeded limit, does
not publish anything: the previous good snapshot stays current, `Reload::last_error` reports the
sanitized failure, and a pending failure is retried on the next poll. The credential factory is
called again on that retry, so a transient secret-store failure can recover without touching the
file. That pending-failure retry is the only case where a poll over unchanged bytes calls the
factory; a successful poll does not.

The watcher is a polling watcher, not an OS notification API. It fingerprints the KDBX path by
streaming the file in fixed-size chunks, so its memory use is bounded by one chunk rather than the
file size. It still reads every byte on every poll, so disk I/O is proportional to the vault size
and the polling interval; it does not impose `KdbxReadLimits` (the load does). Detection latency is
up to the polling interval plus the load time. Write a temporary file and rename it over the vault
to avoid a torn read.

## Secrets and lifetime

**A published snapshot contains the mapped values in plaintext.** `Value::String` is ordinary,
un-zeroized memory. It is copied into the immutable snapshot, and that snapshot remains alive for as
long as any `Config` clone or subscriber notification references it. The provider drops the
`KeePassDatabase` after the load, but that does not erase the copied strings.

This crate makes no claim about zeroization, memory pinning, swap, hibernation, core dumps, or a
process inspected while it runs. Do not log a `Value` tree, a `Config`, or a `Reload` that carries a
vault field. Do not keep the snapshot longer than the process needs it.

`KeePassFile` and `Mapping` have redacted `Debug` implementations: the path, the credential factory,
the entry UUID, the field key, the mapped path, and the values are not printed.

## Licence

[MIT](LICENSE.md)
