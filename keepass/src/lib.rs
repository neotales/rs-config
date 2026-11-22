#![allow(
    clippy::doc_markdown,
    reason = "KeePass is the product name and reads correctly as plain text; the acronyms used \
              here (KDBX, KDF, UUID) are not flagged by the lint"
)]

//! Opt-in KeePass KDBX configuration source for [`neotales_config`].
//!
//! [`KeePassFile`] is a normal [`Source`](neotales_config::Source). It reads one caller-supplied
//! KDBX file, takes one credential value from a caller-supplied factory, and copies the explicitly
//! mapped entry fields into a configuration tree. Nothing is inferred: the path, the credential
//! factory, the read limits, and every entry-field-to-path mapping are supplied by the caller.
//!
//! This crate is separate from `neotales-config` on purpose. The core crate has no dependency on
//! `neotales-keepass`; a program that never registers this provider compiles no KDBX codec and no
//! cryptography.
//!
//! # Contract
//!
//! - The KDBX path is explicit. The provider never searches a directory, reads a key file, or
//!   prompts. The file is opened read-only, passed to the codec as a stream, and never written.
//! - [`CredentialFactory`] is called at most once for every load, after the mappings validate and
//!   the file opens successfully. A load is the initial build, a manual
//!   `neotales_config::Reload::refresh`, or a polling pass that actually reloads because a watched
//!   file changed or an earlier load failed. A polling pass over unchanged files does not load and
//!   does not call the factory, and a load that fails validation or cannot open the file calls it
//!   zero times. A credential-only change is invisible to the file watcher, so call
//!   `Reload::refresh` after rotating a credential. The returned
//!   [`neotales_keepass::KdbxCredentials`] is dropped after that load.
//! - [`KeePassFile::new`] requires [`neotales_keepass::KdbxReadLimits`]. There is no hidden default:
//!   the caller selects the budget for the trust boundary of the vault.
//! - Every [`Mapping`] names an entry UUID, a field key, and a configuration path. A mapped entry
//!   that is absent, a mapped field that is absent or not UTF-8, a duplicate mapping, a duplicate
//!   path, a path that is a prefix of another path, an invalid path, and an unsupported delimiter
//!   all reject the source. A rejected source never contributes a partial tree.
//! - A field value becomes [`neotales_config::Value::String`]. A field key is matched exactly and
//!   case-sensitively against the entry's ordered field list. Presence comes from that list, not
//!   from the codec's entry properties: an absent field rejects the source, and an explicitly
//!   empty field is a present empty string. See [Field presence](#field-presence) for `Title`.
//! - Paths are split on [`KeePassFile::delimiter`], `.` by default. The supported set is the core
//!   builder's: `.`, `:`, `/`, `,`, and `|`. To read the mapped paths back with a string path, the
//!   core builder must use the same delimiter.
//! - [`KeePassFile::optional`] with `true` makes only an actual not-found from opening the KDBX
//!   path an empty source. The credential factory is not called for an absent optional vault,
//!   because no file was opened. A directory, a permission failure, a corrupt or truncated
//!   document, wrong credentials, an exceeded limit, and a mapping that does not resolve still
//!   reject the source. The default is `false`: the vault is required.
//! - [`Source::watch_paths`](neotales_config::Source::watch_paths) reports the KDBX path even when
//!   the file does not exist yet. [`KeePassFile::reload_on_change`] with `false` keeps the path out
//!   of the polling watcher on this source's account only: a poll that loads because another
//!   watched path changed still loads the vault, and a manual `neotales_config::Reload::refresh`
//!   always loads it. The path stays watched if another source reports it. The default is `true`.
//!   With the core `reload` feature, the explicit polling watcher therefore notices a replacement
//!   and a failed reload leaves the previous good snapshot in place. This crate's own `reload`
//!   feature only forwards that core feature.
//!
//! # Field presence
//!
//! The published `neotales-keepass` codec keeps every KDBX `<String>` element in
//! [`neotales_keepass::Entry::field`]. That includes the conventional `Title`, `UserName`,
//! `Password`, `URL`, and `Notes` elements, so they distinguish an absent field from an explicitly
//! empty one: absent is `None` and rejects the source, while present-but-empty is `Some` with an
//! empty value and reads as `""`.
//!
//! KeePassXC's KDBX XML writes the entry title as a normal
//! `<String><Key>Title</Key><Value>…</Value></String>` element, so `Title` is not special: this
//! provider reads it from the field list like any other key. The codec also exposes a separate
//! [`neotales_keepass::Entry::title`], read from an `<Name>` element. That property is a plain
//! `String`, never an `Option`, and `neotales-keepass` 0.0.0-alpha.1 replaces a missing `<Name>`
//! with the literal `"Entry"`, so it cannot distinguish an absent `<Name>` from an explicit
//! `"Entry"`. This provider never reads `Entry::title`, and a vault without a literal `Title`
//! field fails closed. Preserving `<Name>` presence would need an upstream codec change, but
//! `<Name>` is not the conventional KDBX title field.
//!
//! # Secrets
//!
//! **A published snapshot contains the mapped field values in plaintext.**
//! [`neotales_config::Value::String`] has no
//! protection: it is ordinary, un-zeroized memory, it is copied into the immutable snapshot, and
//! the snapshot stays alive as long as any [`neotales_config::Config`] clone or reload subscriber
//! notification references it. This crate makes no claim about memory zeroization, page locking,
//! swap, hibernation, core dumps, or a process that is inspected while it runs. Do not log a value
//! tree or a configuration snapshot that contains a vault field, and do not keep one longer than
//! the process needs it.
//!
//! Errors and [`fmt::Debug`](std::fmt::Debug) are redacted. The provider does not put a path, an
//! entry UUID, a field key, a mapped value, or a `neotales-keepass` error message on an error. A
//! [`neotales_config::SourceError`] carries a kind only, and the credential factory is required to
//! use that same kind-only type, so a caller's credential-store text cannot leak through this
//! crate either.
//!
//! # Example
//!
//! ```no_run
//! use neotales_config::{Builder, SourceError};
//! use neotales_config_keepass::{KeePassFile, Mapping};
//! use neotales_keepass::{KdbxCredentials, KdbxReadLimits, KdbxUuid};
//!
//! # fn main() -> Result<(), neotales_config::Error> {
//! let vault = KeePassFile::new(
//!     "secrets/application.kdbx",
//!     KdbxReadLimits::default(),
//!     // Called at most once per load, after validation and a successful file
//!     // open. Never prompt from here; return the value the process already
//!     // holds, or an unavailable error.
//!     || {
//!         KdbxCredentials::password(b"from-the-caller")
//!             .map_err(|_| SourceError::unavailable())
//!     },
//! )
//! .mapping(Mapping::new(
//!     KdbxUuid::from_base64("EREREREREREREREREREREQ==").expect("fixed UUID"),
//!     "Password",
//!     "database.password",
//! ));
//!
//! let config = Builder::new().source("vault", vault).build()?;
//! let _secret = config.get::<String>("database.password")?;
//! # Ok(())
//! # }
//! ```

mod mapping;
mod source;

pub use self::mapping::Mapping;
pub use self::source::{CredentialFactory, KeePassFile};
