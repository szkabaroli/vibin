//! Tree-sitter syntax highlighting: language registry keyed by file
//! extension, and a highlighter producing per-line styled spans. Colors
//! follow a built-in dark theme.

use ratatui::style::{Color, Modifier, Style};
use tree_sitter_highlight::{HighlightConfiguration, HighlightEvent, Highlighter};

/// Capture names we recognize, in matching-priority order (tree-sitter
/// picks the first name that is a prefix of the capture).
const HIGHLIGHT_NAMES: [&str; 26] = [
    "attribute",
    "comment",
    "constant.builtin",
    "constant",
    "constructor",
    "escape",
    "function.builtin",
    "function.macro",
    "function.method",
    "function",
    "keyword",
    "label",
    "number",
    "operator",
    "property",
    "punctuation.bracket",
    "punctuation.delimiter",
    "punctuation",
    "string.special",
    "string",
    "tag",
    "type.builtin",
    "type",
    "variable.builtin",
    // captured (rendered plain via style_for's default) so the spell-checker
    // can scan identifier names; see is_spell_region
    "variable",
    "parameter",
];

/// Whether a highlight marks text the spell-checker should scan: comments
/// and strings (universally) plus identifiers — function/method/property
/// names, variables, and parameters. Checking identifiers is only viable
/// because we ship per-language technical dictionaries (assets/dict/) that
/// know the standard-library vocabulary (peekable, rsplit, malloc…).
/// Types/keywords/constructors stay excluded (Vec, Rc, Some).
pub fn is_spell_region(highlight: usize) -> bool {
    matches!(
        HIGHLIGHT_NAMES.get(highlight).copied(),
        Some(
            "comment"
                | "string"
                | "string.special"
                | "function"
                | "function.method"
                | "property"
                | "variable"
                | "parameter"
        )
    )
}

/// Styling for each entry of HIGHLIGHT_NAMES, driven by the terminal's
/// ANSI palette: keyword/operator=5, function=2, string=10,
/// number=4, type/escape=12, comment=bright black. Slots the terminal
/// never reported fall back to built-in palettes — one tuned for dark
/// backgrounds, one for light.
pub fn style_for(highlight: usize) -> Style {
    let s = Style::default();
    let ansi = |slot: usize, dark: (u8, u8, u8), light: (u8, u8, u8)| {
        let (r, g, b) = crate::color::ansi16(slot).unwrap_or(if crate::color::is_light() {
            light
        } else {
            dark
        });
        Color::Rgb(r, g, b)
    };
    match HIGHLIGHT_NAMES[highlight] {
        "comment" => s
            .fg(ansi(8, (105, 110, 118), (160, 161, 167)))
            .add_modifier(Modifier::ITALIC),
        "keyword" | "operator" => s.fg(ansi(5, (183, 148, 244), (166, 38, 164))),
        "function" | "function.method" | "function.builtin" => {
            s.fg(ansi(2, (130, 170, 255), (64, 120, 242)))
        }
        // builtins like @import render keyword-colored in the preview
        "function.macro" | "attribute" => s.fg(ansi(5, (134, 220, 214), (1, 132, 188))),
        "string" | "string.special" => s.fg(ansi(10, (152, 195, 121), (80, 161, 79))),
        "escape" => s.fg(ansi(12, (134, 220, 214), (1, 132, 188))),
        "number" | "constant" | "constant.builtin" => {
            s.fg(ansi(4, (239, 159, 118), (152, 104, 1)))
        }
        "type" | "type.builtin" | "constructor" => s.fg(ansi(12, (229, 200, 144), (193, 132, 1))),
        "label" | "tag" => s.fg(ansi(6, (134, 220, 214), (1, 132, 188))),
        // self/this: keyword-colored —
        // red (slot 1) read as a diagnostic, not code
        "variable.builtin" => s.fg(ansi(5, (183, 148, 244), (166, 38, 164))),
        // properties, method-call targets, and punctuation are plain
        // foreground in the preview
        _ => s,
    }
}

/// Highlight configuration for a file, by extension or well-known
/// filename (Dockerfile has no extension). None → plain text.
pub fn config_for(path: &std::path::Path) -> Option<HighlightConfiguration> {
    if let Some(lang) = filename_language(path) {
        return config_for_lang(lang);
    }
    let ext = path.extension()?.to_str()?;
    config_for_lang(ext)
}

/// Languages recognized by filename rather than extension — Dockerfiles,
/// the go.mod family, and lock/manifest files that have no distinguishing
/// extension but are really a format we already highlight.
fn filename_language(path: &std::path::Path) -> Option<&'static str> {
    let name = path.file_name()?.to_str()?.to_lowercase();
    if name == "dockerfile" || name.starts_with("dockerfile.") || name == "containerfile" {
        return Some("dockerfile");
    }
    let lang = match name.as_str() {
        // go modules
        "go.mod" | "go.work" => "gomod",
        // lock files are TOML / JSON / YAML under the hood
        "cargo.lock" | "poetry.lock" | "uv.lock" | "gopls.lock" => "toml",
        "flake.lock" | "composer.lock" | "deno.lock" | "pipfile.lock" | "cargo.lock.json" => {
            "json"
        }
        "podfile.lock" => "yaml",
        // Ruby-DSL manifests (the .lock siblings are custom text, left alone)
        "gemfile" | "rakefile" | "podfile" | "brewfile" | "vagrantfile" | "guardfile"
        | "capfile" | "thorfile" | "berksfile" | "fastfile" | "appfile" | "matchfile" => "ruby",
        // Python / JS manifests without a clear extension
        "pipfile" => "toml",
        // INI-format dotfiles
        ".gitconfig" | ".editorconfig" | ".npmrc" | ".hgrc" | ".pylintrc" | ".coveragerc"
        | ".flake8" | ".gitmodules" | "gitconfig" => "ini",
        _ => return None,
    };
    Some(lang)
}

/// Highlight configuration by language name or extension — also accepts
/// markdown fence tags like "rust", "ts", "shell".
pub fn config_for_lang(lang: &str) -> Option<HighlightConfiguration> {
    let (language, name_str, highlights, injections, locals) = match lang {
        "rs" => (
            tree_sitter_rust::LANGUAGE,
            "rust",
            tree_sitter_rust::HIGHLIGHTS_QUERY,
            tree_sitter_rust::INJECTIONS_QUERY,
            "",
        ),
        "rust" => (
            tree_sitter_rust::LANGUAGE,
            "rust",
            tree_sitter_rust::HIGHLIGHTS_QUERY,
            tree_sitter_rust::INJECTIONS_QUERY,
            "",
        ),
        // .xcstrings is Apple's String Catalog — JSON, not XML
        "json" | "xcstrings" | "jsonc" | "jsonl" | "webmanifest" => (
            tree_sitter_json::LANGUAGE,
            "json",
            tree_sitter_json::HIGHLIGHTS_QUERY,
            "",
            "",
        ),
        "js" | "jsx" | "mjs" | "cjs" | "javascript" => (
            tree_sitter_javascript::LANGUAGE,
            "javascript",
            tree_sitter_javascript::HIGHLIGHT_QUERY,
            tree_sitter_javascript::INJECTIONS_QUERY,
            tree_sitter_javascript::LOCALS_QUERY,
        ),
        "ts" | "typescript" => (
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT,
            "typescript",
            ts_highlights(),
            tree_sitter_javascript::INJECTIONS_QUERY,
            tree_sitter_typescript::LOCALS_QUERY,
        ),
        "tsx" => (
            tree_sitter_typescript::LANGUAGE_TSX,
            "tsx",
            ts_highlights(),
            tree_sitter_javascript::INJECTIONS_QUERY,
            tree_sitter_typescript::LOCALS_QUERY,
        ),
        "py" | "python" => (
            tree_sitter_python::LANGUAGE,
            "python",
            tree_sitter_python::HIGHLIGHTS_QUERY,
            "",
            "",
        ),
        "sh" | "bash" | "zsh" | "shell" | "console" => (
            tree_sitter_bash::LANGUAGE,
            "bash",
            tree_sitter_bash::HIGHLIGHT_QUERY,
            "",
            "",
        ),
        "yaml" | "yml" => (
            tree_sitter_yaml::LANGUAGE,
            "yaml",
            tree_sitter_yaml::HIGHLIGHTS_QUERY,
            "",
            "",
        ),
        "dockerfile" | "docker" => (
            dockerfile_language(),
            "dockerfile",
            include_str!("../../assets/dockerfile-highlights.scm"),
            "",
            "",
        ),
        "proto" | "protobuf" | "proto3" => (
            tree_sitter_proto::LANGUAGE,
            "proto",
            include_str!("../../assets/proto-highlights.scm"),
            "",
            "",
        ),
        "toml" => (
            tree_sitter_toml_ng::LANGUAGE,
            "toml",
            tree_sitter_toml_ng::HIGHLIGHTS_QUERY,
            "",
            "",
        ),
        "md" | "markdown" => (
            tree_sitter_md::LANGUAGE,
            "markdown",
            tree_sitter_md::HIGHLIGHT_QUERY_BLOCK,
            tree_sitter_md::INJECTION_QUERY_BLOCK,
            "",
        ),
        "html" | "htm" => (
            tree_sitter_html::LANGUAGE,
            "html",
            tree_sitter_html::HIGHLIGHTS_QUERY,
            tree_sitter_html::INJECTIONS_QUERY,
            "",
        ),
        _ if is_xml_ext(lang) => (
            tree_sitter_xml::LANGUAGE_XML,
            "xml",
            tree_sitter_xml::XML_HIGHLIGHT_QUERY,
            "",
            "",
        ),
        "c" | "h" => (
            tree_sitter_c::LANGUAGE,
            "c",
            tree_sitter_c::HIGHLIGHT_QUERY,
            "",
            "",
        ),
        "cpp" | "cc" | "cxx" | "c++" | "hpp" | "hh" | "hxx" | "h++" | "ino" => (
            tree_sitter_cpp::LANGUAGE,
            "cpp",
            cpp_highlights(),
            "",
            "",
        ),
        "go" | "golang" => (
            tree_sitter_go::LANGUAGE,
            "go",
            tree_sitter_go::HIGHLIGHTS_QUERY,
            "",
            "",
        ),
        "gomod" => (
            gomod_language(),
            "gomod",
            include_str!("../../assets/gomod-highlights.scm"),
            "",
            "",
        ),
        "css" => (
            tree_sitter_css::LANGUAGE,
            "css",
            tree_sitter_css::HIGHLIGHTS_QUERY,
            "",
            "",
        ),
        "php" | "phtml" => (
            tree_sitter_php::LANGUAGE_PHP,
            "php",
            tree_sitter_php::HIGHLIGHTS_QUERY,
            tree_sitter_php::INJECTIONS_QUERY,
            "",
        ),
        "sql" | "ddl" | "dml" | "mysql" | "pgsql" | "psql" => (
            tree_sitter_sequel::LANGUAGE,
            "sql",
            tree_sitter_sequel::HIGHLIGHTS_QUERY,
            "",
            "",
        ),
        // INI covers .cfg/.conf/.properties, systemd units, and .desktop
        "ini" | "cfg" | "conf" | "cnf" | "properties" | "prefs" | "desktop" | "service"
        | "socket" | "timer" | "target" | "mount" | "path" | "automount" => (
            tree_sitter_ini::LANGUAGE,
            "ini",
            tree_sitter_ini::HIGHLIGHTS_QUERY,
            "",
            "",
        ),
        "m" | "objc" | "objective-c" => (
            tree_sitter_objc::LANGUAGE,
            "objc",
            tree_sitter_objc::HIGHLIGHTS_QUERY,
            tree_sitter_objc::INJECTIONS_QUERY,
            tree_sitter_objc::LOCALS_QUERY,
        ),
        "swift" => (
            tree_sitter_swift::LANGUAGE,
            "swift",
            tree_sitter_swift::HIGHLIGHTS_QUERY,
            tree_sitter_swift::INJECTIONS_QUERY,
            tree_sitter_swift::LOCALS_QUERY,
        ),
        "java" => (
            tree_sitter_java::LANGUAGE,
            "java",
            tree_sitter_java::HIGHLIGHTS_QUERY,
            "",
            "",
        ),
        "kt" | "kts" | "kotlin" => (
            tree_sitter_kotlin_ng::LANGUAGE,
            "kotlin",
            include_str!("../../assets/kotlin-highlights.scm"),
            "",
            "",
        ),
        "rb" | "ruby" => (
            tree_sitter_ruby::LANGUAGE,
            "ruby",
            tree_sitter_ruby::HIGHLIGHTS_QUERY,
            "",
            tree_sitter_ruby::LOCALS_QUERY,
        ),
        "zig" => (
            tree_sitter_zig::LANGUAGE,
            "zig",
            tree_sitter_zig::HIGHLIGHTS_QUERY,
            tree_sitter_zig::INJECTIONS_QUERY,
            "",
        ),
        "odin" => (
            tree_sitter_odin::LANGUAGE,
            "odin",
            tree_sitter_odin::HIGHLIGHTS_QUERY,
            tree_sitter_odin::INJECTIONS_QUERY,
            tree_sitter_odin::LOCALS_QUERY,
        ),
        "cs" | "csharp" | "c#" => (
            tree_sitter_c_sharp::LANGUAGE,
            "c#",
            tree_sitter_c_sharp::HIGHLIGHTS_QUERY,
            "",
            "",
        ),
        _ => return None,
    };
    // Many grammars (nvim-treesitter lineage) tag nodes with pseudo-captures
    // like `@spell`/`@conceal` after the real highlight — e.g.
    // `(comment) @comment @spell`. An unrecognized trailing capture shadows
    // the real one, so comments/strings render plain. Strip them.
    let highlights = strip_pseudo_captures(highlights);
    let mut config =
        HighlightConfiguration::new(language.into(), name_str, &highlights, injections, locals)
            .ok()?;
    config.configure(&HIGHLIGHT_NAMES);
    Some(config)
}

/// Remove capture aliases we don't render (and which, when trailing, shadow
/// the capture we do). Returns the query unchanged when none are present.
fn strip_pseudo_captures(query: &str) -> std::borrow::Cow<'_, str> {
    // longest-first: `@spell.error` must be removed before `@spell`, else
    // the prefix match mangles it into a stray `.error`
    const PSEUDO: [&str; 5] = [" @spell.error", " @nospell", " @conceal", " @none", " @spell"];
    if PSEUDO.iter().any(|p| query.contains(p)) {
        let mut out = query.to_string();
        for p in PSEUDO {
            out = out.replace(p, "");
        }
        std::borrow::Cow::Owned(out)
    } else {
        std::borrow::Cow::Borrowed(query)
    }
}

/// The dockerfile grammar's published crate pins tree-sitter 0.20 (whose
/// C runtime would collide with ours), so we vendor and compile just its
/// parser C source (grammars/dockerfile, built by build.rs) and bind the
/// symbol directly. The parser is ABI 14; our 0.26 runtime accepts it.
fn dockerfile_language() -> tree_sitter_language::LanguageFn {
    unsafe extern "C" {
        fn tree_sitter_dockerfile() -> *const ();
    }
    unsafe { tree_sitter_language::LanguageFn::from_raw(tree_sitter_dockerfile) }
}

/// Vendored go.mod grammar (see build.rs), bound the same way.
fn gomod_language() -> tree_sitter_language::LanguageFn {
    unsafe extern "C" {
        fn tree_sitter_gomod() -> *const ();
    }
    unsafe { tree_sitter_language::LanguageFn::from_raw(tree_sitter_gomod) }
}

/// C++'s grammar is a superset of C; concatenate C's highlight query with
/// the C++-specific additions so plain-C constructs still colorize.
fn cpp_highlights() -> &'static str {
    static QUERY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    QUERY.get_or_init(|| {
        format!(
            "{}\n{}",
            tree_sitter_c::HIGHLIGHT_QUERY,
            tree_sitter_cpp::HIGHLIGHT_QUERY
        )
    })
}

/// TypeScript's highlight query extends JavaScript's: concatenate them
/// (TS-specific patterns first so they win on shared nodes).
fn ts_highlights() -> &'static str {
    static QUERY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    QUERY.get_or_init(|| {
        format!(
            "{}\n{}",
            tree_sitter_typescript::HIGHLIGHTS_QUERY,
            tree_sitter_javascript::HIGHLIGHT_QUERY
        )
    })
}

/// Language display name for the statusline.
pub fn language_name(path: &std::path::Path) -> &'static str {
    if let Some(lang) = filename_language(path) {
        return lang;
    }
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "rs" => "rust",
        "json" | "xcstrings" | "jsonc" | "jsonl" | "webmanifest" => "json",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "ts" | "tsx" => "typescript",
        "py" => "python",
        "sh" | "bash" | "zsh" => "bash",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "dockerfile" => "dockerfile",
        "proto" => "protobuf",
        "md" | "markdown" => "markdown",
        "html" | "htm" => "html",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" | "ino" => "cpp",
        "go" => "go",
        "css" => "css",
        "php" | "phtml" => "php",
        "sql" | "ddl" | "dml" => "sql",
        "ini" | "cfg" | "conf" | "cnf" | "properties" | "prefs" | "desktop" | "service"
        | "socket" | "timer" | "target" | "mount" | "path" | "automount" => "ini",
        "m" => "objc",
        "swift" => "swift",
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "rb" => "ruby",
        "zig" => "zig",
        "odin" => "odin",
        "cs" => "c#",
        ext if is_xml_ext(ext) => "xml",
        _ => "text",
    }
}

/// Extensions of well-known XML-encoded formats. (OOXML `.docx`, ODF
/// `.odt`, `.epub`, `.jar`, `.apk`, `.vsix` and friends are ZIP archives
/// of XML, so they take the binary/zip path, not this text one.)
fn is_xml_ext(ext: &str) -> bool {
    matches!(
        ext,
        // core
        "xml" | "svg" | "xsd" | "xsl" | "xslt" | "rss" | "atom" | "opml" | "rdf" | "wsdl" | "wadl"
        // Apple (XML plists and Interface Builder)
        | "plist" | "strings" | "stringsdict" | "mobileconfig" | "entitlements"
        | "storyboard" | "xib"
        // .NET / MSBuild / Windows
        | "xaml" | "csproj" | "vbproj" | "fsproj" | "vcxproj" | "props" | "targets"
        | "nuspec" | "resx" | "manifest"
        // JVM tooling
        | "fxml" | "jnlp" | "iml" | "pom" | "jrxml" | "bpmn"
        // geo
        | "gpx" | "kml" | "gml" | "osm" | "collada" | "dae"
        // UI toolkits
        | "ui" | "qrc" | "glade"
        // docs / i18n
        | "dita" | "ditamap" | "docbook" | "xliff" | "xlf" | "tmx"
        // flat OpenDocument (uncompressed)
        | "fodt" | "fods" | "fodp" | "fodg"
        // math / music / playlists
        | "mml" | "musicxml" | "xspf"
    )
}

/// A highlighted byte range of the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HighlightSpan {
    pub start: usize,
    pub end: usize,
    pub highlight: usize,
}

/// Highlight one line of code in the given language, using a per-thread
/// config cache (configs are expensive to build). Used by the diff views;
/// single-line parses lose multi-line context but tokenize well enough.
pub fn line_spans(lang: &str, text: &str) -> Vec<HighlightSpan> {
    use std::cell::RefCell;
    use std::collections::HashMap;
    thread_local! {
        static CONFIGS: RefCell<HashMap<String, Option<HighlightConfiguration>>> =
            RefCell::new(HashMap::new());
    }
    CONFIGS.with(|cache| {
        let mut cache = cache.borrow_mut();
        let config = cache
            .entry(lang.to_string())
            .or_insert_with(|| config_for_lang(lang));
        match config {
            Some(config) => highlight_source(config, text),
            None => Vec::new(),
        }
    })
}

/// Incremental highlighter for an open file: keeps the tree-sitter `Tree`
/// between edits so a keystroke costs one `Tree::edit` + reparse of the
/// changed region, not a parse of the whole buffer. Spans come from a
/// plain `QueryCursor` pass over the tree — equivalent to the
/// tree-sitter-highlight event stream here, since injections are never
/// resolved anyway (see `highlight_source`'s `|_| None`).
pub struct FileHighlighter {
    config: HighlightConfiguration,
    /// capture index → HIGHLIGHT_NAMES slot, resolved once per query
    captures: Vec<Option<usize>>,
    parser: tree_sitter::Parser,
    tree: Option<tree_sitter::Tree>,
    /// The source `tree` was parsed from, kept to synthesize the next edit.
    source: String,
}

impl FileHighlighter {
    pub fn new(config: HighlightConfiguration) -> Option<Self> {
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&config.language).ok()?;
        let captures = capture_map(&config.query);
        Some(Self { config, captures, parser, tree: None, source: String::new() })
    }

    /// Re-highlight after the buffer changed to `new`: tell the old tree
    /// what changed (one edit spanning the differing region), reparse
    /// reusing it, and extract spans from the fresh tree.
    pub fn highlight(&mut self, new: String) -> Vec<HighlightSpan> {
        self.highlight_window(new, None)
    }

    /// Like `highlight`, but extracts spans only for the given byte range
    /// (the parse is always whole-file — it has to be — but the query pass
    /// is the expensive part and the renderer only reads the viewport).
    pub fn highlight_window(
        &mut self,
        new: String,
        window: Option<std::ops::Range<usize>>,
    ) -> Vec<HighlightSpan> {
        if let Some(tree) = &mut self.tree {
            if self.source != new {
                tree.edit(&synth_edit(&self.source, &new));
            }
        }
        self.tree = self.parser.parse(&new, self.tree.as_ref());
        self.source = new;
        match &self.tree {
            Some(tree) => {
                spans_from_tree(&self.config.query, &self.captures, tree, &self.source, window)
            }
            None => Vec::new(),
        }
    }

    /// Extract spans for a byte range of the *current* source without
    /// reparsing — the scroll-only path (buffer unchanged, window moved).
    pub fn window_only(&mut self, window: std::ops::Range<usize>) -> Vec<HighlightSpan> {
        match &self.tree {
            Some(tree) => {
                spans_from_tree(&self.config.query, &self.captures, tree, &self.source, Some(window))
            }
            None => Vec::new(),
        }
    }
}

/// The single `InputEdit` covering everything that changed between two
/// versions of the source: common prefix and suffix are trimmed (aligned
/// down to char boundaries) and the middle is reported as replaced.
fn synth_edit(old: &str, new: &str) -> tree_sitter::InputEdit {
    let (ob, nb) = (old.as_bytes(), new.as_bytes());
    let mut prefix = ob.iter().zip(nb).take_while(|(a, b)| a == b).count();
    while !old.is_char_boundary(prefix) {
        prefix -= 1;
    }
    let max_suffix = old.len().min(new.len()) - prefix;
    let mut suffix = (0..max_suffix)
        .take_while(|&i| ob[old.len() - 1 - i] == nb[new.len() - 1 - i])
        .count();
    while !old.is_char_boundary(old.len() - suffix) {
        suffix -= 1;
    }
    tree_sitter::InputEdit {
        start_byte: prefix,
        old_end_byte: old.len() - suffix,
        new_end_byte: new.len() - suffix,
        start_position: point_at(old, prefix),
        old_end_position: point_at(old, old.len() - suffix),
        new_end_position: point_at(new, new.len() - suffix),
    }
}

/// (row, byte-column) of a byte offset, as tree-sitter Points want them.
fn point_at(s: &str, byte: usize) -> tree_sitter::Point {
    let pre = &s.as_bytes()[..byte];
    let row = pre.iter().filter(|&&b| b == b'\n').count();
    let line_start = pre.iter().rposition(|&b| b == b'\n').map(|p| p + 1).unwrap_or(0);
    tree_sitter::Point::new(row, byte - line_start)
}

/// capture name → HIGHLIGHT_NAMES slot, replicating tree-sitter-highlight's
/// `configure`: the recognized name whose dot-separated parts form the
/// longest prefix of the capture name wins.
fn capture_map(query: &tree_sitter::Query) -> Vec<Option<usize>> {
    query
        .capture_names()
        .iter()
        .map(|cap| {
            let mut best = None;
            let mut best_len = 0;
            for (i, name) in HIGHLIGHT_NAMES.iter().enumerate() {
                let mut cap_parts = cap.split('.');
                let mut len = 0;
                let matches = name.split('.').all(|part| {
                    len += 1;
                    cap_parts.next() == Some(part)
                });
                if matches && len > best_len {
                    best = Some(i);
                    best_len = len;
                }
            }
            best
        })
        .collect()
}

/// Extract non-overlapping, sorted spans from a parsed tree by painting
/// captures outer-to-inner (nested captures win, mirroring the event
/// stream's nesting) and compressing the result into runs.
fn spans_from_tree(
    query: &tree_sitter::Query,
    captures: &[Option<usize>],
    tree: &tree_sitter::Tree,
    source: &str,
    window: Option<std::ops::Range<usize>>,
) -> Vec<HighlightSpan> {
    use tree_sitter::StreamingIterator;
    let window = window.unwrap_or(0..source.len());
    let (win_lo, win_hi) = (window.start.min(source.len()), window.end.min(source.len()));
    let mut cursor = tree_sitter::QueryCursor::new();
    cursor.set_byte_range(win_lo..win_hi);
    // (start, end, highlight, match order) — order breaks same-range ties
    // in favor of the earlier pattern, as the highlight crate does
    let mut caps: Vec<(usize, usize, u8, usize)> = Vec::new();
    let mut matches = cursor.matches(query, tree.root_node(), source.as_bytes());
    let mut seq = 0usize;
    while let Some(m) = matches.next() {
        for c in m.captures {
            if let Some(hl) = captures[c.index as usize] {
                // clamp nodes that straddle the window edge
                let (s, e) = (c.node.start_byte().max(win_lo), c.node.end_byte().min(win_hi));
                if s < e {
                    caps.push((s, e, hl as u8, seq));
                    seq += 1;
                }
            }
        }
    }
    caps.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)).then(a.3.cmp(&b.3)));
    caps.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1);
    const NONE: u8 = u8::MAX;
    let mut paint = vec![NONE; win_hi - win_lo];
    for &(s, e, hl, _) in &caps {
        paint[s - win_lo..e - win_lo].fill(hl);
    }
    let mut spans = Vec::new();
    let mut i = 0;
    while i < paint.len() {
        if paint[i] == NONE {
            i += 1;
            continue;
        }
        let (start, hl) = (i, paint[i]);
        while i < paint.len() && paint[i] == hl {
            i += 1;
        }
        spans.push(HighlightSpan {
            start: win_lo + start,
            end: win_lo + i,
            highlight: hl as usize,
        });
    }
    spans
}

/// Highlight the whole source. Returns byte-ranged spans (non-overlapping,
/// sorted). On any parse error, returns no spans — text renders plain.
pub fn highlight_source(config: &HighlightConfiguration, source: &str) -> Vec<HighlightSpan> {
    let mut highlighter = Highlighter::new();
    let Ok(events) = highlighter.highlight(config, source.as_bytes(), None, |_| None) else {
        return Vec::new();
    };
    let mut spans = Vec::new();
    let mut stack: Vec<usize> = Vec::new();
    for event in events.flatten() {
        match event {
            HighlightEvent::HighlightStart(h) => stack.push(h.0),
            HighlightEvent::HighlightEnd => {
                stack.pop();
            }
            HighlightEvent::Source { start, end } => {
                if let Some(&highlight) = stack.last() {
                    spans.push(HighlightSpan {
                        start,
                        end,
                        highlight,
                    });
                }
            }
        }
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// The incremental highlighter's query-cursor extraction must agree
    /// with the tree-sitter-highlight event stream (the path markdown and
    /// diffs still use), or colors would shift after the first keystroke.
    #[test]
    fn incremental_matches_event_stream_extraction() {
        let src = include_str!("../spell.rs");
        let reference = highlight_source(&config_for_lang("rust").unwrap(), src);
        let mut inc = FileHighlighter::new(config_for_lang("rust").unwrap()).unwrap();
        // the extraction merges adjacent same-highlight runs (the event
        // stream splits e.g. consecutive comment lines), so compare the
        // merged forms — per-byte styling must be identical
        fn merged(spans: &[HighlightSpan]) -> Vec<HighlightSpan> {
            let mut out: Vec<HighlightSpan> = Vec::new();
            for &s in spans {
                match out.last_mut() {
                    Some(p) if p.end == s.start && p.highlight == s.highlight => p.end = s.end,
                    _ => out.push(s),
                }
            }
            out
        }
        assert_eq!(merged(&inc.highlight(src.to_string())), merged(&reference));
    }

    /// Spans after a chain of incremental edits must equal a from-scratch
    /// parse of the final text — the Tree::edit bookkeeping is only right
    /// if these can never diverge.
    #[test]
    fn incremental_edits_equal_fresh_parse() {
        let mut inc = FileHighlighter::new(config_for_lang("rust").unwrap()).unwrap();
        let v0 = "fn main() { let x = 1; }\n".to_string();
        let v1 = "fn main() { let x = 1; println!(\"hé\"); }\n".to_string(); // insert (multibyte)
        let v2 = "fn main() { println!(\"hé\"); }\n".to_string(); // delete
        let v3 = "fn máin() { println!(\"hi\"); }\n// trailing comment\n".to_string(); // replace
        for v in [&v0, &v1, &v2, &v3] {
            let incremental = inc.highlight(v.clone());
            let mut fresh = FileHighlighter::new(config_for_lang("rust").unwrap()).unwrap();
            assert_eq!(incremental, fresh.highlight(v.clone()), "diverged at {v:?}");
        }
    }

    /// Windowed extraction must equal the full extraction clipped to the
    /// window — otherwise scrolling would change colors.
    #[test]
    fn windowed_extraction_matches_full() {
        let src = include_str!("../spell.rs");
        let mut inc = FileHighlighter::new(config_for_lang("rust").unwrap()).unwrap();
        let full = inc.highlight(src.to_string());
        // a window on line boundaries somewhere in the middle of the file
        let nth_line = |n: usize| {
            src.split_inclusive('\n').take(n).map(str::len).sum::<usize>()
        };
        let (lo, hi) = (nth_line(40), nth_line(120));
        let windowed = inc.window_only(lo..hi);
        let clipped: Vec<HighlightSpan> = full
            .iter()
            .filter_map(|s| {
                let (cs, ce) = (s.start.max(lo), s.end.min(hi));
                (cs < ce).then_some(HighlightSpan { start: cs, end: ce, highlight: s.highlight })
            })
            .collect();
        assert_eq!(windowed, clipped);
    }

    #[test]
    fn rust_source_gets_keyword_and_string_highlights() {
        let config = config_for(Path::new("main.rs")).unwrap();
        let src = "fn main() { let s = \"hello\"; }\n";
        let spans = highlight_source(&config, src);
        assert!(!spans.is_empty());
        let text_of = |span: &HighlightSpan| &src[span.start..span.end];
        let named = |name: &str| {
            spans
                .iter()
                .find(|s| HIGHLIGHT_NAMES[s.highlight] == name)
                .map(text_of)
        };
        assert_eq!(named("keyword"), Some("fn"));
        assert!(named("string").is_some());
    }

    #[test]
    fn unknown_extension_is_plain() {
        assert!(config_for(Path::new("notes.xyz")).is_none());
        assert!(config_for(Path::new("no_extension")).is_none());
    }

    #[test]
    fn spans_are_sorted_and_in_bounds() {
        let config = config_for(Path::new("lib.rs")).unwrap();
        let src = "pub struct Point { x: f32, y: f32 }\nimpl Point { fn len(&self) -> f32 { (self.x * self.x + self.y * self.y).sqrt() } }\n";
        let spans = highlight_source(&config, src);
        for pair in spans.windows(2) {
            assert!(pair[0].start <= pair[1].start);
        }
        for span in &spans {
            assert!(span.end <= src.len());
            assert!(span.start < span.end);
        }
    }

    #[test]
    fn all_languages_have_working_configs() {
        for file in [
            "a.rs", "a.json", "a.js", "a.ts", "a.py", "a.sh", "a.toml", "a.md", "a.yaml", "a.yml",
        ] {
            let config = config_for(Path::new(file));
            assert!(config.is_some(), "config for {file}");
        }
    }

    #[test]
    fn typescript_specific_keywords_are_highlighted() {
        let config = config_for(Path::new("app.ts")).unwrap();
        let src = "interface Point { x: number }\ntype Alias = Point;\nenum E { A }\n";
        let spans = highlight_source(&config, src);
        let text_of = |s: &HighlightSpan| &src[s.start..s.end];
        let keywords: Vec<&str> = spans
            .iter()
            .filter(|s| HIGHLIGHT_NAMES[s.highlight] == "keyword")
            .map(text_of)
            .collect();
        assert!(keywords.contains(&"interface"), "keywords: {keywords:?}");
        assert!(keywords.contains(&"type"), "keywords: {keywords:?}");
        assert!(keywords.contains(&"enum"), "keywords: {keywords:?}");
        // types get the type color
        let types: Vec<&str> = spans
            .iter()
            .filter(|s| HIGHLIGHT_NAMES[s.highlight].starts_with("type"))
            .map(text_of)
            .collect();
        assert!(types.contains(&"Point"), "types: {types:?}");
    }

    #[test]
    fn tsx_parses_jsx_syntax() {
        let config = config_for(Path::new("app.tsx")).unwrap();
        let src = "const x = (p: {a: string}) => <div>{p.a}</div>;\n";
        assert!(!highlight_source(&config, src).is_empty());
    }

    #[test]
    fn protobuf_files_are_highlighted() {
        let config = config_for(Path::new("api.proto")).unwrap();
        let src = "syntax = \"proto3\";\nmessage User {\n  string name = 1;\n}\n";
        let spans = highlight_source(&config, src);
        assert!(!spans.is_empty());
        let text_of = |s: &HighlightSpan| &src[s.start..s.end];
        let keywords: Vec<&str> = spans
            .iter()
            .filter(|s| HIGHLIGHT_NAMES[s.highlight] == "keyword")
            .map(text_of)
            .collect();
        assert!(keywords.contains(&"message"), "keywords: {keywords:?}");
        assert_eq!(language_name(Path::new("api.proto")), "protobuf");
    }

    #[test]
    fn dockerfile_is_detected_by_filename_and_highlighted() {
        for file in ["Dockerfile", "dockerfile", "Dockerfile.prod", "Containerfile", "x.dockerfile"] {
            assert!(config_for(Path::new(file)).is_some(), "config for {file}");
        }
        assert_eq!(language_name(Path::new("Dockerfile")), "dockerfile");
        let config = config_for(Path::new("Dockerfile")).unwrap();
        let src = "FROM alpine:3.20\nRUN apk add curl\n# comment\n";
        let spans = highlight_source(&config, src);
        assert!(!spans.is_empty());
        let text_of = |s: &HighlightSpan| &src[s.start..s.end];
        assert!(
            spans.iter().any(|s| text_of(s) == "FROM"),
            "FROM captured: {:?}",
            spans.iter().map(text_of).collect::<Vec<_>>()
        );
    }

    #[test]
    fn html_tags_and_attributes_are_highlighted() {
        let config = config_for(Path::new("index.html")).unwrap();
        let src = "<!DOCTYPE html>\n<div class=\"box\">hi</div>\n";
        let spans = highlight_source(&config, src);
        assert!(!spans.is_empty());
        let named = |name: &str| {
            spans.iter().any(|s| HIGHLIGHT_NAMES[s.highlight] == name)
        };
        assert!(named("tag"), "tags highlighted");
        assert!(named("attribute"), "attributes highlighted");
    }

    #[test]
    fn xml_and_svg_share_the_xml_grammar() {
        for file in [
            "config.xml",
            "logo.svg",
            "sheet.xsl",
            "App.xaml",
            "vibin.csproj",
            "route.gpx",
            "place.kml",
            "cfg.mobileconfig",
            "app.entitlements",
            "feed.atom",
            "form.ui",
        ] {
            let config = config_for(Path::new(file)).unwrap();
            let src = "<svg width=\"10\"><rect/></svg>\n";
            let spans = highlight_source(&config, src);
            assert!(!spans.is_empty(), "highlighted {file}");
            assert!(
                spans.iter().any(|s| HIGHLIGHT_NAMES[s.highlight] == "tag"),
                "tag highlighted in {file}"
            );
        }
    }

    #[test]
    fn newly_added_languages_load_and_highlight() {
        // (file, snippet, a keyword expected to be captured)
        let cases: &[(&str, &str, &str)] = &[
            ("main.c", "int main(void) { return 0; }\n", "int"),
            ("h.h", "#define X 1\nstruct P { int x; };\n", "struct"),
            ("a.cpp", "class Foo { public: int bar() { return 0; } };\n", "class"),
            ("m.hpp", "template<typename T> struct Vec { T x; };\n", "template"),
            ("srv.go", "package main\nfunc main() {}\n", "func"),
            ("s.css", "body { color: red; margin: 0; }\n", "color"),
            ("i.php", "<?php\nfunction f() { return 1; }\n", "function"),
            ("q.sql", "-- note\nSELECT id FROM users WHERE id = 1;\n", "SELECT"),
            ("v.m", "@interface Foo\n- (void)bar;\n@end\n", "@end"),
            ("a.swift", "func greet() -> String { return \"hi\" }\n", "func"),
            ("A.java", "public class A { void f() {} }\n", "class"),
            ("m.kt", "fun main() { val x = 1 }\n", "fun"),
            ("s.rb", "def hello\n  puts 'hi'\nend\n", "def"),
            ("z.zig", "pub fn main() void {}\n", "fn"),
            ("o.odin", "main :: proc() {}\n", "proc"),
            ("P.cs", "public class P { void M() {} }\n", "class"),
        ];
        for (file, src, keyword) in cases {
            let config = config_for(Path::new(file))
                .unwrap_or_else(|| panic!("no highlight config for {file}"));
            let spans = highlight_source(&config, src);
            assert!(!spans.is_empty(), "no spans for {file}");
            let hit = spans.iter().any(|s| &&src[s.start..s.end] == keyword);
            assert!(hit, "{file}: expected {keyword:?} to be captured");
        }
    }

    #[test]
    fn new_language_names() {
        assert_eq!(language_name(Path::new("m.c")), "c");
        assert_eq!(language_name(Path::new("a.cpp")), "cpp");
        assert_eq!(language_name(Path::new("m.hpp")), "cpp");
        assert_eq!(language_name(Path::new("srv.go")), "go");
        assert_eq!(language_name(Path::new("s.css")), "css");
        assert_eq!(language_name(Path::new("i.php")), "php");
        assert_eq!(language_name(Path::new("q.sql")), "sql");
        assert_eq!(language_name(Path::new("v.m")), "objc");
        assert_eq!(language_name(Path::new("a.swift")), "swift");
        assert_eq!(language_name(Path::new("A.java")), "java");
        assert_eq!(language_name(Path::new("m.kt")), "kotlin");
        assert_eq!(language_name(Path::new("s.rb")), "ruby");
        assert_eq!(language_name(Path::new("z.zig")), "zig");
        assert_eq!(language_name(Path::new("o.odin")), "odin");
        assert_eq!(language_name(Path::new("P.cs")), "c#");
    }

    #[test]
    fn sql_line_comments_are_highlighted() {
        // regression: the grammar tags comments `(comment) @comment @spell`,
        // and the trailing @spell used to shadow @comment (they rendered plain)
        let config = config_for(Path::new("q.sql")).unwrap();
        let src = "-- a line comment\nSELECT 1;\n/* block */\n";
        let spans = highlight_source(&config, src);
        let comments: Vec<&str> = spans
            .iter()
            .filter(|s| HIGHLIGHT_NAMES[s.highlight] == "comment")
            .map(|s| &src[s.start..s.end])
            .collect();
        assert!(comments.iter().any(|c| c.contains("line comment")), "{comments:?}");
    }

    #[test]
    fn strip_pseudo_captures_removes_trailing_spell() {
        let q = strip_pseudo_captures("(comment) @comment @spell\n(str) @string");
        assert_eq!(q, "(comment) @comment\n(str) @string");
        // @spell.error must strip cleanly, not leave a stray ".error"
        assert_eq!(strip_pseudo_captures("(c) @comment @spell.error"), "(c) @comment");
        // @nospell isn't clobbered by the @spell rule
        assert_eq!(strip_pseudo_captures("(u) @string @nospell"), "(u) @string");
        // untouched when there's nothing to strip
        assert!(matches!(
            strip_pseudo_captures("(x) @keyword"),
            std::borrow::Cow::Borrowed(_)
        ));
    }

    #[test]
    fn ini_and_conf_files_are_highlighted() {
        let config = config_for(Path::new("app.ini")).unwrap();
        let src = "; comment\n[server]\nhost = localhost\nport = 8080\n";
        let spans = highlight_source(&config, src);
        assert!(!spans.is_empty());
        let has = |name: &str| spans.iter().any(|s| HIGHLIGHT_NAMES[s.highlight] == name);
        assert!(has("property"), "keys highlighted");
        assert!(has("comment"), "comments highlighted");
        // extensions and INI-format dotfiles all route to the ini grammar
        for file in ["nginx.conf", "my.cnf", "app.service", "shortcut.desktop", ".gitconfig",
            ".editorconfig", "tox.ini", "setup.cfg"] {
            assert_eq!(language_name(Path::new(file)), "ini", "{file}");
            assert!(config_for(Path::new(file)).is_some(), "config for {file}");
        }
    }

    #[test]
    fn go_mod_is_highlighted_by_filename() {
        for file in ["go.mod", "go.work"] {
            let config = config_for(Path::new(file))
                .unwrap_or_else(|| panic!("no config for {file}"));
            let src = "module example.com/x\n\ngo 1.22\n\nrequire foo v1.2.3\n";
            let spans = highlight_source(&config, src);
            assert!(!spans.is_empty(), "spans for {file}");
            let kw = spans.iter().any(|s| {
                HIGHLIGHT_NAMES[s.highlight] == "keyword" && &src[s.start..s.end] == "module"
            });
            assert!(kw, "{file}: 'module' captured as keyword");
        }
        assert_eq!(language_name(Path::new("go.mod")), "gomod");
    }

    #[test]
    fn lock_and_manifest_files_route_to_their_real_grammar() {
        // (filename, expected language)
        let cases = [
            ("Cargo.lock", "toml"),
            ("poetry.lock", "toml"),
            ("flake.lock", "json"),
            ("composer.lock", "json"),
            ("Podfile.lock", "yaml"),
            ("Gemfile", "ruby"),
            ("Rakefile", "ruby"),
            ("Podfile", "ruby"),
            ("Fastfile", "ruby"),
        ];
        for (file, lang) in cases {
            assert_eq!(language_name(Path::new(file)), lang, "{file}");
            assert!(config_for(Path::new(file)).is_some(), "config for {file}");
        }
    }

    #[test]
    fn language_names() {
        assert_eq!(language_name(Path::new("x.rs")), "rust");
        assert_eq!(language_name(Path::new("x.ts")), "typescript");
        assert_eq!(language_name(Path::new("page.html")), "html");
        assert_eq!(language_name(Path::new("logo.svg")), "xml");
        assert_eq!(language_name(Path::new("data.xml")), "xml");
        assert_eq!(language_name(Path::new("App.xaml")), "xml");
        assert_eq!(language_name(Path::new("route.gpx")), "xml");
        // Apple String Catalog is JSON, not XML
        assert_eq!(language_name(Path::new("Localizable.xcstrings")), "json");
        assert_eq!(language_name(Path::new("x")), "text");
    }
}
