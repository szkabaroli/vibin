//! Data-driven detection of deceptive Unicode: characters *confusable*
//! with ASCII (Cyrillic
//! `а`, Greek `ο`, fullwidth forms…) come from the official unicode.org
//! confusables table (assets/confusables.txt), and *invisible* / format
//! characters (zero-width, unusual whitespace, bidi controls) from a
//! curated set. Legitimate non-ASCII (accented letters, CJK, emoji) is
//! accepted. Each flagged character can name itself for the hover popup.

use std::collections::HashMap;
use std::sync::OnceLock;

/// source char → (its ASCII look-alike, the source's Unicode name).
fn table() -> &'static HashMap<char, (char, String)> {
    static T: OnceLock<HashMap<char, (char, String)>> = OnceLock::new();
    T.get_or_init(|| {
        let mut map = HashMap::new();
        for line in include_str!("../assets/confusables.txt").lines() {
            if line.starts_with('#') || line.trim().is_empty() {
                continue;
            }
            let mut it = line.splitn(3, ' ');
            let (Some(src), Some(ascii), name) = (it.next(), it.next(), it.next()) else {
                continue;
            };
            if let (Ok(s), Ok(a)) = (u32::from_str_radix(src, 16), u32::from_str_radix(ascii, 16))
                && let (Some(sc), Some(ac)) = (char::from_u32(s), char::from_u32(a))
            {
                map.insert(sc, (ac, name.unwrap_or("").to_string()));
            }
        }
        map
    })
}

/// Whether `c` is visually confusable with an ASCII character.
pub fn is_confusable(c: char) -> bool {
    !c.is_ascii() && table().contains_key(&c)
}

/// The ASCII character `c` is confusable with, if any.
#[allow(dead_code)] // accessor for tests and future code-action replacement
pub fn lookalike(c: char) -> Option<char> {
    table().get(&c).map(|(a, _)| *a)
}

/// An invisible or format character with no meaningful glyph: unusual
/// whitespace, zero-width and format
/// characters, bidi controls, variation selectors, the BOM.
pub fn is_invisible(c: char) -> bool {
    if c.is_ascii() {
        return false; // ordinary space/tab/newline are fine
    }
    matches!(c,
        '\u{0085}' | '\u{00A0}' | '\u{00AD}' | '\u{115F}' | '\u{1160}' | '\u{1680}'
        | '\u{17B4}' | '\u{17B5}' | '\u{180E}' | '\u{2000}'..='\u{200F}' | '\u{2028}'..='\u{202F}'
        | '\u{205F}'..='\u{2064}' | '\u{2066}'..='\u{206F}' | '\u{3000}' | '\u{3164}'
        | '\u{FE00}'..='\u{FE0F}' | '\u{FEFF}' | '\u{FFA0}' | '\u{FFF9}'..='\u{FFFB}'
        | '\u{1D173}'..='\u{1D17A}' | '\u{E0000}'..='\u{E007F}'
    )
}

/// Short name of an invisible character for the hover popup.
fn invisible_name(c: char) -> &'static str {
    match c {
        '\u{00A0}' => "NO-BREAK SPACE",
        '\u{00AD}' => "SOFT HYPHEN",
        '\u{200B}' => "ZERO WIDTH SPACE",
        '\u{200C}' => "ZERO WIDTH NON-JOINER",
        '\u{200D}' => "ZERO WIDTH JOINER",
        '\u{200E}' => "LEFT-TO-RIGHT MARK",
        '\u{200F}' => "RIGHT-TO-LEFT MARK",
        '\u{202A}'..='\u{202E}' => "BIDIRECTIONAL OVERRIDE",
        '\u{2060}' => "WORD JOINER",
        '\u{2066}'..='\u{2069}' => "BIDIRECTIONAL ISOLATE",
        '\u{3000}' => "IDEOGRAPHIC SPACE",
        '\u{FEFF}' => "ZERO WIDTH NO-BREAK SPACE (BOM)",
        '\u{2000}'..='\u{200A}' | '\u{202F}' | '\u{205F}' => "UNUSUAL SPACE",
        '\u{FE00}'..='\u{FE0F}' => "VARIATION SELECTOR",
        _ => "INVISIBLE / FORMAT CHARACTER",
    }
}

/// Popup text describing why a character is flagged:
/// "U+0430 CYRILLIC SMALL LETTER A — looks like ASCII 'a'".
pub fn describe(c: char) -> Option<String> {
    let code = format!("U+{:04X}", c as u32);
    if let Some((ascii, name)) = table().get(&c) {
        let name = if name.is_empty() { "confusable character" } else { name.as_str() };
        Some(format!(
            "{code} {name}\nLooks like ASCII '{ascii}' (U+{:04X}) — replace it if unintended.",
            *ascii as u32
        ))
    } else if is_invisible(c) {
        Some(format!(
            "{code} {}\nInvisible / zero-width character — likely unintended.",
            invisible_name(c)
        ))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confusables_come_from_the_unicode_table() {
        // Cyrillic 'а' → 'a', Greek omicron → 'o'
        assert!(is_confusable('\u{0430}'));
        assert_eq!(lookalike('\u{0430}'), Some('a'));
        assert_eq!(lookalike('\u{03BF}'), Some('o')); // Greek omicron
        assert_eq!(lookalike('\u{FF21}'), Some('A')); // fullwidth A
        // legitimate letters are NOT confusable
        assert!(!is_confusable('ü'));
        assert!(!is_confusable('é'));
        assert!(!is_confusable('日'));
        // per the Unicode data: the em-dash maps to a Katakana mark (not
        // ASCII, so accepted), but the en-dash IS confusable with '-'
        assert!(!is_confusable('—'), "em-dash accepted");
        assert_eq!(lookalike('–'), Some('-'), "en-dash confusable with hyphen");
    }

    #[test]
    fn invisible_set_matches_expectations() {
        assert!(is_invisible('\u{00A0}')); // nbsp
        assert!(is_invisible('\u{200B}')); // zero-width space
        assert!(is_invisible('\u{FEFF}')); // BOM
        assert!(is_invisible('\u{202E}')); // RTL override (Trojan Source)
        assert!(!is_invisible(' ')); // ordinary space
        assert!(!is_invisible('ü'));
    }

    #[test]
    fn describe_names_the_character() {
        let d = describe('\u{0430}').unwrap();
        assert!(d.contains("U+0430"), "{d}");
        assert!(d.contains("CYRILLIC"), "{d}");
        assert!(d.contains("'a'"), "{d}");
        let inv = describe('\u{00A0}').unwrap();
        assert!(inv.contains("NO-BREAK SPACE"), "{inv}");
        assert!(describe('a').is_none());
        assert!(describe('ü').is_none());
    }
}
