//! Layered settings, merged with `figment`: built-in defaults, then the
//! global XDG config (`~/.config/vibin/config.toml`), then the nearest
//! repository `.vibin/config.toml` walking up from the workspace — later
//! layers override earlier ones, so a project's `.vibin` wins over the
//! user's global config. Settings persist via [`Config::save_global`].

use figment::providers::{Format, Serialized, Toml};
use figment::Figment;
use serde::{Deserialize, Serialize};
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
}

impl Default for Config {
    fn default() -> Self {
        Self {
            show_hidden: false,
            spell_check: true,
            mark_unicode: true,
            mouse_scroll_multiplier: None,
        }
    }
}

impl Config {
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
    workdir
        .ancestors()
        .map(|d| d.join(".vibin").join("config.toml"))
        .find(|p| p.exists())
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
        let path =
            global_path().ok_or_else(|| Error::new(ErrorKind::NotFound, "no HOME / XDG_CONFIG_HOME"))?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let body = toml::to_string_pretty(self)
            .map_err(|e| Error::new(ErrorKind::InvalidData, e))?;
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
