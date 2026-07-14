//! Layered settings, merged with `figment`: built-in defaults, then the
//! global XDG config (`~/.config/vibin/config.toml`), then the nearest
//! repository `.vibin/config.toml` walking up from the workspace — later
//! layers override earlier ones, so a project's `.vibin` wins over the
//! user's global config. Settings persist via [`Config::save_global`].

use figment::Figment;
use figment::providers::{Format, Serialized, Toml};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Show dotfiles/dotfolders in the file tree (`.git` stays hidden).
    pub show_hidden: bool,
    /// Spell-check comments, strings, and identifiers in the editor.
    pub spell_check: bool,
    /// Underline confusable / highlight invisible Unicode characters.
    pub mark_unicode: bool,
    /// Lines scrolled per mouse-wheel event. Unset -> auto: 1 under Ghostty
    /// (its own `mouse-scroll-multiplier` already scales the event stream,
    /// so multiplying again would compound), the classic 3 elsewhere.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mouse_scroll_multiplier: Option<u16>,
    /// Keybinding overrides: `"trigger=action"` entries layered over the
    /// defaults, e.g. `keybinds = ["f4=diff_all", "ctrl+a>d=unbind"]`.
    /// `vibin +list-actions` names every action.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub keybinds: Vec<String>,
    /// Language servers, in the shape of Neovim's `vim.lsp.config`: each
    /// `[lsp.<name>]` table declares the command, the languages it serves,
    /// and the workspace marker files that start it eagerly at workspace
    /// open. User entries deep-merge over the built-ins, so overriding one
    /// field keeps the rest:
    ///
    /// ```toml
    /// [lsp.rust_analyzer]
    /// cmd = ["rust-analyzer", "--log-file", "/tmp/ra.log"]
    ///
    /// [lsp.clangd]                # a server vibin doesn't ship
    /// cmd = ["clangd"]
    /// filetypes = ["c", "cpp"]
    /// root_markers = ["compile_commands.json"]
    /// ```
    pub lsp: BTreeMap<String, LspServer>,
}

/// One language-server definition (see [`Config::lsp`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct LspServer {
    /// Command and arguments to spawn the server.
    pub cmd: Vec<String>,
    /// Languages this server handles (vibin's language names, as shown in
    /// the editor status bar).
    pub filetypes: Vec<String>,
    /// Files whose presence in the workspace root starts this server at
    /// workspace open, before any file is opened. Empty = lazy only.
    pub root_markers: Vec<String>,
}

fn default_lsp() -> BTreeMap<String, LspServer> {
    fn server(cmd: &[&str], filetypes: &[&str], root_markers: &[&str]) -> LspServer {
        let owned = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect();
        LspServer {
            cmd: owned(cmd),
            filetypes: owned(filetypes),
            root_markers: owned(root_markers),
        }
    }
    BTreeMap::from([
        (
            "rust_analyzer".to_string(),
            server(&["rust-analyzer"], &["rust"], &["Cargo.toml", "rust-project.json"]),
        ),
        (
            "ts_ls".to_string(),
            server(
                &["typescript-language-server", "--stdio"],
                &["typescript", "javascript"],
                &["package.json"],
            ),
        ),
        (
            "pyright".to_string(),
            server(
                &["pyright-langserver", "--stdio"],
                &["python"],
                &["pyproject.toml", "setup.py"],
            ),
        ),
        ("gopls".to_string(), server(&["gopls"], &["go"], &["go.mod", "go.work"])),
        ("bashls".to_string(), server(&["bash-language-server", "start"], &["bash"], &[])),
        ("yamlls".to_string(), server(&["yaml-language-server", "--stdio"], &["yaml"], &[])),
        ("dockerls".to_string(), server(&["docker-langserver", "--stdio"], &["dockerfile"], &[])),
        ("protols".to_string(), server(&["protols"], &["protobuf"], &[])),
    ])
}

impl Default for Config {
    fn default() -> Self {
        Self {
            show_hidden: false,
            spell_check: true,
            mark_unicode: true,
            mouse_scroll_multiplier: None,
            keybinds: Vec::new(),
            lsp: default_lsp(),
        }
    }
}

impl Config {
    /// The server command for a language: the first `[lsp.*]` entry (in
    /// name order) whose filetypes include it. `VIBIN_LSP_CMD` overrides
    /// every language (tests; also handy for one-off custom servers).
    pub fn lsp_command(&self, language: &str) -> Option<Vec<String>> {
        if let Ok(cmd) = std::env::var("VIBIN_LSP_CMD") {
            let parts: Vec<String> = cmd.split_whitespace().map(String::from).collect();
            if !parts.is_empty() {
                return Some(parts);
            }
        }
        self.lsp
            .values()
            .find(|s| !s.cmd.is_empty() && s.filetypes.iter().any(|f| f == language))
            .map(|s| s.cmd.clone())
    }

    /// The language to start eagerly for a workspace: the first `[lsp.*]`
    /// entry (in name order) with a root marker present in `root`.
    pub fn lsp_activation_language(&self, root: &Path) -> Option<String> {
        self.lsp.values().find_map(|s| {
            s.root_markers
                .iter()
                .any(|m| root.join(m).is_file())
                .then(|| s.filetypes.first().cloned())
                .flatten()
        })
    }

    /// Lines per wheel event: the configured multiplier if set, else the
    /// terminal-aware fallback.
    pub fn scroll_step(&self) -> usize {
        self.scroll_step_for(std::env::var("TERM_PROGRAM").ok().as_deref())
    }

    fn scroll_step_for(&self, term_program: Option<&str>) -> usize {
        if let Some(n) = self.mouse_scroll_multiplier {
            return n.max(1) as usize;
        }
        match term_program {
            // Ghostty applies its own mouse-scroll-multiplier before the
            // events reach us -- defer to it rather than stacking a x3
            Some("ghostty") => 1,
            _ => 3,
        }
    }
}

/// The global config path: `$XDG_CONFIG_HOME/vibin/config.toml`, falling
/// back to `~/.config/vibin/config.toml` (the XDG default, honored on macOS
/// too so settings live in one predictable place).
pub fn global_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("vibin").join("config.toml"))
}

/// The nearest `.vibin/config.toml` at or above `workdir`.
fn local_path(workdir: &Path) -> Option<PathBuf> {
    workdir.ancestors().map(|d| d.join(".vibin").join("config.toml")).find(|p| p.exists())
}

impl Config {
    /// Merge defaults ← global ← repo-local (local highest priority).
    pub fn load(workdir: &Path) -> Self {
        Self::load_with(global_path(), local_path(workdir))
    }

    fn load_with(global: Option<PathBuf>, local: Option<PathBuf>) -> Self {
        let mut fig = Figment::from(Serialized::defaults(Config::default()));
        if let Some(g) = global {
            fig = fig.merge(Toml::file(g)); // missing file → no-op
        }
        if let Some(l) = local {
            fig = fig.merge(Toml::file(l));
        }
        // a malformed config shouldn't brick the editor — fall back to defaults
        fig.extract().unwrap_or_default()
    }

    /// Write these settings to the global XDG config, creating the directory.
    pub fn save_global(&self) -> std::io::Result<PathBuf> {
        use std::io::{Error, ErrorKind};
        let path = global_path()
            .ok_or_else(|| Error::new(ErrorKind::NotFound, "no HOME / XDG_CONFIG_HOME"))?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let body =
            toml::to_string_pretty(self).map_err(|e| Error::new(ErrorKind::InvalidData, e))?;
        std::fs::write(&path, body)?;
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn defaults_when_no_files() {
        let cfg = Config::load_with(None, None);
        assert_eq!(cfg, Config::default());
        assert!(!cfg.show_hidden && cfg.spell_check && cfg.mark_unicode);
    }

    #[test]
    fn lsp_command_resolves_by_filetype() {
        let _guard = crate::lsp::ENV_LOCK.lock().unwrap();
        let cfg = Config::default();
        assert_eq!(cfg.lsp_command("rust").unwrap()[0], "rust-analyzer");
        assert_eq!(cfg.lsp_command("javascript").unwrap()[0], "typescript-language-server");
        assert!(cfg.lsp_command("toml").is_none());
    }

    #[test]
    fn lsp_tables_deep_merge_over_builtins() {
        let dir = TempDir::new().unwrap();
        let global = dir.path().join("config.toml");
        fs::write(
            &global,
            r#"
[lsp.rust_analyzer]
cmd = ["ra-custom"]

[lsp.gopls]
cmd = ["gopls"]
filetypes = ["go"]
root_markers = ["go.mod"]
"#,
        )
        .unwrap();
        let cfg = Config::load_with(Some(global), None);
        // overriding one field keeps the built-in rest (vim.lsp.config-style)
        let ra = &cfg.lsp["rust_analyzer"];
        assert_eq!(ra.cmd, vec!["ra-custom"]);
        assert_eq!(ra.filetypes, vec!["rust"]);
        assert_eq!(ra.root_markers, vec!["Cargo.toml", "rust-project.json"]);
        // brand-new servers join the registry
        assert_eq!(cfg.lsp["gopls"].filetypes, vec!["go"]);
        // untouched built-ins survive
        assert!(cfg.lsp.contains_key("pyright"));
    }

    #[test]
    fn local_overrides_global_which_overrides_defaults() {
        let dir = TempDir::new().unwrap();
        let global = dir.path().join("global.toml");
        let local = dir.path().join("local.toml");
        // global turns on hidden files and turns off unicode marking
        fs::write(&global, "show_hidden = true\nmark_unicode = false\n").unwrap();
        // local re-enables unicode marking (and leaves show_hidden from global)
        fs::write(&local, "mark_unicode = true\nspell_check = false\n").unwrap();
        let cfg = Config::load_with(Some(global), Some(local));
        assert!(cfg.show_hidden, "inherited from global");
        assert!(cfg.mark_unicode, "local overrode global");
        assert!(!cfg.spell_check, "set by local");
    }

    #[test]
    fn missing_file_is_ignored_not_an_error() {
        let cfg = Config::load_with(Some("/no/such/global.toml".into()), None);
        assert_eq!(cfg, Config::default());
    }

    #[test]
    fn malformed_config_falls_back_to_defaults() {
        let dir = TempDir::new().unwrap();
        let bad = dir.path().join("bad.toml");
        fs::write(&bad, "show_hidden = = broken\n").unwrap();
        assert_eq!(Config::load_with(Some(bad), None), Config::default());
    }

    #[test]
    fn save_and_reload_round_trips() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("vibin").join("config.toml");
        let cfg = Config { show_hidden: true, spell_check: false, ..Config::default() };
        // write it manually (save_global targets the real XDG path)
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, toml::to_string_pretty(&cfg).unwrap()).unwrap();
        assert_eq!(Config::load_with(Some(path), None), cfg);
    }

    #[test]
    fn scroll_step_prefers_config_then_terminal_fallback() {
        // explicit config wins everywhere (and is clamped to >= 1)
        let cfg = Config { mouse_scroll_multiplier: Some(5), ..Config::default() };
        assert_eq!(cfg.scroll_step_for(Some("ghostty")), 5);
        let cfg = Config { mouse_scroll_multiplier: Some(0), ..Config::default() };
        assert_eq!(cfg.scroll_step_for(None), 1);
        // unset: defer to Ghostty's own mouse-scroll-multiplier...
        let auto = Config::default();
        assert_eq!(auto.scroll_step_for(Some("ghostty")), 1);
        // ...and fall back to the classic 3 lines elsewhere
        assert_eq!(auto.scroll_step_for(Some("iTerm.app")), 3);
        assert_eq!(auto.scroll_step_for(None), 3);
    }

    #[test]
    fn scroll_multiplier_parses_and_survives_save() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "mouse_scroll_multiplier = 2\n").unwrap();
        let cfg = Config::load_with(Some(path.clone()), None);
        assert_eq!(cfg.mouse_scroll_multiplier, Some(2));
        // unset stays unset through a save/reload round trip (None must
        // serialize as an omitted key, not break the TOML writer)
        let unset = Config::default();
        fs::write(&path, toml::to_string_pretty(&unset).unwrap()).unwrap();
        assert_eq!(Config::load_with(Some(path), None), unset);
    }

    #[test]
    fn nearest_local_config_is_found_walking_up() {
        let dir = TempDir::new().unwrap();
        let nested = dir.path().join("a").join("b");
        fs::create_dir_all(&nested).unwrap();
        fs::create_dir_all(dir.path().join(".vibin")).unwrap();
        fs::write(dir.path().join(".vibin").join("config.toml"), "show_hidden = true\n").unwrap();
        assert_eq!(local_path(&nested), Some(dir.path().join(".vibin").join("config.toml")));
    }
}
