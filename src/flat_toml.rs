//! The small, strict piece of TOML that ohjaa's config home is written in:
//! `[table]` and `[dotted.table]` headers, `key = "text"` pairs, comments and
//! blank lines. Every file ohjaa writes in it (`repo.toml`, `applied.toml`) is
//! valid TOML that any TOML reader reads the same way.
//!
//! Anything outside that subset is an error naming its line, never skipped:
//! a config file read differently from what its author meant would apply the
//! wrong gate or undo the wrong change. It is not a TOML parser. The `toml`
//! crate is already built here, for the vendored faija-knowledge, but
//! depending on it directly would change the crate's dependency list, which
//! only a person moves (AGENTS.md); until then, this subset.

use std::collections::BTreeMap;

/// Table name (`""` for the top level) to its keys and their text values.
pub type Tables = BTreeMap<String, BTreeMap<String, String>>;

fn bare_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Read a basic string starting just after its opening quote. Returns the
/// value and what follows the closing quote.
fn basic_string(text: &str) -> Result<(String, &str), String> {
    let mut value = String::new();
    let mut chars = text.char_indices();
    while let Some((at, c)) = chars.next() {
        match c {
            '"' => return Ok((value, &text[at + 1..])),
            '\\' => match chars.next().map(|(_, c)| c) {
                Some('"') => value.push('"'),
                Some('\\') => value.push('\\'),
                Some('n') => value.push('\n'),
                Some('t') => value.push('\t'),
                Some('r') => value.push('\r'),
                Some('u') => {
                    let hex: String = chars.by_ref().take(4).map(|(_, c)| c).collect();
                    let code = u32::from_str_radix(&hex, 16)
                        .ok()
                        .and_then(char::from_u32)
                        .ok_or_else(|| format!("bad \\u escape \\u{hex}"))?;
                    value.push(code);
                }
                other => return Err(format!("unsupported escape \\{}", other.unwrap_or(' '))),
            },
            c if c.is_control() && c != '\t' => {
                return Err("a control character inside a string".to_owned())
            }
            c => value.push(c),
        }
    }
    Err("the string is not closed".to_owned())
}

/// Parse `text`, refusing anything outside the subset, and any key twice.
pub fn parse(text: &str) -> Result<Tables, String> {
    let mut tables = Tables::new();
    tables.insert(String::new(), BTreeMap::new());
    let mut current = String::new();
    let mut headers = std::collections::BTreeSet::new();
    for (index, raw) in text.lines().enumerate() {
        let number = index + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fail = |why: String| format!("line {number}: {why}: {raw}");
        if let Some(header) = line.strip_prefix('[') {
            if header.starts_with('[') {
                return Err(fail(
                    "only [table] headers with plain dotted names are read here".into(),
                ));
            }
            let (name, rest) = header
                .split_once(']')
                .ok_or_else(|| fail("a table header is not closed".into()))?;
            let rest = rest.trim();
            if !(rest.is_empty() || rest.starts_with('#')) {
                return Err(fail("text after a table header".into()));
            }
            let name = name.trim();
            if !name.split('.').all(bare_key) {
                return Err(fail(
                    "only [table] headers with plain dotted names are read here".into(),
                ));
            }
            if !headers.insert(name.to_owned()) {
                return Err(fail(format!("table [{name}] appears twice")));
            }
            tables.entry(name.to_owned()).or_default();
            current = name.to_owned();
            continue;
        }
        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| fail("expected key = \"text\"".into()))?;
        let key = key.trim();
        if !bare_key(key) {
            return Err(fail(format!("{key:?} is not a plain key")));
        }
        let value = value.trim_start();
        let after_quote = value
            .strip_prefix('"')
            .filter(|rest| !rest.starts_with("\"\""))
            .ok_or_else(|| fail("only \"text\" values are read here".into()))?;
        let (value, rest) = basic_string(after_quote).map_err(fail)?;
        let rest = rest.trim();
        if !(rest.is_empty() || rest.starts_with('#')) {
            return Err(fail("text after the value".into()));
        }
        let table = tables.entry(current.clone()).or_default();
        if table.insert(key.to_owned(), value).is_some() {
            return Err(fail(format!("{key} appears twice")));
        }
    }
    Ok(tables)
}

/// `value` as a TOML basic string, quotes included.
pub fn quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if c.is_control() => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Refuse keys other than `allowed` in `table`, so a misspelt key is an error
/// rather than a setting silently not applied.
pub fn only_keys(tables: &Tables, table: &str, allowed: &[&str]) -> Result<(), String> {
    match tables
        .get(table)
        .and_then(|keys| keys.keys().find(|key| !allowed.contains(&key.as_str())))
    {
        Some(key) => Err(format!(
            "unknown key {key}{}; the keys read here are {}",
            if table.is_empty() {
                String::new()
            } else {
                format!(" in [{table}]")
            },
            allowed.join(", ")
        )),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn top_level_and_tables_are_read() {
        let tables = parse(
            "# a comment\nschema = \"x.v1\"  # trailing\n\n[entry.w]\ngit = \"ssh://h/o/r.git\"\nrev = \"abc\"\n",
        )
        .unwrap();
        assert_eq!(tables[""]["schema"], "x.v1");
        assert_eq!(tables["entry.w"]["git"], "ssh://h/o/r.git");
        assert_eq!(tables["entry.w"]["rev"], "abc");
    }

    #[test]
    fn quoted_text_reads_back_unchanged() {
        for value in [
            "plain",
            "with \"quotes\"",
            "back\\slash",
            "tab\there",
            "ä # not a comment",
        ] {
            let text = format!("k = {}\n", quote(value));
            assert_eq!(parse(&text).unwrap()[""]["k"], value, "{text}");
        }
    }

    #[test]
    fn anything_outside_the_subset_is_an_error_naming_its_line() {
        for (text, why) in [
            ("k = 1\n", "only \"text\" values"),
            ("k = [\"a\"]\n", "only \"text\" values"),
            ("k = 'literal'\n", "only \"text\" values"),
            ("k = \"\"\"multi\"\"\"\n", "only \"text\" values"),
            ("\"quoted key\" = \"v\"\n", "not a plain key"),
            ("[[array]]\n", "plain dotted names"),
            ("[open\n", "not closed"),
            ("k = \"v\" extra\n", "text after the value"),
            ("k = \"v\"\nk = \"w\"\n", "appears twice"),
            ("[t]\n[t]\n", "appears twice"),
            ("k = \"open\n", "not closed"),
            ("just words\n", "expected key"),
        ] {
            let error = parse(text).unwrap_err();
            assert!(error.starts_with("line "), "{text:?}: {error}");
            assert!(error.contains(why), "{text:?}: {error}");
        }
    }

    #[test]
    fn unknown_keys_are_named() {
        let tables = parse("branch = \"main\"\nbrnch = \"x\"\n").unwrap();
        let error = only_keys(&tables, "", &["branch"]).unwrap_err();
        assert!(error.contains("brnch"), "{error}");
        assert!(only_keys(&parse("branch = \"main\"\n").unwrap(), "", &["branch"]).is_ok());
    }
}
