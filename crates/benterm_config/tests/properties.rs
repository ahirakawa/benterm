//! No input makes the reader panic, and what it returns is always usable.

use benterm_config::{
    Config, FONT_SIZE_RANGE, LINE_HEIGHT_RANGE, PADDING_RANGE, Problem, Rgba, load, parse,
    setting_keys,
};
use proptest::prelude::*;
use toml::{Table, Value};

fn in_range(v: f32, (min, max): (f32, f32)) -> bool {
    v >= min && v <= max
}

fn assert_usable(c: &Config) {
    assert!(in_range(c.terminal.font.size, FONT_SIZE_RANGE));
    assert!(in_range(c.ui.font.size, FONT_SIZE_RANGE));
    assert!(in_range(c.terminal.line_height, LINE_HEIGHT_RANGE));
    assert!(in_range(c.terminal.padding, PADDING_RANGE));
    for family in [&c.terminal.font.family, &c.ui.font.family] {
        assert!(!family.is_empty());
        assert!(family.iter().all(|f| !f.trim().is_empty()));
    }
}

fn leaf() -> impl Strategy<Value = Value> {
    prop_oneof![
        "#[0-9a-fA-F]{6}".prop_map(Value::String),
        "#[0-9a-fA-F]{8}".prop_map(Value::String),
        "\\PC{0,12}".prop_map(Value::String),
        any::<i64>().prop_map(Value::Integer),
        (-1000i64..1000).prop_map(Value::Integer),
        any::<f64>().prop_map(Value::Float),
        Just(Value::Float(f64::NAN)),
        Just(Value::Float(f64::INFINITY)),
        any::<bool>().prop_map(Value::Boolean),
    ]
}

fn value() -> impl Strategy<Value = Value> {
    leaf().prop_recursive(3, 24, 4, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..4).prop_map(Value::Array),
            prop::collection::vec((key(), inner), 0..4)
                .prop_map(|kv| Value::Table(kv.into_iter().collect())),
        ]
    })
}

/// Mostly real key names, so the generated files reach the real settings.
fn key() -> impl Strategy<Value = String> {
    let mut names: Vec<String> = setting_keys()
        .iter()
        .flat_map(|k| k.split('.').map(str::to_owned).collect::<Vec<_>>())
        .collect();
    names.sort();
    names.dedup();
    prop_oneof![
        4 => prop::sample::select(names),
        1 => "[a-z_]{1,8}",
    ]
}

/// A document built by placing values at real dotted keys, with some noise.
fn document() -> impl Strategy<Value = Table> {
    let paths = setting_keys();
    prop::collection::vec(
        prop_oneof![
            3 => (prop::sample::select(paths), value()).prop_map(|(p, v)| (p, v)),
            1 => (key(), value()),
        ],
        0..16,
    )
    .prop_map(|entries| {
        let mut root = Table::new();
        for (path, v) in entries {
            insert(&mut root, &path, v);
        }
        root
    })
}

fn insert(root: &mut Table, path: &str, v: Value) {
    let mut table = root;
    let mut parts = path.split('.').peekable();
    while let Some(part) = parts.next() {
        if parts.peek().is_none() {
            table.insert(part.to_owned(), v);
            return;
        }
        let entry = table
            .entry(part.to_owned())
            .or_insert_with(|| Value::Table(Table::new()));
        if !entry.is_table() {
            *entry = Value::Table(Table::new());
        }
        table = entry.as_table_mut().unwrap();
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn any_text_gives_a_usable_config(text in "\\PC*") {
        assert_usable(&load(&text).config);
    }

    #[test]
    fn any_bytes_give_a_usable_config(bytes in prop::collection::vec(any::<u8>(), 0..256)) {
        assert_usable(&load(&String::from_utf8_lossy(&bytes)).config);
    }

    #[test]
    fn any_toml_document_gives_a_usable_config(doc in document()) {
        let text = toml::to_string(&doc).unwrap();
        let loaded = parse(&text).unwrap();
        assert_usable(&loaded.config);
        for w in &loaded.warnings {
            prop_assert!(!w.key.is_empty());
            prop_assert!(!matches!(w.problem, Problem::Syntax(_) | Problem::Unreadable(_)));
        }
        if doc.is_empty() {
            prop_assert_eq!(loaded.config, Config::default());
        }
    }

    #[test]
    fn a_color_string_is_used_or_reported(s in prop_oneof!["#[0-9a-f]{6}", "\\PC{0,10}"]) {
        let text = format!("[colors.diff]\nadded_background = {}\n", Value::String(s.clone()));
        let loaded = parse(&text).unwrap();
        match s.parse::<Rgba>() {
            Ok(c) => {
                prop_assert_eq!(loaded.config.colors.diff.added_background, c);
                prop_assert!(loaded.warnings.is_empty());
            }
            Err(_) => {
                prop_assert_eq!(loaded.config, Config::default());
                prop_assert_eq!(loaded.warnings.len(), 1);
                prop_assert_eq!(&loaded.warnings[0].key, "colors.diff.added_background");
            }
        }
    }

    #[test]
    fn a_number_is_used_or_reported(n in prop_oneof![any::<f64>(), -10.0..100.0f64]) {
        let text = format!("[terminal]\npadding = {}\n", Value::Float(n));
        let loaded = parse(&text).unwrap();
        if (0.0..=100.0).contains(&n) {
            prop_assert_eq!(loaded.config.terminal.padding, n as f32);
            prop_assert!(loaded.warnings.is_empty());
        } else {
            prop_assert_eq!(loaded.config, Config::default());
            prop_assert_eq!(loaded.warnings.len(), 1);
        }
    }
}
