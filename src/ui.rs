//! Rendering: layout, sidebar, terminal panes, overlays, status bar.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Tabs};
use ratatui::Frame;
use tui_term::widget::PseudoTerminal;

use crate::app::{App, Focus, Overlay, Screen, SidebarTab};
use crate::diff::{DiffLine, DiffLineKind};
use crate::git::StatusKind;
use crate::projects::display_path;
use crate::session::SessionStatus;

const SIDEBAR_WIDTH: u16 = 34;

/// "VIBIN" in ANSI-shadow block letters, split for two-tone coloring
/// ("VI" dim, "BIN" bright — like opencode's wordmark).
const LOGO: [(&str, &str); 6] = [
    ("██╗   ██╗██╗", "██████╗ ██╗███╗   ██╗"),
    ("██║   ██║██║", "██╔══██╗██║████╗  ██║"),
    ("██║   ██║██║", "██████╔╝██║██╔██╗ ██║"),
    ("╚██╗ ██╔╝██║", "██╔══██╗██║██║╚██╗██║"),
    (" ╚████╔╝ ██║", "██████╔╝██║██║ ╚████║"),
    ("  ╚═══╝  ╚═╝", "╚═════╝ ╚═╝╚═╝  ╚═══╝"),
];
const LOGO_WIDTH: u16 = 33;

pub fn draw(frame: &mut Frame, app: &mut App) {
    if app.screen == Screen::Welcome {
        draw_welcome(frame, app);
        return;
    }
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(1)])
        .split(frame.area());
    let main = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(SIDEBAR_WIDTH), Constraint::Min(10)])
        .split(outer[0]);

    draw_sidebar(frame, app, main[0]);
    draw_terminal_area(frame, app, main[1]);
    draw_status_bar(frame, app, outer[1]);

    match &app.overlay {
        Some(Overlay::Diff(_)) => draw_diff_overlay(frame, app),
        Some(Overlay::Help) => draw_help_overlay(frame),
        Some(Overlay::CommitPrompt(buf)) => {
            draw_prompt(frame, " commit message (Enter commit · Esc cancel) ", buf)
        }
        Some(Overlay::RenamePrompt(buf)) => {
            draw_prompt(frame, " rename session (Enter apply · Esc cancel) ", buf)
        }
        None => {}
    }
}

fn draw_welcome(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let logo_height = LOGO.len() as u16 + 2; // art + version line + gap
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(area.height.saturating_sub(logo_height + 16) / 3),
            Constraint::Length(logo_height),
            Constraint::Length(1),
            Constraint::Min(5),
            Constraint::Length(2),
        ])
        .split(area);

    // logo, centered, two-tone
    let logo_x = area.x + area.width.saturating_sub(LOGO_WIDTH) / 2;
    for (i, (left, right)) in LOGO.iter().enumerate() {
        let line = Line::from(vec![
            Span::styled(*left, Style::default().fg(Color::DarkGray)),
            Span::styled(*right, Style::default().fg(Color::Cyan)),
        ]);
        let rect = Rect::new(logo_x, chunks[1].y + i as u16, LOGO_WIDTH.min(area.width), 1);
        frame.render_widget(Paragraph::new(line), rect);
    }
    let version = format!("v{}", env!("CARGO_PKG_VERSION"));
    let version_rect = Rect::new(
        logo_x,
        chunks[1].y + LOGO.len() as u16 + 1,
        LOGO_WIDTH.min(area.width),
        1,
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            version,
            Style::default().fg(Color::DarkGray),
        )))
        .right_aligned(),
        version_rect,
    );

    // project list, centered column
    let list_width = 64.min(area.width.saturating_sub(4));
    let list_area = Rect::new(
        area.x + area.width.saturating_sub(list_width) / 2,
        chunks[3].y,
        list_width,
        chunks[3].height,
    );
    app.layout.welcome_list = list_area;

    let now = std::time::SystemTime::now();
    let mut items: Vec<ListItem> = Vec::with_capacity(app.welcome.len());
    items.push(ListItem::new(Line::from(vec![
        Span::styled("open ", Style::default().fg(Color::Gray)),
        Span::styled(
            display_path(&app.workdir),
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ),
        Span::styled("  (current directory)", Style::default().fg(Color::DarkGray)),
    ])));
    for project in &app.welcome.projects {
        let chats = format!(
            "{} chat{}",
            project.chat_count,
            if project.chat_count == 1 { "" } else { "s" }
        );
        items.push(ListItem::new(Line::from(vec![
            Span::styled(format!("{:>4}  ", project.age(now)), Style::default().fg(Color::DarkGray)),
            Span::raw(display_path(&project.path)),
            Span::styled(format!("  · {chats}"), Style::default().fg(Color::DarkGray)),
        ])));
    }
    let list = List::new(items)
        .highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD))
        .highlight_symbol("▸ ");
    app.welcome.list.select(Some(app.welcome.selected));
    let mut state = std::mem::take(&mut app.welcome.list);
    frame.render_stateful_widget(list, list_area, &mut state);
    app.welcome.list = state;

    // footer hints
    let hints = Line::from(Span::styled(
        "enter open · j/k move · click select · q quit",
        Style::default().fg(Color::DarkGray),
    ));
    frame.render_widget(
        Paragraph::new(hints).centered(),
        Rect::new(area.x, chunks[4].y, area.width, 1),
    );
}

/// Dashboard glyph and color for a session status.
fn status_indicator(status: SessionStatus) -> (&'static str, Color) {
    match status {
        SessionStatus::Working => ("●", Color::Green),
        SessionStatus::Attention => ("●", Color::Yellow),
        SessionStatus::Idle => ("○", Color::DarkGray),
        SessionStatus::Exited(_) => ("✖", Color::Red),
    }
}

fn border_style(focused: bool) -> Style {
    if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

fn draw_sidebar(frame: &mut Frame, app: &mut App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(area);

    let labels = [
        "Files".to_string(),
        format!("Git ({})", app.git.entries.len()),
        format!("Chats ({})", app.chats.chats.len()),
    ];
    // Record clickable x-ranges: Tabs renders [space][title][space][divider].
    app.layout.sidebar_tabs = chunks[0];
    app.sidebar_tab_hits.clear();
    let mut x = chunks[0].x;
    for (i, label) in labels.iter().enumerate() {
        let width = label.chars().count() as u16 + 2;
        app.sidebar_tab_hits.push((x, x + width, i));
        x += width + 1;
    }
    let tabs = Tabs::new(labels.to_vec())
        .select(app.sidebar_tab.index())
        .highlight_style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD));
    frame.render_widget(tabs, chunks[0]);

    match app.sidebar_tab {
        SidebarTab::Files => draw_file_tree(frame, app, chunks[1]),
        SidebarTab::Git => draw_git_panel(frame, app, chunks[1]),
        SidebarTab::Chats => draw_chats_panel(frame, app, chunks[1]),
    }
}

fn draw_chats_panel(frame: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Sidebar;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style(focused))
        .title(" past chats (Enter resume) ");
    app.layout.sidebar_list = block.inner(area);

    if app.chats.chats.is_empty() {
        let msg = Paragraph::new("no past chats for this directory").block(block);
        frame.render_widget(msg, area);
        return;
    }

    let now = std::time::SystemTime::now();
    let items: Vec<ListItem> = app
        .chats
        .chats
        .iter()
        .map(|chat| {
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{:>3} ", chat.age(now)),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::raw(chat.summary.clone()),
            ]))
        })
        .collect();
    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD));
    app.chats_list.select(Some(app.chats.selected));
    let mut state = std::mem::take(&mut app.chats_list);
    frame.render_stateful_widget(list, area, &mut state);
    app.chats_list = state;
}

fn draw_file_tree(frame: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Sidebar;
    let items: Vec<ListItem> = app
        .tree
        .items
        .iter()
        .map(|item| {
            let indent = "  ".repeat(item.depth);
            let icon = if item.is_dir {
                if item.expanded { "▾ " } else { "▸ " }
            } else {
                "  "
            };
            let style = if item.is_dir {
                Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            ListItem::new(Line::from(vec![
                Span::raw(indent),
                Span::styled(format!("{icon}{}", item.name), style),
            ]))
        })
        .collect();

    let title = app
        .tree
        .root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| app.tree.root.display().to_string());
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style(focused))
        .title(format!(" {title} "));
    app.layout.sidebar_list = block.inner(area);
    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD));

    if app.tree.items.is_empty() {
        app.tree_list.select(None);
    } else {
        app.tree_list.select(Some(app.tree.selected));
    }
    let mut state = std::mem::take(&mut app.tree_list);
    frame.render_stateful_widget(list, area, &mut state);
    app.tree_list = state;
}

fn status_color(kind: StatusKind) -> Color {
    match kind {
        StatusKind::New => Color::Green,
        StatusKind::Modified => Color::Yellow,
        StatusKind::Deleted => Color::Red,
        StatusKind::Renamed => Color::Magenta,
        StatusKind::Typechange => Color::Cyan,
        StatusKind::Conflicted => Color::LightRed,
    }
}

fn draw_git_panel(frame: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Sidebar;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style(focused))
        .title(" changes ");
    app.layout.sidebar_list = block.inner(area);

    if !app.git.is_repo() {
        let msg = Paragraph::new("not a git repository").block(block);
        frame.render_widget(msg, area);
        return;
    }
    if app.git.entries.is_empty() {
        let msg = Paragraph::new("working tree clean").block(block);
        frame.render_widget(msg, area);
        return;
    }

    let items: Vec<ListItem> = app
        .git
        .entries
        .iter()
        .map(|entry| {
            ListItem::new(Line::from(vec![
                Span::styled(
                    entry.code(),
                    Style::default().fg(status_color(entry.kind)).add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
                Span::raw(entry.path.clone()),
            ]))
        })
        .collect();
    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD));
    app.git_list.select(Some(app.git.selected));
    let mut state = std::mem::take(&mut app.git_list);
    frame.render_stateful_widget(list, area, &mut state);
    app.git_list = state;
}

fn draw_terminal_area(frame: &mut Frame, app: &mut App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(area);

    // session tab bar with status dots: ● working · ● attention · ○ idle · ✖ exited
    let statuses = app.sessions.statuses();
    app.statuses = statuses.clone();
    let titles: Vec<Line> = app
        .sessions
        .sessions
        .iter()
        .zip(statuses.iter())
        .enumerate()
        .map(|(i, (s, status))| {
            let (dot, color) = status_indicator(*status);
            Line::from(vec![
                Span::styled(format!(" {dot} "), Style::default().fg(color)),
                Span::raw(format!("{}:{} ", i + 1, s.title)),
            ])
        })
        .collect();
    // Record each tab's clickable x-range: Tabs renders
    // [space][title][space][divider] repeatedly.
    app.layout.session_tabs = chunks[0];
    app.session_tab_hits.clear();
    let mut x = chunks[0].x;
    for (i, title) in titles.iter().enumerate() {
        let width = title.width() as u16 + 2;
        app.session_tab_hits.push((x, x + width, i));
        x += width + 1;
    }
    let tabs = Tabs::new(titles)
        .select(app.sessions.active)
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .divider("|");
    frame.render_widget(tabs, chunks[0]);

    let focused = app.focus == Focus::Terminal;
    let active_exited = matches!(
        statuses.get(app.sessions.active),
        Some(SessionStatus::Exited(_))
    );
    let pane = chunks[1];
    app.layout.terminal_pane = pane;
    let block = Block::default().borders(Borders::ALL).border_style(
        if active_exited {
            Style::default().fg(Color::Red)
        } else {
            border_style(focused)
        },
    );

    let inner = block.inner(pane);
    // Remember pane size for new sessions and keep the active one in sync.
    app.term_size = (inner.height.max(1), inner.width.max(1));
    let scroll_offset = match app.sessions.active_session() {
        Some(session) => {
            session.resize(inner.height.max(1), inner.width.max(1));
            session.scroll_offset
        }
        None => 0,
    };

    let overlay_open = app.overlay.is_some();
    match app.sessions.active_session() {
        Some(session) => {
            let title = if let Some(code) = session.exit_code() {
                format!(
                    " {} — exited ({code}) · Ctrl+A R respawn · Ctrl+A x close ",
                    session.title
                )
            } else if scroll_offset > 0 {
                format!(" {} [scroll:{}] ", session.title, scroll_offset)
            } else {
                format!(" {} ", session.title)
            };
            let parser = session.parser.lock().unwrap();
            let screen = parser.screen();
            // When the pane is focused, place the real (blinking) terminal
            // cursor on the embedded screen's cursor and hide tui-term's
            // painted one; a painted cell can never blink.
            let use_real_cursor =
                focused && !overlay_open && scroll_offset == 0 && !screen.hide_cursor();
            let mut term = PseudoTerminal::new(screen).block(block.title(title));
            if use_real_cursor {
                let mut cursor = tui_term::widget::Cursor::default();
                cursor.hide();
                term = term.cursor(cursor);
            }
            frame.render_widget(term, pane);
            if use_real_cursor {
                let (row, col) = screen.cursor_position();
                let x = (inner.x + col).min(inner.right().saturating_sub(1));
                let y = (inner.y + row).min(inner.bottom().saturating_sub(1));
                frame.set_cursor_position((x, y));
            }
        }
        None => {
            let msg = Paragraph::new(vec![
                Line::from(""),
                Line::from("  no active sessions"),
                Line::from(""),
                Line::from(Span::styled(
                    "  Ctrl+A c  start a new Claude session",
                    Style::default().fg(Color::Cyan),
                )),
                Line::from(Span::styled(
                    "  Ctrl+A ?  help",
                    Style::default().fg(Color::DarkGray),
                )),
            ])
            .block(block.title(" vibin "));
            frame.render_widget(msg, pane);
        }
    }
}

fn draw_status_bar(frame: &mut Frame, app: &App, area: Rect) {
    let mut spans: Vec<Span> = Vec::new();
    if app.leader_pending {
        spans.push(Span::styled(
            " LEADER ",
            Style::default().fg(Color::Black).bg(Color::Yellow).add_modifier(Modifier::BOLD),
        ));
    } else {
        let focus = match app.focus {
            Focus::Terminal => " TERM ",
            Focus::Sidebar => " SIDE ",
        };
        spans.push(Span::styled(
            focus,
            Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD),
        ));
    }
    // agent dashboard summary: how many are working / waiting for you
    let count = |want: fn(&SessionStatus) -> bool| app.statuses.iter().filter(|s| want(s)).count();
    let working = count(|s| matches!(s, SessionStatus::Working));
    let attention = count(|s| matches!(s, SessionStatus::Attention));
    let idle = count(|s| matches!(s, SessionStatus::Idle));
    let exited = count(|s| matches!(s, SessionStatus::Exited(_)));
    for (n, label, color) in [
        (working, "working", Color::Green),
        (attention, "needs you", Color::Yellow),
        (idle, "idle", Color::DarkGray),
        (exited, "exited", Color::Red),
    ] {
        if n > 0 {
            spans.push(Span::styled(
                format!(" {n} {label} "),
                Style::default().fg(color),
            ));
        }
    }
    if let Some(branch) = &app.git.branch {
        spans.push(Span::styled(
            format!("  {branch} "),
            Style::default().fg(Color::Magenta),
        ));
    }
    // message before the workdir: the path is the least important part and
    // the only span that may safely fall off the right edge
    if let Some(msg) = &app.status_msg {
        spans.push(Span::styled(
            format!(" · {msg}"),
            Style::default().fg(Color::Yellow),
        ));
    } else {
        spans.push(Span::styled(
            " · Ctrl+A ? help · Ctrl+A c new · Ctrl+A q quit",
            Style::default().fg(Color::DarkGray),
        ));
    }
    spans.push(Span::styled(
        format!(" · {}", app.workdir.display()),
        Style::default().fg(Color::DarkGray),
    ));
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Grayish base for dialog surfaces — distinct from the terminal background
/// so popups read as a layer above the workspace.
const DIALOG_BG: Color = Color::Rgb(32, 34, 40);
/// Shadow: cells below-right of a dialog keep their glyphs but get crushed
/// to near-black, which reads as a translucent drop shadow.
const SHADOW_BG: Color = Color::Rgb(8, 8, 10);
const SHADOW_FG: Color = Color::Rgb(52, 54, 58);

/// Paint the drop shadow and the dialog's base surface. Call before
/// rendering the dialog's content into `rect`.
fn draw_dialog_base(frame: &mut Frame, rect: Rect) {
    let screen = frame.area();
    // offset +2/+1 because terminal cells are ~half as wide as tall
    let shadow = Rect {
        x: rect.x.saturating_add(2),
        y: rect.y.saturating_add(1),
        width: rect.width,
        height: rect.height,
    }
    .intersection(screen);
    frame.render_widget(
        Block::default().style(Style::default().bg(SHADOW_BG).fg(SHADOW_FG)),
        shadow,
    );
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Block::default().style(Style::default().bg(DIALOG_BG)),
        rect,
    );
}

fn overlay_area(frame: &Frame, pct_x: u16, pct_y: u16) -> Rect {
    let area = frame.area();
    let width = area.width * pct_x / 100;
    let height = area.height * pct_y / 100;
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

// Claude Code-style diff palette: tinted full-width rows on dark background.
// One accent per side, used for BOTH the line number and the +/- marker, so
// the greens/reds always match (ANSI theme colors would drift from the RGB
// backgrounds).
const ADD_BG: Color = Color::Rgb(16, 42, 16);
const ADD_ACCENT: Color = Color::Rgb(110, 190, 110);
const REMOVE_BG: Color = Color::Rgb(52, 18, 18);
const REMOVE_ACCENT: Color = Color::Rgb(210, 120, 120);

/// Render one parsed diff line as a full-width styled row with a
/// line-number gutter, like Claude Code's change view.
fn render_diff_line(line: &DiffLine, width: usize) -> Line<'static> {
    let pad = |text: &str| {
        let visible = text.chars().count();
        let fill = width.saturating_sub(visible + 8);
        format!("{text}{}", " ".repeat(fill))
    };
    match line.kind {
        DiffLineKind::FileHeader => Line::from(vec![
            Span::styled("● ", Style::default().fg(Color::Green)),
            Span::styled(
                "Update(",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                line.text.clone(),
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
            Span::styled(")", Style::default().add_modifier(Modifier::BOLD)),
        ]),
        DiffLineKind::FileStat => Line::from(vec![
            Span::styled("  └ ", Style::default().fg(Color::DarkGray)),
            Span::styled(line.text.clone(), Style::default().fg(Color::Gray)),
        ]),
        DiffLineKind::HunkSep => Line::from(Span::styled(
            "      ⋯",
            Style::default().fg(Color::DarkGray),
        )),
        DiffLineKind::Add => {
            let bg = Style::default().bg(ADD_BG);
            Line::from(vec![
                Span::styled(
                    format!("{:>5} ", line.new_no.unwrap_or(0)),
                    bg.fg(ADD_ACCENT),
                ),
                Span::styled("+ ", bg.fg(ADD_ACCENT).add_modifier(Modifier::BOLD)),
                Span::styled(pad(&line.text), bg),
            ])
        }
        DiffLineKind::Remove => {
            let bg = Style::default().bg(REMOVE_BG);
            Line::from(vec![
                Span::styled(
                    format!("{:>5} ", line.old_no.unwrap_or(0)),
                    bg.fg(REMOVE_ACCENT),
                ),
                Span::styled("- ", bg.fg(REMOVE_ACCENT).add_modifier(Modifier::BOLD)),
                Span::styled(pad(&line.text), bg),
            ])
        }
        DiffLineKind::Context => Line::from(vec![
            Span::styled(
                format!("{:>5} ", line.new_no.unwrap_or(0)),
                Style::default().fg(Color::DarkGray),
            ),
            Span::raw("  "),
            Span::raw(line.text.clone()),
        ]),
    }
}

fn draw_diff_overlay(frame: &mut Frame, app: &mut App) {
    let area = overlay_area(frame, 92, 90);
    draw_dialog_base(frame, area);
    let Some(Overlay::Diff(view)) = &app.overlay else {
        return;
    };
    let inner_height = area.height.saturating_sub(2) as usize;
    let inner_width = area.width.saturating_sub(2) as usize;
    app.diff_viewport = inner_height.max(1);

    let visible: Vec<Line> = view
        .lines
        .iter()
        .skip(view.scroll)
        .take(inner_height)
        .map(|line| render_diff_line(line, inner_width))
        .collect();

    let title = format!(
        " {} — {}/{} (j/k scroll · q close) ",
        view.title,
        (view.scroll + 1).min(view.lines.len()),
        view.lines.len()
    );
    let paragraph = Paragraph::new(visible)
        .style(Style::default().bg(DIALOG_BG))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(title),
        );
    frame.render_widget(paragraph, area);
}

fn draw_help_overlay(frame: &mut Frame) {
    let lines = vec![
        ("Ctrl+A c", "new Claude session"),
        ("Ctrl+A x", "close active session"),
        ("Ctrl+A n / p", "next / previous session"),
        ("Ctrl+A 1..9", "jump to session"),
        ("Ctrl+A ,", "rename session"),
        ("Ctrl+A R", "respawn exited session in place"),
        ("Ctrl+A f", "toggle sidebar/terminal focus"),
        ("Ctrl+A e", "file tree"),
        ("Ctrl+A g", "git changes"),
        ("Ctrl+A h", "past chats (Enter/double-click resumes)"),
        ("Ctrl+A d", "diff of all changes"),
        ("Ctrl+A k / j", "scroll terminal up / down"),
        ("Ctrl+A r", "refresh tree + git"),
        ("Ctrl+A Ctrl+A", "send literal Ctrl+A"),
        ("Ctrl+A q", "quit"),
        ("", ""),
        ("status dots: ● working · ● needs you (bell) · ○ idle · ✖ exited", ""),
        ("files: j/k move · Enter toggle dir · h parent · . hidden · d diff", ""),
        ("git: j/k move · s stage · a stage all · c commit · Enter diff", ""),
        ("diff: j/k scroll · PgUp/PgDn page · g top · q close", ""),
    ];
    let text: Vec<Line> = lines
        .into_iter()
        .map(|(key, desc)| {
            if desc.is_empty() {
                Line::from(Span::styled(key.to_string(), Style::default().fg(Color::DarkGray)))
            } else {
                Line::from(vec![
                    Span::styled(format!(" {key:<16}"), Style::default().fg(Color::Cyan)),
                    Span::raw(desc.to_string()),
                ])
            }
        })
        .collect();
    let height = (text.len() + 2) as u16;
    let area = frame.area();
    let width = 74.min(area.width.saturating_sub(4));
    let rect = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height: height.min(area.height),
    };
    draw_dialog_base(frame, rect);
    frame.render_widget(
        Paragraph::new(text).style(Style::default().bg(DIALOG_BG)).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow))
                .title(" keys (any key to close) "),
        ),
        rect,
    );
}

fn draw_prompt(frame: &mut Frame, title: &str, buf: &str) {
    let area = frame.area();
    let width = 60.min(area.width.saturating_sub(4));
    let rect = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + area.height / 2 - 1,
        width,
        height: 3,
    };
    draw_dialog_base(frame, rect);
    frame.render_widget(
        Paragraph::new(Line::from(Span::raw(buf.to_string())))
            .style(Style::default().bg(DIALOG_BG))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Green))
                    .title(title.to_string()),
            ),
        rect,
    );
    let cursor_x = rect.x + 1 + (buf.chars().count() as u16).min(rect.width.saturating_sub(3));
    frame.set_cursor_position((cursor_x, rect.y + 1));
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn render(app: &mut App) -> ratatui::buffer::Buffer {
        let backend = TestBackend::new(100, 32);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        terminal.backend().buffer().clone()
    }

    fn bg_count(buf: &ratatui::buffer::Buffer, color: Color) -> usize {
        buf.content()
            .iter()
            .filter(|cell| cell.style().bg == Some(color))
            .count()
    }

    fn test_app() -> (tempfile::TempDir, App) {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("file.txt"), "hi\n").unwrap();
        let app = App::new(
            dir.path().to_path_buf(),
            vec!["/bin/sh".into(), "-c".into(), "sleep 30".into()],
        );
        (dir, app)
    }

    #[test]
    fn dialogs_have_gray_base_and_drop_shadow() {
        let (_dir, mut app) = test_app();
        // no overlay → neither dialog base nor shadow anywhere
        let buf = render(&mut app);
        assert_eq!(bg_count(&buf, DIALOG_BG), 0);
        assert_eq!(bg_count(&buf, SHADOW_BG), 0);

        app.overlay = Some(Overlay::Help);
        let buf = render(&mut app);
        assert!(bg_count(&buf, DIALOG_BG) > 100, "dialog surface painted");
        // visible shadow = right column + bottom row of the offset rect
        assert!(bg_count(&buf, SHADOW_BG) > 10, "drop shadow painted");
    }

    #[test]
    fn prompt_dialog_uses_gray_base() {
        let (_dir, mut app) = test_app();
        app.overlay = Some(Overlay::CommitPrompt("msg".into()));
        let buf = render(&mut app);
        assert!(bg_count(&buf, DIALOG_BG) > 50);
        assert!(bg_count(&buf, SHADOW_BG) > 5);
    }
}
