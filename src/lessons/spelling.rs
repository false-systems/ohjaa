//! How a text is read before the deny list is matched against it, so that a
//! name is found however it is spelled: entities decoded, invisible
//! characters taken out, compatibility forms (fullwidth letters) and Latin
//! diacritics folded to ASCII, combining marks dropped, and, in a second
//! reading, HTML comments and Markdown emphasis taken out so a name split by
//! them joins again. A name matches at the start of a word, whatever
//! follows (`selkod`, `habitats`). What is left outside ASCII is said by
//! [`odd_letter`], since no fold here reads it as a letter inside ASCII.
//!
//! No Unicode library: the folds are tables for the Latin letters (Latin-1
//! and Latin Extended-A) and the fullwidth forms. Any other letter outside
//! ASCII is held back by the caller, so a gap in a table holds a line, never
//! lets one through.

/// Every reading of `text` a name is looked for in: as normalised, and with
/// comments and emphasis taken out too.
pub fn readings(text: &str) -> [String; 2] {
    let base = normalised(text);
    let joined = joined(&base);
    [base, joined]
}

/// `text` with entities decoded, invisible characters taken out, spaces of
/// every width as one space, fullwidth forms and Latin diacritics folded,
/// and combining marks dropped.
pub fn normalised(text: &str) -> String {
    let decoded = decode_entities(text);
    let mut out = String::with_capacity(decoded.len());
    for c in decoded.chars() {
        if invisible(c) || combining(c) {
            continue;
        }
        if space(c) {
            out.push(' ');
            continue;
        }
        match fold_char(c) {
            Some(folded) => out.push_str(folded),
            None => match fullwidth(c) {
                Some(ascii) => out.push(ascii),
                None => out.push(c),
            },
        }
    }
    out
}

/// `text` without HTML comments (an unclosed one to the end), HTML tags
/// (`<b>`, `</span>`, `<wbr>`), Markdown link targets (`](…)`) and
/// Markdown's emphasis and code marks, so that `hab<!-- -->itat`,
/// `hab<b></b>itat`, `[hab](x)itat` and `**hab**itat` read `habitat`.
fn joined(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("<!--") {
        out.push_str(&rest[..at]);
        rest = match rest[at + 4..].find("-->") {
            Some(end) => &rest[at + 4 + end + 3..],
            None => "",
        };
    }
    out.push_str(rest);
    let out = without_between(
        &out,
        |rest| {
            rest.starts_with('<')
                && rest[1..].starts_with(|c: char| c.is_ascii_alphabetic() || c == '/')
        },
        '>',
    );
    let out = without_between(&out, |rest| rest.starts_with("]("), ')');
    out.chars()
        .filter(|c| !matches!(c, '*' | '_' | '`' | '~' | '[' | ']'))
        .collect()
}

/// `text` without each stretch that begins where `opens` says and ends at
/// the next `close` (an unclosed one is left).
fn without_between(text: &str, opens: impl Fn(&str) -> bool, close: char) -> String {
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    while at < text.len() {
        let rest = &text[at..];
        if opens(rest) {
            if let Some(end) = rest.find(close) {
                at += end + close.len_utf8();
                continue;
            }
        }
        let c = rest.chars().next().unwrap_or(' ');
        out.push(c);
        at += c.len_utf8();
    }
    out
}

/// Numeric character references (`&#97;`, `&#x61;`) and the few named ones
/// Markdown text uses, decoded; any other `&name;` is left as it is, for
/// [`unread_entity`] to say.
fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let decoded = after.find(';').filter(|&end| end <= 10).and_then(|end| {
            let name = &after[..end];
            let c = match name.strip_prefix('#') {
                Some(number) => match number.strip_prefix(['x', 'X']) {
                    Some(hex) => u32::from_str_radix(hex, 16).ok(),
                    None => number.parse::<u32>().ok(),
                }
                .and_then(char::from_u32),
                None => match name {
                    "amp" => Some('&'),
                    "lt" => Some('<'),
                    "gt" => Some('>'),
                    "quot" => Some('"'),
                    "apos" => Some('\''),
                    "nbsp" => Some(' '),
                    _ => None,
                },
            }?;
            Some((c, end))
        });
        match decoded {
            Some((c, end)) => {
                out.push(c);
                rest = &after[end + 1..];
            }
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// A named entity no decoding here reads (`&auml;`), which could stand for
/// any letter.
pub fn unread_entity(text: &str) -> Option<String> {
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        let after = &rest[at + 1..];
        if let Some(end) = after.find(';') {
            let name = &after[..end];
            if !name.is_empty()
                && name.len() <= 32
                && name.starts_with(|c: char| c.is_ascii_alphabetic())
                && name.chars().all(|c| c.is_ascii_alphanumeric())
            {
                return Some(format!("&{name};"));
            }
        }
        rest = after;
    }
    None
}

/// What prose writes outside ASCII that hides no letter: curly quotes,
/// dashes, the ellipsis and arrows.
const PROSE: &[char] = &['’', '‘', '“', '”', '—', '–', '…', '→', '←'];

/// The first character outside ASCII in `text` (already normalised) that
/// is not [`PROSE`]: a letter no fold here reads, which may stand for
/// another (a Cyrillic а for an a), or a mark no reading drops (U+0591),
/// which may sit on a letter of a name.
pub fn odd_char(text: &str) -> Option<char> {
    text.chars().find(|c| !c.is_ascii() && !PROSE.contains(c))
}

/// One character as matching compares it: lower case.
fn lower(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// Whether `haystack` holds `needle` anywhere, letter case not told apart.
pub fn names_anywhere(haystack: &str, needle: &str) -> bool {
    let hay: String = normalised(haystack).chars().map(lower).collect();
    let needle: String = normalised(needle).chars().map(lower).collect();
    !needle.is_empty() && hay.contains(&needle)
}

/// Whether `haystack` holds `needle` at the start of a word, letter case not
/// told apart, whatever follows it (`selkod`, `habitats`): what is before it
/// is no letter or digit, or a small letter before a capital (`myOte`). A
/// word that is one of `allowed` (the repository's own name, its owner) is
/// not counted, though `needle` begins it.
pub fn names_word(haystack: &str, needle: &str, allowed: &[String]) -> bool {
    let hay: Vec<char> = normalised(haystack).chars().collect();
    let folded: Vec<char> = hay.iter().copied().map(lower).collect();
    let needle: Vec<char> = normalised(needle).chars().map(lower).collect();
    if needle.is_empty() || needle.len() > hay.len() {
        return false;
    }
    let apart = |left: char, right: char| {
        !left.is_alphanumeric()
            || !right.is_alphanumeric()
            || (left.is_lowercase() && right.is_uppercase())
    };
    let word_at = |start: usize| -> String {
        folded[start..]
            .iter()
            .take_while(|c| c.is_alphanumeric() || matches!(c, '-' | '_'))
            .collect()
    };
    (0..=hay.len() - needle.len()).any(|start| {
        folded[start..start + needle.len()] == needle[..]
            && (start == 0 || apart(hay[start - 1], hay[start]))
            && !allowed.iter().any(|word| *word == word_at(start))
    })
}

/// Characters that show nothing, or that join or split words unseen: the
/// soft hyphen, zero-width spaces and joiners, bidirectional controls, the
/// word joiner and its kin, variation selectors, the byte-order mark,
/// Hangul fillers, interlinear annotations and tags.
fn invisible(c: char) -> bool {
    matches!(
        c as u32,
        0x00AD
            | 0x034F
            | 0x061C
            | 0x115F
            | 0x1160
            | 0x17B4
            | 0x17B5
            | 0x180B..=0x180F
            | 0x200B..=0x200F
            | 0x202A..=0x202E
            | 0x2060..=0x206F
            | 0x3164
            | 0xFE00..=0xFE0F
            | 0xFEFF
            | 0xFFA0
            | 0xFFF9..=0xFFFB
            | 0x1D173..=0x1D17A
            | 0xE0000..=0xE0FFF
    )
}

/// Combining marks, dropped once what they mark is folded: `ä` written as
/// `a` and U+0308 reads `a`.
fn combining(c: char) -> bool {
    matches!(
        c as u32,
        0x0300..=0x036F | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF | 0xFE20..=0xFE2F
    )
}

/// Spaces of every width, read as one.
fn space(c: char) -> bool {
    matches!(
        c as u32,
        0x00A0 | 0x2000..=0x200A | 0x202F | 0x205F | 0x3000
    )
}

/// Fullwidth ASCII (U+FF01 to U+FF5E) as ASCII.
fn fullwidth(c: char) -> Option<char> {
    let code = c as u32;
    (0xFF01..=0xFF5E)
        .contains(&code)
        .then(|| char::from_u32(code - 0xFF01 + 0x21))
        .flatten()
}

/// Latin-1 Supplement's letters, from U+00C0, as ASCII: `?` where the
/// letter is two (`AE`, `TH`, `ss`), `*` where the character is no letter.
const LATIN_1: &str = "AAAAAA?CEEEEIIIIDNOOOOO*OUUUUY??aaaaaa?ceeeeiiiidnooooo*ouuuuy?y";
/// Latin Extended-A, U+0100 to U+017F, as ASCII; `?` where it is two.
const LATIN_EXTENDED_A: &str = concat!(
    "AaAaAaCcCcCcCcDd", // U+0100
    "DdEeEeEeEeEeGgGg", // U+0110
    "GgGgHhHhIiIiIiIi", // U+0120
    "Ii??JjKkkLlLlLlL", // U+0130
    "lLlNnNnNnnNnOoOo", // U+0140
    "Oo??RrRrRrSsSsSs", // U+0150
    "SsTtTtTtUuUuUuUu", // U+0160
    "UuUuWwYyYZzZzZzs", // U+0170
);

/// A Latin letter with a diacritic as the ASCII it is written over.
fn fold_char(c: char) -> Option<&'static str> {
    let code = c as u32;
    let two = match c {
        'Æ' => Some("AE"),
        'æ' => Some("ae"),
        'Þ' => Some("TH"),
        'þ' => Some("th"),
        'ß' => Some("ss"),
        'Ĳ' => Some("IJ"),
        'ĳ' => Some("ij"),
        'Œ' => Some("OE"),
        'œ' => Some("oe"),
        _ => None,
    };
    if two.is_some() {
        return two;
    }
    let (table, from) = match code {
        0x00C0..=0x00FF => (LATIN_1, 0x00C0),
        0x0100..=0x017F => (LATIN_EXTENDED_A, 0x0100),
        _ => return None,
    };
    let at = (code - from) as usize;
    let folded = table.get(at..at + 1)?;
    (folded != "?" && folded != "*").then_some(folded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tables_have_one_ascii_letter_per_code_point() {
        assert_eq!(LATIN_1.len(), 0x100 - 0xC0);
        assert_eq!(LATIN_EXTENDED_A.len(), 0x180 - 0x100);
        for (c, folded) in [
            ('Ä', "A"),
            ('ö', "o"),
            ('å', "a"),
            ('é', "e"),
            ('ñ', "n"),
            ('Ł', "L"),
            ('ő', "o"),
            ('Ž', "Z"),
            ('ſ', "s"),
            ('ß', "ss"),
            ('Œ', "OE"),
        ] {
            assert_eq!(fold_char(c), Some(folded), "{c}");
        }
        assert_eq!(fold_char('×'), None);
        assert_eq!(fold_char('÷'), None);
        assert_eq!(fullwidth('ｈ'), Some('h'));
        assert_eq!(fullwidth('！'), Some('!'));
    }

    #[test]
    fn entities_comments_and_emphasis_are_read_through() {
        assert_eq!(normalised("h&#97;b&#x69;t &amp; a&nbsp;b"), "habit & a b");
        assert_eq!(normalised("a&b; &auml; x&"), "a&b; &auml; x&");
        assert_eq!(unread_entity("a &auml; b"), Some("&auml;".to_owned()));
        assert_eq!(unread_entity("a & b; c&d"), None);
        let [base, joined] = readings("**hab**<!-- x -->itat_wal and `ote`");
        assert_eq!(base, "**hab**<!-- x -->itat_wal and `ote`");
        assert_eq!(joined, "habitatwal and ote");
        assert_eq!(normalised("hab\u{200B}i\u{00AD}tat"), "habitat");
        assert_eq!(normalised("Ha\u{0308}bitat"), "Habitat");
    }

    #[test]
    fn a_name_matches_at_the_start_of_a_word_but_not_the_allowed_one() {
        let none: &[String] = &[];
        assert!(names_word("selkod and more", "selko", none));
        assert!(names_word("SelkoClient", "selko", none));
        assert!(names_word("myOte", "ote", none));
        assert!(!names_word("remote", "ote", none));
        assert!(!names_word("Xote", "ote", none));
        let owner = vec!["yairfalse".to_owned()];
        assert!(!names_word("a personal yairfalse repo", "yair", &owner));
        assert!(names_word("yair said", "yair", &owner));
    }
}
