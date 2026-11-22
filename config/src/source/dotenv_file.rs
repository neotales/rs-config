//! The opt-in dotenv file source.
//!
//! The file is read and parsed by `neotales-dotenv`; this crate maps assignments into a value
//! tree and expands `${NAME}` references with `neotales-env`. Expansion is sequential: an earlier
//! assignment is visible to a later one, and the process environment is the fallback. Nothing is
//! written to the process environment.

use std::{
    cell::RefCell,
    collections::BTreeMap,
    fmt,
    path::{Path, PathBuf},
};

use neotales_dotenv::QuoteStyle;
use neotales_env::ExpandOptions;

use crate::{SourceError, Value};

use super::{
    Source,
    file::read_bytes,
    keys::{Keys, insert},
};

/// A dotenv file read as one configuration source.
///
/// The file is read once per load and parsed as a dotenv document, never as a shell script.
/// `export KEY=value`, unquoted, single-quoted, double-quoted, and backtick values are accepted.
/// Every value stays a string after expansion, including a numeric or boolean spelling.
///
/// Assignment order is significant. Each assignment is expanded in source order before the prefix
/// selects it, and its expanded value is kept under its exact key for later assignments. A lookup
/// checks those earlier values first and then the process environment. An unselected assignment
/// therefore still affects a later selected one, a duplicate key sees the earlier value before
/// replacing it, and a forward reference reads the environment because the later assignment has
/// not run yet. [`Self::with_environment`] replaces the process lookup with a fixed set, which is
/// how a test or a captured environment stays out of the process.
///
/// Interpolation is on by default. [`Self::interpolate`] with `false` keeps every value literal,
/// for a document that is not a template. An unquoted or double-quoted value expands; a
/// single-quoted or backtick value stays literal. The accepted forms are `$NAME`, `${NAME}`,
/// `${NAME:-text}`, `${NAME:text}`, `${NAME:=text}`, and `${NAME:?text}`. `$$` and `\$` produce a
/// literal `$`. `${NAME:=text}` stores `text` only in the per-load local scope, so a later
/// assignment can read it; it never writes the process environment. Command substitution, Windows
/// `%NAME%` references, arithmetic, word splitting, globbing, and the other Bash parameter forms
/// are not evaluated. See [Interpolation](#interpolation) for the exact subset and its escape
/// limitations.
///
/// A selected key is split into path components. With [`Self::prefix`] `APP_`, the builder's
/// delimiter `.`, and a file containing `APP_http.port=8080`, the result is the nested map
/// `{"http": {"port": "8080"}}`. A later assignment to the same path wins; a line that is not an
/// assignment, an empty key, an empty path component, an empty separator, a non-UTF-8 file, and a
/// scalar/map conflict each reject the source.
///
/// By default the file is required and its path is watched. [`Self::optional`] and
/// [`Self::reload_on_change`] change those two defaults independently: an optional source treats
/// only a genuine not-found as an empty document, and a source with change watching off removes
/// only its own contribution to the watched path list. A poll loads every source when any watched
/// path changes, and the path stays watched if another source reports it. The key policy is still
/// validated before the file is opened, so an invalid separator fails even when an optional file
/// is absent.
///
/// # Interpolation
///
/// The parser runs before expansion: it removes the outer quotes and decodes the escapes it
/// defines, so expansion sees the decoded text. An unquoted or double-quoted `\$NAME` reaches the
/// expander as `\$NAME` and becomes the literal `$NAME`. A single-quoted value is not expanded at
/// all, though the parser still decodes `\\` and `\'` inside it. A backtick value is not expanded
/// either; the parser decodes its escapes like a double-quoted value's.
///
/// A malformed reference, an unterminated reference, and a `${NAME:?text}` whose value is missing
/// or empty reject the whole source, even when the prefix selects no assignment. No expansion
/// error text, including the `text` after `:?`, is stored on the error or printed.
///
/// `${NAME:=text}` stores the operand as written. `neotales-env` does not expand a `$` inside that
/// operand before storing it, and it returns the stored text for that reference, so a later
/// assignment sees the operand literally. Use a literal default when a later assignment reads the
/// stored name.
///
/// A reference may be nested inside the `text` of a default, assign, or required operand:
/// `${A:-${B:-fallback}}`, `${A:${B:-fallback}}`, `${A:=${B:-fallback}}`, and
/// `${A:${B:${C:-fallback}}}` all resolve. The outer operator is the first `:` outside a nested
/// `${…}`, so an operator inside the operand never becomes the outer one, and the operand is
/// expanded after the outer value is found missing. When the outer value is present, the operand is
/// not expanded at all, so a `${…:?…}` or `${…:=…}` inside it has no effect.
///
/// Nesting is bounded, not unlimited: a value with more than [`Self::MAX_NESTING_DEPTH`] references
/// open at once is rejected as [`SourceError::invalid`] before any expansion work starts. The bound
/// keeps a hostile document from exhausting the stack or making the expansion superlinear. A token
/// whose braces are all literal, such as `${A:-bar{z}}`, and a token whose reference is escaped,
/// such as `${A:-\${B}}`, are not nested references: they are handed to `neotales-env` exactly as
/// written, so its grammar, its `:=` behavior, and its errors are unchanged.
///
/// This is not a shell. The parser and expander never execute a command, never perform word
/// splitting or globbing, and never read a positional parameter or an array. `${NAME-default}`
/// without the colon is a malformed name. A colon always introduces the default, assign, or
/// required form above: `${NAME:+text}` is not an alternate-value operator and
/// `${NAME:offset:length}` is not a substring operator.
///
/// # Errors
///
/// Returns [`SourceError::unavailable`] when the file cannot be read (including when it is absent,
/// unless [`Self::optional`] is enabled), and [`SourceError::invalid`] when it is not UTF-8, does
/// not parse, or holds a reference the expander rejects. The key policy is checked before the file is opened, so an empty separator is
/// invalid even when the file is missing or holds no assignment. The error carries the kind only;
/// it never carries a path, a line, a variable name, or a value.
///
/// # Examples
///
/// ```no_run
/// use neotales_config::{Builder, DotenvFile};
///
/// let config = Builder::new()
///     .dotenv_file("dotenv", DotenvFile::new("config/.env").prefix("APP_"))
///     .build()?;
/// assert_eq!(config.get::<String>("http.port")?, "8080");
/// # Ok::<(), neotales_config::Error>(())
/// ```
///
/// # Secrets
///
/// Do not log a [`Value`] built from this source; a dotenv file usually holds secrets. Its error
/// and its [`fmt::Debug`] omit the path, the environment values, and any value from the document.
#[derive(Clone)]
pub struct DotenvFile {
    path: PathBuf,
    keys: Keys,
    interpolate: bool,
    environment: Option<BTreeMap<String, String>>,
    optional: bool,
    reload_on_change: bool,
}

#[allow(
    clippy::missing_fields_in_debug,
    reason = "the path may identify a host and the fixed environment may hold secrets; only counts \
              and flags are reportable"
)]
impl fmt::Debug for DotenvFile {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("DotenvFile")
            .field("keys", &self.keys)
            .field("interpolate", &self.interpolate)
            .field("environment", &self.environment.as_ref().map(BTreeMap::len))
            .field("optional", &self.optional)
            .field("reload_on_change", &self.reload_on_change)
            .finish()
    }
}

impl DotenvFile {
    /// The greatest number of `${…}` references a value may have open at once.
    ///
    /// A reference can sit inside another reference's operand, so a value can nest. Expansion is
    /// recursive, so this source bounds the depth: a value that nests deeper is rejected with an
    /// invalid source before any expansion begins. The bound applies to the whole value, so both
    /// this source's brace-aware split and the recursive operand expansion of `neotales-env` stay
    /// inside it.
    pub const MAX_NESTING_DEPTH: usize = 16;

    /// Registers `path` to be read when the source loads.
    ///
    /// The path is opened once per load. It is never followed into another file and never
    /// written.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            keys: Keys::default(),
            interpolate: true,
            environment: None,
            optional: false,
            reload_on_change: true,
        }
    }

    /// Treats an absent file as an empty document instead of an error.
    ///
    /// The default is `false`, so an absent file fails the load with
    /// [`SourceError::unavailable`] exactly as before. With `true`, only an actual
    /// [`std::io::ErrorKind::NotFound`] from opening the path becomes an empty map. A path that is
    /// a directory, a permission failure, a non-UTF-8 file, a parse failure, and a reference the
    /// expander rejects all still reject the source. The open result itself decides: there is no
    /// separate existence check for a file to appear or vanish between.
    ///
    /// The key policy is validated before the file is opened, so an empty separator still fails
    /// when the file is absent. An optional file that is absent may appear later, and a watched
    /// optional file that disappears is a change the next refresh observes: the values it
    /// contributed are dropped and any value from an earlier source shows through again. A parse
    /// failure is not an absence; it fails the refresh and keeps the previous snapshot published.
    #[must_use]
    pub const fn optional(mut self, optional: bool) -> Self {
        self.optional = optional;
        self
    }

    /// Reports the configured path to the polling watcher.
    ///
    /// The default is `true`. With `false`, [`Source::watch_paths`] returns an empty list for this
    /// source, so a poll does not watch the file on this source's account. That removes **only this
    /// source's contribution** to the watched path list: a poll that loads for any other reason
    /// still loads every source, including this one, and a manual `Reload::refresh` always loads
    /// every source. If another source reports the same path, the path stays watched and a change
    /// to it is observed. If no source reports a path at all, `Reload::start_watching` returns
    /// `WatchError::NoWatchableSources`.
    ///
    /// This setting is independent of [`Self::optional`]: it changes what is observed, not whether
    /// an absent file is accepted.
    #[must_use]
    pub const fn reload_on_change(mut self, reload_on_change: bool) -> Self {
        self.reload_on_change = reload_on_change;
        self
    }

    /// Keeps only assignments whose key starts with `prefix`, and removes it from every selected
    /// key.
    ///
    /// An empty prefix selects every assignment. A key that equals the prefix has an empty key
    /// and is rejected. The prefix does not affect expansion: every assignment is expanded and
    /// kept for later lookups before selection.
    #[must_use]
    pub fn prefix(mut self, prefix: impl Into<String>) -> Self {
        self.keys = self.keys.prefix(prefix);
        self
    }

    /// Splits a selected key on `separator` into nested maps.
    ///
    /// An explicit separator overrides the builder's delimiter. An empty separator rejects the
    /// source when it loads, even when the file holds no selected assignment.
    #[must_use]
    pub fn separator(mut self, separator: impl Into<String>) -> Self {
        self.keys = self.keys.separator(separator);
        self
    }

    /// Keeps a selected key as one component instead of splitting it.
    #[must_use]
    pub fn flat(mut self) -> Self {
        self.keys = self.keys.flat();
        self
    }

    /// Uses `delimiter` as the default key separator.
    ///
    /// [`crate::Builder::dotenv_file`] replaces it with the builder's final delimiter when the
    /// snapshot is built, whatever order the two calls were made in. Registering the source
    /// through [`crate::Builder::source`] keeps this value; that path keeps `.` unless this
    /// method, an explicit separator, or [`Self::flat`] is used.
    #[must_use]
    pub fn delimiter(mut self, delimiter: char) -> Self {
        self.keys = self.keys.delimiter(delimiter);
        self
    }

    /// Enables or disables `${NAME}` expansion.
    ///
    /// The default is `true`. With `false`, every value is used exactly as the parser decoded it,
    /// including a `$`, and the process environment is not read. This is the explicit opt-out for
    /// a document that is not a template.
    #[must_use]
    pub fn interpolate(mut self, enabled: bool) -> Self {
        self.interpolate = enabled;
        self
    }

    /// Replaces the process-environment lookup used by interpolation with a fixed set of pairs.
    ///
    /// It is for a test or a captured environment: when it is set, the source never reads the
    /// process environment. It affects interpolation only; it never selects an assignment and
    /// never changes an output path. A repeated name keeps its last value. Names are matched
    /// exactly, including on Windows, where a process lookup is case-insensitive.
    /// `${NAME:=text}` writes only the local expansion scope, not this set. It is ignored when
    /// [`Self::interpolate`] is disabled.
    #[must_use]
    pub fn with_environment<K, V>(mut self, pairs: impl IntoIterator<Item = (K, V)>) -> Self
    where
        K: Into<String>,
        V: Into<String>,
    {
        self.environment = Some(
            pairs
                .into_iter()
                .map(|(name, value)| (name.into(), value.into()))
                .collect(),
        );
        self
    }

    /// Returns the configured path.
    ///
    /// The path is not part of any error or of this type's [`fmt::Debug`].
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Expands one value against the current local scope and the configured environment.
    ///
    /// Every lookup goes through this method's callbacks. The default lookup is the process
    /// environment, read with [`neotales_env::get_var`]; [`Self::with_environment`] substitutes a
    /// fixed map. The setter writes only `scope`, so a `${NAME:=text}` assignment is visible to a
    /// later value in the same load and never reaches the process.
    ///
    /// The nesting bound is checked on the raw value first, before a callback or a buffer is
    /// created, so a value past [`Self::MAX_NESTING_DEPTH`] is rejected without descending at all.
    fn expand_value(
        &self,
        value: &str,
        scope: &RefCell<BTreeMap<String, String>>,
    ) -> Result<String, SourceError> {
        if !within_nesting_bound(value) {
            return Err(SourceError::invalid());
        }
        let environment = self.environment.as_ref();
        let mut options = ExpandOptions::new()
            .with_get(|name| lookup_local(scope, environment, name))
            .with_set(|name, value| {
                store_local(scope, name, value);
                Ok(())
            });
        // `neotales-env` reads a braced token with brace awareness but then splits it with a plain
        // substring search, so an operator inside a nested `${…}` can be taken for the outer one.
        // Only a token that holds a genuine nested reference is split here, at its outer brace
        // depth; every other token is handed over unchanged, including one whose braces are literal
        // or escaped.
        expand_nested(value, &mut options, scope, environment, 0)
    }

    fn build(&self) -> Result<Value, SourceError> {
        // Check the policy before the file is read: an empty separator is a caller error even when
        // the file is empty, absent and optional, or the prefix selects none of its assignments.
        self.keys.validate().map_err(|_| SourceError::invalid())?;

        let Some(bytes) = read_bytes(&self.path, self.optional)? else {
            return Ok(Value::Map(BTreeMap::new()));
        };
        let text = String::from_utf8(bytes).map_err(|_| SourceError::invalid())?;
        let document = neotales_dotenv::parse(&text);
        if document.first_invalid().is_some() {
            return Err(SourceError::invalid());
        }

        // The scope is per load. A reload starts empty, so it never observes a value from the
        // previous generation.
        let scope = RefCell::new(BTreeMap::new());
        let mut root = BTreeMap::new();
        for assignment in document.assignments() {
            if assignment.key.is_empty() {
                return Err(SourceError::invalid());
            }

            let expands = self.interpolate
                && matches!(assignment.quote, QuoteStyle::Unquoted | QuoteStyle::Double);
            let value = if expands {
                self.expand_value(&assignment.value, &scope)?
            } else {
                assignment.value.clone()
            };

            // Every assignment enters the lookup scope, including one the prefix does not select:
            // a later value may refer to it.
            scope
                .borrow_mut()
                .insert(assignment.key.clone(), value.clone());

            let Some(components) = self
                .keys
                .components(&assignment.key)
                .map_err(|_| SourceError::invalid())?
            else {
                continue;
            };
            insert(&mut root, &components, Value::String(value))
                .map_err(|_| SourceError::invalid())?;
        }
        Ok(Value::Map(root))
    }
}

/// Looks a name up in the per-load local scope first and then the configured environment.
///
/// The process environment is read only when [`DotenvFile::with_environment`] supplied no fixed
/// set, matching the callbacks an unmodified `neotales-env` expansion uses.
fn lookup_local(
    scope: &RefCell<BTreeMap<String, String>>,
    environment: Option<&BTreeMap<String, String>>,
    name: &str,
) -> Option<String> {
    if let Some(value) = scope.borrow().get(name) {
        return Some(value.clone());
    }
    match environment {
        Some(pairs) => pairs.get(name).cloned(),
        None => neotales_env::get_var(name),
    }
}

/// Stores a value in the per-load local scope only.
///
/// It is the same effect as the setter given to `neotales-env`: a `${NAME:=text}` assignment is
/// visible to a later value in the same load and never reaches the process or the fixed set.
fn store_local(scope: &RefCell<BTreeMap<String, String>>, name: &str, value: &str) {
    scope.borrow_mut().insert(name.to_owned(), value.to_owned());
}

/// Resolves one name the way `neotales-env` resolves a braced or unbraced reference.
///
/// A name that a shell would reject is an error, and an unset name is the empty string. No
/// positional-argument option is configured, so a numeric name is rejected here too.
fn expand_name(
    name: &str,
    scope: &RefCell<BTreeMap<String, String>>,
    environment: Option<&BTreeMap<String, String>>,
) -> Result<String, SourceError> {
    if !valid_name(name) {
        return Err(SourceError::invalid());
    }
    Ok(lookup_local(scope, environment, name).unwrap_or_default())
}

/// Reports whether `name` is a variable a shell accepts.
///
/// The rule matches `neotales-env`: a Unicode letter or `_`, then Unicode letters, digits or `_`.
fn valid_name(name: &str) -> bool {
    let mut characters = name.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    if !first.is_alphabetic() && first != '_' {
        return false;
    }
    characters.all(|character| character.is_alphanumeric() || character == '_')
}

/// What a `${…}` token asks for when its variable has no value.
#[derive(Clone, Copy)]
enum Operator {
    /// `${NAME:-text}` and `${NAME:text}`.
    Default,
    /// `${NAME:=text}`.
    Assign,
    /// `${NAME:?text}`.
    Required,
}

/// Expands `value`, resolving each braced reference at its own brace depth.
///
/// Text that holds no braced reference is copied, `\$` and `$$` become one literal `$`, and an
/// unbraced `$NAME` is resolved as `neotales-env` resolves it. A braced reference is passed to
/// [`expand_token`]. Lookups and stores always use the same per-load scope and environment.
///
/// `depth` is the number of references already open around this text. It is threaded only as a
/// defensive check; [`within_nesting_bound`] has already rejected a value that could exceed
/// [`DotenvFile::MAX_NESTING_DEPTH`], so the check cannot fire for a checked value.
fn expand_nested(
    value: &str,
    options: &mut ExpandOptions<'_>,
    scope: &RefCell<BTreeMap<String, String>>,
    environment: Option<&BTreeMap<String, String>>,
    depth: usize,
) -> Result<String, SourceError> {
    if depth > DotenvFile::MAX_NESTING_DEPTH {
        return Err(SourceError::invalid());
    }
    let characters: Vec<char> = value.chars().collect();
    let mut output = String::with_capacity(value.len());
    let mut index = 0;
    while index < characters.len() {
        let character = characters[index];
        let next = characters.get(index + 1).copied();

        if character == '\\' && next == Some('$') {
            output.push('$');
            index += 2;
            continue;
        }

        if character == '$' {
            if next == Some('$') {
                output.push('$');
                index += 2;
                continue;
            }

            if next == Some('{') {
                let (token, after) = read_braced(&characters, index + 2)?;
                output.push_str(&expand_token(
                    &token,
                    options,
                    scope,
                    environment,
                    depth + 1,
                )?);
                index = after;
                continue;
            }

            if next.is_some_and(|name| name.is_alphanumeric() || name == '_') {
                let start = index + 1;
                let mut end = start;
                while end < characters.len()
                    && (characters[end].is_alphanumeric() || characters[end] == '_')
                {
                    end += 1;
                }
                let name: String = characters[start..end].iter().collect();
                output.push_str(&expand_name(&name, scope, environment)?);
                index = end;
                continue;
            }

            output.push('$');
            index += 1;
            continue;
        }

        output.push(character);
        index += 1;
    }
    Ok(output)
}

/// Expands one braced reference.
///
/// A token with no genuine nested `${…}` is handed to `neotales-env` exactly as it was written, so
/// its grammar and its `${NAME:=text}` store-as-written behavior are unchanged. That includes a
/// token whose braces are literal (`${A:-bar{z}}`) and one whose reference is escaped
/// (`${A:-\${B}}`); neither is a nested reference, and both keep `neotales-env`'s operator search.
/// Such a token also cannot make `neotales-env` recurse, because recursion there starts at an
/// unescaped `${…}` and the token has none.
///
/// Only a token that holds a genuine nested reference is split here, at the first operator outside
/// any nested braces, which is the split the upstream substring search gets wrong. The operand is
/// expanded through [`expand_nested`], so a reference inside it may nest again, up to
/// [`DotenvFile::MAX_NESTING_DEPTH`]. A token with a nested reference but no operator outside it is
/// malformed: any operator `neotales-env` could find is inside the braces, so the name it would
/// read contains a `{` and it rejects the token before expanding anything.
fn expand_token(
    token: &str,
    options: &mut ExpandOptions<'_>,
    scope: &RefCell<BTreeMap<String, String>>,
    environment: Option<&BTreeMap<String, String>>,
    depth: usize,
) -> Result<String, SourceError> {
    if depth > DotenvFile::MAX_NESTING_DEPTH {
        return Err(SourceError::invalid());
    }
    if !holds_nested_reference(token) {
        return options
            .expand(&format!("${{{token}}}"))
            .map_err(|_| SourceError::invalid());
    }

    let Some((name, operator, operand)) = split_token(token) else {
        return Err(SourceError::invalid());
    };

    let value = expand_name(&name, scope, environment)?;
    if !value.is_empty() {
        return Ok(value);
    }

    match operator {
        Operator::Default => expand_nested(&operand, options, scope, environment, depth),
        Operator::Assign => {
            // Store the operand as written, as `neotales-env` does for an assign, and read it back
            // through the same scope. A successful store makes the stored text the result.
            store_local(scope, &name, &operand);
            let stored = lookup_local(scope, environment, &name).unwrap_or_default();
            if stored.is_empty() {
                expand_nested(&operand, options, scope, environment, depth)
            } else {
                Ok(stored)
            }
        }
        Operator::Required => Err(SourceError::invalid()),
    }
}

/// Reports whether `token` holds a reference the expansion grammar treats as nested.
///
/// A `$` followed by `{` opens a nested reference unless the grammar escapes it: `\$` and `$$` are
/// one literal `$`, so `${A:-\${B}}` and `${A:-$${B}}` hold no nested reference. A literal `{` on
/// its own is not a reference either. The scan walks left to right and consumes an escape pair
/// whole, which is how the expander itself reads the text.
fn holds_nested_reference(token: &str) -> bool {
    let mut characters = token.char_indices();
    while let Some((_, character)) = characters.next() {
        match character {
            '\\' if characters
                .clone()
                .next()
                .is_some_and(|(_, next)| next == '$') =>
            {
                characters.next();
            }
            '$' => match characters.clone().next().map(|(_, next)| next) {
                Some('$') => {
                    characters.next();
                }
                Some('{') => return true,
                _ => {}
            },
            _ => {}
        }
    }
    false
}

/// Reports whether every reference in `value` nests within [`DotenvFile::MAX_NESTING_DEPTH`].
///
/// The scan is iterative, so it cannot overflow the stack however deep the input is, and it stops
/// as soon as the bound is passed. It mirrors the brace reading the expanders use: every `{` inside
/// a reference keeps that reference open, while `\$` and `$$` are literal dollars that open
/// nothing. A `{` outside a reference opens a literal brace, tracked so the matching `}` is not
/// mistaken for the end of a reference.
fn within_nesting_bound(value: &str) -> bool {
    // One entry per open brace: `true` when that brace opened a `${…}` reference.
    let mut braces: Vec<bool> = Vec::new();
    let mut references = 0_usize;
    let mut characters = value.char_indices();
    while let Some((_, character)) = characters.next() {
        match character {
            '\\' => {
                if characters
                    .clone()
                    .next()
                    .is_some_and(|(_, next)| next == '$')
                {
                    characters.next();
                }
            }
            '$' => match characters.clone().next().map(|(_, next)| next) {
                Some('$') => {
                    characters.next();
                }
                Some('{') => {
                    characters.next();
                    braces.push(true);
                    references += 1;
                    if references > DotenvFile::MAX_NESTING_DEPTH {
                        return false;
                    }
                }
                _ => {}
            },
            '{' => braces.push(false),
            '}' => references -= usize::from(braces.pop() == Some(true)),
            _ => {}
        }
    }
    true
}

/// Splits a token at its first `:` that is not inside a nested `${…}`.
///
/// Returns the name, the operator, and the operand with the operator's punctuation removed. An
/// empty name is read as the operand, matching `neotales-env`'s handling of a token that starts
/// with the operator.
fn split_token(token: &str) -> Option<(String, Operator, String)> {
    let index = first_depth_zero_colon(token)?;
    let rest = &token[index + 1..];
    let (operator, operand) = match rest.chars().next() {
        Some('-') => (Operator::Default, &rest[1..]),
        Some('=') => (Operator::Assign, &rest[1..]),
        Some('?') => (Operator::Required, &rest[1..]),
        _ => (Operator::Default, rest),
    };
    Some(if index == 0 {
        (operand.to_owned(), operator, String::new())
    } else {
        (token[..index].to_owned(), operator, operand.to_owned())
    })
}

/// Returns the byte index of the first `:` outside any nested braces.
fn first_depth_zero_colon(token: &str) -> Option<usize> {
    let mut depth = 0_usize;
    for (index, character) in token.char_indices() {
        match character {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            ':' if depth == 0 => return Some(index),
            _ => {}
        }
    }
    None
}

/// Reads one braced reference, returning its token and the index after the closing brace.
///
/// A nested `${…}` raises the brace depth, so its `}` does not end the token. An empty token and a
/// missing closing brace are errors, matching `neotales-env`.
fn read_braced(characters: &[char], start: usize) -> Result<(String, usize), SourceError> {
    let mut depth = 0_usize;
    let mut token = String::new();
    for (offset, &character) in characters[start..].iter().enumerate() {
        match character {
            '{' => {
                depth += 1;
                token.push(character);
            }
            '}' if depth == 0 => {
                if token.is_empty() {
                    return Err(SourceError::invalid());
                }
                return Ok((token, start + offset + 1));
            }
            '}' => {
                depth -= 1;
                token.push(character);
            }
            _ => token.push(character),
        }
    }
    Err(SourceError::invalid())
}

impl Source for DotenvFile {
    fn load(&self) -> Result<Value, SourceError> {
        self.build()
    }

    fn watch_paths(&self) -> Vec<PathBuf> {
        if self.reload_on_change {
            vec![self.path.clone()]
        } else {
            Vec::new()
        }
    }
}
