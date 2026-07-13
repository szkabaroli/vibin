//! Code-aware spell-checking, in the spirit of cspell / codebook: check the
//! prose *and* the identifiers of source, splitting compound names
//! (camelCase, snake_case) into their component words and validating each
//! against a real Hunspell dictionary. Using tree-sitter (our highlight
//! spans) to decide *what* to check and `spellbook` (a pure-Rust Hunspell
//! engine) for *whether it's a word* — the latter's affix rules mean
//! inflections like "walked"/"walking" validate against "walk", which a
//! flat word list can't. Misspellings reuse the LSP diagnostic squiggle.

use spellbook::Dictionary;
use std::sync::OnceLock;

/// A patch dictionary of modern software vocabulary the base SCOWL-derived
/// en_US dict lacks (or stores flagless, so it can't inflect). Edited as a
/// data file — assets/en_US.extra.dic — not in code.
const EXTRA_DIC: &str = include_str!("../assets/en_US.extra.dic");

/// The bundled en_US dictionary (assets/en_US.{aff,dic}), parsed once and
/// patched with the supplementary word list. Each patch line uses Hunspell
/// syntax, so `workspace/S` applies the plural affix rule.
fn dictionary() -> Option<&'static Dictionary> {
    static DICT: OnceLock<Option<Dictionary>> = OnceLock::new();
    DICT.get_or_init(|| {
        let aff = include_str!("../assets/en_US.aff");
        let dic = include_str!("../assets/en_US.dic");
        let mut dict = Dictionary::new(aff, dic).ok()?;
        for line in EXTRA_DIC.lines() {
            let word = line.trim();
            if !word.is_empty() && !word.starts_with('#') {
                let _ = dict.add(word);
            }
        }
        Some(dict)
    })
    .as_ref()
}

/// Whether spell-checking is available (the dictionary parsed).
pub fn available() -> bool {
    dictionary().is_some()
}

use std::collections::HashSet;

fn load_words(text: &str) -> HashSet<String> {
    text.lines()
        .map(str::trim)
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

/// Universal programming vocabulary (cspell software-terms + fullstack),
/// applied to every language's identifiers.
fn common_words() -> &'static HashSet<String> {
    static S: OnceLock<HashSet<String>> = OnceLock::new();
    S.get_or_init(|| load_words(include_str!("../assets/dict/_common.txt")))
}

/// Per-language technical dictionary (cspell @cspell/dict-*), the standard
/// library / API vocabulary of that language, loaded lazily by file
/// language. Each arm has its own cache. `None` when we ship no dict for
/// the language (only comments/strings are checked there, plus common).
fn lang_words(lang: &str) -> Option<&'static HashSet<String>> {
    macro_rules! dict {
        ($file:literal) => {{
            static S: OnceLock<HashSet<String>> = OnceLock::new();
            Some(S.get_or_init(|| load_words(include_str!($file))))
        }};
    }
    match lang {
        "rust" => dict!("../assets/dict/rust.txt"),
        "python" => dict!("../assets/dict/python.txt"),
        "go" => dict!("../assets/dict/go.txt"),
        "c" | "cpp" => dict!("../assets/dict/cpp.txt"),
        "typescript" | "javascript" | "tsx" => dict!("../assets/dict/ts.txt"),
        "c#" => dict!("../assets/dict/csharp.txt"),
        "php" => dict!("../assets/dict/php.txt"),
        "ruby" => dict!("../assets/dict/ruby.txt"),
        "html" => dict!("../assets/dict/html.txt"),
        "css" => dict!("../assets/dict/css.txt"),
        "bash" => dict!("../assets/dict/shell.txt"),
        _ => None,
    }
}

/// Whether `word` is known: the base en_US dictionary, the universal
/// software vocabulary, or the file language's technical dictionary.
/// The hash-set dictionaries are consulted before the Hunspell engine:
/// a set hit is a couple of ns, while a Hunspell miss walks affix rules.
fn known(base: &Dictionary, word: &str, lang: &str) -> bool {
    let lower = word.to_ascii_lowercase();
    common_words().contains(&lower)
        || lang_words(lang).is_some_and(|s| s.contains(&lower))
        || base.check(word)
        || (lower != word && base.check(&lower))
}

use std::collections::HashMap;
use std::sync::Mutex;

/// Word-verdict memo: (lang, word) → known. A file's vocabulary is small
/// and repeats across lines and frames, so after warm-up every word is one
/// hash probe instead of a Hunspell affix walk. Case-sensitive key because
/// `known` treats case (proper nouns) specially.
fn known_cached(base: &Dictionary, word: &str, lang: &str) -> bool {
    static CACHE: OnceLock<Mutex<HashMap<String, HashMap<String, bool>>>> = OnceLock::new();
    let mut cache = CACHE.get_or_init(|| Mutex::new(HashMap::new())).lock().unwrap();
    if !cache.contains_key(lang) {
        cache.insert(lang.to_string(), HashMap::new());
    }
    let per_lang = cache.get_mut(lang).unwrap();
    if let Some(&hit) = per_lang.get(word) {
        return hit;
    }
    let verdict = known(base, word, lang);
    if per_lang.len() >= 65_536 {
        per_lang.clear(); // unbounded growth guard; refills in one frame
    }
    per_lang.insert(word.to_string(), verdict);
    verdict
}

/// Split an identifier into its component alphabetic words with their char
/// offsets: `getHTTPResponse_v2` → [(0,"get"),(3,"HTTP"),(7,"Response")].
/// Digits and underscores are separators; case transitions split camelCase.
fn subwords(word: &[char], base: usize) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < word.len() {
        if !word[i].is_ascii_alphabetic() {
            i += 1;
            continue;
        }
        let start = i;
        i += 1;
        while i < word.len() && word[i].is_ascii_alphabetic() {
            let prev = word[i - 1];
            let cur = word[i];
            // camelCase: lower→Upper ends a word
            if prev.is_ascii_lowercase() && cur.is_ascii_uppercase() {
                break;
            }
            // acronym→Word: UPPER followed by Upper+lower ends before the Upper
            if prev.is_ascii_uppercase()
                && cur.is_ascii_uppercase()
                && word.get(i + 1).is_some_and(|c| c.is_ascii_lowercase())
            {
                break;
            }
            i += 1;
        }
        out.push((base + start, word[start..i].iter().collect()));
    }
    out
}

/// Whether a component word should be checked at all: ≥3 letters and not an
/// ALL-CAPS acronym. (Technical terms are handled by the dictionaries now.)
fn should_check(word: &str) -> bool {
    let chars: Vec<char> = word.chars().collect();
    chars.len() >= 3 && !chars.iter().all(|c| c.is_ascii_uppercase())
}

/// Character ranges (start, end) of misspelled component words within
/// `text`, limited to positions where `spellable[i]` is true (comment,
/// string, or identifier regions). `lang` selects the technical dictionary.
/// Indexed by character.
pub fn misspelled_ranges(text: &str, spellable: &[bool], lang: &str) -> Vec<(usize, usize)> {
    let Some(dict) = dictionary() else {
        return Vec::new();
    };
    // nothing spellable on this line — the common case for code-only lines
    if !spellable.contains(&true) {
        return Vec::new();
    }
    // Line-result memo: the renderer re-checks the same visible lines every
    // frame (cursor moves, scrolling), so identical (line, mask, lang)
    // inputs are answered with one hash probe.
    static LINES: OnceLock<Mutex<HashMap<u64, Vec<(usize, usize)>>>> = OnceLock::new();
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    lang.hash(&mut h);
    text.hash(&mut h);
    spellable.hash(&mut h);
    let key = h.finish();
    let cache = LINES.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(hit) = cache.lock().unwrap().get(&key) {
        return hit.clone();
    }
    let ranges = ranges_against(text, spellable, dict, lang);
    let mut cache = cache.lock().unwrap();
    if cache.len() >= 16_384 {
        cache.clear(); // unbounded growth guard; visible lines refill it
    }
    cache.insert(key, ranges.clone());
    ranges
}

fn ranges_against(
    text: &str,
    spellable: &[bool],
    dict: &Dictionary,
    lang: &str,
) -> Vec<(usize, usize)> {
    let chars: Vec<char> = text.chars().collect();
    let is_word_char = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        // an identifier/word: a run of word-chars fully inside a spell region
        if !is_word_char(chars[i]) || !spellable.get(i).copied().unwrap_or(false) {
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && is_word_char(chars[i]) && spellable.get(i).copied().unwrap_or(false)
        {
            i += 1;
        }
        // split the run and check each component
        for (off, sub) in subwords(&chars[start..i], start) {
            if should_check(&sub) && !known_cached(dict, &sub, lang) {
                out.push((off, off + sub.chars().count()));
            }
        }
    }
    out
}

/// Correction suggestions for a word (for a future hover / code action).
#[allow(dead_code)] // wired into a hover/code-action later
pub fn suggestions(word: &str) -> Vec<String> {
    match dictionary() {
        Some(dict) => {
            let mut out = Vec::new();
            dict.suggest(word, &mut out);
            out.truncate(5);
            out
        }
        None => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Not a correctness test — a timing probe for the render hot path.
    /// Run: cargo test --release bench_render_hot_path -- --ignored --nocapture
    #[test]
    #[ignore]
    fn bench_render_hot_path() {
        let dict = dictionary().unwrap();
        let text = include_str!("../src/ui.rs");
        let lines: Vec<&str> = text.lines().take(2000).collect();
        // warm the dictionaries
        let _ = known(dict, "warm", "rust");
        const FRAMES: usize = 100;
        let run = |name: &str, scroll: bool| {
            let start = std::time::Instant::now();
            let mut total = 0usize;
            for frame in 0..FRAMES {
                // a "frame": spell-check 60 visible lines, all spellable
                let off = if scroll { frame } else { 0 };
                for line in lines.iter().cycle().skip(off).take(60) {
                    let mask = vec![true; line.chars().count()];
                    total += misspelled_ranges(line, &mask, "rust").len();
                }
            }
            let el = start.elapsed();
            println!("{name}: {el:?} ({:?}/frame, {total} ranges)", el / FRAMES as u32);
        };
        run("static (cursor moves, same 60 lines)", false);
        run("scrolling (window shifts each frame) ", true);
    }

    #[test]
    fn dictionary_loads_and_knows_common_words() {
        let dict = dictionary().expect("bundled dictionary parses");
        assert!(known(dict, "hello", "text"));
        assert!(known(dict, "computer", "text"));
        // affix morphology: inflections validate against their stem
        assert!(known(dict, "walked", "text"));
        assert!(known(dict, "walking", "text"));
        assert!(known(dict, "cats", "text"));
        // genuine misspellings do not
        assert!(!known(dict, "teh", "text"));
        assert!(!known(dict, "recieve", "text"));
    }

    #[test]
    fn patch_dictionary_adds_software_words_with_proper_affixes() {
        let dict = dictionary().unwrap();
        // "workspace" is a bare stem in the base dict (no plural); the patch
        // re-adds it with the /S flag so the plural inflects correctly
        assert!(known(dict, "workspace", "text"));
        assert!(known(dict, "workspaces", "text"), "the reported false positive");
        assert!(known(dict, "namespace", "text"));
        assert!(known(dict, "namespaces", "text"));
        assert!(known(dict, "filesystem", "text"));
        assert!(known(dict, "filesystems", "text"));
        assert!(known(dict, "middleware", "text"));
        // and via the full pipeline, "workspaces" in prose isn't flagged
        let text = "the workspaces are open";
        let mask = vec![true; text.chars().count()];
        assert!(ranges_against(text, &mask, dict, "text").is_empty());
    }

    #[test]
    fn patch_file_is_well_formed() {
        // every non-comment line is a bare word or word/FLAGS, no stray spaces
        for line in EXTRA_DIC.lines() {
            let w = line.trim();
            if w.is_empty() || w.starts_with('#') {
                continue;
            }
            let stem = w.split('/').next().unwrap();
            assert!(
                stem.chars().all(|c| c.is_ascii_alphabetic()),
                "patch entry {w:?} has a non-alphabetic stem"
            );
        }
    }

    #[test]
    fn splits_compound_identifiers() {
        let split = |s: &str| -> Vec<String> {
            let chars: Vec<char> = s.chars().collect();
            subwords(&chars, 0).into_iter().map(|(_, w)| w).collect()
        };
        assert_eq!(split("getUserName"), vec!["get", "User", "Name"]);
        assert_eq!(split("HTTPResponse"), vec!["HTTP", "Response"]);
        assert_eq!(split("parse_config_v2"), vec!["parse", "config", "v"]);
        assert_eq!(split("snake_case"), vec!["snake", "case"]);
    }

    #[test]
    fn flags_misspelled_components_in_identifiers_and_prose() {
        let dict = dictionary().unwrap();
        // "recieve" inside camelCase is caught after splitting
        let text = "let recieveBuffer = 1;";
        let mask = vec![true; text.chars().count()];
        let ranges = ranges_against(text, &mask, dict, "text");
        let flagged: Vec<&str> = ranges.iter().map(|&(s, e)| &text[s..e]).collect();
        assert!(flagged.contains(&"recieve"), "{flagged:?}");
        // "Buffer" is a real word → not flagged; "let" is real too
        assert!(!flagged.contains(&"Buffer"));
        assert!(!flagged.contains(&"let"));
    }

    #[test]
    fn skips_technical_terms_and_short_fragments() {
        let dict = dictionary().unwrap();
        // fmt/ptr are Rust-specific, so they need the rust dictionary
        let text = "impl fmt ptr usize argv stdin";
        let mask = vec![true; text.chars().count()];
        assert!(ranges_against(text, &mask, dict, "rust").is_empty(), "tech terms skipped");
    }

    #[test]
    fn respects_the_spellable_mask() {
        let dict = dictionary().unwrap();
        let text = "recieve recieve";
        let mut mask = vec![false; text.chars().count()];
        mask[0..7].fill(true); // only the first occurrence is in a spell region
        let ranges = ranges_against(text, &mask, dict, "text");
        assert_eq!(ranges, vec![(0, 7)]);
    }

    #[test]
    fn suggestions_offer_the_correct_word() {
        let sugg = suggestions("recieve");
        assert!(sugg.iter().any(|s| s == "receive"), "{sugg:?}");
    }

    #[test]
    fn project_vocabulary_from_the_patch_is_accepted() {
        let dict = dictionary().unwrap();
        for w in [
            "workspaces", "namespaces", "ratatui", "crossterm", "truecolor", "endianness",
            "undercurl", "worktrees", "renderer", "scrollback", "fourcc", "varint", "glyphs",
        ] {
            assert!(known(dict, w, "text"), "patch should accept {w:?}");
        }
    }

    #[test]
    fn per_language_dictionary_suppresses_stdlib_method_noise() {
        let dict = dictionary().unwrap();
        // Rust std method names that aren't English words and aren't in the
        // base dict — only the scanned rust dictionary knows them
        for m in ["rsplit", "splitn", "rposition"] {
            assert!(!dict.check(m), "{m} shouldn't be in the base dict");
            assert!(known(dict, m, "rust"), "{m} should be in the rust dict");
        }
        // via the pipeline: these std methods aren't flagged under rust…
        let text = "iter.rsplit().splitn()";
        let mask = vec![true; text.chars().count()];
        assert!(ranges_against(text, &mask, dict, "rust").is_empty(), "rust: clean");
        // …but the same text under a language without those terms flags them
        assert!(!ranges_against(text, &mask, dict, "text").is_empty(), "text: flagged");
        // C stdlib lives in the cpp dict
        assert!(known(dict, "malloc", "cpp"));
        assert!(known(dict, "strlen", "cpp"));
    }
}
