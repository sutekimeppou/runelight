//! The search field's inline completion.

pub fn config() -> frizbee::Config {
    frizbee::Config {
        max_typos: Some(2),
        ..Default::default()
    }
}

/// What the field shows after the caret for the selected result.
#[derive(Debug, PartialEq, Eq)]
pub enum Completion<'a> {
    /// The query is a prefix of the name: the rest of the name, completing what was typed.
    Suffix(&'a str),
    /// The name matched some other way: shown whole, as a hint beside the query.
    Hint(&'a str),
}

pub fn completion<'a>(query: &str, name: &'a str) -> Option<Completion<'a>> {
    if query.trim().is_empty() || query.starts_with(char::is_whitespace) {
        return None;
    }

    let mut rest = name.char_indices();
    for typed in query.chars() {
        let Some((_, actual)) = rest.next() else {
            return Some(Completion::Hint(name));
        };
        if !typed.to_lowercase().eq(actual.to_lowercase()) {
            return Some(Completion::Hint(name));
        }
    }

    // Fully typed already: nothing left to suggest.
    rest.next()
        .map(|(start, _)| Completion::Suffix(&name[start..]))
}
