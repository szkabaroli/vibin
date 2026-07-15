//! Nerd Font devicons: filetype → Private Use Area glyph + brand color,
//! the same convention nvim-web-devicons uses. Rendering needs a Nerd
//! Font, or a terminal with a built-in symbols fallback (Ghostty ships
//! one); `icons = false` in the config keeps the plain tree glyphs.
//!
//! Brand colors are deliberately fixed RGB rather than theme-derived —
//! a rust file is rust-orange in every editor that speaks this
//! convention, and that recognizability is the point.

/// Closed folder.
pub const FOLDER: &str = "\u{f07b}";
/// Expanded folder.
pub const FOLDER_OPEN: &str = "\u{f07c}";

/// Icon + brand color for a file name. Well-known filenames win over
/// extensions; unknown files get a neutral document glyph.
pub fn icon(name: &str) -> (&'static str, (u8, u8, u8)) {
    let lower = name.to_lowercase();
    // filenames first
    match lower.as_str() {
        "dockerfile" | "containerfile" => return ("\u{f308}", (56, 148, 231)),
        "makefile" | "justfile" => return ("\u{e779}", (110, 120, 130)),
        "license" | "license.md" | "licence" => return ("\u{f0219}", (203, 190, 145)),
        _ => {}
    }
    if lower.starts_with(".git") {
        return ("\u{f1d3}", (241, 80, 47));
    }
    let ext = lower.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    match ext {
        "rs" => ("\u{e7a8}", (222, 165, 132)),
        "toml" => ("\u{e615}", (156, 116, 87)),
        "md" | "markdown" => ("\u{e609}", (66, 153, 221)),
        "json" | "jsonc" => ("\u{e60b}", (203, 203, 65)),
        "js" | "mjs" | "cjs" => ("\u{e74e}", (241, 224, 90)),
        "ts" | "mts" => ("\u{e628}", (49, 120, 198)),
        "jsx" | "tsx" => ("\u{e7ba}", (32, 173, 232)),
        "py" => ("\u{e606}", (255, 213, 79)),
        "go" => ("\u{e627}", (81, 154, 186)),
        "sh" | "bash" | "zsh" => ("\u{e795}", (76, 175, 80)),
        "yaml" | "yml" => ("\u{e615}", (110, 120, 130)),
        "lock" => ("\u{f023}", (215, 173, 114)),
        "html" | "htm" => ("\u{e736}", (228, 79, 57)),
        "css" | "scss" => ("\u{e749}", (66, 165, 245)),
        "c" | "h" => ("\u{e61e}", (89, 155, 220)),
        "cpp" | "cc" | "cxx" | "hpp" => ("\u{e61d}", (243, 75, 125)),
        "java" => ("\u{e738}", (204, 52, 45)),
        "kt" | "kts" => ("\u{e634}", (127, 82, 224)),
        "rb" => ("\u{e791}", (192, 52, 33)),
        "php" => ("\u{e73d}", (167, 139, 250)),
        "swift" => ("\u{e755}", (226, 88, 34)),
        "zig" => ("\u{e6a9}", (247, 164, 29)),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "ico" => ("\u{f1c5}", (110, 159, 206)),
        "pdf" => ("\u{f1c1}", (203, 65, 65)),
        "zip" | "gz" | "tar" | "xz" | "bz2" => ("\u{f1c6}", (175, 180, 43)),
        "txt" => ("\u{f15c}", (170, 176, 190)),
        _ => ("\u{f15b}", (150, 155, 165)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_types_get_brand_icons() {
        assert_eq!(icon("main.rs").0, "\u{e7a8}");
        assert_eq!(icon("Cargo.toml").0, "\u{e615}");
        assert_eq!(icon("Dockerfile").0, "\u{f308}", "filename beats extension");
        assert_eq!(icon(".gitignore").0, "\u{f1d3}");
        assert_eq!(icon("photo.JPG").0, "\u{f1c5}", "case-insensitive");
        // unknown → neutral document, never tofu-prone emoji
        assert_eq!(icon("data.bin").0, "\u{f15b}");
        assert_eq!(icon("README").0, "\u{f15b}");
    }
}
