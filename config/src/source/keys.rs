//! Key selection and nesting shared by the opt-in providers.

use std::collections::{BTreeMap, btree_map::Entry};

use crate::Value;

/// A provider key that is selected but cannot become a path.
#[derive(Clone, Copy, Debug)]
pub(crate) struct KeyError;

/// How a provider turns a name into path components.
///
/// `prefix` is removed first. The remaining text is split on `separator`, or on the builder's
/// delimiter when no separator was chosen. `flat` disables splitting entirely. Every component
/// must be non-empty.
#[derive(Clone, Debug)]
pub(crate) struct Keys {
    prefix: String,
    separator: Option<String>,
    delimiter: String,
    flat: bool,
}

impl Default for Keys {
    fn default() -> Self {
        Self {
            prefix: String::new(),
            separator: None,
            delimiter: String::from("."),
            flat: false,
        }
    }
}

impl Keys {
    #[cfg(any(feature = "environment", feature = "dotenv"))]
    pub(crate) fn prefix(mut self, prefix: impl Into<String>) -> Self {
        self.prefix = prefix.into();
        self
    }

    pub(crate) fn separator(mut self, separator: impl Into<String>) -> Self {
        self.separator = Some(separator.into());
        self.flat = false;
        self
    }

    pub(crate) fn flat(mut self) -> Self {
        self.flat = true;
        self.separator = None;
        self
    }

    pub(crate) fn delimiter(mut self, delimiter: char) -> Self {
        self.delimiter = delimiter.to_string();
        self
    }

    /// Returns the components of a selected name, or `None` when the prefix does not match.
    ///
    /// An empty remainder, an empty separator, and an empty component are all errors.
    pub(crate) fn components(&self, name: &str) -> Result<Option<Vec<String>>, KeyError> {
        let Some(rest) = name.strip_prefix(&self.prefix) else {
            return Ok(None);
        };
        if rest.is_empty() {
            return Err(KeyError);
        }

        let separator = if self.flat {
            None
        } else {
            Some(self.separator.as_deref().unwrap_or(self.delimiter.as_str()))
        };
        let raw: Vec<&str> = match separator {
            Some("") => return Err(KeyError),
            Some(separator) => rest.split(separator).collect(),
            None => vec![rest],
        };

        let mut components = Vec::with_capacity(raw.len());
        for component in raw {
            if component.is_empty() {
                return Err(KeyError);
            }
            components.push(component.to_owned());
        }
        Ok(Some(components))
    }
}

/// Inserts a scalar at `components`.
///
/// A repeated leaf keeps the later value. A leaf that would replace a map, or a scalar where a
/// later component needs a map, is rejected rather than resolved by order.
pub(crate) fn insert(
    root: &mut BTreeMap<String, Value>,
    components: &[String],
    value: Value,
) -> Result<(), KeyError> {
    let Some((last, parents)) = components.split_last() else {
        return Err(KeyError);
    };

    let mut current = root;
    for component in parents {
        let node = match current.entry(component.clone()) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => entry.insert(Value::Map(BTreeMap::new())),
        };
        let Value::Map(map) = node else {
            return Err(KeyError);
        };
        current = map;
    }

    match current.get_mut(last) {
        Some(Value::Map(_)) => Err(KeyError),
        Some(leaf) => {
            *leaf = value;
            Ok(())
        }
        None => {
            current.insert(last.clone(), value);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{KeyError, Keys, insert};
    use crate::Value;

    #[cfg(any(feature = "environment", feature = "dotenv"))]
    #[test]
    fn the_builder_delimiter_is_the_default_separator() {
        let keys = Keys::default().prefix("APP_").delimiter(':');
        let components = keys.components("APP_http:port").unwrap().unwrap();
        assert_eq!(components, ["http", "port"]);
    }

    #[cfg(any(feature = "environment", feature = "dotenv"))]
    #[test]
    fn an_explicit_separator_overrides_the_delimiter() {
        let keys = Keys::default()
            .prefix("APP_")
            .delimiter(':')
            .separator("__");
        let components = keys.components("APP_http__port").unwrap().unwrap();
        assert_eq!(components, ["http", "port"]);
    }

    #[cfg(any(feature = "environment", feature = "dotenv"))]
    #[test]
    fn flat_keeps_the_remainder_as_one_component() {
        let keys = Keys::default().prefix("APP_").flat();
        let components = keys.components("APP_http.port").unwrap().unwrap();
        assert_eq!(components, ["http.port"]);
    }

    #[cfg(any(feature = "environment", feature = "dotenv"))]
    #[test]
    fn empty_parts_are_rejected() {
        for (keys, name) in [
            (Keys::default().prefix("APP_"), "APP_"),
            (Keys::default().separator(""), "a"),
            (Keys::default(), "a..b"),
            (Keys::default(), ".a"),
        ] {
            assert!(keys.components(name).is_err(), "{name}");
        }
    }

    #[test]
    fn scalar_and_map_are_never_the_same_path() {
        let mut root = std::collections::BTreeMap::new();
        insert(&mut root, &[String::from("a")], Value::from(1_i64)).unwrap();
        assert!(matches!(
            insert(
                &mut root,
                &[String::from("a"), String::from("b")],
                Value::from(2_i64)
            ),
            Err(KeyError)
        ));

        let mut root = std::collections::BTreeMap::new();
        insert(
            &mut root,
            &[String::from("a"), String::from("b")],
            Value::from(1_i64),
        )
        .unwrap();
        assert!(insert(&mut root, &[String::from("a")], Value::from(2_i64)).is_err());
    }

    #[test]
    fn a_repeated_leaf_keeps_the_later_value() {
        let mut root = std::collections::BTreeMap::new();
        insert(&mut root, &[String::from("a")], Value::from("first")).unwrap();
        insert(&mut root, &[String::from("a")], Value::from("second")).unwrap();
        assert_eq!(root["a"], Value::from("second"));
    }
}
