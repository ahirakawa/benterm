//! Which commands the palette shows for what has been typed, and in what
//! order. Nothing here knows about the screen.

/// The name a command is shown with, made from the name of its action:
/// `tab::SelectNextTab` is shown as `Tab: Select Next Tab`.
pub fn command_name(action_name: &str) -> String {
    let (namespace, name) = match action_name.rsplit_once("::") {
        Some((namespace, name)) => (Some(namespace), name),
        None => (None, action_name),
    };
    let words = camel_words(name).join(" ");
    match namespace {
        Some(namespace) => {
            let namespace: Vec<String> = namespace
                .split(['_', ':'])
                .filter(|word| !word.is_empty())
                .map(capitalized)
                .collect();
            format!("{}: {words}", namespace.join(" "))
        }
        None => words,
    }
}

/// `SelectNextTab` as `["Select", "Next", "Tab"]`.
fn camel_words(name: &str) -> Vec<&str> {
    let mut words = Vec::new();
    let mut start = 0;
    for (offset, ch) in name.char_indices().skip(1) {
        if ch.is_uppercase() {
            words.push(&name[start..offset]);
            start = offset;
        }
    }
    if start < name.len() {
        words.push(&name[start..]);
    }
    words
}

fn capitalized(word: &str) -> String {
    let mut chars = word.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// How well a name matches, best first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Tier {
    /// The name starts with what was typed.
    Start,
    /// A word of the name starts with it.
    WordStart,
    /// The name holds it somewhere.
    Inside,
    /// The name has its letters, in order but apart.
    Scattered,
}

/// The indexes of the `names` that `query` matches, best first. A name
/// matches if it has the letters of the query in order, whatever their
/// case; spaces in the query need not be in the name. Names that start
/// with the query come first, then those with a word that starts with it,
/// then those that hold it, then those with its letters apart. Names that
/// match equally well keep their order.
pub fn matches<S: AsRef<str>>(query: &str, names: &[S]) -> Vec<usize> {
    let query = query.split_whitespace().collect::<Vec<_>>().join(" ");
    let query = query.to_lowercase();
    let mut found: Vec<(Tier, usize)> = names
        .iter()
        .enumerate()
        .filter_map(|(index, name)| tier(&query, name.as_ref()).map(|tier| (tier, index)))
        .collect();
    // A stable sort keeps the order of names in the same tier.
    found.sort_by_key(|(tier, _)| *tier);
    found.into_iter().map(|(_, index)| index).collect()
}

/// How well `name` matches `query`, which is in lower case with single
/// spaces, or `None` if it does not.
fn tier(query: &str, name: &str) -> Option<Tier> {
    let name = name.to_lowercase();
    if name.starts_with(query) {
        return Some(Tier::Start);
    }
    // Every place the query starts, overlapping ones too.
    let mut inside = false;
    let mut before = None;
    for (offset, ch) in name.char_indices() {
        if name[offset..].starts_with(query) {
            if before.is_none_or(|before: char| !before.is_alphanumeric()) {
                return Some(Tier::WordStart);
            }
            inside = true;
        }
        before = Some(ch);
    }
    if inside {
        return Some(Tier::Inside);
    }
    let mut rest = name.chars();
    query
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .all(|wanted| rest.any(|ch| ch == wanted))
        .then_some(Tier::Scattered)
}

/// A key of a binding written as macOS menus write it, such as `⇧⌘D`.
pub fn key_label(modifiers: &gpui::Modifiers, key: &str) -> String {
    let mut label = String::new();
    for (held, mark) in [
        (modifiers.control, "⌃"),
        (modifiers.alt, "⌥"),
        (modifiers.shift, "⇧"),
        (modifiers.platform, "⌘"),
    ] {
        if held {
            label.push_str(mark);
        }
    }
    let named = match key {
        "left" => Some("←"),
        "right" => Some("→"),
        "up" => Some("↑"),
        "down" => Some("↓"),
        "enter" => Some("↩"),
        "escape" => Some("⎋"),
        "backspace" => Some("⌫"),
        "delete" => Some("⌦"),
        "tab" => Some("⇥"),
        "space" => Some("Space"),
        _ => None,
    };
    match named {
        Some(name) => label.push_str(name),
        None => label.push_str(&key.to_uppercase()),
    }
    label
}

/// Splits `name` into `(category, label)` at the first `": "`, if present.
pub fn split_category(name: &str) -> (Option<&str>, &str) {
    match name.split_once(": ") {
        Some((cat, rest)) if !cat.is_empty() && !rest.is_empty() => (Some(cat), rest),
        _ => (None, name),
    }
}

/// Returns non-overlapping byte ranges in `text` that match `query` (case-insensitive),
/// preferring a contiguous substring match before falling back to scattered characters.
pub fn match_ranges(query: &str, text: &str) -> Vec<std::ops::Range<usize>> {
    let q: String = query
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    if q.is_empty() || text.is_empty() {
        return Vec::new();
    }
    let chars: Vec<(usize, usize, char)> = text
        .char_indices()
        .map(|(start, ch)| (start, start + ch.len_utf8(), ch))
        .collect();
    let lowered: Vec<char> = chars
        .iter()
        .map(|(_, _, ch)| ch.to_lowercase().next().unwrap_or(*ch))
        .collect();
    let q_chars: Vec<char> = q.chars().collect();

    // Prefer a contiguous match starting at a word boundary, then anywhere.
    let mut first_inside: Option<std::ops::Range<usize>> = None;
    if q_chars.len() <= lowered.len() {
        for i in 0..=(lowered.len() - q_chars.len()) {
            if lowered[i..i + q_chars.len()] == q_chars[..] {
                let range = chars[i].0..chars[i + q_chars.len() - 1].1;
                let at_boundary = i == 0 || !chars[i - 1].2.is_alphanumeric();
                if at_boundary {
                    return vec![range];
                }
                if first_inside.is_none() {
                    first_inside = Some(range);
                }
            }
        }
    }
    if let Some(range) = first_inside {
        return vec![range];
    }

    // Fallback: scattered characters in order.
    let mut ranges: Vec<std::ops::Range<usize>> = Vec::new();
    let mut cursor = 0;
    for wanted in q_chars.into_iter().filter(|ch| !ch.is_whitespace()) {
        if let Some(pos) = lowered[cursor..].iter().position(|&c| c == wanted) {
            let idx = cursor + pos;
            let (start, end, _) = chars[idx];
            if let Some(last) = ranges.last_mut()
                && last.end == start
            {
                last.end = end;
            } else {
                ranges.push(start..end);
            }
            cursor = idx + 1;
        }
    }
    ranges
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_category_extracts_prefix() {
        assert_eq!(
            split_category("Tab: Select Next Tab"),
            (Some("Tab"), "Select Next Tab")
        );
        assert_eq!(split_category("Plain"), (None, "Plain"));
    }

    #[test]
    fn match_ranges_prefers_word_boundary_then_scattered() {
        assert_eq!(match_ranges("next", "Tab: Select Next Tab"), vec![12..16]);
        assert_eq!(
            match_ranges("snb", "Split New Box"),
            vec![0..1, 6..7, 10..11]
        );
        assert!(match_ranges("", "Tab: Close").is_empty());
    }
}
