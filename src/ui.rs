//! Rendering: layout, sidebar, terminal panes, overlays, status bar.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Padding, Paragraph, Tabs};
use ratatui::Frame;
use tui_term::widget::PseudoTerminal;

use crate::app::{App, Focus, Overlay, Screen, Shell};
use crate::editor::Mode;
use crate::diff::{DiffLine, DiffLineKind};
use crate::git::{GitView, StatusKind};
use crate::projects::display_path;
use crate::session::SessionStatus;

const SIDEBAR_WIDTH: u16 = 34;

/// "VIBIN" in ANSI-shadow block letters.
const LOGO: [&str; 6] = [
    "██╗   ██╗██╗██████╗ ██╗███╗   ██╗",
    "██║   ██║██║██╔══██╗██║████╗  ██║",
    "██║   ██║██║██████╔╝██║██╔██╗ ██║",
    "╚██╗ ██╔╝██║██╔══██╗██║██║╚██╗██║",
    " ╚████╔╝ ██║██████╔╝██║██║ ╚████║",
    "  ╚═══╝  ╚═╝╚═════╝ ╚═╝╚═╝  ╚═══╝",
];
const LOGO_WIDTH: u16 = 33;

/// Pastel rainbow stops for the wordmark, interpolated cyclically so the
/// animated gradient loops without a seam.
const GRADIENT_STOPS: [(u8, u8, u8); 6] = [
    (255, 110, 140), // rose
    (255, 175, 95),  // apricot
    (250, 235, 120), // soft yellow
    (120, 225, 160), // mint
    (110, 175, 255), // sky
    (200, 135, 255), // lilac
];

/// Smooth cyclic gradient color: `t` wraps around 1.0.
fn gradient_color(t: f32) -> Color {
    let n = GRADIENT_STOPS.len();
    let t = t.rem_euclid(1.0) * n as f32;
    let i = (t.floor() as usize) % n;
    let f = t - t.floor();
    let (r0, g0, b0) = GRADIENT_STOPS[i];
    let (r1, g1, b1) = GRADIENT_STOPS[(i + 1) % n];
    let lerp = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * f).round() as u8;
    Color::Rgb(lerp(r0, r1), lerp(g0, g1), lerp(b0, b1))
}

/// White accent for the current-directory highlight.
const ACCENT: Color = Color::Rgb(240, 244, 250);

const PARROT_GAP: u16 = 4;

/// Color a string with a smooth rainbow gradient, shifted by `phase` so the
/// colors drift across the text as the phase advances.
fn rainbow_line(text: &str, phase: f32) -> Line<'static> {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len().max(2);
    Line::from(
        chars
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let t = i as f32 / (n - 1) as f32 + phase;
                Span::styled(c.to_string(), Style::default().fg(gradient_color(t)))
            })
            .collect::<Vec<_>>(),
    )
}

pub fn draw(frame: &mut Frame, app: &mut App) {
    app.squiggle_overlays.clear();
    app.link_hits.clear();
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
    draw_main_area(frame, app, main[1]);
    draw_status_bar(frame, app, outer[1]);

    // modal dimming: with a dialog or the leader menu up, everything
    // behind it steps back (hover popups are tooltips, not modals)
    let modal_open = app.leader_pending
        || matches!(
            &app.overlay,
            Some(
                Overlay::Diff(_)
                    | Overlay::Help
                    | Overlay::CommitPrompt(_)
                    | Overlay::RenamePrompt(_)
                    | Overlay::Palette(_)
            )
        );
    if modal_open {
        dim_background(frame.buffer_mut());
        // squiggles are re-printed post-draw at full color — skip them
        // while dimmed or they'd glow through the veil
        app.squiggle_overlays.clear();
    }

    match &app.overlay {
        Some(Overlay::Diff(_)) => draw_diff_overlay(frame, app),
        Some(Overlay::Help) => draw_help_overlay(frame, app.welcome.phase),
        Some(Overlay::CommitPrompt(buf)) => {
            draw_prompt(frame, "commit message (Enter commit · Esc cancel)", buf, app.welcome.phase)
        }
        Some(Overlay::RenamePrompt(buf)) => {
            draw_prompt(frame, "rename session (Enter apply · Esc cancel)", buf, app.welcome.phase)
        }
        Some(Overlay::Hover(_)) => draw_hover_overlay(frame, app),
        Some(Overlay::Palette(_)) => draw_palette(frame, app),
        None => {}
    }

    // which-key: the leader menu shows itself the moment Ctrl+A is pressed
    if app.leader_pending {
        draw_whichkey(frame, app);
    }
}

/// Bottom-anchored menu of every leader binding, shown while the leader is
/// pending — no memorization needed.
fn draw_whichkey(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let key = |k: &str| Span::styled(format!(" {k:<5}"), Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD));
    let label = |l: &str| Span::styled(format!("{l:<18}"), Style::default());
    let head = |t: &str| {
        Span::styled(
            format!(" {t:<23}"),
            Style::default().fg(Color::DarkGray).add_modifier(Modifier::BOLD),
        )
    };
    let editor_label = if app.editor.is_some() { "editor" } else { "editor (none open)" };
    let rows: Vec<Line> = vec![
        Line::from(vec![head("── agents"), head("── panels"), head("── other")]),
        Line::from(vec![
            key("c"), label("new agent"),
            key("F1/h"), label("agents shell"),
            key("r"), label("rename agent"),
        ]),
        Line::from(vec![
            key("1-9"), label("jump to agent"),
            key("F2/g"), label("git shell"),
            key("R"), label("respawn agent"),
        ]),
        Line::from(vec![
            key("⇥/n"), label("next agent"),
            key("F3/f"), label("code shell"),
            key("u"), label("refresh panels"),
        ]),
        Line::from(vec![
            key("p"), label("previous agent"),
            key("e"), label(editor_label),
            key("k/j"), label("scroll terminal"),
        ]),
        Line::from(vec![
            key("x"), label("close agent"),
            key("d"), label("diff all changes"),
            key("q"), label("quit vibin"),
        ]),
        Line::from(Span::styled(
            " esc cancel · ctrl+a ctrl+a literal · ctrl+k palette · ? full help",
            Style::default().fg(Color::DarkGray),
        )),
    ];
    let width = 76.min(area.width.saturating_sub(2));
    let height = rows.len() as u16 + 2;
    let rect = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
    .intersection(area);
    draw_dialog_base(frame, rect);
    frame.render_widget(
        Paragraph::new(rows)
            .style(Style::default().bg(DIALOG_BG()))
            .block(dialog_block()),
        rect,
    );
    draw_dialog_frame(frame, rect, "", app.welcome.phase);
}

/// Command palette: input on top, fuzzy results below. Files by
/// default; `>` switches to commands.
fn draw_palette(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let (input, is_cmd, selected, rows) = {
        let Some(Overlay::Palette(palette)) = &mut app.overlay else {
            return;
        };
        let rows: Vec<String> = palette.results().into_iter().map(|(l, _)| l).collect();
        (palette.input.clone(), palette.is_command_mode(), palette.selected, rows)
    };
    let width = 72.min(area.width.saturating_sub(4));
    let height = rows.len().max(1) as u16 + 3; // border(title) + input + rows + border
    let height = height.min(area.height.saturating_sub(4));
    let rect = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
    .intersection(area);
    draw_dialog_base(frame, rect);

    let mut lines: Vec<Line> = Vec::new();
    let prompt = if is_cmd { "❯" } else { "🔍" };
    lines.push(Line::from(vec![
        Span::styled(format!(" {prompt} "), Style::default().fg(Color::Cyan)),
        Span::raw(input.clone()),
    ]));
    if rows.is_empty() {
        lines.push(Line::from(Span::styled(
            "   no matches",
            Style::default().fg(Color::DarkGray),
        )));
    }
    for (i, label) in rows.iter().enumerate() {
        let style = if i == selected {
            Style::default().bg(Color::Rgb(58, 62, 78)).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        let marker = if i == selected { "▸ " } else { "  " };
        let text: String = label.chars().take(width.saturating_sub(5) as usize).collect();
        lines.push(Line::from(Span::styled(format!(" {marker}{text}"), style)));
    }
    frame.render_widget(
        Paragraph::new(lines)
            .style(Style::default().bg(DIALOG_BG()))
            .block(dialog_block()),
        rect,
    );
    // result rows start after title + input
    app.layout.palette_list = Rect {
        x: rect.x,
        y: rect.y + 2,
        width: rect.width,
        height: rect.height.saturating_sub(3),
    };
    let prefix = if is_cmd { 3 } else { 4 }; // " ❯ " vs " 🔍 " (emoji is 2 wide)
    frame.set_cursor_position((
        rect.x + 2 + prefix + input.chars().count() as u16,
        rect.y + 1,
    ));
    draw_dialog_frame(frame, rect, "", app.welcome.phase);
}

/// Greedy word wrap for diagnostic messages.
fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let width = width.max(8);
    let mut out = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if !current.is_empty() && current.chars().count() + 1 + word.chars().count() > width {
            out.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() || out.is_empty() {
        out.push(current);
    }
    out
}

/// LSP hover docs: a deliberately plain popup — markdown on the dialog
/// surface, no frame or badge, scrollable when tall.
fn draw_hover_overlay(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let anchor = app.hover_anchor;
    // inline code and unlabeled fences highlight as the hovered file's
    // language, so docs read like the source they describe
    let hover_lang = app
        .editor
        .as_ref()
        .map(|e| crate::editor::highlight::language_name(&e.path))
        .unwrap_or("");
    let Some(Overlay::Hover(doc)) = &mut app.overlay else {
        return;
    };
    let max_width = area.width.saturating_sub(6).min(84);
    let wrap_width = max_width.saturating_sub(4) as usize;

    // diagnostics at the hovered position come first
    let mut diag_lines: Vec<Line> = Vec::new();
    for diag in &doc.diagnostics {
        let color = severity_color(diag.severity);
        for (i, piece) in wrap_text(&diag.message, wrap_width.saturating_sub(2)).into_iter().enumerate() {
            let prefix = if i == 0 { "■ " } else { "  " };
            diag_lines.push(Line::from(vec![
                Span::styled(prefix, Style::default().fg(color)),
                Span::styled(piece, Style::default().fg(color)),
            ]));
        }
        let meta = match (diag.source.is_empty(), diag.code.is_empty()) {
            (false, false) => format!("{}({})", diag.source, diag.code),
            (false, true) => diag.source.clone(),
            (true, false) => diag.code.clone(),
            (true, true) => String::new(),
        };
        if !meta.is_empty() {
            diag_lines.push(Line::from(Span::styled(
                format!("  {meta}"),
                Style::default().fg(STATUS_DIM()),
            )));
        }
    }

    let (mut lines, mut links) =
        crate::markdown::render_with_links_lang(&doc.text, wrap_width, hover_lang);
    // no blank framing rows: trim empty lines at both ends (keep link
    // line indices in step)
    while lines.first().is_some_and(|l| l.width() == 0) {
        lines.remove(0);
        links.retain_mut(|l| {
            if l.line == 0 {
                false
            } else {
                l.line -= 1;
                true
            }
        });
    }
    while lines.last().is_some_and(|l| l.width() == 0) {
        lines.pop();
    }
    links.retain(|l| l.line < lines.len());
    // stitch: diagnostics, separator, docs
    if !diag_lines.is_empty() {
        let offset = if lines.is_empty() {
            diag_lines.len()
        } else {
            diag_lines.push(Line::from(Span::styled(
                "─".repeat(wrap_width),
                Style::default().fg(DIALOG_BORDER()),
            )));
            diag_lines.len()
        };
        for link in &mut links {
            link.line += offset;
        }
        diag_lines.extend(lines);
        lines = diag_lines;
    }
    // +2 = exactly the horizontal padding. Cap at wrap_width + 2 too:
    // code panels and rules render wrap_width wide, so a popup stretched
    // further by one long unwrapped line would leave them short of the
    // right edge again
    let width = lines
        .iter()
        .map(|l| l.width() as u16 + 2)
        .max()
        .unwrap_or(20)
        .clamp(24, wrap_width as u16 + 2);
    // cap the popup and scroll the rest
    let max_height = (area.height * 2 / 5).clamp(5, area.height.saturating_sub(4));
    let height = (lines.len() as u16).clamp(1, max_height);
    // scrolling popups get static chrome: a hairline header rule on top
    // and a footer with the scroll position; short popups use every row
    let has_footer = lines.len() > height as usize && height >= 3;
    let has_header = has_footer && height >= 4;
    let chrome_rows = has_footer as usize + has_header as usize;
    let viewport = (height as usize).saturating_sub(chrome_rows).max(1);
    let max_scroll = lines.len().saturating_sub(viewport);
    doc.scroll = doc.scroll.min(max_scroll);
    let scroll = doc.scroll;
    let total = lines.len();
    // anchor next to the hovered symbol like a tooltip: below it when there
    // is room, above otherwise; centered only without an anchor
    let rect = match anchor {
        Some(pos) => {
            let x = pos.x.min(area.right().saturating_sub(width)).max(area.x);
            let below = pos.y + 1;
            let y = if below + height <= area.bottom() {
                below
            } else {
                pos.y.saturating_sub(height).max(area.y)
            };
            Rect { x, y, width, height }
        }
        None => Rect {
            x: area.x + (area.width.saturating_sub(width)) / 2,
            y: area.y + (area.height.saturating_sub(height)) / 3,
            width,
            height,
        },
    }
    .intersection(area);
    draw_dialog_base(frame, rect);
    let content_rect = Rect::new(
        rect.x,
        rect.y + has_header as u16,
        rect.width,
        rect.height.saturating_sub(chrome_rows as u16),
    )
    .intersection(area);
    let visible: Vec<Line> = lines.into_iter().skip(scroll).take(viewport).collect();
    frame.render_widget(
        Paragraph::new(visible)
            .style(Style::default().bg(DIALOG_BG()))
            .block(Block::default().padding(Padding::horizontal(1))),
        content_rect,
    );
    if has_header {
        // static header: a hairline rule matching the footer
        let rule = "─".repeat(rect.width.saturating_sub(2) as usize);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(rule, Style::default().fg(DIALOG_BORDER()))))
                .style(Style::default().bg(DIALOG_BG()))
                .block(Block::default().padding(Padding::horizontal(1))),
            Rect::new(rect.x, rect.y, rect.width, 1).intersection(area),
        );
    }
    if has_footer {
        // static footer row: a hairline rule with the scroll position
        // right-aligned; content scrolls above it, this row never moves
        let hint = format!(" ↕ {}/{} ", (scroll + viewport).min(total), total);
        let fill = rect.width.saturating_sub(2) as usize;
        let rule_len = fill.saturating_sub(hint.chars().count());
        let footer = Line::from(vec![
            Span::styled("─".repeat(rule_len), Style::default().fg(DIALOG_BORDER())),
            Span::styled(hint, Style::default().fg(STATUS_DIM())),
        ]);
        frame.render_widget(
            Paragraph::new(footer)
                .style(Style::default().bg(DIALOG_BG()))
                .block(Block::default().padding(Padding::horizontal(1))),
            Rect::new(rect.x, rect.bottom().saturating_sub(1), rect.width, 1).intersection(area),
        );
    }
    app.layout.hover_rect = rect;
    // clickable links (OSC 8) as buffer cells, for visible unclipped lines
    let inner_width = rect.width.saturating_sub(2) as usize;
    for link in links {
        if link.line < scroll || link.line >= scroll + viewport {
            continue;
        }
        if link.col + link.text.chars().count() > inner_width {
            continue; // clipped labels would render a broken sequence
        }
        let (x, y) = (rect.x + 1 + link.col as u16, content_rect.y + (link.line - scroll) as u16);
        let hitbox = Rect::new(x, y, link.text.chars().count().max(1) as u16, 1);
        // keep the styling the paragraph gave the label
        let style = frame.buffer_mut().cell((x, y)).map(|c| c.style()).unwrap_or_default();
        // OSC 8 hyperlink: pack the whole wrapped label into the first cell
        // and force its diff width to the label's display width, so the
        // terminal writes the escape sequence verbatim in one run
        if let Some(cell) = frame.buffer_mut().cell_mut((x, y)) {
            use unicode_width::UnicodeWidthStr;
            let label_width = link.text.as_str().width().max(1) as u16;
            cell.set_symbol(&format!("\x1b]8;;{}\x1b\\{}\x1b]8;;\x1b\\", link.url, link.text));
            cell.set_style(style);
            cell.set_diff_option(ratatui::buffer::CellDiffOption::ForcedWidth(
                std::num::NonZeroU16::new(label_width).expect("max(1) above"),
            ));
        }
        app.link_hits.push((hitbox, link.url));
    }
}

fn draw_welcome(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let art_height = LOGO.len() as u16 + 1; // wordmark + version row
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(area.height.saturating_sub(art_height + 14) / 3),
            Constraint::Length(art_height),
            Constraint::Length(1),
            Constraint::Min(5),
            Constraint::Length(2),
        ])
        .split(area);

    // the real party parrot GIF + wordmark centered as one ensemble; the
    // parrot faces right, so it sits LEFT of the title, looking at it.
    // Dropped on narrow terminals.
    let parrot_width = crate::parrot::width();
    let with_parrot =
        parrot_width > 0 && area.width >= LOGO_WIDTH + PARROT_GAP + parrot_width + 2;
    let total = if with_parrot {
        LOGO_WIDTH + PARROT_GAP + parrot_width
    } else {
        LOGO_WIDTH
    };
    let start_x = area.x + area.width.saturating_sub(total) / 2;
    let logo_x = if with_parrot {
        start_x + parrot_width + PARROT_GAP
    } else {
        start_x
    };
    let width = LOGO_WIDTH.min(area.width);
    // parrot and wordmark are the same height, top-aligned on the same rows
    let logo_y = chunks[1].y;
    // animated gradient flows across the wordmark; each row is offset a
    // touch so the colors run diagonally
    for (i, row) in LOGO.iter().enumerate() {
        let phase = app.welcome.phase + i as f32 * 0.03;
        let rect = Rect::new(logo_x, logo_y + i as u16, width, 1).intersection(area);
        frame.render_widget(Paragraph::new(rainbow_line(row, phase)), rect);
    }
    if with_parrot {
        let frames = crate::parrot::frames();
        let parrot = &frames[app.welcome.frame % frames.len()];
        for (i, line) in parrot.lines.iter().enumerate() {
            let rect = Rect::new(start_x, logo_y + i as u16, parrot.width, 1).intersection(area);
            frame.render_widget(Paragraph::new(line.clone()), rect);
        }
    }
    let version = format!("v{}", env!("CARGO_PKG_VERSION"));
    let version_rect =
        Rect::new(logo_x, logo_y + LOGO.len() as u16, width, 1).intersection(area);
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
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
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
    match app.shell {
        Shell::Code => draw_file_tree(frame, app, area),
        Shell::Git => draw_git_panel(frame, app, area),
        Shell::Agents => draw_chats_panel(frame, app, area),
    }
}

fn draw_chats_panel(frame: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Sidebar;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style(focused));
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
    use std::collections::{HashMap, HashSet};
    let focused = app.focus == Focus::Sidebar;

    // git status: color file names by their change kind, and
    // color a folder that contains changes. Match on paths relative to the
    // tree root (== repo root in the common case), which sidesteps absolute-
    // path canonicalization differences (macOS /var vs /private/var).
    let base = app.workdir.clone();
    let file_status: HashMap<String, StatusKind> =
        app.git.entries.iter().map(|e| (e.path.clone(), e.kind)).collect();
    // every ancestor directory of a changed file (relative, forward slashes)
    let mut changed_dirs: HashSet<String> = HashSet::new();
    for e in &app.git.entries {
        let mut p = std::path::Path::new(&e.path);
        while let Some(parent) = p.parent() {
            if parent.as_os_str().is_empty() {
                break;
            }
            changed_dirs.insert(parent.to_string_lossy().replace('\\', "/"));
            p = parent;
        }
    }
    let rel_of = |path: &std::path::Path| -> Option<String> {
        path.strip_prefix(&base).ok().map(|p| p.to_string_lossy().replace('\\', "/"))
    };

    // LSP problem counts per file (errors, warnings)
    let diag = app.lsp.as_ref().map(|c| c.diagnostic_counts()).unwrap_or_default();

    let items: Vec<ListItem> = app
        .tree
        .items
        .iter()
        .map(|item| {
            let indent = "  ".repeat(item.depth);
            let icon = if item.is_dir {
                if item.expanded { "📂 " } else { "📁 " }
            } else {
                "   " // aligns with the double-width folder emoji
            };
            let rel = rel_of(&item.path);
            // name color: git status wins, else folders are blue
            let name_style = if let Some(kind) = rel.as_ref().and_then(|r| file_status.get(r)) {
                Style::default().fg(status_color(*kind))
            } else if item.is_dir && rel.as_ref().is_some_and(|r| changed_dirs.contains(r)) {
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
            } else if item.is_dir {
                Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            let mut spans = vec![
                Span::raw(indent.clone()),
                Span::raw(icon.to_string()),
                Span::styled(item.name.clone(), name_style),
            ];
            // problem badge, right-aligned at the row's edge:
            // red count for errors, yellow for warnings
            let badge = diag.get(&item.path).and_then(|&(errors, warnings)| {
                if errors > 0 {
                    Some(((errors + warnings).to_string(), severity_color(1), true))
                } else if warnings > 0 {
                    Some((warnings.to_string(), severity_color(2), false))
                } else {
                    None
                }
            });
            if let Some((count, color, bold)) = badge {
                // indent (2/level) + icon (3 cols) + name, then pad to the edge
                let used = indent.chars().count() + 3 + item.name.chars().count();
                let inner_w = area.width.saturating_sub(2) as usize; // inside borders
                let pad = inner_w.saturating_sub(used + count.chars().count()).max(1);
                spans.push(Span::raw(" ".repeat(pad)));
                let mut style = Style::default().fg(color);
                if bold {
                    style = style.add_modifier(Modifier::BOLD);
                }
                spans.push(Span::styled(count, style));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style(focused));
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
        .border_style(border_style(focused));
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

    let (items, selected_row) = match app.git.view {
        GitView::List => {
            let items: Vec<ListItem> = app
                .git
                .entries
                .iter()
                .map(|entry| {
                    ListItem::new(Line::from(vec![
                        Span::styled(
                            entry.code(),
                            Style::default()
                                .fg(status_color(entry.kind))
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::raw(" "),
                        Span::raw(entry.path.clone()),
                    ]))
                })
                .collect();
            (items, app.git.selected)
        }
        GitView::Tree => {
            let rows = app.git.tree_rows();
            let selected_row = rows
                .iter()
                .position(|r| r.entry == Some(app.git.selected))
                .unwrap_or(0);
            let items: Vec<ListItem> = rows
                .iter()
                .map(|row| {
                    let indent = "  ".repeat(row.depth);
                    match row.entry {
                        None => ListItem::new(Line::from(vec![
                            Span::raw(format!("   {indent}")),
                            Span::raw("📂 "),
                            Span::styled(
                                row.name.clone(),
                                Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD),
                            ),
                        ])),
                        Some(idx) => {
                            let entry = &app.git.entries[idx];
                            ListItem::new(Line::from(vec![
                                Span::styled(
                                    entry.code(),
                                    Style::default()
                                        .fg(status_color(entry.kind))
                                        .add_modifier(Modifier::BOLD),
                                ),
                                Span::raw(format!(" {indent}")),
                                Span::raw(row.name.clone()),
                            ]))
                        }
                    }
                })
                .collect();
            (items, selected_row)
        }
    };
    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD));
    app.git_list.select(Some(selected_row));
    let mut state = std::mem::take(&mut app.git_list);
    frame.render_stateful_widget(list, area, &mut state);
    app.git_list = state;
}

fn draw_main_area(frame: &mut Frame, app: &mut App, area: Rect) {
    match app.shell {
        Shell::Agents => draw_terminal_area(frame, app, area),
        Shell::Git => draw_git_diff_main(frame, app, area),
        Shell::Code => {
            app.layout.terminal_pane = area;
            if app.hex.is_some() {
                draw_hex_view(frame, app, area);
            } else if app.editor.is_some() {
                draw_editor(frame, app, area);
            } else {
                draw_editor_placeholder(frame, app, area);
            }
        }
    }
}

/// Empty state: a dim mark above a centered list of
/// actions with keycap-styled shortcuts.
fn draw_editor_placeholder(frame: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Terminal;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style(focused));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    const MARK: [&str; 4] = [
        "  ▗▄▄▖  ",
        " ▟████▙ ",
        " ▜██▛▀  ",
        "  ▝▀▘   ",
    ];
    let dim = Color::Rgb(70, 74, 84);
    let label_fg = Color::Rgb(150, 156, 168);
    let keys_fg = Color::Rgb(105, 110, 122);

    let items = crate::app::App::code_home_items();
    let row_width = 38u16;
    let total_height = MARK.len() as u16 + 2 + items.len() as u16;
    let top = inner.y + inner.height.saturating_sub(total_height) / 2;
    let mark_x = inner.x + inner.width.saturating_sub(MARK[0].chars().count() as u16) / 2;
    for (i, line) in MARK.iter().enumerate() {
        let rect = Rect::new(mark_x, top + i as u16, MARK[0].chars().count() as u16, 1)
            .intersection(inner);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(*line, Style::default().fg(dim)))),
            rect,
        );
    }
    // palette-style selectable rows: label left, keybind at the end
    let rows_x = inner.x + inner.width.saturating_sub(row_width) / 2;
    let rows_y = top + MARK.len() as u16 + 2;
    app.layout.home_list =
        Rect::new(rows_x, rows_y, row_width, items.len() as u16).intersection(inner);
    for (i, (label, keys)) in items.iter().enumerate() {
        let y = rows_y + i as u16;
        if y >= inner.bottom() {
            break;
        }
        let selected = i == app.code_home_selected;
        let base = if selected {
            Style::default().bg(Color::Rgb(58, 62, 78))
        } else {
            Style::default()
        };
        let marker = if selected { "▸ " } else { "  " };
        let pad = (row_width as usize)
            .saturating_sub(2 + label.chars().count() + keys.chars().count() + 1);
        let spans = vec![
            Span::styled(marker.to_string(), base.fg(Color::Cyan)),
            Span::styled(
                label.to_string(),
                if selected {
                    base.fg(Color::Rgb(220, 225, 235)).add_modifier(Modifier::BOLD)
                } else {
                    base.fg(label_fg)
                },
            ),
            Span::styled(" ".repeat(pad), base),
            Span::styled(format!("{keys} "), base.fg(keys_fg)),
        ];
        let rect = Rect::new(rows_x, y, row_width, 1).intersection(inner);
        frame.render_widget(Paragraph::new(Line::from(spans)), rect);
    }
}

/// Git shell main pane: the selected file's diff (or all changes),
/// rendered persistently with its own scroll.
fn draw_git_diff_main(frame: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Terminal;
    app.layout.terminal_pane = area;
    let text = if !app.git.is_repo() {
        String::new()
    } else {
        let path = app.git.selected_entry().map(|e| e.path.clone());
        app.git.diff(path.as_deref()).unwrap_or_default()
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style(focused));
    let inner = block.inner(area);
    let lines = crate::diff::parse(&text);
    let viewport = inner.height as usize;
    app.git_diff_viewport = viewport.max(1);
    let max_scroll = lines.len().saturating_sub(viewport);
    app.git_diff_scroll = app.git_diff_scroll.min(max_scroll);
    let visible: Vec<Line> = lines
        .iter()
        .skip(app.git_diff_scroll)
        .take(viewport)
        .map(|line| render_diff_line(line, inner.width as usize))
        .collect();
    if text.trim().is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                if app.git.is_repo() { " no changes " } else { " not a git repository " },
                Style::default().fg(Color::DarkGray),
            )))
            .block(block),
            area,
        );
        return;
    }
    frame.render_widget(Paragraph::new(visible).block(block), area);
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
    let selected_tab = app.sessions.active;
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
        .select(selected_tab)
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

    let overlay_open = app.overlay.is_some() || app.leader_pending;
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

// Mode segment colors.
fn mode_colors(mode: Mode) -> (Color, Color) {
    match mode {
        Mode::Normal => (Color::Rgb(122, 132, 160), Color::Rgb(16, 18, 24)),
        Mode::Insert => (Color::Rgb(166, 218, 149), Color::Rgb(16, 18, 24)),
        Mode::Select => (Color::Rgb(198, 160, 246), Color::Rgb(16, 18, 24)),
    }
}

/// dark-vs-light pick, keyed off the OSC 11 terminal-background luminance
fn adaptive(dark: Color, light: Color) -> Color {
    if crate::color::is_light() { light } else { dark }
}

/// Theme-native grey (see color::wash), as a ratatui Color.
fn wash(weight: u32) -> Option<Color> {
    crate::color::wash(weight).map(|(r, g, b)| Color::Rgb(r, g, b))
}

/// Recolor every cell toward the terminal background so an open modal
/// reads as the only lit layer: RGB colors blend ~55% toward the
/// background, default-colored text gets an explicit dim wash (there is
/// no RGB to blend), and indexed colors lean on the DIM attribute.
fn dim_background(buf: &mut ratatui::buffer::Buffer) {
    let (br, bgc, bb) = crate::color::terminal_bg()
        .unwrap_or(if crate::color::is_light() { (255, 255, 255) } else { (0, 0, 0) });
    let default_fg =
        wash(110).unwrap_or_else(|| adaptive(Color::Rgb(90, 94, 104), Color::Rgb(172, 176, 184)));
    let blend = |c: u8, base: u8| ((c as u32 * 115 + base as u32 * 141) / 256) as u8;
    let dim_color = |c: Color| match c {
        Color::Rgb(r, g, b) => Color::Rgb(blend(r, br), blend(g, bgc), blend(b, bb)),
        other => other,
    };
    let area = buf.area;
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            let Some(cell) = buf.cell_mut((x, y)) else { continue };
            let style = cell.style();
            let fg = match style.fg {
                Some(Color::Reset) | None => Some(default_fg),
                Some(c) => Some(dim_color(c)),
            };
            let bg = style.bg.map(dim_color);
            let underline = style.underline_color.map(dim_color);
            cell.set_style(
                Style { fg, bg, underline_color: underline, ..style }
                    .add_modifier(Modifier::DIM),
            );
        }
    }
}


/// Status/app bar background: a whisper above the terminal background.
#[allow(non_snake_case)]
fn STATUSBAR_BG() -> Color {
    wash(22).unwrap_or_else(|| adaptive(Color::Rgb(30, 32, 40), Color::Rgb(232, 233, 237)))
}

/// Dim statusline text (positions, separators, secondary info).
#[allow(non_snake_case)]
fn STATUS_DIM() -> Color {
    wash(120).unwrap_or_else(|| adaptive(Color::Rgb(140, 146, 155), Color::Rgb(110, 114, 122)))
}

/// Gutter line numbers: 256-color palette slot 238 (a mid grey) when
/// available; theme-derived washes otherwise.
fn gutter_nr_fg() -> Color {
    if let Some((r, g, b)) = crate::color::ansi16(238) {
        return Color::Rgb(r, g, b);
    }
    wash(60).unwrap_or_else(|| adaptive(Color::Rgb(58, 61, 70), Color::Rgb(196, 200, 208)))
}

/// Selection background, best source first: the terminal's own highlight
/// color (OSC 17) so selections match every other app; else derived from
/// the theme — ANSI blue washed ~30% into the real background; else the
/// hardcoded adaptive pair.
#[allow(non_snake_case)]
fn SELECTION_BG() -> Color {
    if let Some((r, g, b)) = crate::color::selection_bg() {
        return Color::Rgb(r, g, b);
    }
    // azure accent: midpoint of the theme's blue and cyan. Pure ANSI blue
    // alone washes out purple (equal R/G); the cyan half restores the
    // green that real selection colors (#a4c9ff-style sky blues) have.
    let blue = crate::color::ansi16(12).or(crate::color::ansi16(4));
    let cyan = crate::color::ansi16(14).or(crate::color::ansi16(6));
    let accent = match (blue, cyan) {
        (Some(b), Some(c)) => {
            Some(((b.0 / 2 + c.0 / 2), (b.1 / 2 + c.1 / 2), (b.2 / 2 + c.2 / 2)))
        }
        (b, c) => b.or(c),
    };
    if let (Some((ar, ag, ab)), Some((br, bg, bb))) = (accent, crate::color::terminal_bg()) {
        let mix = |a: u8, b: u8| ((a as u32 * 77 + b as u32 * 179) / 256) as u8;
        return Color::Rgb(mix(ar, br), mix(ag, bg), mix(ab, bb));
    }
    adaptive(Color::Rgb(54, 58, 74), Color::Rgb(208, 216, 238))
}
#[allow(non_snake_case)]
fn CURSORLINE_NR() -> Color {
    wash(210).unwrap_or_else(|| adaptive(Color::Rgb(200, 205, 215), Color::Rgb(60, 64, 72)))
}
/// Cursor-line background when the terminal didn't answer OSC 11: a very
/// dim grey, well under SELECTION_BG() so a selection stays distinguishable.
const CURSORLINE_BG_FALLBACK: Color = Color::Rgb(30, 32, 38);

/// Cursor-line background: the terminal's own background nudged ~7% toward
/// the opposite pole — reads as a translucent wash over whatever color (or
/// wallpaper-tinted transparency) the terminal actually shows. Terminal
/// cells have no alpha, so this is as close to transparency as it gets.
fn cursorline_bg() -> Color {
    match crate::color::terminal_bg() {
        Some((r, g, b)) => {
            let lum = (r as u32 * 299 + g as u32 * 587 + b as u32 * 114) / 1000;
            let toward: i32 = if lum > 128 { 0 } else { 255 };
            let blend = |c: u8| (c as i32 + (toward - c as i32) * 18 / 256) as u8;
            Color::Rgb(blend(r), blend(g), blend(b))
        }
        None => CURSORLINE_BG_FALLBACK,
    }
}
/// Spell-check underline: a muted blue, distinct from the red/yellow of
/// diagnostics so misspellings don't read as errors.
#[allow(non_snake_case)]
fn SPELL_CURL() -> (u8, u8, u8) {
    if crate::color::is_light() { (70, 100, 170) } else { (110, 140, 200) }
}
/// Suspicious-Unicode highlight: a dim amber background box
/// behind confusable characters; invisible characters render as a bright
/// ▒ dither glyph over the same background so they become visible.
#[allow(non_snake_case)]
fn INVISIBLE_FG() -> Color {
    adaptive(Color::Rgb(224, 196, 96), Color::Rgb(146, 116, 10))
}
#[allow(non_snake_case)]
fn INVISIBLE_BG() -> Color {
    adaptive(Color::Rgb(74, 62, 24), Color::Rgb(250, 236, 184))
}


/// Diagnostic colors, from the terminal's own ANSI palette when it
/// answered OSC 4 (error = red slot, warning = yellow, info = blue), so
/// they match the active theme; hardcoded fallbacks otherwise.
fn severity_rgb(severity: u8) -> (u8, u8, u8) {
    let (slot, dark, light) = match severity {
        1 => (1, (240, 90, 105), (200, 40, 50)), // error: red
        2 => (3, (250, 210, 60), (176, 130, 10)), // warning: yellow/amber
        _ => (4, (140, 170, 200), (60, 110, 160)), // info/hint: blue
    };
    crate::color::ansi16(slot)
        .unwrap_or(if crate::color::is_light() { light } else { dark })
}

fn severity_color(severity: u8) -> Color {
    let (r, g, b) = severity_rgb(severity);
    Color::Rgb(r, g, b)
}

fn draw_editor(frame: &mut Frame, app: &mut App, pane: Rect) {
    let focused = app.focus == Focus::Terminal;
    let diagnostics = match (&app.lsp, &app.editor) {
        (Some(client), Some(editor)) => client.diagnostics(&editor.path),
        _ => Vec::new(),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style(focused));
    let inner = block.inner(pane);
    let Some(editor) = &mut app.editor else {
        return;
    };
    frame.render_widget(block, pane);
    if inner.height < 2 || inner.width < 6 {
        return;
    }

    let text_height = inner.height as usize; // statusline lives in the app bar
    if editor.follow_cursor {
        editor.ensure_visible(text_height);
    }

    let total_lines = editor.text.len_lines();
    // marker column + right-aligned number + trailing space
    let gutter_width = (total_lines.max(1).ilog10() as u16 + 3).max(5);
    let text_area = Rect::new(
        inner.x + gutter_width,
        inner.y,
        inner.width.saturating_sub(gutter_width),
        inner.height,
    );
    app.layout.editor_text = text_area;

    let (cursor_line, cursor_col) = editor.cursor_line_col();
    let (sel_lo, sel_hi) = editor.selection();
    let scroll = editor.scroll;
    let source = editor.text.clone();
    // widest visible line (chars): the horizontal scroll range
    let total_lines_now = source.len_lines();
    let widest = (0..text_height)
        .map(|r| scroll + r)
        .take_while(|&l| l < total_lines_now)
        .map(|l| {
            let line = source.line(l);
            let mut n = line.len_chars();
            if n > 0 && line.char(n - 1) == '\n' {
                n -= 1;
            }
            n
        })
        .max()
        .unwrap_or(0);
    if editor.follow_cursor {
        editor.ensure_visible_cols(text_area.width as usize);
    } else {
        // free scrolling: clamp against the widest visible line
        editor.hscroll = editor.hscroll.min(widest.saturating_sub(text_area.width as usize));
    }
    let hscroll = editor.hscroll;
    let spans = editor.highlights().to_vec();
    let spell_lang = crate::editor::highlight::language_name(&editor.path);

    for row in 0..text_height {
        let line_idx = scroll + row;
        if line_idx >= total_lines {
            break;
        }
        // gutter: line number + diagnostic dot for lines with findings
        let nr_style = if line_idx == cursor_line {
            Style::default().fg(CURSORLINE_NR())
        } else {
            Style::default().fg(gutter_nr_fg())
        };
        let line_severity = diagnostics
            .iter()
            .filter(|d| d.line == line_idx)
            .map(|d| d.severity)
            .min();
        // gutter sign letters: E(rror) W(arning) H(int/info)
        let marker = match line_severity {
            Some(sev) => Span::styled(
                match sev {
                    1 => "E",
                    2 => "W",
                    _ => "H",
                },
                Style::default().fg(severity_color(sev)).add_modifier(Modifier::BOLD),
            ),
            None => Span::raw(" "),
        };
        let gutter = Rect::new(inner.x, inner.y + row as u16, gutter_width, 1);
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                marker,
                Span::styled(
                    format!("{:>w$} ", line_idx + 1, w = (gutter_width - 2) as usize),
                    nr_style,
                ),
            ])),
            gutter,
        );

        // line text with syntax styles + selection background
        let line_start_char = source.line_to_char(line_idx);
        let line_start_byte = source.line_to_byte(line_idx);
        let line = source.line(line_idx);
        let mut content: String = line.to_string();
        if content.ends_with('\n') {
            content.pop();
        }
        let visible: String = content
            .replace('\t', " ")
            .chars()
            .skip(hscroll)
            .take(text_area.width as usize)
            .collect();
        let mut styles: Vec<Style> = vec![Style::default(); visible.chars().count()];
        // which visible chars are prose (comment/string) — spell-check scope
        let mut spellable: Vec<bool> = vec![false; visible.chars().count()];

        // syntax: spans are byte-ranged over the whole source
        let first = spans.partition_point(|s| s.end <= line_start_byte);
        for span in spans[first..].iter() {
            if span.start >= line_start_byte + content.len() {
                break;
            }
            let s = span.start.saturating_sub(line_start_byte);
            let e = (span.end - line_start_byte).min(content.len());
            // byte offsets → char offsets within the line, shifted into
            // the horizontally scrolled viewport
            let s_raw = content.get(..s).map(|t| t.chars().count()).unwrap_or(0);
            let e_raw = content.get(..e).map(|t| t.chars().count()).unwrap_or(s_raw);
            let s_chars = s_raw.saturating_sub(hscroll);
            let e_chars = e_raw.saturating_sub(hscroll);
            let style = crate::editor::highlight::style_for(span.highlight);
            let prose = crate::editor::highlight::is_spell_region(span.highlight);
            let upto = e_chars.min(styles.len());
            for slot in styles.iter_mut().take(upto).skip(s_chars) {
                *slot = style;
            }
            if prose {
                for slot in spellable.iter_mut().take(upto).skip(s_chars) {
                    *slot = true;
                }
            }
        }
        // misspelled words in this line's prose/identifier regions, checked
        // against the base + this file's language dictionary
        let spell_ranges = if editor.spell_check {
            crate::spell::misspelled_ranges(&visible, &spellable, spell_lang)
        } else {
            Vec::new()
        };
        // plain terminals: dotted underline for misspellings (suspicious
        // Unicode gets a background highlight in the segment loop below)
        if !crate::color::fancy_glyphs() {
            for &(s, e) in &spell_ranges {
                let (r, g, b) = SPELL_CURL();
                for slot in styles.iter_mut().take(e).skip(s) {
                    *slot = slot
                        .add_modifier(Modifier::UNDERLINED)
                        .underline_color(Color::Rgb(r, g, b));
                }
            }
        }
        // diagnostic underlines: straight (in-buffer) on plain terminals,
        // wavy undercurl (post-draw overlay) on capable ones
        let fancy = crate::color::fancy_glyphs();
        if !fancy {
            for diag in diagnostics.iter().filter(|d| d.line == line_idx) {
                let upto = diag.col_end.saturating_sub(hscroll).min(styles.len());
                for slot in styles.iter_mut().take(upto).skip(diag.col_start.saturating_sub(hscroll)) {
                    *slot = slot
                        .add_modifier(Modifier::UNDERLINED)
                        .underline_color(severity_color(diag.severity));
                }
            }
        }
        // cursor-line tint first, so the selection background below wins
        let on_cursor_line = line_idx == cursor_line;
        if on_cursor_line {
            for slot in styles.iter_mut() {
                *slot = slot.bg(cursorline_bg());
            }
        }
        // selection background (Normal/Select; hidden while inserting)
        if editor.mode != Mode::Insert {
            let line_len = styles.len().max(1);
            for (i, slot) in styles.iter_mut().enumerate().take(line_len) {
                let ch = line_start_char + hscroll + i;
                if ch >= sel_lo && ch < sel_hi {
                    *slot = slot.bg(SELECTION_BG());
                }
            }
        }

        // suspicious Unicode gets highlighted — a dim amber
        // background box. Invisible chars (no glyph) also render as a
        // dithered ▒ block so they become visible; confusables keep their
        // glyph on the highlighted background.
        let mut segments: Vec<Span> = Vec::new();
        for (c, style) in visible.chars().zip(styles.iter()) {
            let (glyph, st) = if editor.mark_unicode && crate::confusable::is_invisible(c) {
                (if fancy { '▒' } else { ' ' }, style.fg(INVISIBLE_FG()).bg(INVISIBLE_BG()))
            } else if editor.mark_unicode && crate::confusable::is_confusable(c) {
                (c, style.bg(INVISIBLE_BG()))
            } else {
                (c, *style)
            };
            match segments.last_mut() {
                Some(last) if last.style == st => last.content.to_mut().push(glyph),
                _ => segments.push(Span::styled(glyph.to_string(), st)),
            }
        }
        let row_base = if on_cursor_line {
            Style::default().bg(cursorline_bg())
        } else {
            Style::default()
        };
        frame.render_widget(
            Paragraph::new(Line::from(segments)).style(row_base),
            Rect::new(text_area.x, inner.y + row as u16, text_area.width, 1),
        );

        // wavy squiggle overlays: the final on-screen chars + styles of
        // each visible diagnostic span, re-printed post-draw with SGR 4:3
        if fancy {
            let visible_chars: Vec<char> = visible.chars().collect();
            for diag in diagnostics.iter().filter(|d| d.line == line_idx) {
                let start = diag.col_start.saturating_sub(hscroll).min(visible_chars.len());
                let end = diag.col_end.saturating_sub(hscroll).min(visible_chars.len());
                if start >= end {
                    continue;
                }
                let curl = severity_rgb(diag.severity);
                let as_rgb = |c: Option<Color>| match c {
                    Some(Color::Rgb(r, g, b)) => Some((r, g, b)),
                    _ => None,
                };
                let mut run_start = start;
                while run_start < end {
                    let style = styles[run_start];
                    let mut run_end = run_start + 1;
                    while run_end < end && styles[run_end] == style {
                        run_end += 1;
                    }
                    app.squiggle_overlays.push(crate::app::Squiggle {
                        x: text_area.x + run_start as u16,
                        y: inner.y + row as u16,
                        text: visible_chars[run_start..run_end].iter().collect(),
                        fg: as_rgb(style.fg),
                        bg: as_rgb(style.bg),
                        curl,
                    });
                    run_start = run_end;
                }
            }
            // spell squiggles: same wavy overlay, muted color
            let visible_chars: Vec<char> = visible.chars().collect();
            let as_rgb = |c: Option<Color>| match c {
                Some(Color::Rgb(r, g, b)) => Some((r, g, b)),
                _ => None,
            };
            let mut wavy = |ranges: &[(usize, usize)], curl: (u8, u8, u8)| {
                for &(s, e) in ranges {
                    let end = e.min(visible_chars.len());
                    let mut run_start = s.min(end);
                    while run_start < end {
                        let style = styles[run_start];
                        let mut run_end = run_start + 1;
                        while run_end < end && styles[run_end] == style {
                            run_end += 1;
                        }
                        app.squiggle_overlays.push(crate::app::Squiggle {
                            x: text_area.x + run_start as u16,
                            y: inner.y + row as u16,
                            text: visible_chars[run_start..run_end].iter().collect(),
                            fg: as_rgb(style.fg),
                            bg: as_rgb(style.bg),
                            curl,
                        });
                        run_start = run_end;
                    }
                }
            };
            wavy(&spell_ranges, SPELL_CURL());
        }
    }

    // real terminal cursor on the text — only when the pane is focused
    // and nothing (overlay or leader menu) is drawn on top
    let cursor_allowed = focused && app.overlay.is_none() && !app.leader_pending;
    if cursor_allowed && editor.command.is_none() && cursor_line >= scroll && cursor_col >= hscroll {
        let row = (cursor_line - scroll) as u16;
        if row < text_area.height {
            let col = (cursor_col - hscroll) as u16;
            let x = text_area.x + col.min(text_area.width.saturating_sub(1));
            frame.set_cursor_position((x, text_area.y + row));
        }
    }

    // scrollbars drawn over the pane borders: vertical right, horizontal
    // bottom (both hide themselves when everything fits)
    let vbar = Rect::new(
        pane.right().saturating_sub(1),
        pane.y + 1,
        1,
        pane.height.saturating_sub(2),
    );
    draw_pane_scrollbar(frame, vbar, total_lines, text_height, scroll);
    let hbar = Rect::new(text_area.x, pane.bottom().saturating_sub(1), text_area.width, 1);
    draw_pane_hscrollbar(frame, hbar, widest, text_area.width as usize, hscroll);
}

/// Editor variant of the bottom bar: mode │ file │ diagnostics … E/W · pos
/// · language · branch. The `:` command line also renders here.
/// Read-only hex viewer: structure tree on the left (for recognized
/// formats), offset + hex + ascii dump on the right. The selected tree
/// node's byte range is tinted in the dump.
fn draw_hex_view(frame: &mut Frame, app: &mut App, pane: Rect) {
    use crate::hex::HexFocus;
    let focused = app.focus == Focus::Terminal;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style(focused));
    let inner = block.inner(pane);
    frame.render_widget(block, pane);
    let Some(hex) = &mut app.hex else { return };
    if inner.height < 1 || inner.width < 14 {
        return;
    }

    let dim = Color::Rgb(105, 110, 122);
    let label_fg = Color::Rgb(150, 156, 168);
    let byte_fg = Color::Rgb(190, 195, 205);
    let zero_fg = Color::Rgb(80, 84, 94);
    let select_bg = Color::Rgb(58, 62, 78);
    let rule_fg = Color::Rgb(58, 61, 70);

    // hex layout: dump on top, "pattern data" table pane at the bottom
    let has_tree = !hex.nodes.is_empty();
    let body_rows = if has_tree && inner.height >= 10 {
        ((inner.height as usize) / 3).clamp(3, 12)
    } else {
        0
    };
    // pattern pane = rule + header + body
    let pattern_h = if body_rows > 0 { body_rows as u16 + 2 } else { 0 };
    let dump = Rect::new(inner.x, inner.y, inner.width, inner.height - pattern_h);
    // right column of each pane is its scrollbar
    let dump_body = Rect::new(dump.x, dump.y, dump.width.saturating_sub(1), dump.height);
    app.layout.hex_dump = dump_body;

    // 8 hex offset + 2 gap + N*3 hex (+1 mid gap) + 2 gap + N ascii
    let fits = |n: u16| 8 + 2 + n * 3 + 1 + 2 + n <= dump_body.width;
    let bpr = if fits(16) { 16 } else if fits(8) { 8 } else { 4 };
    hex.bytes_per_row = bpr as usize;
    hex.viewport_rows = dump_body.height as usize;
    let max_scroll = hex.total_rows().saturating_sub(hex.viewport_rows);
    hex.scroll = hex.scroll.min(max_scroll);

    // every pattern gets a color; its bytes wear it in the dump
    let node_dark = |i: usize| pattern_color(i).0;
    let node_bright = |i: usize| pattern_color(i).1;
    let highlight = hex.selected_range();
    let in_sel = |o: usize| highlight.is_some_and(|(s, e)| o >= s && o < e);
    let byte_bg = |hex: &crate::hex::HexView, o: usize| -> Option<Color> {
        if o >= hex.data.len() {
            return None;
        }
        if in_sel(o) {
            return Some(node_bright(hex.selected));
        }
        hex.covering_node(o).map(node_dark)
    };

    for row in 0..dump_body.height as usize {
        let base_offset = (hex.scroll + row) * hex.bytes_per_row;
        if base_offset >= hex.data.len() {
            break;
        }
        let mut spans: Vec<Span> = vec![
            Span::styled(format!("{base_offset:08x}"), Style::default().fg(dim)),
            Span::raw("  "),
        ];
        for i in 0..hex.bytes_per_row {
            if i > 0 && i % 8 == 0 {
                spans.push(Span::raw(" "));
            }
            let offset = base_offset + i;
            match hex.data.get(offset) {
                Some(&b) => {
                    let bg = byte_bg(hex, offset);
                    let fg = match (b, bg.is_some(), in_sel(offset)) {
                        (_, _, true) => Color::Rgb(235, 238, 245),
                        (0, false, _) => zero_fg,
                        (0, true, _) => Color::Rgb(140, 146, 158),
                        _ => byte_fg,
                    };
                    let mut style = Style::default().fg(fg);
                    if let Some(bg) = bg {
                        style = style.bg(bg);
                    }
                    spans.push(Span::styled(format!("{b:02x}"), style));
                    // the gap joins the tint inside a colored run
                    let gap_bg = bg.filter(|_| (i + 1) % 8 != 0 && byte_bg(hex, offset + 1) == bg);
                    let gap = match gap_bg {
                        Some(bg) => Style::default().bg(bg),
                        None => Style::default(),
                    };
                    spans.push(Span::styled(" ", gap));
                }
                None => spans.push(Span::raw("   ")),
            }
        }
        spans.push(Span::raw(" "));
        for i in 0..hex.bytes_per_row {
            let offset = base_offset + i;
            let Some(&b) = hex.data.get(offset) else { break };
            let (ch, fg) = if (0x20..0x7f).contains(&b) {
                (b as char, label_fg)
            } else {
                ('·', zero_fg)
            };
            let mut style = Style::default().fg(fg);
            if let Some(bg) = byte_bg(hex, offset) {
                style = style.bg(bg);
                if in_sel(offset) {
                    style = style.fg(Color::Rgb(235, 238, 245));
                }
            }
            spans.push(Span::styled(ch.to_string(), style));
        }
        frame.render_widget(
            Paragraph::new(Line::from(spans)),
            Rect::new(dump_body.x, dump_body.y + row as u16, dump_body.width, 1),
        );
    }
    draw_pane_scrollbar(frame, dump, hex.total_rows(), hex.viewport_rows, hex.scroll);

    if body_rows == 0 {
        app.layout.hex_tree = Rect::default();
        return;
    }

    // ── pattern data pane ──────────────────────────────────────────────
    let rule_y = dump.bottom();
    frame.render_widget(
        Paragraph::new(Span::styled(
            "─".repeat(inner.width as usize),
            Style::default().fg(rule_fg),
        )),
        Rect::new(inner.x, rule_y, inner.width, 1),
    );
    let table_area = Rect::new(
        inner.x,
        rule_y + 1,
        inner.width.saturating_sub(1),
        body_rows as u16 + 1,
    );
    // click hit-testing targets the body rows below the header
    app.layout.hex_tree = Rect::new(table_area.x, table_area.y + 1, table_area.width, body_rows as u16);

    let guide_fg = Color::Rgb(70, 74, 84);
    let rows: Vec<ratatui::widgets::Row> = hex
        .nodes
        .iter()
        .enumerate()
        .map(|(i, node)| {
            // └╴/├╴ connectors with │ guides through open ancestors
            let mut prefix = String::from(" ");
            for level in 1..node.depth {
                let ancestor = (0..i).rev().find(|&j| hex.nodes[j].depth == level);
                let open = ancestor.is_some_and(|j| !hex.is_last_sibling(j));
                prefix.push_str(if open { "│ " } else { "  " });
            }
            if node.depth > 0 {
                prefix.push_str(if hex.is_last_sibling(i) { "└╴" } else { "├╴" });
            }
            let name_style = if i == hex.selected && hex.focus == HexFocus::Tree {
                Style::default().fg(Color::Rgb(220, 225, 235)).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(label_fg)
            };
            ratatui::widgets::Row::new(vec![
                ratatui::widgets::Cell::from(Line::from(vec![
                    Span::styled(prefix, Style::default().fg(guide_fg)),
                    Span::styled(node.name.clone(), name_style),
                ])),
                ratatui::widgets::Cell::from(Span::styled(
                    if node.depth > 0 { "▆▆" } else { "" },
                    Style::default().fg(node_bright(i)),
                )),
                ratatui::widgets::Cell::from(Span::styled(
                    format!("{:#08x} : {:#08x}", node.start, node.end),
                    Style::default().fg(dim),
                )),
                ratatui::widgets::Cell::from(Span::styled(
                    format!("{:#x}", node.end - node.start),
                    Style::default().fg(dim),
                )),
                ratatui::widgets::Cell::from(Span::styled(
                    node.ty.clone(),
                    Style::default().fg(Color::Rgb(229, 192, 123)),
                )),
                ratatui::widgets::Cell::from(Span::styled(
                    node.detail.clone(),
                    Style::default().fg(label_fg),
                )),
            ])
        })
        .collect();
    let name_w = ((table_area.width as usize) * 18 / 100).clamp(11, 30) as u16;
    let table = ratatui::widgets::Table::new(
        rows,
        [
            Constraint::Length(name_w),
            Constraint::Length(5),
            Constraint::Length(19),
            Constraint::Length(6),
            Constraint::Length(11),
            Constraint::Fill(1),
        ],
    )
    .header(
        ratatui::widgets::Row::new(vec![" Name", "Color", "Offset", "Size", "Type", "Value"])
            .style(Style::default().fg(dim)),
    )
    .row_highlight_style(Style::default().bg(select_bg));
    let mut state = ratatui::widgets::TableState::new()
        .with_offset(hex.tree_scroll)
        .with_selected(Some(hex.selected));
    frame.render_stateful_widget(table, table_area, &mut state);
    // the table keeps the selection visible: adopt its offset for clicks
    hex.tree_scroll = state.offset();
    let pattern_pane = Rect::new(table_area.x, table_area.y + 1, inner.width, body_rows as u16);
    draw_pane_scrollbar(frame, pattern_pane, hex.nodes.len(), body_rows, hex.tree_scroll);
}

/// Pattern palette: (dim byte-background, bright accent) per
/// node index, cycling through distinguishable hues.
fn pattern_color(node: usize) -> (Color, Color) {
    type Rgb = (u8, u8, u8);
    const COLORS: [(Rgb, Rgb); 8] = [
        ((96, 48, 48), (150, 75, 75)),   // red
        ((52, 82, 50), (82, 130, 78)),   // green
        ((92, 84, 40), (145, 132, 62)),  // yellow
        ((44, 62, 92), (70, 98, 145)),   // blue
        ((84, 50, 88), (132, 78, 138)),  // magenta
        ((42, 80, 84), (66, 126, 132)),  // cyan
        ((96, 64, 38), (150, 100, 60)),  // orange
        ((60, 56, 92), (94, 88, 145)),   // purple
    ];
    let ((dr, dg, db), (br, bg, bb)) = COLORS[node.saturating_sub(1) % COLORS.len()];
    (Color::Rgb(dr, dg, db), Color::Rgb(br, bg, bb))
}

/// Vertical scrollbar on the right edge of `pane`, hidden when everything
/// fits.
fn draw_pane_scrollbar(frame: &mut Frame, pane: Rect, total: usize, viewport: usize, scroll: usize) {
    use ratatui::widgets::{Scrollbar, ScrollbarOrientation, ScrollbarState};
    if total <= viewport || pane.height == 0 || pane.width == 0 {
        return;
    }
    let mut state = ScrollbarState::new(total.saturating_sub(viewport))
        .viewport_content_length(viewport)
        .position(scroll);
    frame.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .track_symbol(Some("│"))
            .thumb_symbol("┃")
            .thumb_style(Style::default().fg(adaptive(Color::Rgb(96, 101, 112), Color::Rgb(150, 155, 166))))
            .track_style(Style::default().fg(adaptive(Color::Rgb(44, 47, 56), Color::Rgb(216, 219, 226)))),
        pane,
        &mut state,
    );
}

/// Horizontal twin of draw_pane_scrollbar, on the bottom edge of `pane`.
fn draw_pane_hscrollbar(frame: &mut Frame, pane: Rect, total: usize, viewport: usize, scroll: usize) {
    use ratatui::widgets::{Scrollbar, ScrollbarOrientation, ScrollbarState};
    if total <= viewport || pane.height == 0 || pane.width == 0 {
        return;
    }
    let mut state = ScrollbarState::new(total.saturating_sub(viewport))
        .viewport_content_length(viewport)
        .position(scroll);
    frame.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::HorizontalBottom)
            .begin_symbol(None)
            .end_symbol(None)
            .track_symbol(Some("─"))
            .thumb_symbol("━")
            .thumb_style(Style::default().fg(adaptive(Color::Rgb(96, 101, 112), Color::Rgb(150, 155, 166))))
            .track_style(Style::default().fg(adaptive(Color::Rgb(44, 47, 56), Color::Rgb(216, 219, 226)))),
        pane,
        &mut state,
    );
}

/// The app bar while the hex viewer is open: HEX chip, file, selected
/// section with its byte range, total size.
fn draw_hex_status_bar(frame: &mut Frame, app: &App, hex: &crate::hex::HexView, area: Rect) {
    let chip_bg = Color::Rgb(180, 142, 173);
    let mut left = vec![
        Span::styled(
            " HEX ",
            Style::default().bg(chip_bg).fg(Color::Rgb(20, 22, 28)).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            if crate::color::fancy_glyphs() { "◤" } else { "" },
            Style::default().fg(chip_bg),
        ),
        Span::raw(format!(" {} [read-only]", hex.file_name())),
    ];
    if let Some(msg) = &app.status_msg {
        left.push(Span::styled(format!("  {msg}"), Style::default().fg(Color::Yellow)));
    }
    let mut right: Vec<Span> = Vec::new();
    if let Some(node) = hex.nodes.get(hex.selected) {
        right.push(Span::styled(
            format!("{}  {:#x}..{:#x}  ", node.name, node.start, node.end),
            Style::default().fg(STATUS_DIM()),
        ));
    }
    right.push(Span::styled(
        crate::hex::human_size(hex.data.len()),
        Style::default().fg(STATUS_DIM()),
    ));
    let left_width: usize = left.iter().map(|s| s.content.chars().count()).sum();
    let right_width: usize = right.iter().map(|s| s.content.chars().count()).sum();
    let pad = (area.width as usize).saturating_sub(left_width + right_width + 1);
    left.push(Span::raw(" ".repeat(pad)));
    left.extend(right);
    left.push(Span::raw(" "));
    frame.render_widget(
        Paragraph::new(Line::from(left)).style(Style::default().bg(STATUSBAR_BG())),
        area,
    );
}

fn draw_editor_status_bar(frame: &mut Frame, app: &App, editor: &crate::editor::Editor, area: Rect) {
    // `:` command line takes over the whole bar
    if let Some(cmd) = &editor.command {
        frame.render_widget(
            Paragraph::new(Line::from(Span::raw(format!(" :{cmd}"))))
                .style(Style::default().bg(STATUSBAR_BG())),
            area,
        );
        if app.overlay.is_none() && !app.leader_pending {
            frame.set_cursor_position((area.x + 2 + cmd.chars().count() as u16, area.y));
        }
        return;
    }
    let diagnostics = match &app.lsp {
        Some(client) => client.diagnostics(&editor.path),
        None => Vec::new(),
    };
    let (cursor_line, cursor_col) = editor.cursor_line_col();
    let dirty = if editor.dirty { " [+]" } else { "" };
    let (chip_bg, chip_fg) = mode_colors(editor.mode);
    let mut left = vec![
        Span::styled(
            format!(" {} ", editor.mode.label()),
            Style::default().bg(chip_bg).fg(chip_fg).add_modifier(Modifier::BOLD),
        ),
        // angled cap, same slant language as the dialog badges
        Span::styled(
            if crate::color::fancy_glyphs() { "◤" } else { "" },
            Style::default().fg(chip_bg),
        ),
        Span::raw(format!(" {}{}", editor.file_name(), dirty)),
    ];
    if let Some(msg) = &app.status_msg {
        left.push(Span::styled(
            format!("  {msg}"),
            Style::default().fg(Color::Yellow),
        ));
    } else if let Some(msg) = &editor.status {
        left.push(Span::styled(
            format!("  {msg}"),
            Style::default().fg(Color::Yellow),
        ));
    } else if let Some(diag) = diagnostics.iter().find(|d| d.line == cursor_line) {
        let msg: String = diag.message.replace('\n', " ").chars().take(80).collect();
        left.push(Span::styled(
            format!("  ■ {msg}"),
            Style::default().fg(severity_color(diag.severity)),
        ));
    }
    let errors = diagnostics.iter().filter(|d| d.severity == 1).count();
    let warnings = diagnostics.iter().filter(|d| d.severity == 2).count();
    let mut right: Vec<Span> = Vec::new();
    // LSP work-done progress (indexing, cargo check…) — a live server status
    if let Some(prog) = app.lsp.as_ref().and_then(|c| c.progress()) {
        let prog: String = prog.chars().take(40).collect();
        let spin = if crate::color::fancy_glyphs() { "⟳ " } else { "" };
        right.push(Span::styled(
            format!("{spin}{prog}   "),
            Style::default().fg(STATUS_DIM()),
        ));
    }
    if errors > 0 {
        right.push(Span::styled(
            format!("E {errors} "),
            Style::default().fg(severity_color(1)),
        ));
    }
    if warnings > 0 {
        right.push(Span::styled(
            format!("W {warnings} "),
            Style::default().fg(severity_color(2)),
        ));
    }
    right.push(Span::styled(
        format!(
            "{}:{}  {}",
            cursor_line + 1,
            cursor_col + 1,
            crate::editor::highlight::language_name(&editor.path)
        ),
        Style::default().fg(STATUS_DIM()),
    ));
    if let Some(branch) = &app.git.branch {
        right.push(Span::styled(
            format!("   {branch} "),
            Style::default().fg(Color::Magenta),
        ));
    }
    let left_width: usize = left.iter().map(|s| s.content.chars().count()).sum();
    let right_width: usize = right.iter().map(|s| s.content.chars().count()).sum();
    let pad = (area.width as usize).saturating_sub(left_width + right_width);
    left.push(Span::raw(" ".repeat(pad)));
    left.extend(right);
    frame.render_widget(
        Paragraph::new(Line::from(left)).style(Style::default().bg(STATUSBAR_BG())),
        area,
    );
}

fn draw_status_bar(frame: &mut Frame, app: &App, area: Rect) {
    // with the editor or hex viewer active, the app bar IS its statusline
    if app.screen == Screen::Workspace && app.shell == Shell::Code {
        if let Some(hex) = &app.hex {
            draw_hex_status_bar(frame, app, hex, area);
            return;
        }
        if let Some(editor) = &app.editor {
            draw_editor_status_bar(frame, app, editor, area);
            return;
        }
    }
    let mut spans: Vec<Span> = Vec::new();
    if app.leader_pending {
        spans.push(Span::styled(
            " LEADER ",
            Style::default().fg(Color::Black).bg(Color::Yellow).add_modifier(Modifier::BOLD),
        ));
    } else {
        spans.push(Span::styled(
            format!(" {} ", app.shell.label()),
            Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD),
        ));
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
    }
    spans.push(Span::styled(
        format!(" · {}", app.workdir.display()),
        Style::default().fg(Color::DarkGray),
    ));
    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(STATUSBAR_BG())),
        area,
    );
}

/// Grayish base for dialog surfaces — distinct from the terminal background
/// so popups read as a layer above the workspace.
#[allow(non_snake_case)]
fn DIALOG_BG() -> Color {
    wash(30).unwrap_or_else(|| adaptive(Color::Rgb(40, 42, 48), Color::Rgb(238, 239, 243)))
}
/// Square-cornered border around every dialog.
#[allow(non_snake_case)]
fn DIALOG_BORDER() -> Color {
    wash(110).unwrap_or_else(|| adaptive(Color::Rgb(96, 100, 112), Color::Rgb(152, 156, 166)))
}

/// Hairline border drawn with eighth-block glyphs hugging the cell edges
/// (exabind-style) — line-drawing chars like ┌ stroke through the cell
/// center, which reads as an inset frame.
const HAIRLINE: ratatui::symbols::border::Set = ratatui::symbols::border::Set {
    top_left: "▔",
    top_right: "▜",
    bottom_left: "▙",
    bottom_right: "▟",
    vertical_left: "▏",
    vertical_right: "▕",
    horizontal_top: "▔",
    horizontal_bottom: "▁",
};

/// Rainbow frame: the border gradient and the title badge sample the SAME
/// perimeter gradient, so one continuous rainbow flows through badge and
/// border alike. Render after the dialog body.
fn draw_dialog_frame(frame: &mut Frame, rect: Rect, title: &str, phase: f32) {
    let fancy = crate::color::fancy_glyphs();
    rainbow_border(frame, rect, phase);
    if title.is_empty() {
        return;
    }
    // the badge occupies the first cells of the top border: give each char
    // the border color it replaces (cell i of the perimeter)
    let len = (2 * (rect.width as usize + rect.height as usize) - 4).max(1) as f32;
    let at = |i: usize| gradient_color(phase + i as f32 / len);
    let content = format!(" {} ", title.trim());
    let mut spans = vec![if fancy {
        Span::styled("◢", Style::default().fg(at(0)).bg(Color::Reset))
    } else {
        Span::styled(" ", Style::default().bg(Color::Reset))
    }];
    let mut i = 0usize;
    for c in content.chars() {
        i += 1;
        spans.push(Span::styled(
            c.to_string(),
            Style::default()
                .bg(at(i))
                .fg(Color::Rgb(8, 8, 10))
                .add_modifier(Modifier::BOLD),
        ));
    }
    spans.push(if fancy {
        Span::styled("◤", Style::default().fg(at(i + 1)))
    } else {
        Span::styled(" ", Style::default())
    });
    let width = (i as u16 + 2).min(rect.width);
    let badge_rect = Rect::new(rect.x, rect.y, width, 1).intersection(frame.area());
    frame.render_widget(Paragraph::new(Line::from(spans)), badge_rect);
}

/// Standard dialog chrome: hairline borders on the gray base — plain
/// box-drawing borders on terminals whose fonts can't render the fancy set.
fn dialog_block() -> Block<'static> {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(DIALOG_BORDER()))
        .padding(Padding::horizontal(1));
    if crate::color::fancy_glyphs() {
        block.border_set(HAIRLINE)
    } else {
        block
    }
}

/// Repaint a dialog's border cells with the flowing pastel gradient —
/// exabind-style color cycling along the perimeter.
fn rainbow_border(frame: &mut Frame, rect: Rect, phase: f32) {
    if rect.width < 2 || rect.height < 2 {
        return;
    }
    let buf = frame.buffer_mut();
    let mut cells: Vec<(u16, u16)> = Vec::new();
    for x in rect.left()..rect.right() {
        cells.push((x, rect.top()));
    }
    for y in rect.top() + 1..rect.bottom() {
        cells.push((rect.right() - 1, y));
    }
    for x in (rect.left()..rect.right() - 1).rev() {
        cells.push((x, rect.bottom() - 1));
    }
    for y in (rect.top() + 1..rect.bottom() - 1).rev() {
        cells.push((rect.left(), y));
    }
    let len = cells.len().max(1) as f32;
    for (i, (x, y)) in cells.into_iter().enumerate() {
        let t = i as f32 / len + phase;
        buf[(x, y)].set_fg(gradient_color(t));
    }
}

/// Paint the dialog's base surface. Call before rendering the dialog's
/// content into `rect`.
fn draw_dialog_base(frame: &mut Frame, rect: Rect) {
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Block::default().style(Style::default().bg(DIALOG_BG())),
        rect,
    );
}


// Claude Code-style diff palette: tinted full-width rows on dark background.
// One accent per side, used for BOTH the line number and the +/- marker, so
// the greens/reds always match (ANSI theme colors would drift from the RGB
// backgrounds).
/// Diff accents from the terminal's ANSI green/red when available, and
/// backgrounds blended from that accent and the real terminal background
/// (~18% accent) — theme-true tints in both light and dark schemes.
#[allow(non_snake_case)]
fn ADD_ACCENT() -> Color {
    match crate::color::ansi16(2) {
        Some((r, g, b)) => Color::Rgb(r, g, b),
        None => adaptive(Color::Rgb(110, 190, 110), Color::Rgb(40, 130, 60)),
    }
}
#[allow(non_snake_case)]
fn REMOVE_ACCENT() -> Color {
    match crate::color::ansi16(1) {
        Some((r, g, b)) => Color::Rgb(r, g, b),
        None => adaptive(Color::Rgb(210, 120, 120), Color::Rgb(180, 60, 60)),
    }
}
#[allow(non_snake_case)]
fn ADD_BG() -> Color {
    diff_tint(2, Color::Rgb(16, 42, 16), Color::Rgb(228, 244, 228))
}
#[allow(non_snake_case)]
fn REMOVE_BG() -> Color {
    diff_tint(1, Color::Rgb(52, 18, 18), Color::Rgb(250, 230, 230))
}
fn diff_tint(slot: usize, dark_fb: Color, light_fb: Color) -> Color {
    match (crate::color::ansi16(slot), crate::color::terminal_bg()) {
        (Some((r, g, b)), Some((br, bgc, bb))) => {
            let mix = |c: u8, base: u8| ((c as u32 * 45 + base as u32 * 211) / 256) as u8;
            Color::Rgb(mix(r, br), mix(g, bgc), mix(b, bb))
        }
        _ => adaptive(dark_fb, light_fb),
    }
}

/// Syntax-colored spans for a diff content line, on the given background.
fn diff_code_spans(line: &DiffLine, bg: Option<Color>) -> Vec<Span<'static>> {
    let base = match bg {
        Some(bg) => Style::default().bg(bg),
        None => Style::default(),
    };
    let Some(lang) = line.lang else {
        return vec![Span::styled(line.text.clone(), base)];
    };
    let spans = crate::editor::highlight::line_spans(lang, &line.text);
    if spans.is_empty() {
        return vec![Span::styled(line.text.clone(), base)];
    }
    let text = &line.text;
    let mut styles = vec![base; text.chars().count()];
    for span in &spans {
        let s = text.get(..span.start).map(|t| t.chars().count()).unwrap_or(0);
        let e = text.get(..span.end.min(text.len())).map(|t| t.chars().count()).unwrap_or(s);
        let style = crate::editor::highlight::style_for(span.highlight);
        if style.fg.is_none() {
            continue;
        }
        let style = match bg {
            Some(bg) => style.bg(bg),
            None => style,
        };
        for slot in styles.iter_mut().take(e.min(text.chars().count())).skip(s) {
            *slot = style;
        }
    }
    let mut segments: Vec<Span> = Vec::new();
    for (c, style) in text.chars().zip(styles.iter()) {
        match segments.last_mut() {
            Some(last) if last.style == *style => last.content.to_mut().push(c),
            _ => segments.push(Span::styled(c.to_string(), *style)),
        }
    }
    if segments.is_empty() {
        segments.push(Span::styled(String::new(), base));
    }
    segments
}

/// Render one parsed diff line as a full-width styled row with a
/// line-number gutter, like Claude Code's change view.
fn render_diff_line(line: &DiffLine, width: usize) -> Line<'static> {
    let pad = |text: &str| {
        let visible = text.chars().count();
        let fill = width.saturating_sub(visible + 8);
        " ".repeat(fill)
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
            let bg = Style::default().bg(ADD_BG());
            let mut spans = vec![
                Span::styled(
                    format!("{:>5} ", line.new_no.unwrap_or(0)),
                    bg.fg(ADD_ACCENT()),
                ),
                Span::styled("+ ", bg.fg(ADD_ACCENT()).add_modifier(Modifier::BOLD)),
            ];
            spans.extend(diff_code_spans(line, Some(ADD_BG())));
            spans.push(Span::styled(pad(&line.text), bg));
            Line::from(spans)
        }
        DiffLineKind::Remove => {
            let bg = Style::default().bg(REMOVE_BG());
            let mut spans = vec![
                Span::styled(
                    format!("{:>5} ", line.old_no.unwrap_or(0)),
                    bg.fg(REMOVE_ACCENT()),
                ),
                Span::styled("- ", bg.fg(REMOVE_ACCENT()).add_modifier(Modifier::BOLD)),
            ];
            spans.extend(diff_code_spans(line, Some(REMOVE_BG())));
            spans.push(Span::styled(pad(&line.text), bg));
            Line::from(spans)
        }
        DiffLineKind::Context => {
            let mut spans = vec![
                Span::styled(
                    format!("{:>5} ", line.new_no.unwrap_or(0)),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::raw("  "),
            ];
            spans.extend(diff_code_spans(line, None));
            Line::from(spans)
        }
    }
}

fn draw_diff_overlay(frame: &mut Frame, app: &mut App) {
    // full-screen: the border sits at the terminal edge
    let area = frame.area();
    draw_dialog_base(frame, area);
    let Some(Overlay::Diff(view)) = &app.overlay else {
        return;
    };
    // bordered: 2 rows for the frame, 2 cols border + 2 cols padding
    let inner_height = area.height.saturating_sub(2) as usize;
    let inner_width = area.width.saturating_sub(4) as usize;
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
    let (diff_total, diff_scroll) = (view.lines.len(), view.scroll);
    let paragraph = Paragraph::new(visible)
        .style(Style::default().bg(DIALOG_BG()))
        .block(dialog_block());
    frame.render_widget(paragraph, area);
    draw_dialog_frame(frame, area, &title, app.welcome.phase);
    // vertical scrollbar over the right border of the dialog
    let bar = Rect::new(
        area.right().saturating_sub(1),
        area.y + 1,
        1,
        area.height.saturating_sub(2),
    );
    draw_pane_scrollbar(frame, bar, diff_total, inner_height, diff_scroll);
}

fn draw_help_overlay(frame: &mut Frame, phase: f32) {
    let lines = vec![
        ("Ctrl+K", "command palette: files · > commands"),
        ("Ctrl+A c", "new Claude session"),
        ("Ctrl+A x", "close active session"),
        ("Ctrl+A n / p", "next / previous session"),
        ("Ctrl+A 1..9", "jump to session"),
        ("Ctrl+A r", "rename session"),
        ("Ctrl+A R", "respawn exited session in place"),
        ("F1 / Ctrl+A h", "agents shell: chats · claude terminals"),
        ("F2 / Ctrl+A g", "git shell: changes · diff"),
        ("F3 / Ctrl+A f", "code shell: file tree · editor"),
        ("Ctrl+A e", "focus the editor tab"),
        ("Ctrl+A d", "diff of all changes"),
        ("Ctrl+A k / j", "scroll terminal up / down"),
        ("Ctrl+A u", "refresh tree + git + chats"),
        ("Ctrl+A Ctrl+A", "send literal Ctrl+A"),
        ("Ctrl+A q", "quit"),
        ("", ""),
        ("status dots: ● working · ● needs you (bell) · ○ idle · ✖ exited", ""),
        ("files: j/k move · Enter open file / toggle dir · h parent · . hidden · d diff", ""),
        ("editor: modal keys — hjkl/wbe move · x line · d/c/y/p edit · i/a insert", ""),
        ("        v select · u/U undo/redo · gg/ge top/bottom · :w :q :wq", ""),
        ("        hover docs: rest the mouse on a symbol (or space-k) · diagnostics inline", ""),
        ("        gd / ctrl+click goto definition · ctrl+o jump back", ""),
        ("git: j/k move · s stage · a stage all · c commit · Enter diff · t list/tree", ""),
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
        Paragraph::new(text).style(Style::default().bg(DIALOG_BG())).block(
            dialog_block(),
        ),
        rect,
    );
    draw_dialog_frame(frame, rect, "", phase);
}

fn draw_prompt(frame: &mut Frame, title: &str, buf: &str, phase: f32) {
    let area = frame.area();
    let width = 60.min(area.width.saturating_sub(4));
    let rect = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + area.height / 2 - 1,
        width,
        height: 3, // border(title) + input row + border
    };
    draw_dialog_base(frame, rect);
    frame.render_widget(
        Paragraph::new(Line::from(Span::raw(buf.to_string())))
            .style(Style::default().bg(DIALOG_BG()))
            .block(dialog_block()),
        rect,
    );
    draw_dialog_frame(frame, rect, title, phase);
    let cursor_x = rect.x + 2 + (buf.chars().count() as u16).min(rect.width.saturating_sub(4));
    frame.set_cursor_position((cursor_x, rect.y + 1));
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn render(app: &mut App) -> ratatui::buffer::Buffer {
        unsafe { std::env::set_var("VIBIN_FANCY", "1") };
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

    /// Quadrant corner cells of the hairline dialog border.
    fn border_count(buf: &ratatui::buffer::Buffer) -> usize {
        buf.content()
            .iter()
            .filter(|cell| {
                matches!(cell.symbol(), "▜" | "▙" | "▟")
                    && matches!(cell.style().fg, Some(Color::Rgb(..)))
            })
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
    fn dialogs_have_gray_base_and_plain_border() {
        let (_dir, mut app) = test_app();
        // no overlay → neither dialog base nor shadow anywhere
        let buf = render(&mut app);
        assert_eq!(bg_count(&buf, DIALOG_BG()), 0);
        assert_eq!(border_count(&buf), 0);

        app.overlay = Some(Overlay::Help);
        let buf = render(&mut app);
        assert!(bg_count(&buf, DIALOG_BG()) > 100, "dialog surface painted");
        // four square corners of the plain border
        assert_eq!(border_count(&buf), 3, "hairline border + slant corners drawn");
    }

    #[test]
    fn hex_view_renders_tree_dump_and_highlight() {
        let (dir, mut app) = test_app();
        let path = dir.path().join("m.wasm");
        let mut wasm: Vec<u8> = b"\0asm\x01\0\0\0".to_vec();
        wasm.extend_from_slice(&[1, 4, 1, 0x60, 0, 0]);
        std::fs::write(&path, wasm).unwrap();
        app.open_file(&path);
        let buf = render(&mut app);
        let text: String = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(text.contains("wasm"), "pattern table shows the root node");
        assert!(text.contains("header"));
        assert!(text.contains("Name"), "pattern table has column headers");
        assert!(text.contains("Offset"));
        assert!(text.contains("▆▆"), "color swatches render");
        assert!(text.contains("0x000000 : 0x000008"), "header row shows its range");
        assert!(text.contains("00000000"), "dump shows offsets");
        assert!(text.contains("61 73 6d"), "dump shows the magic bytes");
        assert!(text.contains("·asm"), "ascii column renders");
        assert!(text.contains(" HEX "), "status bar shows the HEX chip");
        assert!(text.contains("Type"), "type column present");
        // the 11-wide type column clips "wasm_section[3]" at this width
        assert!(text.contains("wasm_sectio"), "section array type shown");
        // the value column is clipped at this narrow test width; the full
        // "1 (0x1)" string is asserted against the model in hex::tests
        assert!(text.contains("1 (0x"), "scalar value decoded");
        // the magic bytes wear their pattern color (innermost)
        let magic = app.hex.as_ref().unwrap().nodes.iter().position(|n| n.name == "magic").unwrap();
        assert!(bg_count(&buf, pattern_color(magic).0) >= 4, "magic bytes tinted");
        // selecting the type section brightens its byte range in the dump
        let hex = app.hex.as_mut().unwrap();
        let section = hex.nodes.iter().position(|n| n.name == "type").unwrap();
        hex.select_node(section);
        let bright = pattern_color(section).1;
        let buf = render(&mut app);
        assert!(bg_count(&buf, bright) >= 6, "selection highlight painted");
    }

    fn logo_colors(buf: &ratatui::buffer::Buffer) -> std::collections::HashSet<Color> {
        buf.content()
            .iter()
            .filter(|cell| matches!(cell.symbol(), "█" | "╗" | "║" | "╚" | "═" | "╝" | "╔"))
            .filter_map(|cell| match cell.style().fg {
                Some(fg @ Color::Rgb(..)) => Some(fg),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn welcome_wordmark_has_smooth_animated_gradient() {
        let (_dir, mut app) = test_app();
        app.screen = Screen::Welcome;
        let buf = render(&mut app);
        let colors = logo_colors(&buf);
        // a smooth gradient produces many distinct colors, not a few bands
        assert!(colors.len() > 15, "{} distinct colors", colors.len());
        // advancing the phase shifts the colors → animation frames differ
        app.welcome.phase = 0.5;
        let shifted = logo_colors(&render(&mut app));
        assert_ne!(colors, shifted, "phase change must move the gradient");
    }

    #[test]
    fn gradient_wraps_cyclically() {
        // same color at t and t+1 → the animation loops without a seam
        assert_eq!(gradient_color(0.25), gradient_color(1.25));
        assert_eq!(gradient_color(0.0), gradient_color(1.0));
    }

    #[test]
    fn editor_pane_gets_scrollbars_when_content_overflows() {
        let (dir, mut app) = test_app();
        let path = dir.path().join("long.txt");
        // 100 lines; the first is 300 chars wide (the horizontal range is
        // measured over *visible* lines): both bars must appear
        let mut body = "wide ".repeat(60);
        body.push('\n');
        body.push_str(&(0..100).map(|i| format!("line {i}\n")).collect::<String>());
        std::fs::write(&path, body).unwrap();
        app.open_file(&path);
        let buf = render(&mut app);
        let cells: Vec<&str> = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(cells.iter().any(|s| *s == "┃"), "vertical thumb rendered");
        assert!(cells.iter().any(|s| *s == "━"), "horizontal thumb rendered");
        // a short file shows neither
        let small = dir.path().join("small.txt");
        std::fs::write(&small, "hi\n").unwrap();
        app.open_file(&small);
        let buf = render(&mut app);
        let cells: Vec<&str> = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(!cells.iter().any(|s| *s == "┃"), "no vertical thumb when it fits");
        assert!(!cells.iter().any(|s| *s == "━"), "no horizontal thumb when it fits");
    }

    #[test]
    fn cursor_line_gets_a_dim_background() {
        let (dir, mut app) = test_app();
        let path = dir.path().join("code.txt");
        std::fs::write(&path, "first\nsecond\nthird\n").unwrap();
        app.open_file(&path);
        let buf = render(&mut app);
        // cursor starts on line 1: its row carries the tint (including the
        // padding past the text), other rows do not
        assert!(bg_count(&buf, cursorline_bg()) > 10, "cursor row tinted across its width");
        // move the cursor down and the tint follows
        let editor = app.editor.as_mut().unwrap();
        editor.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('j'),
            crossterm::event::KeyModifiers::NONE,
        ));
        let buf2 = render(&mut app);
        let row_of = |buf: &ratatui::buffer::Buffer| {
            buf.content()
                .iter()
                .enumerate()
                .find(|(_, c)| c.style().bg == Some(cursorline_bg()))
                .map(|(i, _)| i as u16 / buf.area.width)
        };
        assert_eq!(row_of(&buf2), row_of(&buf).map(|r| r + 1), "tint follows the cursor");
    }

    #[test]
    fn diff_overlay_gets_a_scrollbar() {
        let (_dir, mut app) = test_app();
        let long: String = (0..200).map(|i| format!("+line {i}\n")).collect();
        let text = format!("diff --git a/f b/f\n@@ -0,0 +1,200 @@\n{long}");
        app.overlay = Some(crate::app::Overlay::Diff(crate::diff::DiffView::new("f", &text)));
        let buf = render(&mut app);
        let cells: Vec<&str> = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(cells.iter().any(|s| *s == "┃"), "diff scrollbar thumb rendered");
    }

    #[test]
    fn misspelled_words_in_comments_get_spell_squiggles() {
        // needs a system dictionary; skip where none exists (some CI)
        if !crate::spell::available() {
            return;
        }
        let (dir, mut app) = test_app();
        let path = dir.path().join("code.rs");
        std::fs::write(&path, "// teh mispeld wrds\nfn ok() {}\n").unwrap();
        app.open_file(&path);
        assert!(app.editor.as_ref().unwrap().spell_check, "spell on by default");
        let buf = render(&mut app);
        let _ = buf;
        // the misspelled comment words are recorded as spell-colored squiggles
        let spell: Vec<&str> = app
            .squiggle_overlays
            .iter()
            .filter(|s| s.curl == SPELL_CURL())
            .map(|s| s.text.as_str())
            .collect();
        let joined = spell.join(" ");
        assert!(joined.contains("teh"), "flagged 'teh': {spell:?}");
        assert!(joined.contains("mispeld"), "flagged 'mispeld': {spell:?}");
        // the correctly-spelled code identifier `ok` is NOT flagged
        assert!(!spell.contains(&"ok"), "code not spell-checked");

        // :spell toggles it off
        app.editor.as_mut().unwrap().spell_check = false;
        app.squiggle_overlays.clear();
        let _ = render(&mut app);
        assert!(app.squiggle_overlays.iter().all(|s| s.curl != SPELL_CURL()), "toggled off");
    }

    #[test]
    fn file_tree_colors_files_by_git_status() {
        let dir = tempfile::TempDir::new().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        let mut cfg = repo.config().unwrap();
        cfg.set_str("user.name", "T").unwrap();
        cfg.set_str("user.email", "t@e.com").unwrap();
        drop(cfg);
        drop(repo);
        std::fs::write(dir.path().join("added.txt"), "hi\n").unwrap();
        let mut app = App::new(
            dir.path().to_path_buf(),
            vec!["/bin/sh".into(), "-c".into(), "sleep 30".into()],
        );
        app.lsp_enabled = false;
        app.git.refresh();
        app.switch_shell(crate::app::Shell::Code); // file tree sidebar
        let buf = render(&mut app);
        // the untracked file's name is rendered green (StatusKind::New)
        let green: String = buf
            .content()
            .iter()
            .filter(|c| c.style().fg == Some(Color::Green))
            .map(|c| c.symbol())
            .collect();
        assert!(green.contains("added"), "untracked file green: {green:?}");
    }

    #[test]
    fn suspicious_unicode_gets_an_amber_highlight() {
        let (dir, mut app) = test_app();
        let path = dir.path().join("code.rs");
        // "café" is fine; the identifier uses a Cyrillic 'а' (looks Latin);
        // a non-breaking space hides in the string
        std::fs::write(&path, "let s = \"a\u{00A0}fé\";\nlet v\u{0430}lue = 1;\n").unwrap();
        app.open_file(&path);
        assert!(app.editor.as_ref().unwrap().mark_unicode, "on by default");
        let buf = render(&mut app);
        // the Cyrillic 'а' keeps its glyph but on the amber highlight bg
        let confusable_hit = buf
            .content()
            .iter()
            .any(|c| c.symbol() == "\u{0430}" && c.style().bg == Some(INVISIBLE_BG()));
        assert!(confusable_hit, "confusable highlighted");
        // the invisible nbsp renders as a ▒ on the amber bg
        let invisible_hit = buf
            .content()
            .iter()
            .any(|c| c.symbol() == "▒" && c.style().bg == Some(INVISIBLE_BG()));
        assert!(invisible_hit, "invisible char shown as ▒");
        // legitimate 'é' is NOT highlighted
        let accent_clean = buf
            .content()
            .iter()
            .any(|c| c.symbol() == "é" && c.style().bg != Some(INVISIBLE_BG()));
        assert!(accent_clean, "legit accent accepted");

        // :unicode toggles it off — nothing carries the highlight bg
        app.editor.as_mut().unwrap().mark_unicode = false;
        let buf = render(&mut app);
        assert!(
            buf.content().iter().all(|c| c.style().bg != Some(INVISIBLE_BG())),
            "highlight off"
        );
    }

    #[test]
    fn diagnostics_become_squiggle_overlays_in_fancy_mode() {
        let _guard = crate::lsp::ENV_LOCK.lock().unwrap();
        let (dir, mut app) = test_app();
        let script = crate::lsp::fake_server_script(dir.path());
        let path = dir.path().join("code.rs");
        std::fs::write(&path, "fn main() {}\n").unwrap();
        app.lsp_enabled = true;
        unsafe { std::env::set_var("VIBIN_LSP_CMD", &script[0]) };
        app.open_file(&path);
        unsafe { std::env::remove_var("VIBIN_LSP_CMD") };
        // wait for the fake diagnostic (cols 0..3, severity 1)
        let file = path.canonicalize().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            app.tick();
            if !app.lsp.as_ref().unwrap().diagnostics(&file).is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let buf = render(&mut app);
        assert!(!app.squiggle_overlays.is_empty(), "squiggle recorded");
        // the span may split into style runs (cursor/selection/syntax);
        // together they cover the diagnostic range
        let combined: String = app.squiggle_overlays.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(combined, "fn ");
        let s = &app.squiggle_overlays[0];
        assert_eq!(s.curl, (240, 90, 105), "error red curl");
        // fancy mode: no straight underline in the buffer for that span
        let cell = &buf[(s.x, s.y)];
        assert!(!cell.style().add_modifier.contains(Modifier::UNDERLINED));
    }

    #[test]
    fn modals_dim_the_background() {
        let (_dir, mut app) = test_app();
        let buf = render(&mut app);
        let normal = bg_count(&buf, STATUSBAR_BG());
        assert!(normal > 0, "status bar visible when no modal is open");
        // help dialog up: the status bar's cells are dimmed (or covered) —
        // none keep the undimmed background
        app.overlay = Some(Overlay::Help);
        let buf = render(&mut app);
        assert_eq!(bg_count(&buf, STATUSBAR_BG()), 0, "backdrop dimmed behind the modal");
        // hover popups are tooltips, not modals: no dimming
        app.overlay = Some(Overlay::Hover(crate::app::HoverDoc {
            text: "docs".into(),
            scroll: 0,
            diagnostics: vec![],
        }));
        let buf = render(&mut app);
        assert!(bg_count(&buf, STATUSBAR_BG()) > 0, "hover leaves the backdrop lit");
    }

    #[test]
    fn hover_footer_is_static_and_tracks_scroll() {
        let (_dir, mut app) = test_app();
        let long: String = (1..=40).map(|i| format!("doc line {i}\n\n")).collect();
        app.overlay =
            Some(Overlay::Hover(crate::app::HoverDoc { text: long, scroll: 0, diagnostics: vec![] }));
        let footer_row = |buf: &ratatui::buffer::Buffer, rect: Rect| -> String {
            let y = rect.bottom() - 1;
            (rect.x..rect.right())
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect()
        };
        let buf = render(&mut app);
        let rect = app.layout.hover_rect;
        // static header: a rule row, no content on it
        let header: String =
            (rect.x..rect.right()).map(|x| buf[(x, rect.y)].symbol().to_string()).collect();
        assert!(header.contains("─"), "header rule rendered: {header:?}");
        assert!(!header.contains("doc line"), "header row is not content: {header:?}");
        // first content row sits below the header
        let first: String =
            (rect.x..rect.right()).map(|x| buf[(x, rect.y + 1)].symbol().to_string()).collect();
        assert!(first.contains("doc line 1"), "content starts under the header: {first:?}");
        let f0 = footer_row(&buf, rect);
        assert!(f0.contains("↕"), "footer with scroll position: {f0:?}");
        assert!(f0.contains('/'), "shows position/total: {f0:?}");
        assert!(!f0.contains("doc line"), "footer row is not content: {f0:?}");
        // scroll: the footer stays on the same row, only the numbers move
        if let Some(Overlay::Hover(doc)) = &mut app.overlay {
            doc.scroll = 5;
        }
        let buf = render(&mut app);
        assert_eq!(app.layout.hover_rect, rect, "popup geometry unchanged");
        let f5 = footer_row(&buf, rect);
        assert!(f5.contains("↕"), "footer still present after scrolling");
        assert_ne!(f0, f5, "position readout advanced");
    }

    #[test]
    fn hover_code_panel_padding_is_symmetric() {
        let (_dir, mut app) = test_app();
        app.overlay = Some(Overlay::Hover(crate::app::HoverDoc {
            text: format!(
                "```rust\nfn tiny() {{}}\n```\n---\n{}",
                "long documentation paragraph ".repeat(8)
            ),
            scroll: 0,
            diagnostics: vec![],
        }));
        let buf = render(&mut app);
        let rect = app.layout.hover_rect;
        // find the code row and check the panel reaches both content edges,
        // leaving exactly the 1-column dialog padding on each side
        let code_bg = |x: u16, y: u16| buf[(x, y)].style().bg;
        let row = (rect.y..rect.bottom())
            .find(|&y| {
                (rect.x..rect.right()).any(|x| {
                    buf[(x, y)].symbol() == "f" && buf[(x + 1, y)].symbol() == "n"
                })
            })
            .expect("code row rendered");
        let left_inner = code_bg(rect.x + 1, row);
        let right_inner = code_bg(rect.right() - 2, row);
        assert_eq!(left_inner, right_inner, "code panel spans to both inner edges");
        assert_ne!(code_bg(rect.x, row), left_inner, "padding column stays dialog-colored");
        assert_ne!(code_bg(rect.right() - 1, row), right_inner, "right padding symmetric");
    }

    #[test]
    fn hover_popup_shows_diagnostics_above_docs() {
        let (_dir, mut app) = test_app();
        app.overlay = Some(Overlay::Hover(crate::app::HoverDoc {
            text: "```rust\nfn broken()\n```".into(),
            scroll: 0,
            diagnostics: vec![crate::lsp::Diagnostic {
                line: 0,
                col_start: 0,
                col_end: 3,
                severity: 1,
                message: "mismatched types expected i32 found String".into(),
                source: "rust-analyzer".into(),
                code: "E0308".into(),
            }],
        }));
        let buf = render(&mut app);
        let text = format!("{buf:?}");
        assert!(text.contains("mismatched types"), "diagnostic message shown");
        assert!(text.contains("rust-analyzer(E0308)"), "source(code) shown");
        assert!(text.contains("fn broken"), "hover docs still shown");
    }

    #[test]
    fn wrap_text_wraps_greedily() {
        let wrapped = wrap_text("aaa bbb ccc ddd", 7);
        assert_eq!(wrapped, vec!["aaa bbb", "ccc ddd"]);
        assert_eq!(wrap_text("", 10), vec![""]);
        // words longer than the width stay on their own line
        assert_eq!(wrap_text("supercalifragilistic ok", 10).len(), 2);
    }

    #[test]
    fn hover_links_become_osc8_cells() {
        let (_dir, mut app) = test_app();
        app.overlay = Some(Overlay::Hover(crate::app::HoverDoc {
            text: "docs: [example](https://example.com)".into(),
            scroll: 0,
            diagnostics: Vec::new(),
        }));
        let buf = render(&mut app);
        let rect = app.layout.hover_rect;
        // one cell inside the popup carries the whole OSC 8 sequence
        let cell = buf
            .content()
            .iter()
            .find(|c| c.symbol().contains("\x1b]8;;https://example.com"))
            .expect("link cell rendered");
        assert!(cell.symbol().contains("example"), "label embedded: {:?}", cell.symbol());
        assert!(cell.symbol().ends_with("\x1b]8;;\x1b\\"), "sequence closed");
        let pos = buf
            .content()
            .iter()
            .position(|c| c.symbol().contains("]8;;https"))
            .unwrap() as u16;
        let (x, y) = (pos % buf.area.width, pos / buf.area.width);
        assert!(rect.contains(ratatui::layout::Position::new(x, y)), "inside the popup");
        // clicking the link label opens it instead of dismissing the popup
        let (hit, url) = app.link_hits[0].clone();
        let opened = app.handle_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: hit.x,
            row: hit.y,
            modifiers: crossterm::event::KeyModifiers::NONE,
        });
        assert!(opened);
        assert!(app.overlay.is_some(), "popup stays open after a link click");
        assert_eq!(app.status_msg.as_deref(), Some(format!("opened {url}").as_str()));
        // no overlay → no link cells
        app.overlay = None;
        let buf = render(&mut app);
        assert!(!buf.content().iter().any(|c| c.symbol().contains("]8;;")));
    }

    #[test]
    fn whichkey_menu_appears_while_leader_pending() {
        let (_dir, mut app) = test_app();
        let buf = render(&mut app);
        let text = format!("{buf:?}");
        assert!(!text.contains("new agent"));
        app.leader_pending = true;
        let buf = render(&mut app);
        let text = format!("{buf:?}");
        assert!(text.contains("new agent"), "bindings listed");
        assert!(text.contains("rename agent"));
    }

    #[test]
    fn prompt_dialog_uses_gray_base() {
        let (_dir, mut app) = test_app();
        app.overlay = Some(Overlay::CommitPrompt("msg".into()));
        let buf = render(&mut app);
        assert!(bg_count(&buf, DIALOG_BG()) > 50);
        assert_eq!(border_count(&buf), 3);
    }
}
