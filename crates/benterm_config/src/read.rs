//! Turning TOML text into a [`Config`] and a list of warnings.

use crate::color::Rgba;
use crate::schema::Config;
use std::fmt;
use std::path::PathBuf;
use toml::{Table, Value};

/// A problem with one key of the file, or with the whole file.
#[derive(Clone, Debug, PartialEq)]
pub struct Warning {
    /// The dotted key, such as `colors.tab.text`. Empty for the whole file.
    pub key: String,
    pub problem: Problem,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Problem {
    /// The file is not valid TOML. No key was read from it.
    Syntax(String),
    /// The file exists but could not be read.
    Unreadable(String),
    /// The key is not a setting.
    UnknownKey,
    /// The value has the wrong TOML type.
    WrongType { expected: &'static str },
    /// The value is a string but not a `#rrggbb` or `#rrggbbaa` color.
    InvalidColor(String),
    /// The number is outside `min..=max`, or is not finite.
    OutOfRange { min: f32, max: f32 },
    /// The font family is empty.
    EmptyFontFamily,
    /// The file is not a role, for this reason, and is left out.
    InvalidRole(String),
}

impl fmt::Display for Warning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !self.key.is_empty() {
            write!(f, "{}: ", self.key)?;
        }
        match &self.problem {
            Problem::Syntax(message) => write!(f, "the file is not valid TOML: {message}"),
            Problem::Unreadable(message) => write!(f, "the file could not be read: {message}"),
            Problem::UnknownKey => write!(f, "unknown setting"),
            Problem::WrongType { expected } => write!(f, "expected {expected}"),
            Problem::InvalidColor(value) => {
                write!(
                    f,
                    "{value:?} is not a color like \"#rrggbb\" or \"#rrggbbaa\""
                )
            }
            Problem::OutOfRange { min, max } => write!(f, "must be between {min} and {max}"),
            Problem::EmptyFontFamily => write!(f, "the font family is empty"),
            Problem::InvalidRole(reason) => write!(f, "the role is left out: {reason}"),
        }
    }
}

/// A config read from text, with the problems found on the way. Every key
/// with a problem has its default value.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Loaded {
    pub config: Config,
    pub warnings: Vec<Warning>,
}

/// Reads a config from TOML text. Fails only when the text is not TOML;
/// bad values become warnings.
pub fn parse(text: &str) -> Result<Loaded, Warning> {
    let root: Table = toml::from_str(text).map_err(|e| Warning {
        key: String::new(),
        problem: Problem::Syntax(e.to_string().trim_end().to_owned()),
    })?;
    let mut reader = Reader {
        warnings: Vec::new(),
    };
    let config = Config::read(&mut reader, &root);
    Ok(Loaded {
        config,
        warnings: reader.warnings,
    })
}

/// Like [`parse`], but text that is not TOML gives the default config and a
/// [`Problem::Syntax`] warning.
pub fn load(text: &str) -> Loaded {
    parse(text).unwrap_or_else(|w| Loaded {
        config: Config::default(),
        warnings: vec![w],
    })
}

pub(crate) struct Reader {
    pub(crate) warnings: Vec<Warning>,
}

fn join(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_owned()
    } else {
        format!("{path}.{key}")
    }
}

impl Reader {
    pub(crate) fn warn(&mut self, path: &str, key: &str, problem: Problem) {
        self.warnings.push(Warning {
            key: join(path, key),
            problem,
        });
    }

    /// The sub-table at `key`, or `None` if it is absent or not a table.
    pub(crate) fn table<'a>(
        &mut self,
        parent: Option<&'a Table>,
        parent_path: &str,
        key: &str,
    ) -> Option<&'a Table> {
        match parent?.get(key)? {
            Value::Table(t) => Some(t),
            _ => {
                let expected = "a table";
                self.warn(parent_path, key, Problem::WrongType { expected });
                None
            }
        }
    }

    pub(crate) fn unknown_keys(&mut self, table: Option<&Table>, path: &str, known: &[&str]) {
        let Some(table) = table else { return };
        for key in table.keys() {
            if !known.contains(&key.as_str()) {
                self.warn(path, key, Problem::UnknownKey);
            }
        }
    }

    pub(crate) fn color(
        &mut self,
        table: Option<&Table>,
        path: &str,
        key: &str,
        default: Rgba,
    ) -> Rgba {
        let Some(value) = table.and_then(|t| t.get(key)) else {
            return default;
        };
        let problem = match value {
            Value::String(s) => match s.parse() {
                Ok(c) => return c,
                Err(_) => Problem::InvalidColor(s.clone()),
            },
            _ => Problem::WrongType {
                expected: "a color string",
            },
        };
        self.warn(path, key, problem);
        default
    }

    pub(crate) fn number(
        &mut self,
        table: Option<&Table>,
        path: &str,
        key: &str,
        default: f32,
        (min, max): (f32, f32),
    ) -> f32 {
        let Some(value) = table.and_then(|t| t.get(key)) else {
            return default;
        };
        let n = match value {
            Value::Integer(i) => *i as f64,
            Value::Float(f) => *f,
            _ => {
                let expected = "a number";
                self.warn(path, key, Problem::WrongType { expected });
                return default;
            }
        };
        // NaN fails both comparisons, so it is out of range too.
        if n >= f64::from(min) && n <= f64::from(max) {
            n as f32
        } else {
            self.warn(path, key, Problem::OutOfRange { min, max });
            default
        }
    }

    /// A whole number from `min` to `max`.
    pub(crate) fn count(
        &mut self,
        table: Option<&Table>,
        path: &str,
        key: &str,
        default: usize,
        (min, max): (usize, usize),
    ) -> usize {
        match table.and_then(|t| t.get(key)) {
            None => default,
            Some(Value::Integer(i)) => match usize::try_from(*i) {
                Ok(n) if n >= min && n <= max => n,
                _ => {
                    let (min, max) = (min as f32, max as f32);
                    self.warn(path, key, Problem::OutOfRange { min, max });
                    default
                }
            },
            Some(_) => {
                let expected = "a whole number";
                self.warn(path, key, Problem::WrongType { expected });
                default
            }
        }
    }

    pub(crate) fn families(
        &mut self,
        table: Option<&Table>,
        path: &str,
        key: &str,
        default: Vec<String>,
    ) -> Vec<String> {
        let Some(value) = table.and_then(|t| t.get(key)) else {
            return default;
        };
        let names: Option<Vec<String>> = match value {
            Value::String(s) => Some(vec![s.clone()]),
            Value::Array(items) => items
                .iter()
                .map(|v| v.as_str().map(str::to_owned))
                .collect(),
            _ => None,
        };
        let problem = match names {
            None => Problem::WrongType {
                expected: "a string or a list of strings",
            },
            Some(names) if names.is_empty() || names.iter().any(|n| n.trim().is_empty()) => {
                Problem::EmptyFontFamily
            }
            Some(names) => return names,
        };
        self.warn(path, key, problem);
        default
    }

    pub(crate) fn boolean(
        &mut self,
        table: Option<&Table>,
        path: &str,
        key: &str,
        default: bool,
    ) -> bool {
        match table.and_then(|t| t.get(key)) {
            None => default,
            Some(Value::Boolean(b)) => *b,
            Some(_) => {
                let expected = "true or false";
                self.warn(path, key, Problem::WrongType { expected });
                default
            }
        }
    }

    pub(crate) fn optional_string(
        &mut self,
        table: Option<&Table>,
        path: &str,
        key: &str,
    ) -> Option<String> {
        match table.and_then(|t| t.get(key)) {
            None => None,
            Some(Value::String(s)) => Some(s.clone()),
            Some(_) => {
                let expected = "a string";
                self.warn(path, key, Problem::WrongType { expected });
                None
            }
        }
    }

    /// A non-empty string, taken as a path.
    pub(crate) fn path(
        &mut self,
        table: Option<&Table>,
        path: &str,
        key: &str,
        default: PathBuf,
    ) -> PathBuf {
        match table.and_then(|t| t.get(key)) {
            None => default,
            Some(Value::String(s)) if !s.is_empty() => PathBuf::from(s),
            Some(_) => {
                let expected = "a path";
                self.warn(path, key, Problem::WrongType { expected });
                default
            }
        }
    }
}
