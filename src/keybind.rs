//! Named actions and configurable keybindings: every chord maps to an
//! action with a stable string name (`new_session`, `goto_session:3`),
//! defaults can be rebound from config (`keybinds = ["ctrl+a>d=diff_all"]`),
//! and `vibin +list-keybinds` dumps the live table.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::Shell;

/// Everything a key chord can do, by name. `name:arg` for parameterized
/// actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Launch the configured ACP agent in the agents shell.
    StartAgent,
    /// Switch to the next / previous running agent.
    NextAgent,
    PrevAgent,
    /// Switch to the Nth running agent (0-based).
    GotoAgent(usize),
    /// Close the active agent (kills its subprocess).
    CloseAgent,
    GotoShell(Shell),
    FocusEditor,
    /// Switch to the agents shell.
    FocusAgent,
    DiffAll,
    Refresh,
    ScrollUp,
    ScrollDown,
    TogglePalette,
    Help,
    Quit,
    // editor-targeted actions: no default app-level triggers (the editor
    // handles its own chords when focused; binding e.g. ctrl+c globally
    // would steal it from the editor) — bindable and used by the
    // right-click context menu
    Copy,
    Cut,
    Paste,
    SelectAll,
    Undo,
    Redo,
    GotoDefinition,
    HoverDocs,
    Format,
}

/// The ghostty-style `name` or `name:arg` form.
impl std::fmt::Display for Action {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Action::StartAgent => write!(f, "start_agent"),
            Action::NextAgent => write!(f, "next_agent"),
            Action::PrevAgent => write!(f, "prev_agent"),
            Action::GotoAgent(n) => write!(f, "goto_agent:{}", n + 1),
            Action::CloseAgent => write!(f, "close_agent"),
            Action::GotoShell(Shell::Agents) => write!(f, "goto_shell:agents"),
            Action::GotoShell(Shell::Git) => write!(f, "goto_shell:git"),
            Action::GotoShell(Shell::Code) => write!(f, "goto_shell:code"),
            Action::FocusEditor => write!(f, "focus_editor"),
            Action::FocusAgent => write!(f, "focus_agent"),
            Action::DiffAll => write!(f, "diff_all"),
            Action::Refresh => write!(f, "refresh"),
            Action::ScrollUp => write!(f, "scroll_up"),
            Action::ScrollDown => write!(f, "scroll_down"),
            Action::TogglePalette => write!(f, "toggle_palette"),
            Action::Help => write!(f, "help"),
            Action::Quit => write!(f, "quit"),
            Action::Copy => write!(f, "copy"),
            Action::Cut => write!(f, "cut"),
            Action::Paste => write!(f, "paste"),
            Action::SelectAll => write!(f, "select_all"),
            Action::Undo => write!(f, "undo"),
            Action::Redo => write!(f, "redo"),
            Action::GotoDefinition => write!(f, "goto_definition"),
            Action::HoverDocs => write!(f, "hover_docs"),
            Action::Format => write!(f, "format"),
        }
    }
}

impl std::str::FromStr for Action {
    type Err = ();

    fn from_str(s: &str) -> Result<Action, ()> {
        let (name, arg) = match s.split_once(':') {
            Some((n, a)) => (n, Some(a)),
            None => (s, None),
        };
        Ok(match (name, arg) {
            ("start_agent", None) => Action::StartAgent,
            ("next_agent", None) => Action::NextAgent,
            ("prev_agent", None) => Action::PrevAgent,
            ("goto_agent", Some(n)) => {
                let n: usize = n.parse().map_err(|_| ())?;
                Action::GotoAgent(n.checked_sub(1).ok_or(())?)
            }
            ("close_agent", None) => Action::CloseAgent,
            ("goto_shell", Some("agents")) => Action::GotoShell(Shell::Agents),
            ("goto_shell", Some("git")) => Action::GotoShell(Shell::Git),
            ("goto_shell", Some("code")) => Action::GotoShell(Shell::Code),
            ("focus_editor", None) => Action::FocusEditor,
            ("focus_agent", None) => Action::FocusAgent,
            ("diff_all", None) => Action::DiffAll,
            ("refresh", None) => Action::Refresh,
            ("scroll_up", None) => Action::ScrollUp,
            ("scroll_down", None) => Action::ScrollDown,
            ("toggle_palette", None) => Action::TogglePalette,
            ("help", None) => Action::Help,
            ("quit", None) => Action::Quit,
            ("copy", None) => Action::Copy,
            ("cut", None) => Action::Cut,
            ("paste", None) => Action::Paste,
            ("select_all", None) => Action::SelectAll,
            ("undo", None) => Action::Undo,
            ("redo", None) => Action::Redo,
            ("goto_definition", None) => Action::GotoDefinition,
            ("hover_docs", None) => Action::HoverDocs,
            ("format", None) => Action::Format,
            _ => return Err(()),
        })
    }
}

impl Action {
    /// Every action, for `+list-actions`.
    pub fn all_names() -> Vec<&'static str> {
        vec![
            "start_agent",
            "next_agent",
            "prev_agent",
            "goto_agent:N",
            "close_agent",
            "goto_shell:agents|git|code",
            "focus_editor",
            "focus_agent",
            "diff_all",
            "refresh",
            "scroll_up",
            "scroll_down",
            "toggle_palette",
            "help",
            "quit",
            "copy",
            "cut",
            "paste",
            "select_all",
            "undo",
            "redo",
            "goto_definition",
            "hover_docs",
            "format",
        ]
    }
}

/// A key chord: optional leader prefix (Ctrl+A), modifiers, key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Trigger {
    pub leader: bool,
    pub mods: KeyModifiers,
    pub code: KeyCode,
}

impl std::str::FromStr for Trigger {
    type Err = ();

    /// Parse `ctrl+k`, `f1`, `ctrl+a>c`, `leader>shift+r`, `leader>tab`.
    /// A bare uppercase letter means shift (`R` == `shift+r`).
    fn from_str(s: &str) -> Result<Trigger, ()> {
        let s = s.trim();
        let (leader, rest) = match s.split_once('>') {
            Some((prefix, rest))
                if prefix.eq_ignore_ascii_case("leader")
                    || prefix.eq_ignore_ascii_case("ctrl+a") =>
            {
                (true, rest)
            }
            Some(_) => return Err(()),
            None => (false, s),
        };
        let mut mods = KeyModifiers::NONE;
        let mut code = None;
        for part in rest.split('+') {
            match part.trim().to_lowercase().as_str() {
                "ctrl" | "control" => mods |= KeyModifiers::CONTROL,
                "alt" | "option" => mods |= KeyModifiers::ALT,
                "shift" => mods |= KeyModifiers::SHIFT,
                "super" | "cmd" => mods |= KeyModifiers::SUPER,
                key => {
                    // single chars keep their case (uppercase implies shift)
                    code = Some(if part.trim().chars().count() == 1 {
                        KeyCode::Char(part.trim().chars().next().ok_or(())?)
                    } else {
                        parse_key(key).ok_or(())?
                    });
                }
            }
        }
        if let Some(KeyCode::Char(c)) = code
            && c.is_ascii_uppercase()
        {
            mods |= KeyModifiers::SHIFT;
        }
        let mut code = code.ok_or(())?;
        // shift+letter normalizes to the uppercase char, as crossterm sends
        if mods.contains(KeyModifiers::SHIFT)
            && let KeyCode::Char(c) = code
            && c.is_ascii_lowercase()
        {
            code = KeyCode::Char(c.to_ascii_uppercase());
        }
        Ok(Trigger { leader, mods, code })
    }
}

/// Human/config form: `ctrl+a>c`, `f1`, `ctrl+k`.
impl std::fmt::Display for Trigger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.leader {
            write!(f, "ctrl+a>")?;
        }
        if self.mods.contains(KeyModifiers::CONTROL) {
            write!(f, "ctrl+")?;
        }
        if self.mods.contains(KeyModifiers::ALT) {
            write!(f, "alt+")?;
        }
        if self.mods.contains(KeyModifiers::SUPER) {
            write!(f, "super+")?;
        }
        if self.mods.contains(KeyModifiers::SHIFT)
            && !matches!(self.code, KeyCode::Char(c) if c.is_ascii_uppercase())
        {
            write!(f, "shift+")?;
        }
        write!(f, "{}", key_label(self.code))
    }
}

impl Trigger {
    fn matches(&self, leader: bool, key: &KeyEvent) -> bool {
        if self.leader != leader || self.code != key.code {
            return false;
        }
        // uppercase chars carry their own shift; ignore SHIFT in the mask
        let mask = !KeyModifiers::SHIFT;
        (self.mods & mask) == (key.modifiers & mask)
    }
}

fn parse_key(s: &str) -> Option<KeyCode> {
    Some(match s {
        "tab" => KeyCode::Tab,
        "enter" | "return" => KeyCode::Enter,
        "esc" | "escape" => KeyCode::Esc,
        "space" => KeyCode::Char(' '),
        "up" | "arrow_up" => KeyCode::Up,
        "down" | "arrow_down" => KeyCode::Down,
        "left" | "arrow_left" => KeyCode::Left,
        "right" | "arrow_right" => KeyCode::Right,
        "pageup" | "page_up" => KeyCode::PageUp,
        "pagedown" | "page_down" => KeyCode::PageDown,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        s if s.len() == 1 => KeyCode::Char(s.chars().next()?),
        s if s.starts_with('f') => KeyCode::F(s[1..].parse().ok()?),
        _ => return None,
    })
}

fn key_label(code: KeyCode) -> String {
    match code {
        KeyCode::Char(' ') => "space".into(),
        KeyCode::Char(c) => c.to_string(),
        KeyCode::F(n) => format!("f{n}"),
        KeyCode::Tab => "tab".into(),
        KeyCode::Enter => "enter".into(),
        KeyCode::Esc => "esc".into(),
        KeyCode::Up => "up".into(),
        KeyCode::Down => "down".into(),
        KeyCode::Left => "left".into(),
        KeyCode::Right => "right".into(),
        KeyCode::PageUp => "page_up".into(),
        KeyCode::PageDown => "page_down".into(),
        KeyCode::Home => "home".into(),
        KeyCode::End => "end".into(),
        other => format!("{other:?}").to_lowercase(),
    }
}

/// The binding table: defaults, config overrides, lookup, and listing.
pub struct Keybinds {
    binds: Vec<(Trigger, Action)>,
}

impl Keybinds {
    /// The built-in bindings (mirrors the traditional hardcoded set).
    pub fn defaults() -> Keybinds {
        let mut binds = Vec::new();
        let mut bind = |spec: &str, action: Action| {
            binds.push((spec.parse::<Trigger>().expect(spec), action));
        };
        bind("ctrl+a>c", Action::StartAgent);
        bind("ctrl+a>n", Action::NextAgent);
        bind("ctrl+a>tab", Action::NextAgent);
        bind("ctrl+a>p", Action::PrevAgent);
        bind("ctrl+a>x", Action::CloseAgent);
        for n in 1..=9 {
            bind(&format!("ctrl+a>{n}"), Action::GotoAgent(n - 1));
        }
        bind("ctrl+a>h", Action::FocusAgent);
        bind("ctrl+a>g", Action::GotoShell(Shell::Git));
        bind("ctrl+a>f", Action::GotoShell(Shell::Code));
        bind("ctrl+a>e", Action::FocusEditor);
        bind("ctrl+a>d", Action::DiffAll);
        bind("ctrl+a>u", Action::Refresh);
        bind("ctrl+a>k", Action::ScrollUp);
        bind("ctrl+a>up", Action::ScrollUp);
        bind("ctrl+a>page_up", Action::ScrollUp);
        bind("ctrl+a>j", Action::ScrollDown);
        bind("ctrl+a>down", Action::ScrollDown);
        bind("ctrl+a>page_down", Action::ScrollDown);
        bind("ctrl+a>?", Action::Help);
        bind("ctrl+a>q", Action::Quit);
        bind("ctrl+k", Action::TogglePalette);
        bind("f1", Action::GotoShell(Shell::Agents));
        bind("f2", Action::GotoShell(Shell::Git));
        bind("f3", Action::GotoShell(Shell::Code));
        Keybinds { binds }
    }

    /// Defaults plus config `keybinds = ["trigger=action", ...]` entries.
    /// Invalid entries are reported, not fatal.
    pub fn from_config(specs: &[String]) -> (Keybinds, Vec<String>) {
        let mut kb = Keybinds::defaults();
        let mut errors = Vec::new();
        for spec in specs {
            if let Err(e) = kb.apply(spec) {
                errors.push(format!("keybind {spec:?}: {e}"));
            }
        }
        (kb, errors)
    }

    /// Apply one `trigger=action` (or `trigger=unbind`) spec.
    pub fn apply(&mut self, spec: &str) -> Result<(), String> {
        let (trigger, action) = spec.split_once('=').ok_or("expected trigger=action")?;
        let trigger: Trigger = trigger.parse().map_err(|_| "bad trigger")?;
        self.binds.retain(|(t, _)| *t != trigger);
        if action.trim() == "unbind" {
            return Ok(());
        }
        let action: Action = action.trim().parse().map_err(|_| "unknown action")?;
        self.binds.push((trigger, action));
        Ok(())
    }

    pub fn lookup(&self, leader: bool, key: &KeyEvent) -> Option<Action> {
        self.binds.iter().find(|(t, _)| t.matches(leader, key)).map(|&(_, a)| a)
    }

    /// (trigger label, action name) rows, sorted for display.
    pub fn list(&self) -> Vec<(String, String)> {
        let mut rows: Vec<(String, String)> =
            self.binds.iter().map(|(t, a)| (t.to_string(), a.to_string())).collect();
        rows.sort();
        rows
    }
}

/// `vibin +list-keybinds`: dump the live table, ghostty-style — modifiers
/// and chord separators dimmed, action parameters accented.
pub fn print_keybinds(kb: &Keybinds) {
    for (trigger, action) in kb.list() {
        let colored_trigger = trigger
            .replace('>', "\x1b[2m > \x1b[0m")
            .replace("ctrl+", "\x1b[35mctrl\x1b[0m + ")
            .replace("alt+", "\x1b[33malt\x1b[0m + ")
            .replace("shift+", "\x1b[34mshift\x1b[0m + ")
            .replace("super+", "\x1b[31msuper\x1b[0m + ");
        let visible: usize = strip_ansi_len(&colored_trigger);
        let pad = " ".repeat(28usize.saturating_sub(visible));
        let colored_action = match action.split_once(':') {
            Some((name, arg)) => format!("{name}:\x1b[33m{arg}\x1b[0m"),
            None => action,
        };
        println!("{colored_trigger}{pad}{colored_action}");
    }
}

fn strip_ansi_len(s: &str) -> usize {
    let mut len = 0;
    let mut in_esc = false;
    for c in s.chars() {
        match (in_esc, c) {
            (false, '\x1b') => in_esc = true,
            (false, _) => len += 1,
            (true, 'm') => in_esc = false,
            (true, _) => {}
        }
    }
    len
}

pub fn print_actions() {
    for name in Action::all_names() {
        match name.split_once(':') {
            Some((n, arg)) => println!("{n}:\x1b[33m{arg}\x1b[0m"),
            None => println!("{name}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn triggers_parse_and_roundtrip() {
        for spec in ["ctrl+k", "f1", "ctrl+a>c", "ctrl+a>shift+r", "ctrl+a>page_up", "alt+enter"] {
            let t: Trigger = spec.parse().expect(spec);
            assert_eq!(t.to_string().parse::<Trigger>(), Ok(t), "roundtrip {spec}");
        }
        // leader> is an alias for ctrl+a>
        assert_eq!("leader>c".parse::<Trigger>(), "ctrl+a>c".parse::<Trigger>());
        // shift+letter normalizes to the uppercase char
        assert_eq!("ctrl+a>shift+r".parse::<Trigger>().unwrap().code, KeyCode::Char('R'));
        assert!("bogus+x".parse::<Trigger>().is_err());
    }

    #[test]
    fn actions_parse_and_roundtrip() {
        for name in ["start_agent", "goto_shell:git", "toggle_palette", "quit"] {
            let a: Action = name.parse().expect(name);
            assert_eq!(a.to_string(), name);
        }
        assert!("nonsense".parse::<Action>().is_err());
    }

    #[test]
    fn defaults_cover_the_traditional_chords() {
        let kb = Keybinds::defaults();
        let key = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        assert_eq!(kb.lookup(true, &key('c')), Some(Action::StartAgent));
        assert_eq!(kb.lookup(true, &key('q')), Some(Action::Quit));
        assert_eq!(
            kb.lookup(false, &KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE)),
            Some(Action::GotoShell(Shell::Git))
        );
        assert_eq!(
            kb.lookup(false, &KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL)),
            Some(Action::TogglePalette)
        );
        assert_eq!(kb.lookup(false, &key('c')), None, "leader chords need the leader");
    }

    #[test]
    fn config_rebinds_and_unbinds() {
        let (kb, errors) = Keybinds::from_config(&[
            "f4=diff_all".into(),
            "ctrl+a>d=unbind".into(),
            "ctrl+a>w=focus_agent".into(),
            "broken".into(),
            "f5=made_up_action".into(),
        ]);
        assert_eq!(errors.len(), 2, "{errors:?}");
        assert_eq!(
            kb.lookup(false, &KeyEvent::new(KeyCode::F(4), KeyModifiers::NONE)),
            Some(Action::DiffAll)
        );
        let d = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE);
        assert_eq!(kb.lookup(true, &d), None, "unbound");
        let w = KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE);
        assert_eq!(kb.lookup(true, &w), Some(Action::FocusAgent));
    }
}
