//! Rendering: layout, sidebar, terminal panes, overlays, status bar.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, List, ListItem, Padding, Paragraph};

use crate::app::{App, Focus, Overlay, Screen, Shell};
use crate::diff::{DiffLine, DiffLineKind};
use crate::editor::Mode;
use crate::git::StatusKind;

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
    // the double-line box glyphs are the FIGlet art's shadow scaffolding,
    // not the letters — keep them dim so the solid blocks carry the color
    let scaffold =
        wash(80).unwrap_or_else(|| adaptive(Color::Rgb(58, 61, 70), Color::Rgb(196, 200, 208)));
    Line::from(
        chars
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let style =
                    if matches!(c, '═' | '║' | '╔' | '╗' | '╚' | '╝' | '╠' | '╣' | '╦' | '╩' | '╬')
                    {
                        Style::default().fg(scaffold)
                    } else {
                        let t = i as f32 / (n - 1) as f32 + phase;
                        Style::default().fg(gradient_color(t))
                    };
                Span::styled(c.to_string(), style)
            })
            .collect::<Vec<_>>(),
    )
}

pub fn draw(frame: &mut Frame, app: &mut App) {
    app.link_hits.clear();
    app.toast_hits.clear();
    if app.screen == Screen::Welcome {
        draw_welcome(frame, app);
        return;
    }
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(3), Constraint::Length(1)])
        .split(frame.area());
    draw_menu_bar(frame, app, outer[0]);
    let main = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(SIDEBAR_WIDTH), Constraint::Min(10)])
        .split(outer[1]);

    draw_sidebar(frame, app, main[0]);
    // the sidebar keeps its own frame — the main pane sits beside it with
    // its own border, so the two read as separate layers (the same
    // treatment the notification panel gets against the code pane, below)
    let mut pane = main[1];
    // bell pane: carve the notification panel off the pane's right side —
    // same idea, its own frame next to the code
    let notifications = app.notifications_open.then(|| {
        let w = 38.min(pane.width / 2);
        let panel = Rect::new(pane.right().saturating_sub(w), pane.y, w, pane.height);
        pane.width -= w;
        panel
    });
    draw_main_area(frame, app, pane);
    if let Some(panel) = notifications {
        draw_notifications(frame, app, panel);
    }
    draw_status_bar(frame, app, outer[2]);
    draw_menu_dropdown(frame, app);

    // modal dimming: with a dialog or the leader menu up, everything
    // behind it steps back (hover popups are tooltips, not modals)
    let modal_open = app.leader_pending
        || matches!(
            &app.overlay,
            Some(Overlay::Diff(_) | Overlay::Help | Overlay::CommitPrompt(_) | Overlay::Palette(_))
        );
    if modal_open {
        dim_background(frame.buffer_mut());
    }

    match &app.overlay {
        Some(Overlay::Diff(_)) => draw_diff_overlay(frame, app),
        Some(Overlay::Help) => draw_help_overlay(frame, app.welcome.phase),
        Some(Overlay::CommitPrompt(buf)) => {
            draw_prompt(frame, "commit message (Enter commit · Esc cancel)", buf, app.welcome.phase)
        }
        Some(Overlay::Hover(_)) => draw_hover_overlay(frame, app),
        Some(Overlay::Palette(_)) => draw_palette(frame, app),
        None => {}
    }

    // browser-style link preview: the URL under the pointer, in a chip
    // at the bottom-left (above the status bar) — hover-popup links and
    // LSP document links in the editor both land in hovered_link
    if let Some(url) = &app.hovered_link {
        let area = frame.area();
        let text = format!(" {url} ");
        let w = (text.chars().count() as u16).min(area.width);
        let chip = Rect::new(area.x, area.bottom().saturating_sub(2), w, 1);
        frame.render_widget(
            Paragraph::new(Span::styled(text, Style::default().fg(STATUS_DIM()).bg(DIALOG_BG()))),
            chip,
        );
    }

    // the LSP completion popup floats over the editor, below the cursor
    if app.completion.is_some() && app.overlay.is_none() && !app.leader_pending {
        draw_completion_popup(frame, app);
    }

    // right-click context menu, above everything but the debug overlay
    if app.context_menu.is_some() {
        draw_context_menu(frame, app);
    }

    // toast notifications float above everything, undimmed
    draw_toasts(frame, app);

    // which-key: the leader menu shows itself the moment Ctrl+A is pressed
    if app.leader_pending {
        draw_whichkey(frame, app);
    }

    // debug builds: VIBIN_HITBOXES=1 outlines every tracked mouse target
    #[cfg(debug_assertions)]
    {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if *ON.get_or_init(|| std::env::var_os("VIBIN_HITBOXES").is_some()) {
            draw_hitbox_debug(frame, app);
        }
    }
}

/// Bottom-anchored menu of every leader binding, shown while the leader is
/// pending — no memorization needed.
/// Debug overlay (VIBIN_HITBOXES=1, debug builds only): draw a colored
/// outline + label over every rect the mouse code hit-tests against, plus
/// the per-frame link hitboxes. For eyeballing why a click landed where
/// it did — deliberately crude, draws over everything.
#[cfg(debug_assertions)]
fn draw_hitbox_debug(frame: &mut Frame, app: &App) {
    let boxes: Vec<(&str, Rect)> = vec![
        ("sidebar", app.layout.sidebar_list),
        ("term", app.layout.terminal_pane),
        ("welcome", app.layout.welcome_list),
        ("editor", app.layout.editor_text),
        ("palette", app.layout.palette_list),
        ("hover", app.layout.hover_rect),
        ("home", app.layout.home_list),
        ("hextree", app.layout.hex_tree),
        ("hexdump", app.layout.hex_dump),
    ];
    let area = frame.area();
    let buf = frame.buffer_mut();
    let mut tint = |label: &str, rect: Rect, i: usize| {
        let rect = rect.intersection(area);
        if rect.width == 0 || rect.height == 0 {
            return;
        }
        // fill the whole hitbox with the pattern color's dark shade so the
        // content underneath stays readable; overlaps simply overpaint
        let (fill, bright) = pattern_color(i);
        for y in rect.y..rect.bottom() {
            for x in rect.x..rect.right() {
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_style(Style::default().bg(fill));
                }
            }
        }
        // label in the top-left corner, in the bright shade
        for (n, ch) in label.chars().enumerate() {
            let x = rect.x + n as u16;
            if x >= rect.right() {
                break;
            }
            if let Some(cell) = buf.cell_mut((x, rect.y)) {
                cell.set_symbol(&ch.to_string());
                cell.set_style(Style::default().fg(bright).bg(fill).add_modifier(Modifier::BOLD));
            }
        }
    };
    for (i, (label, rect)) in boxes.iter().enumerate() {
        tint(label, *rect, i);
    }
    for (i, (rect, _)) in app.link_hits.iter().enumerate() {
        tint("link", *rect, boxes.len() + i);
    }
}

/// Right-click menu: a small bordered list anchored at the click, below
/// it when there's room, above otherwise. The item area is recorded in
/// the layout map for mouse hit-testing.
/// The LSP autocomplete popup: a list of candidates (label + kind) anchored
/// below the cursor, with a documentation panel beside it showing the
/// selected item's signature and docs.
fn draw_completion_popup(frame: &mut Frame, app: &mut App) {
    let Some(anchor) = app.layout.editor_cursor else { return };
    let Some(completion) = &app.completion else { return };
    if completion.filtered.is_empty() {
        return;
    }
    let area = frame.area();
    let items = &completion.items;
    let rows: Vec<usize> = completion.filtered.clone();

    // list geometry: widest label + kind, capped; at most 10 rows visible
    let label_w = rows.iter().map(|&i| items[i].label.chars().count()).max().unwrap_or(4);
    let kind_w = rows.iter().map(|&i| items[i].kind.len()).max().unwrap_or(0);
    let list_w = ((label_w + kind_w + 4) as u16).clamp(14, 48).min(area.width);
    let visible = (rows.len() as u16).min(10);
    // below the cursor if it fits, else above
    let (y, height) = if anchor.y + 1 + visible <= area.bottom() {
        (anchor.y + 1, visible)
    } else {
        (anchor.y.saturating_sub(visible), visible)
    };
    let x = anchor.x.min(area.right().saturating_sub(list_w));
    let list_rect = Rect::new(x, y, list_w, height);

    // scroll so the selection stays visible
    let off = completion.selected.saturating_sub(height.saturating_sub(1) as usize);
    let dim = STATUS_DIM();
    let inner = list_w.saturating_sub(2) as usize; // one pad each side
    let lines: Vec<Line> = (off..off + height as usize)
        .filter_map(|vi| rows.get(vi).map(|&i| (vi, &items[i])))
        .map(|(vi, item)| {
            let selected = vi == completion.selected;
            let mut label: String =
                item.label.chars().take(inner.saturating_sub(item.kind.len() + 1)).collect();
            let used = label.chars().count() + item.kind.len();
            label.push_str(&" ".repeat(inner.saturating_sub(used).max(1)));
            let bg = if selected { SELECTION_BG() } else { DIALOG_BG() };
            let name_style = Style::default().bg(bg);
            let name_style =
                if selected { name_style.add_modifier(Modifier::BOLD) } else { name_style };
            Line::from(vec![
                Span::styled(" ", Style::default().bg(bg)),
                Span::styled(label, name_style),
                Span::styled(format!("{} ", item.kind), Style::default().fg(dim).bg(bg)),
            ])
        })
        .collect();
    frame.render_widget(Clear, list_rect);
    frame.render_widget(Paragraph::new(lines).style(Style::default().bg(DIALOG_BG())), list_rect);

    // documentation panel for the selected item, beside the list
    let item = &items[rows[completion.selected]];
    let mut doc_lines: Vec<String> = Vec::new();
    if let Some(detail) = &item.detail {
        doc_lines.extend(detail.lines().map(str::to_string));
    }
    if let Some(docs) = &item.documentation {
        if !doc_lines.is_empty() {
            doc_lines.push(String::new());
        }
        doc_lines.extend(docs.lines().map(str::to_string));
    }
    if doc_lines.is_empty() {
        return;
    }
    let doc_w = 44u16.min(area.width);
    // right of the list if it fits, else left
    let doc_x = if list_rect.right() + doc_w <= area.right() {
        list_rect.right()
    } else {
        list_rect.x.saturating_sub(doc_w)
    };
    let doc_h = (doc_lines.len() as u16).min(12);
    let doc_rect = Rect::new(doc_x, y, doc_w, doc_h).intersection(area);
    if doc_rect.width < 6 {
        return;
    }
    let wrapped: Vec<Line> = doc_lines
        .iter()
        .flat_map(|l| wrap_text(l, doc_rect.width.saturating_sub(2) as usize))
        .take(doc_rect.height as usize)
        .map(|l| Line::from(Span::raw(format!(" {l}"))))
        .collect();
    frame.render_widget(Clear, doc_rect);
    frame.render_widget(
        Paragraph::new(wrapped).style(Style::default().bg(DIALOG_BG()).fg(STATUS_DIM())),
        doc_rect,
    );
}

fn draw_context_menu(frame: &mut Frame, app: &mut App) {
    let Some(menu) = &app.context_menu else { return };
    let area = frame.area();
    let width = (menu.items.iter().map(|(l, _)| l.chars().count()).max().unwrap_or(8) as u16 + 4)
        .min(area.width);
    let height = (menu.items.len() as u16 + 2).min(area.height);
    let x = menu.pos.x.min(area.right().saturating_sub(width));
    let below = menu.pos.y + 1;
    let y = if below + height <= area.bottom() {
        below
    } else {
        menu.pos.y.saturating_sub(height).max(area.y)
    };
    let rect = Rect::new(x, y, width, height).intersection(area);
    draw_dialog_base(frame, rect);
    let items: Vec<Line> = menu
        .items
        .iter()
        .enumerate()
        .map(|(i, (label, _))| {
            let style = if i == menu.selected {
                Style::default().bg(SELECTION_BG())
            } else {
                Style::default()
            };
            Line::from(Span::styled(format!(" {label:w$}", w = width as usize - 4), style))
        })
        .collect();
    frame.render_widget(
        Paragraph::new(items).style(Style::default().bg(DIALOG_BG())).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(DIALOG_BORDER())),
        ),
        rect,
    );
    app.layout.context_menu = Rect::new(
        rect.x + 1,
        rect.y + 1,
        rect.width.saturating_sub(2),
        rect.height.saturating_sub(2),
    );
}

/// Severity-tinted toast fill: the accent blended gently into the
/// terminal background, so cards sit in the theme instead of on it.
fn toast_tint(accent: Color) -> Color {
    let (br, bg, bb) = crate::color::terminal_bg().unwrap_or(if crate::color::is_light() {
        (250, 250, 252)
    } else {
        (24, 26, 32)
    });
    match accent {
        Color::Rgb(r, g, b) => {
            let mix = |a: u8, base: u8| ((a as u32 * 52 + base as u32 * 204) / 256) as u8;
            Color::Rgb(mix(r, br), mix(g, bg), mix(b, bb))
        }
        _ => DIALOG_BG(),
    }
}

/// Toast notifications: borderless cards stacked top-right under the menu
/// bar, oldest on top. Each is a severity-tinted block with a bold accent
/// bar on the left and the app's slant tail on the right — same slant
/// language as the chips. Cards with buttons grow a button row and stick
/// around until answered; plain ones expire in `App::tick`. Hitboxes for
/// buttons and card bodies land in `app.toast_hits`.
fn draw_toasts(frame: &mut Frame, app: &mut App) {
    if app.toasts.is_empty() {
        return;
    }
    let area = frame.area();
    let max_text = 40.min(area.width.saturating_sub(8) as usize);
    let fancy = crate::color::fancy_glyphs();
    let hover = app.toast_hover;
    let mut hits: Vec<(Rect, usize, Option<usize>)> = Vec::new();
    // below the menu bar and the panes' top border row
    let mut y = area.y + 3;
    for (index, toast) in app.toasts.iter().enumerate() {
        let accent = match toast.level {
            crate::app::ToastLevel::Info => chip_accent(6, (134, 220, 214), (1, 132, 188)),
            crate::app::ToastLevel::Warn => severity_color(2),
            crate::app::ToastLevel::Error => severity_color(1),
        };
        let fill = toast_tint(accent);
        // markdown body: bold, code spans, links… (links become OSC 8
        // cells + hitboxes below, like the hover popup)
        let (mut lines, mut links) = crate::markdown::render_with_links(&toast.text, max_text);
        // trim blank framing rows, keeping link line indices in step
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
        if lines.is_empty() {
            lines.push(Line::default());
        }
        // each inner button boundary is a two-cell slant notch (◤◢); the
        // group's outer edges stay vertical
        let buttons_w: usize = toast.buttons.iter().map(|b| b.chars().count() + 2).sum::<usize>()
            + 2 * toast.buttons.len().saturating_sub(1);
        let text_w = lines.iter().map(|l| l.width()).max().unwrap_or(0).max(buttons_w);
        // the slant tail only reads right on one-line, button-free cards;
        // taller ones keep a straight edge
        let tail = fancy && lines.len() == 1 && toast.buttons.is_empty();
        let w = (text_w as u16 + 3 + tail as u16).min(area.width); // ▌ + padding (+ ◤)
        let h = lines.len() as u16 + (!toast.buttons.is_empty()) as u16;
        if y + h > area.bottom().saturating_sub(1) {
            break;
        }
        // inset from the right edge: clear of the pane border and its
        // scrollbar column — and of the notification pane when it's open
        let right_edge = if app.notifications_open && app.layout.notifications.width > 0 {
            app.layout.notifications.x.saturating_sub(1)
        } else {
            area.right().saturating_sub(2)
        };
        let rect = Rect::new(right_edge.saturating_sub(w + 1), y, w, h);
        let mut rows: Vec<Line> = lines
            .iter()
            .map(|l| {
                let pad = text_w.saturating_sub(l.width());
                let mut spans =
                    vec![Span::styled("▌", Style::default().fg(accent)), Span::raw(" ")];
                spans.extend(l.spans.iter().cloned());
                spans.push(Span::raw(" ".repeat(pad + 1)));
                if tail {
                    // the tail sits outside the card: default background
                    spans.push(Span::styled("◤", Style::default().fg(fill).bg(Color::Reset)));
                }
                Line::from(spans)
            })
            .collect();
        if !toast.buttons.is_empty() {
            let mut spans = vec![
                Span::styled("▌", Style::default().fg(accent).bg(fill)),
                Span::styled(" ", Style::default().bg(fill)),
            ];
            let button_y = rect.y + lines.len() as u16;
            let mut x = rect.x + 2;
            let last = toast.buttons.len() - 1;
            for (b, label) in toast.buttons.iter().enumerate() {
                let chip = format!(" {label} ");
                let bg = if hover == Some((index, b)) { accent } else { MENU_TINT() };
                let style = if hover == Some((index, b)) {
                    Style::default().fg(chip_text()).bg(bg).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().bg(bg)
                };
                let cap = |s: &'static str| {
                    Span::styled(if fancy { s } else { " " }, Style::default().fg(bg).bg(fill))
                };
                let mut chip_w = chip.chars().count() as u16;
                if b > 0 {
                    spans.push(cap("◢"));
                    chip_w += 1;
                }
                spans.push(Span::styled(chip, style));
                if b < last {
                    spans.push(cap("◤"));
                    chip_w += 1;
                }
                hits.push((Rect::new(x, button_y, chip_w, 1), index, Some(b)));
                x += chip_w;
            }
            let used = 2 + buttons_w;
            spans.push(Span::styled(
                " ".repeat((w as usize).saturating_sub(used)),
                Style::default().bg(fill),
            ));
            rows.push(Line::from(spans));
        }
        frame.render_widget(Paragraph::new(rows).style(Style::default().bg(fill)), rect);
        // clickable markdown links: OSC 8 cells + hitboxes, exactly like
        // the hover popup (see draw_hover_overlay)
        for link in &links {
            if link.col + link.text.chars().count() > text_w {
                continue; // clipped labels would render a broken sequence
            }
            let (x, y) = (rect.x + 2 + link.col as u16, rect.y + link.line as u16);
            let hitbox = Rect::new(x, y, link.text.chars().count().max(1) as u16, 1);
            let style = frame.buffer_mut().cell((x, y)).map(|c| c.style()).unwrap_or_default();
            if let Some(cell) = frame.buffer_mut().cell_mut((x, y)) {
                use unicode_width::UnicodeWidthStr;
                let label_width = link.text.as_str().width().max(1) as u16;
                cell.set_symbol(&format!("\x1b]8;;{}\x1b\\{}\x1b]8;;\x1b\\", link.url, link.text));
                cell.set_style(style);
                cell.set_diff_option(ratatui::buffer::CellDiffOption::ForcedWidth(
                    std::num::NonZeroU16::new(label_width).expect("max(1) above"),
                ));
            }
            app.link_hits.push((hitbox, link.url.clone()));
        }
        // the body hit comes after the button hits: find() prefers buttons
        hits.push((rect, index, None));
        y += h; // flush stack — tint and ragged widths separate the cards
    }
    app.toast_hits.extend(hits);
}

/// The notification pane (bell toggle): every notification of the
/// session, newest first — severity dot, markdown body (links become
/// OSC 8 cells + hitboxes, like toasts), dim age.
fn draw_notifications(frame: &mut Frame, app: &mut App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border_style(false));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    app.layout.notifications = area;
    if app.notifications.is_empty() {
        app.layout.notifications_clear = Rect::default();
        frame.render_widget(
            Paragraph::new(Span::styled(" no notifications", Style::default().fg(STATUS_DIM()))),
            inner,
        );
        return;
    }
    let age = |elapsed: std::time::Duration| -> String {
        let s = elapsed.as_secs();
        match s {
            0..=59 => "now".into(),
            60..=3599 => format!("{}m", s / 60),
            _ => format!("{}h", s / 3600),
        }
    };
    let wrap = (inner.width as usize).saturating_sub(3).max(8);
    // header: title left, "clear all" right (clickable)
    let clear = "clear all";
    let title = " notifications";
    let pad =
        (inner.width as usize).saturating_sub(title.chars().count() + clear.chars().count() + 1);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(title, Style::default().fg(STATUS_DIM()).add_modifier(Modifier::BOLD)),
            Span::raw(" ".repeat(pad)),
            Span::styled(
                clear,
                Style::default().fg(STATUS_DIM()).add_modifier(Modifier::UNDERLINED),
            ),
        ])),
        Rect::new(inner.x, inner.y, inner.width, 1),
    );
    app.layout.notifications_clear = Rect::new(
        inner.right().saturating_sub(clear.len() as u16 + 1),
        inner.y,
        clear.len() as u16,
        1,
    );
    let inner = Rect::new(inner.x, inner.y + 1, inner.width, inner.height.saturating_sub(1));
    let mut rows: Vec<Line> = Vec::new();
    // (row, col, label, url) of every rendered link, for the OSC 8 pass
    let mut pane_links: Vec<(usize, usize, String, String)> = Vec::new();
    // pending-question button hitboxes (toast index + button index)
    let mut button_hits: Vec<(Rect, usize, Option<usize>)> = Vec::new();
    for (level, text, born) in app.notifications.iter().rev() {
        let dot = match level {
            crate::app::ToastLevel::Info => chip_accent(6, (134, 220, 214), (1, 132, 188)),
            crate::app::ToastLevel::Warn => severity_color(2),
            crate::app::ToastLevel::Error => severity_color(1),
        };
        let (mut lines, mut links) = crate::markdown::render_with_links(text, wrap);
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
        let base = rows.len();
        for link in links {
            pane_links.push((base + link.line, link.col, link.text, link.url));
        }
        for (i, line) in lines.into_iter().enumerate() {
            let head = if i == 0 { " ● " } else { "   " };
            let mut spans = vec![Span::styled(head, Style::default().fg(dot))];
            spans.extend(line.spans);
            rows.push(Line::from(spans));
        }
        // a pending question renders its buttons here too — same chips
        // as the toast, resolved through the same machinery on click
        if let Some((toast_index, toast)) =
            app.toasts.iter().enumerate().find(|(_, t)| !t.buttons.is_empty() && t.text == *text)
        {
            let fancy = crate::color::fancy_glyphs();
            let hover = app.toast_hover;
            let row_y = inner.y + rows.len() as u16;
            let mut spans = vec![Span::raw("   ")];
            let mut x = inner.x + 3;
            let last = toast.buttons.len() - 1;
            for (b, blabel) in toast.buttons.iter().enumerate() {
                let chip = format!(" {blabel} ");
                let hovered = hover == Some((toast_index, b));
                let bg = if hovered { dot } else { MENU_TINT() };
                let style = if hovered {
                    Style::default().fg(chip_text()).bg(bg).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().bg(bg)
                };
                let cap = |g: &'static str| {
                    Span::styled(if fancy { g } else { " " }, Style::default().fg(bg))
                };
                let mut chip_w = chip.chars().count() as u16;
                if b > 0 {
                    spans.push(cap("\u{25e2}"));
                    chip_w += 1;
                }
                spans.push(Span::styled(chip, style));
                if b < last {
                    spans.push(cap("\u{25e4}"));
                    chip_w += 1;
                }
                if (row_y as usize) < (inner.bottom()) as usize {
                    button_hits.push((Rect::new(x, row_y, chip_w, 1), toast_index, Some(b)));
                }
                x += chip_w;
            }
            rows.push(Line::from(spans));
        }
        rows.push(Line::from(Span::styled(
            format!("   {}", age(born.elapsed())),
            Style::default().fg(STATUS_DIM()),
        )));
        if rows.len() >= inner.height as usize {
            break;
        }
    }
    app.toast_hits.extend(button_hits);
    rows.truncate(inner.height as usize);
    frame.render_widget(Paragraph::new(rows), inner);
    // clickable links: OSC 8 cells + hitboxes (see draw_toasts)
    for (row, col, label, url) in pane_links {
        if row >= inner.height as usize || 3 + col + label.chars().count() > inner.width as usize {
            continue; // clipped labels would render a broken sequence
        }
        let (x, y) = (inner.x + 3 + col as u16, inner.y + row as u16);
        let hitbox = Rect::new(x, y, label.chars().count().max(1) as u16, 1);
        let style = frame.buffer_mut().cell((x, y)).map(|c| c.style()).unwrap_or_default();
        if let Some(cell) = frame.buffer_mut().cell_mut((x, y)) {
            use unicode_width::UnicodeWidthStr;
            let label_width = label.as_str().width().max(1) as u16;
            cell.set_symbol(&format!("\x1b]8;;{url}\x1b\\{label}\x1b]8;;\x1b\\"));
            cell.set_style(style);
            cell.set_diff_option(ratatui::buffer::CellDiffOption::ForcedWidth(
                std::num::NonZeroU16::new(label_width).expect("max(1) above"),
            ));
        }
        app.link_hits.push((hitbox, url));
    }
}

fn draw_whichkey(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let accent = chip_accent(6, (134, 220, 214), (1, 132, 188));
    let key = |k: &str| {
        Span::styled(format!(" {k:<5}"), Style::default().fg(accent).add_modifier(Modifier::BOLD))
    };
    let label = |l: &str| Span::styled(format!("{l:<18}"), Style::default());
    let head = |t: &str| {
        Span::styled(
            format!(" {t:<23}"),
            Style::default().fg(STATUS_DIM()).add_modifier(Modifier::BOLD),
        )
    };
    let editor_label = if app.editor.is_some() { "editor" } else { "editor (none open)" };
    let rows: Vec<Line> = vec![
        Line::from(vec![head("── agents"), head("── panels"), head("── other")]),
        Line::from(vec![
            key("c"),
            label("new agent"),
            key("F1/h"),
            label("agents shell"),
            key("r"),
            label("rename agent"),
        ]),
        Line::from(vec![
            key("1-9"),
            label("jump to agent"),
            key("F2/g"),
            label("git shell"),
            key("R"),
            label("respawn agent"),
        ]),
        Line::from(vec![
            key("⇥/n"),
            label("next agent"),
            key("F3/f"),
            label("code shell"),
            key("u"),
            label("refresh panels"),
        ]),
        Line::from(vec![
            key("p"),
            label("previous agent"),
            key("e"),
            label(editor_label),
            key("k/j"),
            label("scroll terminal"),
        ]),
        Line::from(vec![
            key("x"),
            label("close agent"),
            key("d"),
            label("diff all changes"),
            key("q"),
            label("quit vibin"),
        ]),
        Line::from(Span::styled(
            " esc cancel · ctrl+a ctrl+a literal · ctrl+k palette · ? full help",
            Style::default().fg(STATUS_DIM()),
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
        Paragraph::new(rows).style(Style::default().bg(DIALOG_BG())).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(DIALOG_BORDER())),
        ),
        rect,
    );
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
    let accent = chip_accent(6, (134, 220, 214), (1, 132, 188));
    lines.push(Line::from(vec![
        Span::styled(format!(" {prompt} "), Style::default().fg(accent)),
        Span::raw(input.clone()),
    ]));
    if rows.is_empty() {
        lines.push(Line::from(Span::styled("   no matches", Style::default().fg(STATUS_DIM()))));
    }
    for (i, label) in rows.iter().enumerate() {
        let style = if i == selected {
            Style::default().bg(SELECTION_BG()).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        let marker = if i == selected { "▸ " } else { "  " };
        let text: String = label.chars().take(width.saturating_sub(5) as usize).collect();
        lines.push(Line::from(Span::styled(format!(" {marker}{text}"), style)));
    }
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(DIALOG_BG())).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(DIALOG_BORDER())),
        ),
        rect,
    );
    // result rows start after the padding + input rows
    app.layout.palette_list =
        Rect { x: rect.x, y: rect.y + 2, width: rect.width, height: rect.height.saturating_sub(3) };
    let prefix = if is_cmd { 3 } else { 4 }; // " ❯ " vs " 🔍 " (emoji is 2 wide)
    frame.set_cursor_position((rect.x + 2 + prefix + input.chars().count() as u16, rect.y + 1));
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
    let anchor = app.code_view.hover_anchor;
    // inline code and unlabeled fences highlight as the hovered file's
    // language, so docs read like the source they describe
    let hover_lang =
        app.editor.as_ref().map(|e| crate::editor::highlight::language_name(&e.path)).unwrap_or("");
    let Some(Overlay::Hover(doc)) = &mut app.overlay else {
        return;
    };
    let max_width = area.width.saturating_sub(6).min(84);
    let wrap_width = max_width.saturating_sub(4) as usize;

    // diagnostics at the hovered position come first
    let mut diag_lines: Vec<Line> = Vec::new();
    for diag in &doc.diagnostics {
        let color = severity_color(diag.severity);
        for (i, piece) in
            wrap_text(&diag.message, wrap_width.saturating_sub(2)).into_iter().enumerate()
        {
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
        // static header: double rule with corner pieces that step down to
        // a single stub (╒ ╕) at the card's edges
        let rule = format!("╒{}╕", "═".repeat(rect.width.saturating_sub(2) as usize));
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(rule, Style::default().fg(DIALOG_BORDER()))))
                .style(Style::default().bg(DIALOG_BG())),
            Rect::new(rect.x, rect.y, rect.width, 1).intersection(area),
        );
    }
    if has_footer {
        // static footer row: a hairline rule with the scroll position
        // right-aligned; content scrolls above it, this row never moves
        let hint = format!(" ↕ {}/{} ", (scroll + viewport).min(total), total);
        let inner = rect.width.saturating_sub(2) as usize;
        let rule_len = inner.saturating_sub(hint.chars().count());
        let footer = Line::from(vec![
            Span::styled(
                format!("╘{}", "═".repeat(rule_len)),
                Style::default().fg(DIALOG_BORDER()),
            ),
            Span::styled(hint, Style::default().fg(STATUS_DIM())),
            Span::styled("╛", Style::default().fg(DIALOG_BORDER())),
        ]);
        frame.render_widget(
            Paragraph::new(footer).style(Style::default().bg(DIALOG_BG())),
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

/// A path for display: the home directory abbreviated to `~`.
fn display_path(path: &std::path::Path) -> String {
    if let Some(home) = std::env::var_os("HOME")
        && let Ok(rest) = path.strip_prefix(&home)
    {
        let rest = rest.to_string_lossy();
        return if rest.is_empty() { "~".into() } else { format!("~/{rest}") };
    }
    path.to_string_lossy().into_owned()
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
    let with_parrot = parrot_width > 0 && area.width >= LOGO_WIDTH + PARROT_GAP + parrot_width + 2;
    let total = if with_parrot { LOGO_WIDTH + PARROT_GAP + parrot_width } else { LOGO_WIDTH };
    let start_x = area.x + area.width.saturating_sub(total) / 2;
    let logo_x = if with_parrot { start_x + parrot_width + PARROT_GAP } else { start_x };
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
    let version_rect = Rect::new(logo_x, logo_y + LOGO.len() as u16, width, 1).intersection(area);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(version, Style::default().fg(Color::DarkGray))))
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

    let items: Vec<ListItem> = vec![ListItem::new(Line::from(vec![
        Span::styled("open ", Style::default().fg(Color::Gray)),
        Span::styled(
            display_path(&app.workdir),
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::styled("  (current directory)", Style::default().fg(Color::DarkGray)),
    ]))];
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

/// Experimental top menu bar: borderless — a blank spacer row, then the
/// entries as slightly tinted chips, user + gear dimmed on the right.
/// Each label's rect lands in the layout map; hovering one opens its
/// dropdown (see [`draw_menu_dropdown`]).
fn draw_menu_bar(frame: &mut Frame, app: &mut App, area: Rect) {
    use unicode_width::UnicodeWidthStr;
    if area.height < 2 || area.width < 4 {
        return;
    }
    let tint = MENU_TINT();
    // slanted chips, same slant language as the dialog badges and the mode
    // chip: ◢ label ◤ back to back — the caps carve the gap themselves
    let fancy = crate::color::fancy_glyphs();
    let mut spans = vec![Span::raw(" ")];
    let mut used: u16 = 1;
    for (i, (item, _)) in crate::app::MENU_BAR.iter().enumerate() {
        let label = format!(" {item} ");
        let bg = if app.menu_open == Some(i) { SELECTION_BG() } else { tint };
        let cap =
            |s: &'static str| Span::styled(if fancy { s } else { " " }, Style::default().fg(bg));
        let chip_width = label.width() as u16 + 2;
        spans.push(cap("◢"));
        spans.push(Span::styled(label, Style::default().bg(bg)));
        spans.push(cap("◤"));
        app.layout.menu_items[i] = Rect::new(area.x + used, area.y + 1, chip_width, 1);
        used += chip_width;
    }
    // bell chip at the right edge (a notification center, someday) —
    // same slant chip treatment as the menu items
    let bell = if app.config.icons { "\u{f009a}" } else { "•" };
    let unread = app.notifications.len().saturating_sub(app.notifications_seen);
    let label = if unread > 0 && !app.notifications_open {
        format!(" {bell} {unread} ")
    } else {
        format!(" {bell} ")
    };
    let bell_bg = if app.notifications_open {
        SELECTION_BG()
    } else if unread > 0 {
        // unread badge accent, like VS Code's dot on the bell
        chip_accent(6, (134, 220, 214), (1, 132, 188))
    } else {
        tint
    };
    let cap =
        |s: &'static str| Span::styled(if fancy { s } else { " " }, Style::default().fg(bell_bg));
    let right_width = label.width() + 3; // caps + one column of margin
    let pad = (area.width as usize).saturating_sub(used as usize + right_width);
    let chip_width = label.width() as u16 + 2;
    app.layout.menu_bell = Rect::new(area.x + used + pad as u16, area.y + 1, chip_width, 1);
    spans.push(Span::raw(" ".repeat(pad)));
    spans.push(cap("◢"));
    spans.push(Span::styled(label, Style::default().bg(bell_bg)));
    spans.push(cap("◤"));
    frame.render_widget(
        Paragraph::new(Line::from(spans)),
        Rect::new(area.x, area.y + 1, area.width, 1),
    );
}

/// The open menu-bar dropdown, anchored under its label — same surface,
/// border, and selection treatment as the right-click context menu.
fn draw_menu_dropdown(frame: &mut Frame, app: &mut App) {
    let Some(open) = app.menu_open else {
        app.layout.menu_dropdown = Rect::default();
        return;
    };
    let (_, items) = crate::app::MENU_BAR[open];
    let anchor = app.layout.menu_items[open];
    let area = frame.area();
    let width = (items.iter().map(|(l, _)| l.chars().count()).max().unwrap_or(8) as u16 + 4)
        .min(area.width);
    let height = (items.len() as u16 + 2).min(area.height.saturating_sub(anchor.bottom()));
    let x = anchor.x.saturating_sub(1).min(area.right().saturating_sub(width));
    let rect = Rect::new(x, anchor.bottom(), width, height).intersection(area);
    draw_dialog_base(frame, rect);
    let rows: Vec<Line> = items
        .iter()
        .enumerate()
        .map(|(i, (label, _))| {
            let style = if i == app.menu_row {
                Style::default().bg(SELECTION_BG())
            } else {
                Style::default()
            };
            Line::from(Span::styled(format!(" {label:w$}", w = width as usize - 4), style))
        })
        .collect();
    frame.render_widget(
        Paragraph::new(rows).style(Style::default().bg(DIALOG_BG())).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(DIALOG_BORDER())),
        ),
        rect,
    );
    app.layout.menu_dropdown = rect;
}

/// Dashboard glyph and color for a session status.
/// Hairline chrome grey shared by unfocused pane borders and the editor's
/// gutter rule — derived from the terminal's own colors (wash), so the two
/// always match and follow light/dark theme flips together.
fn chrome_grey() -> Color {
    wash(70).unwrap_or_else(|| adaptive(Color::Rgb(70, 74, 84), Color::Rgb(178, 182, 190)))
}

fn border_style(focused: bool) -> Style {
    if focused { Style::default().fg(Color::Cyan) } else { Style::default().fg(chrome_grey()) }
}

fn draw_sidebar(frame: &mut Frame, app: &mut App, area: Rect) {
    match app.shell {
        Shell::Code => draw_file_tree(frame, app, area),
        Shell::Git => draw_git_panel(frame, app, area),
        Shell::Agents => draw_agents_sidebar(frame, app, area),
    }
}

/// A status icon, one-word label, and accent color for a connection state.
/// The icon is a Material Design Nerd Font glyph when the terminal supports
/// them, else a geometric fallback — matching the tool-call glyphs.
fn agent_status(
    state: &crate::acp::ConnState,
    working: bool,
) -> (&'static str, &'static str, Color) {
    use crate::acp::ConnState;
    let fancy = crate::color::fancy_glyphs();
    let icon = |nerd: &'static str, plain: &'static str| if fancy { nerd } else { plain };
    match (state, working) {
        (ConnState::Starting, _) => (icon("\u{f051f}", "○"), "connecting", STATUS_DIM()),
        (ConnState::NeedsAuth, _) => {
            (icon("\u{f033e}", "!"), "sign in", chip_accent(3, (250, 210, 60), (176, 130, 10)))
        }
        (ConnState::Failed, _) => (icon("\u{f0159}", "✖"), "exited", REMOVE_BG()),
        (_, true) => {
            (icon("\u{f04e6}", "◐"), "working", chip_accent(3, (229, 200, 144), (193, 132, 1)))
        }
        _ => (icon("\u{f05e0}", "●"), "ready", chip_accent(6, (134, 220, 214), (1, 132, 188))),
    }
}

/// The status icon + color for a session row: a permission request, a
/// running turn, or idle. Same Nerd-vs-plain discipline as [`agent_status`].
fn session_dot(needs_perm: bool, working: bool) -> (&'static str, Color) {
    let fancy = crate::color::fancy_glyphs();
    let icon = |nerd: &'static str, plain: &'static str| if fancy { nerd } else { plain };
    if needs_perm {
        (icon("\u{f0028}", "●"), chip_accent(1, (240, 120, 120), (200, 40, 40)))
    } else if working {
        (icon("\u{f04e6}", "◐"), chip_accent(3, (229, 200, 144), (193, 132, 1)))
    } else {
        (icon("\u{f09de}", "○"), STATUS_DIM())
    }
}

/// The agents sidebar: a file-tree-style view of every agent connection
/// with its sessions nested under it. The cursor row is highlighted; a
/// session opens in the main pane, an agent collapses/expands.
fn draw_agents_sidebar(frame: &mut Frame, app: &mut App, area: Rect) {
    use crate::app::AcpRow;
    let focused = app.focus == Focus::Sidebar;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border_style(focused))
        .title(Span::styled(" agents ", Style::default().fg(STATUS_DIM())));
    app.layout.sidebar_list = block.inner(area);

    if app.acp.is_empty() {
        let msg = Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::styled("  no agent running", Style::default().fg(STATUS_DIM()))),
            Line::from(""),
            Line::from(Span::styled("  Ctrl+A c  start agent", Style::default().fg(Color::Cyan))),
        ])
        .block(block);
        frame.render_widget(msg, area);
        return;
    }

    let dim = Style::default().fg(STATUS_DIM());
    let open = app.agent_view.open.clone();
    let fancy = crate::color::fancy_glyphs();
    let rows = app.acp_rows();
    let cursor = app.agent_view.cursor.min(rows.len().saturating_sub(1));
    let items: Vec<ListItem> = rows
        .iter()
        .map(|row| match row {
            AcpRow::Agent(ci) => {
                let client = app.acp.conn(*ci);
                let name = app.acp.name(*ci);
                let working = client.map(session_working).unwrap_or(false);
                let state = client.map(|c| c.state()).unwrap_or(crate::acp::ConnState::Failed);
                let (icon, _, color) = agent_status(&state, working);
                let collapsed = app.agent_view.collapsed.contains(ci);
                let caret = match (fancy, collapsed) {
                    (true, true) => "▸ ",
                    (true, false) => "▾ ",
                    (false, true) => "> ",
                    (false, false) => "v ",
                };
                let mut spans = vec![
                    Span::styled(caret, dim),
                    Span::styled(format!("{icon} "), Style::default().fg(color)),
                    Span::styled(name, Style::default().add_modifier(Modifier::BOLD)),
                ];
                // a failed agent: append its error inline so the red dot
                // isn't a silent dead end
                if state == crate::acp::ConnState::Failed
                    && let Some(err) = client.and_then(|c| c.error())
                {
                    let brief: String = err.chars().take(24).collect();
                    spans.push(Span::styled(format!(" — {brief}"), dim));
                }
                ListItem::new(Line::from(spans))
            }
            AcpRow::Session(ci, id) => {
                let client = app.acp.conn(*ci);
                let label = app.acp_session_label(*ci, id);
                let working = client.map(|c| c.turn_active(id)).unwrap_or(false);
                let needs_perm =
                    client.map(|c| c.pending_permission(id).is_some()).unwrap_or(false);
                let (dot, dot_color) = session_dot(needs_perm, working);
                let is_open = open.as_ref().is_some_and(|(c, s)| c == ci && s == id);
                let style = if is_open {
                    Style::default().add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                ListItem::new(Line::from(vec![
                    Span::styled("   ", dim),
                    Span::styled(format!("{dot} "), Style::default().fg(dot_color)),
                    Span::styled(label, style),
                ]))
            }
        })
        .collect();

    // a List with the file-tree's persistent selection highlight
    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD));
    let mut state = std::mem::take(&mut app.agent_view.list);
    state.select((!rows.is_empty()).then_some(cursor));
    frame.render_stateful_widget(list, area, &mut state);
    app.agent_view.list = state;
    let _ = focused;
}

/// True if any of a connection's sessions has a turn running.
fn session_working(client: &crate::acp::AcpClient) -> bool {
    client.sessions().iter().any(|s| s.working)
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
            // devicons when enabled (folders too); plain glyphs otherwise
            let (icon, icon_style) = if app.config.icons {
                if item.is_dir {
                    let glyph = if item.expanded {
                        crate::devicons::FOLDER_OPEN
                    } else {
                        crate::devicons::FOLDER
                    };
                    (format!("{glyph}  "), Style::default().fg(Color::Blue))
                } else {
                    let (glyph, (r, g, b)) = crate::devicons::icon(&item.name);
                    (format!("{glyph}  "), Style::default().fg(Color::Rgb(r, g, b)))
                }
            } else if item.is_dir {
                let glyph = if item.expanded { "[▼] " } else { "[▶] " };
                (glyph.to_string(), Style::default())
            } else {
                ("   ".to_string(), Style::default())
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
                Span::styled(icon.clone(), icon_style),
                Span::styled(item.name.clone(), name_style),
            ];
            // right-aligned badges at the row's edge: a dim link arrow for
            // symlinks (VS Code's overlay), then the problem count
            // (red for errors, yellow for warnings) rightmost
            let link = item.is_symlink.then_some("↪");
            let badge = diag.get(&item.path).and_then(|&(errors, warnings)| {
                if errors > 0 {
                    Some(((errors + warnings).to_string(), severity_color(1), true))
                } else if warnings > 0 {
                    Some((warnings.to_string(), severity_color(2), false))
                } else {
                    None
                }
            });
            if link.is_some() || badge.is_some() {
                // indent (2/level) + icon (4 cols for dirs, 3 for files) +
                // name, then pad to the edge
                let used =
                    indent.chars().count() + icon.chars().count() + item.name.chars().count();
                let right = link.map_or(0, |_| 1)
                    + badge.as_ref().map_or(0, |(c, ..)| c.chars().count())
                    + (link.is_some() && badge.is_some()) as usize;
                // inside the borders, with one column of air on the right
                let inner_w = area.width.saturating_sub(3) as usize;
                let pad = inner_w.saturating_sub(used + right).max(1);
                spans.push(Span::raw(" ".repeat(pad)));
                if let Some(arrow) = link {
                    spans.push(Span::styled(arrow, Style::default().fg(STATUS_DIM())));
                    if badge.is_some() {
                        spans.push(Span::raw(" "));
                    }
                }
                if let Some((count, color, bold)) = badge {
                    let mut style = Style::default().fg(color);
                    if bold {
                        style = style.add_modifier(Modifier::BOLD);
                    }
                    spans.push(Span::styled(count, style));
                }
            }
            ListItem::new(Line::from(spans))
        })
        .collect();

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border_style(focused));
    app.layout.sidebar_list = block.inner(area);
    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD));

    if app.tree.items.is_empty() {
        app.code_view.tree_list.select(None);
    } else {
        app.code_view.tree_list.select(Some(app.tree.selected));
    }
    let mut state = std::mem::take(&mut app.code_view.tree_list);
    frame.render_stateful_widget(list, area, &mut state);
    app.code_view.tree_list = state;
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

/// One row of the changes panel, shared by both boxes and both views.
fn git_row_item(app: &App, row: &crate::git::GitRow) -> ListItem<'static> {
    let indent = "  ".repeat(row.depth);
    match row.entry {
        // same folder affordances as the file tree
        None => {
            let icon = if app.config.icons {
                if row.collapsed {
                    format!("{}  ", crate::devicons::FOLDER)
                } else {
                    format!("{}  ", crate::devicons::FOLDER_OPEN)
                }
            } else if row.collapsed {
                "[▶] ".to_string()
            } else {
                "[▼] ".to_string()
            };
            ListItem::new(Line::from(vec![
                Span::raw(format!("   {indent}")),
                Span::styled(icon, Style::default().fg(Color::Blue)),
                Span::styled(
                    row.name.clone(),
                    Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD),
                ),
            ]))
        }
        Some(idx) => {
            let entry = &app.git.entries[idx];
            let mut spans = vec![
                Span::styled(
                    entry.code(),
                    Style::default().fg(status_color(entry.kind)).add_modifier(Modifier::BOLD),
                ),
                Span::raw(format!(" {indent}")),
            ];
            if app.config.icons {
                let name = row.name.rsplit('/').next().unwrap_or(&row.name);
                let (glyph, (r, g, b)) = crate::devicons::icon(name);
                spans.push(Span::styled(
                    format!("{glyph}  "),
                    Style::default().fg(Color::Rgb(r, g, b)),
                ));
            }
            spans.push(Span::raw(row.name.clone()));
            ListItem::new(spans.into_iter().collect::<Line>())
        }
    }
}

fn draw_git_panel(frame: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Sidebar;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border_style(focused));
    let inner = block.inner(area);
    app.layout.sidebar_list = inner;

    if !app.git.is_repo() {
        frame.render_widget(Paragraph::new("not a git repository").block(block), area);
        return;
    }
    if app.git.entries.is_empty() {
        frame.render_widget(Paragraph::new("working tree clean").block(block), area);
        return;
    }

    let rows = app.git.rows();
    let split = rows.iter().filter(|r| r.staged).count();
    let cursor = app.git.cursor.min(rows.len().saturating_sub(1));
    // a rule row marks the staged/unstaged boundary (only while both
    // sections have rows); the cursor steps over it
    let sep = split > 0 && split < rows.len();

    let mut items: Vec<ListItem> = Vec::with_capacity(rows.len() + 1);
    for (i, row) in rows.iter().enumerate() {
        if sep && i == split {
            items.push(ListItem::new(Line::from(Span::styled(
                "─".repeat(inner.width as usize),
                Style::default().fg(chrome_grey()),
            ))));
        }
        items.push(git_row_item(app, row));
    }
    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD));
    app.git_view.list.select(Some(cursor + (sep && cursor >= split) as usize));
    let mut state = std::mem::take(&mut app.git_view.list);
    frame.render_stateful_widget(list, area, &mut state);
    // the separator joins the pane borders with proper junctions
    if sep && split >= state.offset() {
        let row_in_view = (split - state.offset()) as u16;
        if row_in_view < inner.height {
            let y = inner.y + row_in_view;
            let buf = frame.buffer_mut();
            for (x, arm) in [(area.x, 8u8), (area.right().saturating_sub(1), 4u8)] {
                let cell = &mut buf[(x, y)];
                if let Some(arms) = box_arms(cell.symbol())
                    && let Some(merged) = arms_box(arms | arm)
                {
                    cell.set_symbol(merged);
                }
            }
        }
    }
    app.git_view.list = state;
}

fn draw_main_area(frame: &mut Frame, app: &mut App, area: Rect) {
    match app.shell {
        Shell::Agents if app.agent_view.open.is_some() => draw_acp_conversation(frame, app, area),
        Shell::Agents => draw_agent_placeholder(frame, app, area),
        Shell::Git => draw_git_diff_main(frame, app, area),
        Shell::Code => {
            app.layout.terminal_pane = area;
            if app.image.is_some() {
                draw_image_view(frame, app, area);
            } else if app.hex.is_some() {
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
        .border_type(BorderType::Rounded)
        .border_style(border_style(focused));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    const MARK: [&str; 4] = ["  ▗▄▄▖  ", " ▟████▙ ", " ▜██▛▀  ", "  ▝▀▘   "];
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
        let selected = i == app.code_view.home_selected;
        let base =
            if selected { Style::default().bg(Color::Rgb(58, 62, 78)) } else { Style::default() };
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
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border_style(focused));
    let inner = block.inner(area);
    // pure render of the cached pane model (built in the update phase —
    // see App::refresh_git_pane); draw does no git/fs work
    let lines = &app.git_view.pane.lines;
    let viewport = inner.height as usize;
    app.git_view.diff_viewport = viewport.max(1);
    let max_scroll = lines.len().saturating_sub(viewport);
    app.git_view.diff_scroll = app.git_view.diff_scroll.min(max_scroll);
    app.git_view.diff_scroll_rendered = app.git_view.diff_scroll;
    if lines.is_empty() {
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
    let visible: Vec<Line> = lines
        .iter()
        .enumerate()
        .skip(app.git_view.diff_scroll)
        .take(viewport)
        .map(|(idx, line)| {
            render_diff_line(
                line,
                inner.width as usize,
                app.git_view.gap_hover == Some(idx),
                app.config.icons,
            )
        })
        .collect();
    frame.render_widget(Paragraph::new(visible).block(block), area);
    // the gutter rule joins the pane border with proper junctions
    // (the bar sits after the two 5-wide line-number columns)
    if inner.width > 10 {
        let x = inner.x + 10;
        let buf = frame.buffer_mut();
        for (y, arm) in [(area.y, 2u8), (area.bottom().saturating_sub(1), 1u8)] {
            let cell = &mut buf[(x, y)];
            if let Some(arms) = box_arms(cell.symbol())
                && let Some(merged) = arms_box(arms | arm)
            {
                cell.set_symbol(merged);
            }
        }
    }
}

/// The agents shell with no agent running: a prompt to start one.
fn draw_agent_placeholder(frame: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Terminal;
    app.layout.terminal_pane = area;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border_style(focused));
    let dim = Style::default().fg(STATUS_DIM());
    // a connection awaiting authentication: show its methods, keyed 1–9
    if let Some(conn) = app.acp_auth_target()
        && let Some(client) = app.acp.conn(conn)
    {
        let accent = chip_accent(3, (250, 210, 60), (176, 130, 10));
        let lock = if crate::color::fancy_glyphs() { "\u{f0341} " } else { "" };
        let mut lines = vec![
            Line::from(""),
            Line::from(vec![
                Span::styled(format!("  {lock}"), Style::default().fg(accent)),
                Span::styled(
                    format!("{} needs you to sign in", app.acp.name(conn)),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(""),
        ];
        for (i, method) in client.auth_methods().iter().enumerate() {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  {} ", i + 1),
                    Style::default().fg(accent).add_modifier(Modifier::BOLD),
                ),
                Span::raw(method.name.clone()),
            ]));
            if let Some(desc) = &method.description {
                lines.push(Line::from(Span::styled(format!("      {desc}"), dim)));
            }
        }
        if let Some(err) = client.auth_error() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                format!("  ✖ {err}"),
                Style::default().fg(REMOVE_BG()),
            )));
        }
        frame.render_widget(Paragraph::new(lines).block(block), area);
        return;
    }
    // a connection that died before opening a session: show WHY (its last
    // stderr line) instead of a perpetual "connecting…"
    let failed = app
        .acp
        .conns()
        .iter()
        .enumerate()
        .find(|(_, c)| c.state() == crate::acp::ConnState::Failed);
    let lines = if let Some((ci, client)) = failed {
        let err = client.error().unwrap_or_else(|| "the agent exited during startup".into());
        let mut out = vec![
            Line::from(""),
            Line::from(vec![
                Span::styled("  ✖ ", Style::default().fg(REMOVE_BG())),
                Span::styled(
                    format!("{} failed to start", app.acp.name(ci)),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(""),
        ];
        for line in wrap_text(&err, area.width.saturating_sub(6) as usize) {
            out.push(Line::from(Span::styled(format!("  {line}"), dim)));
        }
        out
    } else if !app.acp.is_empty() {
        // a connection exists but no session is open yet: handshake in flight
        vec![
            Line::from(""),
            Line::from(Span::styled("  connecting to the agent…", dim)),
            Line::from(""),
            Line::from(Span::styled("  pick a session in the sidebar", dim)),
        ]
    } else {
        let hint = if app.config.agent_command().is_some() {
            "  Ctrl+A c  start the configured agent"
        } else {
            "  set `agent` in .vibin/config.toml, then Ctrl+A c"
        };
        vec![
            Line::from(""),
            Line::from(Span::styled("  no agent running", dim)),
            Line::from(""),
            Line::from(Span::styled(hint, Style::default().fg(Color::Cyan))),
        ]
    };
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// Kind → glyph for an ACP tool call. Nerd Font icons when fancy, ASCII
/// otherwise — the same slant/glyph discipline as the rest of the chrome.
fn acp_tool_glyph(kind: &str) -> &'static str {
    let fancy = crate::color::fancy_glyphs();
    match (kind, fancy) {
        ("read", true) => "\u{f0e2f}",    // book-open
        ("edit", true) => "\u{f03eb}",    // pencil
        ("delete", true) => "\u{f0a7a}",  // trash
        ("move", true) => "\u{f0450}",    // file-move
        ("search", true) => "\u{f0349}",  // magnify
        ("execute", true) => "\u{f018d}", // console
        ("fetch", true) => "\u{f0ac0}",   // download
        ("think", true) => "\u{f0210}",   // lightbulb
        (_, true) => "\u{f0877}",         // wrench
        _ => "*",
    }
}

/// The ACP agent conversation: a scrollable transcript (user prompts,
/// streamed agent text, tool calls, plans) with a permission prompt and a
/// prompt composer pinned to the bottom. Pure reader of `app.acp` — the
/// client's reader thread owns all the state (see [`crate::acp`]).
fn draw_acp_conversation(frame: &mut Frame, app: &mut App, area: Rect) {
    use crate::acp::{ConnState, Entry};
    let Some((conn, id)) = app.agent_view.open.clone() else { return };
    let Some(client) = app.acp.conn(conn) else { return };
    let focused = app.focus == Focus::Terminal;
    app.layout.terminal_pane = area;
    // cleared each draw, re-recorded only when the composer paints them, so a
    // permission prompt or a dead agent leaves no phantom hit target behind
    app.layout.agent_mode_chip = Rect::default();
    app.layout.agent_mode_menu = Rect::default();
    app.layout.agent_mention_menu = Rect::default();
    let mode_hover = app.agent_view.mode_hover;
    let menu_open = app.agent_view.mode_menu.is_some();

    let (state, entries, pending) =
        (client.state(), client.entries(&id), client.pending_permission(&id));
    let working = client.turn_active(&id);
    // the session's selectable modes and which one is active, for the meta
    // line's dropdown (empty when the agent has no modes)
    let modes = client.modes(&id);
    let current_mode = client.current_mode(&id);
    let ui = app.agent_view.ui.get(&id);
    let composer_text = ui.map(|u| u.input.text()).unwrap_or_default();
    let composer_cursor = ui.map(|u| u.input.cursor()).unwrap_or(0);
    let composer_sel = ui.and_then(|u| u.input.selection());
    let mut scroll = ui.map(|u| u.scroll).unwrap_or(0);

    // the composer meta line just names the agent; the mode (if any) sits
    // beside it as a dropdown
    let agent_brand = app.acp.name(conn);
    let mode_name = current_mode
        .as_ref()
        .and_then(|cur| modes.iter().find(|m| &m.id == cur))
        .map(|m| m.name.clone())
        .or_else(|| modes.first().map(|m| m.name.clone()));

    // the turn-status accent, for the composer prompt + permission block
    let accent = match (&state, working) {
        (ConnState::Starting, _) => MENU_TINT(),
        (ConnState::Failed, _) => REMOVE_BG(),
        (_, true) => chip_accent(3, (229, 200, 144), (193, 132, 1)),
        _ => chip_accent(6, (134, 220, 214), (1, 132, 188)),
    };
    // plain pane frame, like the editor and git diff panes — the session's
    // name lives in the status bar (as the editor bar shows the filename)
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border_style(focused));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // vertical split: transcript · [permission] · composer · key hints.
    // the composer is a padded box (input, blank, meta line); a permission
    // prompt or a dead agent collapses it to a single status line.
    let perm_h = pending
        .as_ref()
        .map(|p| p.options.len() as u16 + 3) // title + options + frame
        .unwrap_or(0);
    let composer_h = if pending.is_some() || matches!(state, ConnState::Failed) { 1 } else { 5 };
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(perm_h),
            Constraint::Length(composer_h),
            Constraint::Length(1),
        ])
        .split(inner);

    // ---- transcript ----
    let width = rows[0].width as usize;
    let mut lines: Vec<Line> = Vec::new();
    // (line index into `lines`, col, label, url) of every markdown link, for
    // the OSC 8 + hitbox pass once the visible window is known
    let mut transcript_links: Vec<(usize, usize, String, String)> = Vec::new();
    let dim = Style::default().fg(STATUS_DIM());
    for entry in &entries {
        match entry {
            Entry::User(text) => {
                lines.push(Line::from(""));
                for (i, w) in wrap_text(text, width.saturating_sub(4)).into_iter().enumerate() {
                    let prefix = if i == 0 { " › " } else { "   " };
                    lines.push(Line::from(vec![
                        Span::styled(
                            prefix,
                            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(w, Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
                    ]));
                }
            }
            Entry::Agent(text) => {
                // agent prose is markdown: headings, lists, code fences, and
                // inline styling, wrapped to the pane and indented a column to
                // match the transcript's left margin
                let (md_lines, md_links) =
                    crate::markdown::render_with_links(text, width.saturating_sub(2));
                let base = lines.len();
                for link in md_links {
                    // +1 col for the indent space prepended below
                    transcript_links.push((base + link.line, link.col + 1, link.text, link.url));
                }
                for md in md_lines {
                    let mut spans = vec![Span::raw(" ")];
                    spans.extend(md.spans);
                    lines.push(Line::from(spans));
                }
            }
            Entry::Tool(tc) => {
                let glyph = acp_tool_glyph(&tc.kind);
                let (mark, mstyle) = match tc.status.as_str() {
                    "completed" => {
                        ("✓", Style::default().fg(chip_accent(2, (152, 195, 121), (80, 161, 79))))
                    }
                    "failed" => ("✗", Style::default().fg(REMOVE_BG())),
                    "in_progress" => ("◐", Style::default().fg(accent)),
                    _ => ("·", dim),
                };
                let mut spans = vec![
                    Span::raw("   "),
                    Span::styled(format!("{mark} "), mstyle),
                    Span::styled(format!("{glyph} "), dim),
                    Span::raw(tc.title.clone()),
                ];
                if !tc.locations.is_empty() {
                    spans.push(Span::styled(format!("  {}", tc.locations.join(", ")), dim));
                }
                lines.push(Line::from(spans));
            }
            Entry::Plan(plan) => {
                for e in plan {
                    let g = match e.status.as_str() {
                        "completed" => "✓",
                        "in_progress" => "◐",
                        _ => "○",
                    };
                    lines.push(Line::from(vec![
                        Span::styled(format!("   {g} "), dim),
                        Span::raw(e.content.clone()),
                    ]));
                }
            }
            Entry::Notice(text) => {
                lines.push(Line::from(Span::styled(format!("   ! {text}"), dim)));
            }
        }
    }
    if lines.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled("  ask the agent anything to begin", dim)));
    }

    // scroll counts lines above the tail (0 = follow the tail); clamp and
    // write the clamped value back onto the open session
    let height = rows[0].height as usize;
    let total = lines.len();
    let max_up = total.saturating_sub(height);
    scroll = scroll.min(max_up);
    app.agent_view.ui.entry(id.clone()).or_default().scroll = scroll;
    let start = max_up.saturating_sub(scroll);
    let view: Vec<Line> = lines.into_iter().skip(start).take(height).collect();
    frame.render_widget(Paragraph::new(view), rows[0]);

    // clickable markdown links on the visible rows: OSC 8 cells (native
    // terminal click) + hitboxes (our own click + hand pointer), like the
    // notification pane
    for (row, col, label, url) in transcript_links {
        if row < start || row >= start + height {
            continue; // scrolled out of view
        }
        if col + label.chars().count() > width {
            continue; // clipped labels would render a broken sequence
        }
        let x = rows[0].x + col as u16;
        let y = rows[0].y + (row - start) as u16;
        let hitbox = Rect::new(x, y, label.chars().count().max(1) as u16, 1);
        let style = frame.buffer_mut().cell((x, y)).map(|c| c.style()).unwrap_or_default();
        if let Some(cell) = frame.buffer_mut().cell_mut((x, y)) {
            use unicode_width::UnicodeWidthStr;
            let label_width = label.as_str().width().max(1) as u16;
            cell.set_symbol(&format!("\x1b]8;;{url}\x1b\\{label}\x1b]8;;\x1b\\"));
            cell.set_style(style);
            cell.set_diff_option(ratatui::buffer::CellDiffOption::ForcedWidth(
                std::num::NonZeroU16::new(label_width).expect("max(1) above"),
            ));
        }
        app.link_hits.push((hitbox, url));
    }

    // ---- permission prompt ----
    if let Some(perm) = &pending {
        let pblock = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(accent))
            .title(Span::styled(
                " permission ",
                Style::default().fg(accent).add_modifier(Modifier::BOLD),
            ));
        let pinner = pblock.inner(rows[1]);
        frame.render_widget(pblock, rows[1]);
        let mut plines = vec![Line::from(format!(" {}", perm.title))];
        for (i, opt) in perm.options.iter().enumerate() {
            let key = Style::default().fg(accent).add_modifier(Modifier::BOLD);
            plines.push(Line::from(vec![
                Span::styled(format!("  {} ", i + 1), key),
                Span::raw(opt.name.clone()),
            ]));
        }
        frame.render_widget(Paragraph::new(plines), pinner);
    }

    // ---- composer ----
    if pending.is_some() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(" 1–9 choose · Esc reject", dim))),
            rows[2],
        );
    } else if matches!(state, ConnState::Failed) {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                " agent exited — Ctrl+A then Tools to restart",
                dim,
            ))),
            rows[2],
        );
    } else {
        // an accent-barred input box: the message on top, then a meta line
        // naming the agent · session · status. inset a column on each side,
        // with half-block top and bottom edges, so it floats as a soft panel;
        // a left bar in the turn-status color and a faint fill are its frame.
        let box_area = rows[2];
        let panel_x = box_area.x + 1;
        let panel_w = box_area.width.saturating_sub(2);
        let panel_h = box_area.height;
        let bg = wash(22);
        let base = bg.map(|c| Style::default().bg(c)).unwrap_or_default();
        if let Some(fill) = bg {
            // filled middle, then upper/lower half blocks for the soft edges
            frame.render_widget(
                Block::default().style(base),
                Rect { x: panel_x, y: box_area.y + 1, width: panel_w, height: panel_h - 2 },
            );
            let edge = Style::default().fg(fill);
            frame.render_widget(
                Paragraph::new(Span::styled("▄".repeat(panel_w as usize), edge)),
                Rect { x: panel_x, y: box_area.y, width: panel_w, height: 1 },
            );
            frame.render_widget(
                Paragraph::new(Span::styled("▀".repeat(panel_w as usize), edge)),
                Rect { x: panel_x, y: box_area.y + panel_h - 1, width: panel_w, height: 1 },
            );
        }
        // the accent bar spans the content rows, between the soft edges
        let bar = if crate::color::fancy_glyphs() { "▎" } else { "│" };
        for dy in 1..panel_h.saturating_sub(1) {
            frame.render_widget(
                Paragraph::new(Span::styled(bar, base.fg(accent))),
                Rect { x: panel_x, y: box_area.y + dy, width: 1, height: 1 },
            );
        }
        // one column of padding inside the bar, one before the right edge
        let content_x = panel_x + 2;
        let field_w = panel_w.saturating_sub(3) as usize;

        // the message row, with the selection highlighted; a horizontal window
        // keeps the cursor visible in a long line
        let input_y = box_area.y + 1;
        let chars: Vec<char> = composer_text.chars().collect();
        let start = composer_cursor.saturating_sub(field_w);
        let field_rect = Rect { x: content_x, y: input_y, width: field_w as u16, height: 1 };
        if chars.is_empty() {
            frame.render_widget(
                Paragraph::new(Span::styled(format!("message {agent_brand}…"), base.patch(dim)))
                    .style(base),
                field_rect,
            );
        } else {
            let sel_style = Style::default().bg(SELECTION_BG());
            let mut spans = Vec::new();
            for (i, c) in chars.iter().enumerate().skip(start).take(field_w) {
                let selected = composer_sel.is_some_and(|(s, e)| i >= s && i < e);
                let style = if selected { sel_style } else { base };
                spans.push(Span::styled(c.to_string(), style));
            }
            frame.render_widget(Paragraph::new(Line::from(spans)).style(base), field_rect);
        }
        // the real (blinking) terminal cursor sits at the composer cursor
        if focused {
            let x = content_x + (composer_cursor - start) as u16;
            let right = panel_x + panel_w.saturating_sub(1);
            frame.set_cursor_position((x.min(right), input_y));
        }

        // the meta row: the active mode as a dropdown (accent), then the
        // agent's brand (dim); one blank line below the message
        if panel_h >= 5 {
            let meta_y = box_area.y + 3;
            let mut meta = Vec::new();
            if let Some(name) = &mode_name {
                let caret = if crate::color::fancy_glyphs() { " ▾" } else { " v" };
                let label = format!("{name}{caret}");
                // record the chip's rect so a click opens the dropdown; a
                // hover (or the open menu) tints it like a menu-bar button
                let chip_w = label.chars().count() as u16;
                app.layout.agent_mode_chip =
                    Rect { x: content_x, y: meta_y, width: chip_w, height: 1 };
                let mut chip = base.fg(accent).add_modifier(Modifier::BOLD);
                if mode_hover || menu_open {
                    chip = chip.bg(SELECTION_BG());
                }
                meta.push(Span::styled(label, chip));
                meta.push(Span::styled("   ", base));
            }
            meta.push(Span::styled(agent_brand.clone(), base.patch(dim)));
            frame.render_widget(
                Paragraph::new(Line::from(meta)).style(base),
                Rect { x: content_x, y: meta_y, width: field_w as u16, height: 1 },
            );
        }
    }

    // ---- key hints, right-aligned beneath the composer ----
    if pending.is_none() && !matches!(state, ConnState::Failed) {
        let key = Style::default().add_modifier(Modifier::BOLD);
        let scroll_key = if crate::color::fancy_glyphs() { "↑↓" } else { "pgup" };
        let mut hint = vec![Span::styled(scroll_key, key), Span::styled(" scroll   ", dim)];
        if !modes.is_empty() {
            hint.push(Span::styled("shift+tab", key));
            hint.push(Span::styled(" mode   ", dim));
        }
        hint.push(Span::styled("esc", key));
        hint.push(Span::styled(" back   ", dim));
        hint.push(Span::styled("enter", key));
        hint.push(Span::styled(" send ", dim));
        frame.render_widget(
            Paragraph::new(Line::from(hint)).alignment(ratatui::layout::Alignment::Right),
            rows[3],
        );
    }

    // ---- mode dropdown: a floating list popping up from the meta line ----
    if let Some(sel) = app.agent_view.mode_menu
        && !modes.is_empty()
    {
        let width =
            modes.iter().map(|m| m.name.chars().count()).max().unwrap_or(4).clamp(8, 28) as u16 + 4;
        let height = modes.len() as u16 + 2;
        let anchor_x = rows[2].x + 3; // aligns under the meta line's mode chip
        let meta_y = rows[2].y + 3;
        let y = meta_y.saturating_sub(height);
        let popup = Rect {
            x: anchor_x.min(area.right().saturating_sub(width)),
            y,
            width: width.min(area.width),
            height,
        };
        app.layout.agent_mode_menu = popup;
        frame.render_widget(Clear, popup);
        // square borders mark a floating layer, unlike the rounded panes
        let mblock =
            Block::default().borders(Borders::ALL).border_style(Style::default().fg(accent)).title(
                Span::styled(" mode ", Style::default().fg(accent).add_modifier(Modifier::BOLD)),
            );
        let minner = mblock.inner(popup);
        frame.render_widget(mblock, popup);
        let items: Vec<ListItem> = modes
            .iter()
            .enumerate()
            .map(|(i, m)| {
                let active = current_mode.as_deref() == Some(m.id.as_str());
                let mark = if active { "● " } else { "  " };
                let style = if i == sel {
                    Style::default().fg(accent).add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                ListItem::new(Line::from(vec![
                    Span::styled(mark, Style::default().fg(accent)),
                    Span::styled(m.name.clone(), style),
                ]))
            })
            .collect();
        let mut list_state = ratatui::widgets::ListState::default();
        list_state.select(Some(sel));
        frame.render_stateful_widget(
            List::new(items).highlight_style(Style::default().bg(SELECTION_BG())),
            minner,
            &mut list_state,
        );
    }

    // ---- @-mention file picker, popping up from the composer input line ----
    if let Some(mention) = &app.agent_view.mention
        && !mention.results.is_empty()
    {
        let width =
            mention.results.iter().map(|r| r.chars().count()).max().unwrap_or(10).clamp(12, 48)
                as u16
                + 2;
        let height = mention.results.len() as u16 + 2;
        let anchor_x = rows[2].x + 3;
        let input_y = rows[2].y + 1;
        let popup = Rect {
            x: anchor_x.min(area.right().saturating_sub(width)),
            y: input_y.saturating_sub(height),
            width: width.min(area.width),
            height,
        };
        app.layout.agent_mention_menu = popup;
        frame.render_widget(Clear, popup);
        let mblock =
            Block::default().borders(Borders::ALL).border_style(Style::default().fg(accent)).title(
                Span::styled(" @file ", Style::default().fg(accent).add_modifier(Modifier::BOLD)),
            );
        let minner = mblock.inner(popup);
        frame.render_widget(mblock, popup);
        let items: Vec<ListItem> = mention
            .results
            .iter()
            .map(|r| ListItem::new(Line::from(Span::raw(r.clone()))))
            .collect();
        let mut list_state = ratatui::widgets::ListState::default();
        list_state.select(Some(mention.selected));
        frame.render_stateful_widget(
            List::new(items).highlight_style(Style::default().bg(SELECTION_BG())),
            minner,
            &mut list_state,
        );
    }
}

/// Chip label color: the terminal background — readable on any accent.
fn chip_text() -> Color {
    match crate::color::terminal_bg() {
        Some((r, g, b)) => Color::Rgb(r, g, b),
        None => adaptive(Color::Rgb(16, 18, 24), Color::Rgb(240, 240, 244)),
    }
}

/// Subtle chip fill shared by the menu-bar items and other clickable
/// chips (like the status-bar branch box).
#[allow(non_snake_case)]
fn MENU_TINT() -> Color {
    wash(40).unwrap_or_else(|| adaptive(Color::Rgb(48, 51, 58), Color::Rgb(226, 228, 233)))
}

/// A slanted chip in the menu-bar shape: ◢ label ◤. Pass a text color for
/// bold accent chips (modes), None for quiet default-text chips. The caps
/// degrade to spaces without fancy glyphs, keeping widths stable.
fn slant_chip(label: String, bg: Color, fg: Option<Color>) -> [Span<'static>; 3] {
    let fancy = crate::color::fancy_glyphs();
    let cap = |s: &'static str| Span::styled(if fancy { s } else { " " }, Style::default().fg(bg));
    let mut style = Style::default().bg(bg);
    if let Some(fg) = fg {
        style = style.fg(fg).add_modifier(Modifier::BOLD);
    }
    [cap("◢"), Span::styled(label, style), cap("◤")]
}

/// Chip accent from the terminal palette, with dark/light fallbacks.
fn chip_accent(slot: usize, dark: (u8, u8, u8), light: (u8, u8, u8)) -> Color {
    let (r, g, b) =
        crate::color::ansi16(slot).unwrap_or(if crate::color::is_light() { light } else { dark });
    Color::Rgb(r, g, b)
}

/// Mode chip colors, from the terminal theme: NORMAL stays a quiet grey
/// (the resting state), INSERT is the theme's green, SELECT its magenta.
fn mode_colors(mode: Mode) -> (Color, Color) {
    let bg = match mode {
        Mode::Normal => wash(150)
            .unwrap_or_else(|| adaptive(Color::Rgb(122, 132, 160), Color::Rgb(120, 126, 140))),
        Mode::Insert => chip_accent(2, (166, 218, 149), (60, 140, 60)),
        Mode::Select => chip_accent(5, (198, 160, 246), (150, 60, 150)),
    };
    (bg, chip_text())
}

/// dark-vs-light pick, keyed off the OSC 11 terminal-background luminance
fn adaptive(dark: Color, light: Color) -> Color {
    if crate::color::is_light() { light } else { dark }
}

/// Visible characters of a cell symbol that embeds escape sequences
/// (CSI ... letter, OSC ... BEL/ST).
fn strip_escapes(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('[') => {
                while let Some(&n) = chars.peek() {
                    chars.next();
                    if n.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            Some(']') => {
                // OSC: runs to BEL or ESC \
                while let Some(n) = chars.next() {
                    if n == '\x07' {
                        break;
                    }
                    if n == '\x1b' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

// Box-drawing arms (light set): U=1 D=2 L=4 R=8.
fn box_arms(sym: &str) -> Option<u8> {
    Some(match sym {
        "─" => 4 | 8,
        "│" => 1 | 2,
        "┌" | "╭" => 2 | 8,
        "┐" | "╮" => 2 | 4,
        "└" | "╰" => 1 | 8,
        "┘" | "╯" => 1 | 4,
        "├" => 1 | 2 | 8,
        "┤" => 1 | 2 | 4,
        "┬" => 2 | 4 | 8,
        "┴" => 1 | 4 | 8,
        "┼" => 1 | 2 | 4 | 8,
        _ => return None,
    })
}

fn arms_box(arms: u8) -> Option<&'static str> {
    Some(match arms {
        0b1100 => "─",
        0b0011 => "│",
        // corners come back rounded: only pane borders get merged, and
        // panes draw rounded corners
        0b1010 => "╭",
        0b0110 => "╮",
        0b1001 => "╰",
        0b0101 => "╯",
        0b1011 => "├",
        0b0111 => "┤",
        0b1110 => "┬",
        0b1101 => "┴",
        0b1111 => "┼",
        _ => return None,
    })
}

/// Theme-native grey (see color::wash), as a ratatui Color.
fn wash(weight: u32) -> Option<Color> {
    crate::color::wash(weight).map(|(r, g, b)| Color::Rgb(r, g, b))
}

/// Wavy undercurl as plain cell style: the custom [`crate::backend::UNDERCURL`]
/// modifier plus `underline_color` for the curl. The backend wrapper
/// renders the SGR 4:3; here it's ordinary per-char styling that diffs,
/// clips, and dims like any other cell.
fn render_undercurl(
    buf: &mut ratatui::buffer::Buffer,
    x: u16,
    y: u16,
    text: &str,
    curl: (u8, u8, u8),
) {
    let (r, g, b) = curl;
    for i in 0..text.chars().count() as u16 {
        if let Some(cell) = buf.cell_mut((x + i, y)) {
            let style = cell
                .style()
                .add_modifier(crate::backend::UNDERCURL)
                .underline_color(Color::Rgb(r, g, b));
            cell.set_style(style);
        }
    }
}

/// Recolor every cell toward the terminal background so an open modal
/// reads as the only lit layer: RGB colors blend ~55% toward the
/// background, default-colored text gets an explicit dim wash (there is
/// no RGB to blend), and indexed colors lean on the DIM attribute.
fn dim_background(buf: &mut ratatui::buffer::Buffer) {
    let (br, bgc, bb) = crate::color::terminal_bg().unwrap_or(if crate::color::is_light() {
        (255, 255, 255)
    } else {
        (0, 0, 0)
    });
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
            // escape-packed cells (undercurls, OSC 8 links) would keep
            // their embedded colors under the veil — flatten them to text
            if cell.symbol().contains('\x1b') {
                let plain = strip_escapes(cell.symbol());
                cell.set_symbol(&plain);
            }
            let style = cell.style();
            let fg = match style.fg {
                Some(Color::Reset) | None => Some(default_fg),
                Some(c) => Some(dim_color(c)),
            };
            let bg = style.bg.map(dim_color);
            let underline = style.underline_color.map(dim_color);
            cell.set_style(
                Style { fg, bg, underline_color: underline, ..style }.add_modifier(Modifier::DIM),
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
#[allow(non_snake_case)]
fn HOVER_SPAN_BG() -> Color {
    // background tint for the symbol a hover popup describes — its
    // origin stays marked while the popup is up
    wash(52).unwrap_or_else(|| adaptive(Color::Rgb(48, 52, 64), Color::Rgb(214, 218, 228)))
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
        (Some(b), Some(c)) => Some(((b.0 / 2 + c.0 / 2), (b.1 / 2 + c.1 / 2), (b.2 / 2 + c.2 / 2))),
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
        1 => (1, (240, 90, 105), (200, 40, 50)),   // error: red
        2 => (3, (250, 210, 60), (176, 130, 10)),  // warning: yellow/amber
        _ => (4, (140, 170, 200), (60, 110, 160)), // info/hint: blue
    };
    crate::color::ansi16(slot).unwrap_or(if crate::color::is_light() { light } else { dark })
}

fn severity_color(severity: u8) -> Color {
    let (r, g, b) = severity_rgb(severity);
    Color::Rgb(r, g, b)
}

fn draw_editor(frame: &mut Frame, app: &mut App, pane: Rect) {
    let focused = app.focus == Focus::Terminal;
    // git change markers for the gutter (added/modified/deleted vs HEAD)
    let gutter_marks = app.editor_gutter_diff().unwrap_or_default();
    let diagnostics = match (&app.lsp, &app.editor) {
        (Some(client), Some(editor)) => client.diagnostics(&editor.path),
        _ => Vec::new(),
    };
    let doc_links = match (&app.lsp, &app.editor) {
        (Some(client), Some(editor)) => client.document_links(&editor.path),
        _ => Vec::new(),
    };
    // code lens titles per line, joined for the end-of-line annotation
    let mut lens_titles: std::collections::HashMap<usize, String> =
        std::collections::HashMap::new();
    if let (Some(client), Some(editor)) = (&app.lsp, &app.editor) {
        for lens in client.code_lenses(&editor.path) {
            let slot = lens_titles.entry(lens.line).or_default();
            if !slot.is_empty() {
                slot.push_str(" · ");
            }
            slot.push_str(&lens.title);
        }
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
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
    // marker column + right-aligned number + space + border rule
    let gutter_width = (total_lines.max(1).ilog10() as u16 + 4).max(6);
    let text_area = Rect::new(
        inner.x + gutter_width,
        inner.y,
        inner.width.saturating_sub(gutter_width),
        inner.height,
    );
    app.layout.editor_text = text_area;
    app.layout.editor_gutter = Rect::new(inner.x, inner.y, gutter_width, inner.height);

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
    // ghost skeleton while the background parse runs: the real text in a
    // dim theme-grey, no syntax colors — resolves in place a frame or two
    // later. Readable, so it reads as loading rather than a wall of bars.
    let hl_pending = editor.highlight_pending();
    let ghost_style = Style::default().fg(STATUS_DIM());

    for row in 0..text_height {
        let line_idx = scroll + row;
        if line_idx >= total_lines {
            break;
        }
        // gutter: line number + diagnostic dot for lines with findings
        let nr_style = if line_idx == cursor_line {
            Style::default().fg(CURSORLINE_NR())
        } else {
            // same dim as the status bar's secondary text
            Style::default().fg(STATUS_DIM())
        };
        let line_severity =
            diagnostics.iter().filter(|d| d.line == line_idx).map(|d| d.severity).min();
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
        // change markers live in the gutter rule itself: the │ turns into
        // a double ║ in green (added) or amber (modified), and a red ▸
        // marks a boundary where lines were deleted
        let last_content_line = total_lines.saturating_sub(1);
        // hovering a marker fills its whole hunk — the contiguous group
        // of added/modified rows thickens together, previewing exactly
        // what the dwell popup will describe
        let hovered = app
            .code_view
            .gutter_hover
            .and_then(|l| gutter_marks.hover_range(l, total_lines))
            .is_some_and(|r| r.contains(&line_idx));
        let fill = |glyph| if hovered { "█" } else { glyph };
        let (rule_glyph, rule_color) = if gutter_marks.deleted_at.contains(&line_idx)
            || (line_idx == last_content_line && gutter_marks.deleted_at.contains(&total_lines))
        {
            (fill("▸"), REMOVE_ACCENT())
        } else if gutter_marks.added.contains(&line_idx) {
            (fill("║"), ADD_ACCENT())
        } else if gutter_marks.modified.contains(&line_idx) {
            (fill("║"), MOD_ACCENT())
        } else {
            // plain rule: same grey as the pane borders it joins into
            ("│", chrome_grey())
        };
        let gutter = Rect::new(inner.x, inner.y + row as u16, gutter_width, 1);
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                marker,
                Span::styled(
                    format!("{:>w$} ", line_idx + 1, w = (gutter_width - 3) as usize),
                    nr_style,
                ),
                Span::styled(rule_glyph, Style::default().fg(rule_color)),
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
        let base_style = if hl_pending { ghost_style } else { Style::default() };
        let mut styles: Vec<Style> = vec![base_style; visible.chars().count()];
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
                for slot in
                    styles.iter_mut().take(upto).skip(diag.col_start.saturating_sub(hscroll))
                {
                    *slot = slot
                        .add_modifier(Modifier::UNDERLINED)
                        .underline_color(severity_color(diag.severity));
                }
            }
        }
        // document links (LSP): quiet colored underline, ctrl+click opens
        for link in doc_links.iter().filter(|l| l.line == line_idx) {
            let upto = link.col_end.saturating_sub(hscroll).min(styles.len());
            for slot in styles.iter_mut().take(upto).skip(link.col_start.saturating_sub(hscroll)) {
                *slot = slot
                    .add_modifier(Modifier::UNDERLINED)
                    .underline_color(crate::markdown::LINK_FG());
            }
        }
        // cursor-line tint first, so the selection background below wins
        let on_cursor_line = line_idx == cursor_line;
        if on_cursor_line {
            for slot in styles.iter_mut() {
                *slot = slot.bg(cursorline_bg());
            }
        }
        // hover origin: while an LSP hover popup is open, the word it
        // describes keeps a marker tint
        if let (Some(crate::app::Overlay::Hover(_)), Some((h_line, h_col))) =
            (&app.overlay, app.code_view.hover_doc_pos)
            && line_idx == h_line
        {
            let chars: Vec<char> = content.chars().collect();
            let is_word = |c: &char| c.is_alphanumeric() || *c == '_';
            let col = h_col.min(chars.len());
            if chars.get(col).is_some_and(is_word) {
                let mut lo = col;
                while lo > 0 && is_word(&chars[lo - 1]) {
                    lo -= 1;
                }
                let mut hi = col;
                while hi < chars.len() && is_word(&chars[hi]) {
                    hi += 1;
                }
                let (lo, hi) = (lo.saturating_sub(hscroll), hi.saturating_sub(hscroll));
                let upto = hi.min(styles.len());
                for slot in styles.iter_mut().take(upto).skip(lo) {
                    *slot = slot.bg(HOVER_SPAN_BG());
                }
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
        // code lens annotation: dim italic command titles after the code
        if let Some(titles) = lens_titles.get(&line_idx) {
            segments.push(Span::styled(
                format!("  ▸ {titles}"),
                Style::default().fg(STATUS_DIM()).add_modifier(Modifier::ITALIC),
            ));
        }
        let row_base =
            if on_cursor_line { Style::default().bg(cursorline_bg()) } else { Style::default() };
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
                let mut run_start = start;
                while run_start < end {
                    let style = styles[run_start];
                    let mut run_end = run_start + 1;
                    while run_end < end && styles[run_end] == style {
                        run_end += 1;
                    }
                    let text: String = visible_chars[run_start..run_end].iter().collect();
                    render_undercurl(
                        frame.buffer_mut(),
                        text_area.x + run_start as u16,
                        inner.y + row as u16,
                        &text,
                        curl,
                    );
                    run_start = run_end;
                }
            }
            // spell squiggles: same wavy cells, muted color
            let visible_chars: Vec<char> = visible.chars().collect();
            for &(s, e) in &spell_ranges {
                let end = e.min(visible_chars.len());
                let mut run_start = s.min(end);
                while run_start < end {
                    let style = styles[run_start];
                    let mut run_end = run_start + 1;
                    while run_end < end && styles[run_end] == style {
                        run_end += 1;
                    }
                    let text: String = visible_chars[run_start..run_end].iter().collect();
                    render_undercurl(
                        frame.buffer_mut(),
                        text_area.x + run_start as u16,
                        inner.y + row as u16,
                        &text,
                        SPELL_CURL(),
                    );
                    run_start = run_end;
                }
            }
        }
    }

    // real terminal cursor on the text — only when the pane is focused
    // and nothing (overlay or leader menu) is drawn on top
    let cursor_allowed = focused && app.overlay.is_none() && !app.leader_pending;
    app.layout.editor_cursor = None;
    if cursor_allowed && editor.command.is_none() && cursor_line >= scroll && cursor_col >= hscroll
    {
        let row = (cursor_line - scroll) as u16;
        if row < text_area.height {
            let col = (cursor_col - hscroll) as u16;
            let x = text_area.x + col.min(text_area.width.saturating_sub(1));
            let pos = ratatui::layout::Position::new(x, text_area.y + row);
            frame.set_cursor_position(pos);
            app.layout.editor_cursor = Some(pos);
        }
    }

    // register each visible document link as a hitbox, so the shared hover
    // preview and hand pointer treat it like any other link (ctrl+click still
    // opens it, via link_at)
    for link in &doc_links {
        if link.line < scroll || link.line >= scroll + text_height {
            continue;
        }
        let col = link.col_start.saturating_sub(hscroll);
        let end = link.col_end.saturating_sub(hscroll).min(text_area.width as usize);
        if end <= col {
            continue;
        }
        let x = text_area.x + col as u16;
        let y = text_area.y + (link.line - scroll) as u16;
        app.link_hits.push((Rect::new(x, y, (end - col) as u16, 1), link.target.clone()));
    }

    // the gutter rule joins the pane border with proper junctions
    {
        let x = inner.x + gutter_width - 1;
        let buf = frame.buffer_mut();
        for (y, arm) in [(pane.y, 2u8), (pane.bottom().saturating_sub(1), 1u8)] {
            let cell = &mut buf[(x, y)];
            if let Some(arms) = box_arms(cell.symbol())
                && let Some(merged) = arms_box(arms | arm)
            {
                cell.set_symbol(merged);
            }
        }
    }

    // scrollbars drawn over the pane borders: vertical right, horizontal
    // bottom (both hide themselves when everything fits)
    // one cell inside the frame, floating over the content edge rather
    // than sitting on the border line itself
    let vbar =
        Rect::new(pane.right().saturating_sub(2), pane.y + 1, 1, pane.height.saturating_sub(2));
    draw_pane_scrollbar(frame, vbar, total_lines, text_height, scroll);
    let hbar = Rect::new(
        text_area.x,
        pane.bottom().saturating_sub(2),
        text_area.width.saturating_sub(1),
        1,
    );
    draw_pane_hscrollbar(frame, hbar, widest, text_area.width as usize, hscroll);
}

/// Image preview: the decoded image centered in the pane, drawn through
/// the negotiated terminal graphics protocol — pixel-perfect where the
/// terminal supports one, colored half-block cells elsewhere.
fn draw_image_view(frame: &mut Frame, app: &mut App, pane: Rect) {
    let focused = app.focus == Focus::Terminal;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border_style(focused));
    let inner = block.inner(pane);
    frame.render_widget(block, pane);
    let Some(view) = &mut app.image else { return };
    if inner.width < 2 || inner.height < 2 {
        return;
    }
    use ratatui_image::{Resize, StatefulImage};
    let font = app.picker.font_size();
    let (px_w, px_h) = view.frame_px;
    // stills may upscale 2x (hidpi cells report physical pixels, so 1x
    // looks half-size); widget GIFs stay 1x -- every displayed pixel of
    // every frame is re-encoded and transmitted on these terminals
    let upscale = if view.frame_count() == 1 { 2 } else { 1 };
    match view.visual() {
        // decode still running on its thread; poll() swaps this in
        crate::imageview::Visual::Loading => {
            let row = Rect::new(inner.x, inner.y + inner.height / 2, inner.width, 1);
            frame.render_widget(
                Paragraph::new("decoding image…")
                    .alignment(ratatui::layout::Alignment::Center)
                    .style(Style::default().fg(STATUS_DIM())),
                row,
            );
        }
        // Never upscale the widget path: the render area decides how many
        // pixels get encoded and pushed through the pty — upscaling to
        // hidpi pane resolution costs megabytes per image, and for a GIF
        // pays that once per frame. Cap at the natural cell size, centered.
        crate::imageview::Visual::Widget(protocol) => {
            let natural_w =
                (px_w * upscale).div_ceil(font.width.max(1) as u32).min(u16::MAX as u32) as u16;
            let natural_h =
                (px_h * upscale).div_ceil(font.height.max(1) as u32).min(u16::MAX as u32) as u16;
            let fit = protocol.size_for(
                Resize::Fit(None),
                ratatui::layout::Size::new(inner.width.min(natural_w), inner.height.min(natural_h)),
            );
            let area = Rect::new(
                inner.x + (inner.width.saturating_sub(fit.width)) / 2,
                inner.y + (inner.height.saturating_sub(fit.height)) / 2,
                fit.width.min(inner.width),
                fit.height.min(inner.height),
            );
            frame.render_stateful_widget(StatefulImage::default(), area, protocol);
        }
        // kitty animation: pixels were transmitted once, the placeholder
        // grid scales on the GPU — fill the pane freely
        crate::imageview::Visual::Anim(anim) => {
            let (cols, rows) = anim.grid(font, (inner.width, inner.height));
            let area = Rect::new(
                inner.x + (inner.width.saturating_sub(cols)) / 2,
                inner.y + (inner.height.saturating_sub(rows)) / 2,
                cols.min(inner.width),
                rows.min(inner.height),
            );
            anim.render(area, frame.buffer_mut());
        }
    }
}

/// Read-only hex viewer: structure tree on the left (for recognized
/// formats), offset + hex + ascii dump on the right. The selected tree
/// node's byte range is tinted in the dump.
fn draw_hex_view(frame: &mut Frame, app: &mut App, pane: Rect) {
    use crate::hex::HexFocus;
    let focused = app.focus == Focus::Terminal;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
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
    let body_rows =
        if has_tree && inner.height >= 10 { ((inner.height as usize) / 3).clamp(3, 12) } else { 0 };
    // pattern pane = rule + header + body
    let pattern_h = if body_rows > 0 { body_rows as u16 + 2 } else { 0 };
    let dump = Rect::new(inner.x, inner.y, inner.width, inner.height - pattern_h);
    // right column of each pane is its scrollbar
    let dump_body = Rect::new(dump.x, dump.y, dump.width.saturating_sub(1), dump.height);
    app.layout.hex_dump = dump_body;

    // 8 hex offset + 2 gap + N*3 hex (+1 mid gap) + 2 gap + N ascii
    let fits = |n: u16| 8 + 2 + n * 3 + 1 + 2 + n <= dump_body.width;
    let bpr = if fits(16) {
        16
    } else if fits(8) {
        8
    } else {
        4
    };
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
            let (ch, fg) =
                if (0x20..0x7f).contains(&b) { (b as char, label_fg) } else { ('·', zero_fg) };
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
    let table_area =
        Rect::new(inner.x, rule_y + 1, inner.width.saturating_sub(1), body_rows as u16 + 1);
    // click hit-testing targets the body rows below the header
    app.layout.hex_tree =
        Rect::new(table_area.x, table_area.y + 1, table_area.width, body_rows as u16);

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
        ((96, 48, 48), (150, 75, 75)),  // red
        ((52, 82, 50), (82, 130, 78)),  // green
        ((92, 84, 40), (145, 132, 62)), // yellow
        ((44, 62, 92), (70, 98, 145)),  // blue
        ((84, 50, 88), (132, 78, 138)), // magenta
        ((42, 80, 84), (66, 126, 132)), // cyan
        ((96, 64, 38), (150, 100, 60)), // orange
        ((60, 56, 92), (94, 88, 145)),  // purple
    ];
    let ((dr, dg, db), (br, bg, bb)) = COLORS[node.saturating_sub(1) % COLORS.len()];
    (Color::Rgb(dr, dg, db), Color::Rgb(br, bg, bb))
}

/// Vertical scrollbar on the right edge of `pane`, hidden when everything
/// fits.
fn draw_pane_scrollbar(
    frame: &mut Frame,
    pane: Rect,
    total: usize,
    viewport: usize,
    scroll: usize,
) {
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
            .thumb_style(
                Style::default().fg(adaptive(Color::Rgb(96, 101, 112), Color::Rgb(150, 155, 166))),
            )
            .track_style(
                Style::default().fg(adaptive(Color::Rgb(44, 47, 56), Color::Rgb(216, 219, 226))),
            ),
        pane,
        &mut state,
    );
}

/// Horizontal twin of draw_pane_scrollbar, on the bottom edge of `pane`.
fn draw_pane_hscrollbar(
    frame: &mut Frame,
    pane: Rect,
    total: usize,
    viewport: usize,
    scroll: usize,
) {
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
            .thumb_style(
                Style::default().fg(adaptive(Color::Rgb(96, 101, 112), Color::Rgb(150, 155, 166))),
            )
            .track_style(
                Style::default().fg(adaptive(Color::Rgb(44, 47, 56), Color::Rgb(216, 219, 226))),
            ),
        pane,
        &mut state,
    );
}

/// The app bar while the hex viewer is open: HEX chip, file, selected
/// Status bar while the image preview is open: name on the left,
/// pixel dimensions and file size on the right.
fn draw_image_status_bar(
    frame: &mut Frame,
    app: &App,
    view: &crate::imageview::ImageView,
    area: Rect,
) {
    let chip_bg = chip_accent(6, (136, 192, 208), (60, 120, 150));
    let mut left = vec![Span::raw(" ")];
    left.extend(slant_chip(" IMG ".into(), chip_bg, Some(chip_text())));
    left.push(Span::raw(format!(" {} [read-only]", view.file_name())));
    if let Some(msg) = &app.status_msg {
        left.push(Span::styled(format!("  {msg}"), Style::default().fg(Color::Yellow)));
    }
    let detail = if !view.ready() {
        "decoding…  ".to_string()
    } else if view.frame_count() > 1 {
        format!("{} frames  {}×{}  ", view.frame_count(), view.width, view.height)
    } else {
        format!("{}×{}  ", view.width, view.height)
    };
    let right = vec![Span::styled(
        format!("{detail}{}", crate::hex::human_size(view.data.len())),
        Style::default().fg(STATUS_DIM()),
    )];
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

/// section with its byte range, total size.
fn draw_hex_status_bar(frame: &mut Frame, app: &App, hex: &crate::hex::HexView, area: Rect) {
    let chip_bg = chip_accent(13, (180, 142, 173), (150, 90, 140));
    let mut left = vec![Span::raw(" ")];
    left.extend(slant_chip(" HEX ".into(), chip_bg, Some(chip_text())));
    left.push(Span::raw(format!(" {} [read-only]", hex.file_name())));
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

/// Editor variant of the bottom bar: mode │ file │ diagnostics … E/W · pos
/// · language · branch. The `:` command line also renders here.
fn draw_editor_status_bar(
    frame: &mut Frame,
    app: &App,
    editor: &crate::editor::Editor,
    area: Rect,
) {
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
    let mut left = vec![Span::raw(" ")];
    left.extend(slant_chip(format!(" {} ", editor.mode.label()), chip_bg, Some(chip_fg)));
    // branch chip right after the mode chip, same as the app bar, with the
    // push/pull counts trailing it
    if let Some(label) = branch_chip_label(&app.git) {
        left.extend(slant_chip(label, MENU_TINT(), None));
    }
    left.extend(push_pull_spans(&app.git));
    left.push(Span::raw(format!(" {}{}", editor.file_name(), dirty)));
    if let Some(msg) = &app.status_msg {
        left.push(Span::styled(format!("  {msg}"), Style::default().fg(Color::Yellow)));
    } else if let Some(msg) = &editor.status {
        left.push(Span::styled(format!("  {msg}"), Style::default().fg(Color::Yellow)));
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
        right.push(Span::styled(format!("{spin}{prog}   "), Style::default().fg(STATUS_DIM())));
    }
    if errors > 0 {
        right.push(Span::styled(format!("E {errors} "), Style::default().fg(severity_color(1))));
    }
    if warnings > 0 {
        right.push(Span::styled(format!("W {warnings} "), Style::default().fg(severity_color(2))));
    }
    let selected = editor.selected_chars();
    if selected > 0 {
        right.push(Span::styled(format!("{selected} sel "), Style::default().fg(STATUS_DIM())));
    }
    let mut meta = format!("{}:{}", cursor_line + 1, cursor_col + 1);
    if let Some(indent) = &editor.indent_label {
        meta.push_str(&format!(" {indent}"));
    }
    meta.push_str(if editor.crlf { " crlf" } else { " lf" });
    // read_to_string guarantees the buffer is UTF-8
    meta.push_str(" utf-8");
    meta.push_str(&format!(" {} ", crate::editor::highlight::language_name(&editor.path)));
    right.push(Span::styled(meta, Style::default().fg(STATUS_DIM())));
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

/// Branch chip text: just the branch name (the push/pull counts ride in
/// their own segment right after — see [`push_pull_spans`]).
fn branch_chip_label(git: &crate::git::GitState) -> Option<String> {
    Some(format!(" {} ", git.branch.as_ref()?))
}

/// Sync indicator: a circular arrow followed by the incoming/outgoing
/// commit counts vs the branch's upstream — ` ↺ 1↓ 2↑ ` (behind then
/// ahead, count before arrow). Just the arrow when in sync; empty when
/// there's no upstream. The ASCII fallback has no glyph, so it only shows
/// once there's a count to report.
fn push_pull_spans(git: &crate::git::GitState) -> Vec<Span<'static>> {
    let Some((ahead, behind)) = git.upstream else { return Vec::new() };
    let fancy = crate::color::fancy_glyphs();
    let diverged = ahead + behind > 0;
    if !fancy && !diverged {
        return Vec::new();
    }
    let style = Style::default().add_modifier(Modifier::BOLD);
    let mut spans = vec![Span::raw(" ")];
    if fancy {
        spans.push(Span::styled("↺", style));
    }
    if diverged {
        let (down, up) = if fancy { ("↓", "↑") } else { ("v", "^") };
        spans.push(Span::styled(format!(" {behind}{down} {ahead}{up}"), style));
    }
    spans.push(Span::raw(" "));
    spans
}

fn draw_status_bar(frame: &mut Frame, app: &App, area: Rect) {
    // with the editor or hex viewer active, the app bar IS its statusline
    if app.screen == Screen::Workspace && app.shell == Shell::Code {
        if let Some(view) = &app.image {
            draw_image_status_bar(frame, app, view, area);
            return;
        }
        if let Some(hex) = &app.hex {
            draw_hex_status_bar(frame, app, hex, area);
            return;
        }
        if let Some(editor) = &app.editor {
            draw_editor_status_bar(frame, app, editor, area);
            return;
        }
    }
    let mut spans: Vec<Span> = vec![Span::raw(" ")];
    if app.leader_pending {
        spans.extend(slant_chip(
            " LEADER ".into(),
            chip_accent(3, (250, 210, 60), (176, 130, 10)),
            Some(chip_text()),
        ));
    } else {
        spans.extend(slant_chip(
            format!(" {} ", app.shell.label()),
            chip_accent(6, (134, 220, 214), (1, 132, 188)),
            Some(chip_text()),
        ));
    }
    // agents shell: the open session's connection/turn status as a chip
    if app.shell == Shell::Agents
        && let Some((conn, id)) = &app.agent_view.open
        && let Some(client) = app.acp.conn(*conn)
    {
        let (icon, label, color) = agent_status(&client.state(), client.turn_active(id));
        let text = if crate::color::fancy_glyphs() {
            format!(" {icon} {label} ")
        } else {
            format!(" {label} ")
        };
        spans.extend(slant_chip(text, color, Some(chip_text())));
    }
    if let Some(label) = branch_chip_label(&app.git) {
        // quiet menu-tint box — same treatment as the menu-bar chips
        // (clickable someday); back to back, the caps carve the gap
        spans.extend(slant_chip(label, MENU_TINT(), None));
    }
    // commits to push/pull vs upstream, right after the branch
    spans.extend(push_pull_spans(&app.git));
    // message before the workdir: the path is the least important part and
    // the only span that may safely fall off the right edge
    if let Some(msg) = &app.status_msg {
        spans.push(Span::styled(format!(" · {msg}"), Style::default().fg(Color::Yellow)));
    }
    // after the chips: the selected file in the git shell / the open
    // session's name in the agents shell (styled like the editor bar's
    // filename), the workspace path everywhere else
    match app.shell {
        Shell::Git => {
            if let Some(entry) = app.git.selected_entry() {
                spans.push(Span::raw(format!(" {}", entry.path)));
            }
        }
        Shell::Agents if app.agent_view.open.is_some() => {
            if let Some((conn, id)) = &app.agent_view.open {
                spans.push(Span::raw(format!(" {}", app.acp_session_label(*conn, id))));
            }
        }
        _ => spans.push(Span::styled(
            format!(" · {}", app.workdir.display()),
            Style::default().fg(Color::DarkGray),
        )),
    }
    // git shell: the visible diff's line counts, on the same tints the
    // diff pane highlights its lines with
    if app.shell == Shell::Git
        && let (added, removed) = (app.git_view.pane.added, app.git_view.pane.removed)
        && added + removed > 0
    {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            format!(" +{added} "),
            Style::default().fg(ADD_ACCENT()).bg(ADD_BG()),
        ));
        spans.push(Span::styled(
            format!(" -{removed} "),
            Style::default().fg(REMOVE_ACCENT()).bg(REMOVE_BG()),
        ));
    }
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
            Style::default().bg(at(i)).fg(Color::Rgb(8, 8, 10)).add_modifier(Modifier::BOLD),
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
    if crate::color::fancy_glyphs() { block.border_set(HAIRLINE) } else { block }
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
    frame.render_widget(Block::default().style(Style::default().bg(DIALOG_BG())), rect);
}

// Claude Code-style diff palette: tinted full-width rows on dark background.
// One accent per side, used for BOTH the line number and the +/- marker, so
// the greens/reds always match (ANSI theme colors would drift from the RGB
// backgrounds).
/// Diff accents from the terminal's ANSI green/red when available, and
/// backgrounds blended from that accent and the real terminal background
/// (~18% accent) — theme-true tints in both light and dark schemes.
/// Modified-line gutter marker: the theme's yellow.
#[allow(non_snake_case)]
fn MOD_ACCENT() -> Color {
    match crate::color::ansi16(3) {
        Some((r, g, b)) => Color::Rgb(r, g, b),
        None => adaptive(Color::Rgb(229, 192, 123), Color::Rgb(176, 130, 10)),
    }
}

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
fn render_diff_line(line: &DiffLine, width: usize, hovered: bool, icons: bool) -> Line<'static> {
    // gutter: old number, new number, colored bar, marker — 13 columns
    let pad = |text: &str| {
        let visible = text.chars().count();
        let fill = width.saturating_sub(visible + 13);
        " ".repeat(fill)
    };
    let no = |n: Option<u32>| match n {
        Some(n) => format!("{n:>4} "),
        None => "     ".to_string(),
    };
    match line.kind {
        DiffLineKind::FileHeader => Line::from(vec![
            Span::styled("● ", Style::default().fg(Color::Green)),
            Span::styled("Update(", Style::default().add_modifier(Modifier::BOLD)),
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
        DiffLineKind::HunkSep => {
            // folded gap: the number gutter stays blank and the divider
            // hangs off the gutter rule with a ├ junction, so the vertical
            // line flows through instead of being cut
            if let Ok(hidden) = line.text.parse::<u32>() {
                let unit = if hidden == 1 { "unchanged line" } else { "unchanged lines" };
                // click to expand — nf-md-arrow_expand_vertical when the
                // devicons are on, a plain chevron otherwise (NBSP after
                // the glyph keeps it one cell wide)
                let marker = if icons { "\u{f084f}\u{a0}" } else { "⌄ " };
                let label = format!(" {marker}{hidden} {unit} ");
                let label_style = if hovered {
                    Style::default()
                        .fg(chip_accent(6, (134, 220, 214), (1, 132, 188)))
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(STATUS_DIM())
                };
                let rule = Style::default().fg(chrome_grey());
                let used = 10 + 1 + 2 + label.chars().count();
                return Line::from(vec![
                    Span::raw(" ".repeat(10)), // the two number columns
                    Span::styled("├", rule),
                    Span::styled("──", rule),
                    Span::styled(label, label_style),
                    Span::styled("─".repeat(width.saturating_sub(used)), rule),
                ]);
            }
            Line::from(Span::styled("           ⋯", Style::default().fg(Color::DarkGray)))
        }
        DiffLineKind::Add => {
            let bg = Style::default().bg(ADD_BG());
            let mut spans = vec![
                Span::styled(no(line.old_no), bg.fg(STATUS_DIM())),
                Span::styled(no(line.new_no), bg.fg(ADD_ACCENT())),
                Span::styled("┃", bg.fg(ADD_ACCENT())),
                Span::styled("+ ", bg.fg(ADD_ACCENT()).add_modifier(Modifier::BOLD)),
            ];
            spans.extend(diff_code_spans(line, Some(ADD_BG())));
            spans.push(Span::styled(pad(&line.text), bg));
            Line::from(spans)
        }
        DiffLineKind::Remove => {
            let bg = Style::default().bg(REMOVE_BG());
            let mut spans = vec![
                Span::styled(no(line.old_no), bg.fg(REMOVE_ACCENT())),
                Span::styled(no(line.new_no), bg.fg(STATUS_DIM())),
                Span::styled("┃", bg.fg(REMOVE_ACCENT())),
                Span::styled("- ", bg.fg(REMOVE_ACCENT()).add_modifier(Modifier::BOLD)),
            ];
            spans.extend(diff_code_spans(line, Some(REMOVE_BG())));
            spans.push(Span::styled(pad(&line.text), bg));
            Line::from(spans)
        }
        DiffLineKind::Context => {
            let dim = Style::default().fg(STATUS_DIM());
            let mut spans = vec![
                Span::styled(no(line.old_no), dim),
                Span::styled(no(line.new_no), dim),
                // unchanged lines: a plain rule in the editor gutter's grey,
                // so the accent bars read as the changes
                Span::styled("│", Style::default().fg(chrome_grey())),
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
        .map(|line| render_diff_line(line, inner_width, false, false))
        .collect();

    let title = format!(
        " {} — {}/{} (j/k scroll · q close) ",
        view.title,
        (view.scroll + 1).min(view.lines.len()),
        view.lines.len()
    );
    let (diff_total, diff_scroll) = (view.lines.len(), view.scroll);
    let paragraph =
        Paragraph::new(visible).style(Style::default().bg(DIALOG_BG())).block(dialog_block());
    frame.render_widget(paragraph, area);
    draw_dialog_frame(frame, area, &title, app.welcome.phase);
    // vertical scrollbar over the right border of the dialog
    let bar =
        Rect::new(area.right().saturating_sub(1), area.y + 1, 1, area.height.saturating_sub(2));
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
        ("git: j/k move · s/u stage/unstage · a stage all · c commit · t list/tree", ""),
        ("git: p pull · P push · f fetch · Enter diff", ""),
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
        Paragraph::new(text).style(Style::default().bg(DIALOG_BG())).block(dialog_block()),
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
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn render(app: &mut App) -> ratatui::buffer::Buffer {
        unsafe { std::env::set_var("VIBIN_FANCY", "1") };
        let backend = TestBackend::new(100, 32);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        terminal.backend().buffer().clone()
    }

    fn bg_count(buf: &ratatui::buffer::Buffer, color: Color) -> usize {
        buf.content().iter().filter(|cell| cell.style().bg == Some(color)).count()
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
        let app = App::new(dir.path().to_path_buf());
        (dir, app)
    }

    /// A canned ACP agent (see acp::tests) whose live session and title are
    /// tagged, so two agents can run with distinct session ids. On a prompt
    /// it streams a chunk, opens a tool call, asks permission, reflects the
    /// choice, ends the turn — all routed by sessionId.
    fn fake_acp_agent_tagged(dir: &std::path::Path, tag: &str) -> Vec<String> {
        let script = dir.join(format!("fake-acp-{tag}.sh"));
        let body = format!(
            r#"#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  sid=$(printf '%s' "$line" | sed -n 's/.*"sessionId":"\([^"]*\)".*/\1/p')
  case "$line" in
    *'"method":"initialize"'*)
      printf '{{"jsonrpc":"2.0","id":%s,"result":{{"protocolVersion":1,"agentCapabilities":{{"sessionCapabilities":{{"list":true}}}},"authMethods":[]}}}}\n' "$id" ;;
    *'"method":"session/new"'*)
      printf '{{"jsonrpc":"2.0","id":%s,"result":{{"sessionId":"{tag}","modes":{{"currentModeId":"ask","availableModes":[{{"id":"ask","name":"Ask"}},{{"id":"code","name":"Code"}}]}}}}}}\n' "$id" ;;
    *'"method":"session/set_mode"'*)
      mid=$(printf '%s' "$line" | sed -n 's/.*"modeId":"\([^"]*\)".*/\1/p')
      printf '{{"jsonrpc":"2.0","method":"session/update","params":{{"sessionId":"%s","update":{{"sessionUpdate":"current_mode_update","modeId":"%s"}}}}}}}}\n' "$sid" "$mid"
      printf '{{"jsonrpc":"2.0","id":%s,"result":null}}\n' "$id" ;;
    *'"method":"session/list"'*)
      printf '{{"jsonrpc":"2.0","id":%s,"result":{{"sessions":[{{"sessionId":"{tag}","title":"work {tag}"}}]}}}}\n' "$id" ;;
    *'"method":"session/prompt"'*)
      printf '{{"jsonrpc":"2.0","method":"session/update","params":{{"sessionId":"%s","update":{{"sessionUpdate":"agent_message_chunk","content":{{"type":"text","text":"hello world"}}}}}}}}\n' "$sid"
      printf '{{"jsonrpc":"2.0","method":"session/update","params":{{"sessionId":"%s","update":{{"sessionUpdate":"tool_call","toolCallId":"c1","title":"Read file","kind":"read","status":"pending"}}}}}}}}\n' "$sid"
      printf '{{"jsonrpc":"2.0","id":900,"method":"session/request_permission","params":{{"sessionId":"%s","toolCall":{{"toolCallId":"c1","title":"Read file"}},"options":[{{"optionId":"allow-once","name":"Allow once","kind":"allow_once"}},{{"optionId":"reject-once","name":"Reject","kind":"reject_once"}}]}}}}\n' "$sid"
      IFS= read -r perm
      opt=$(printf '%s' "$perm" | sed -n 's/.*"optionId":"\([^"]*\)".*/\1/p')
      printf '{{"jsonrpc":"2.0","id":%s,"result":{{"stopReason":"end_turn"}}}}\n' "$id" ;;
  esac
done
"#,
        );
        std::fs::write(&script, body).unwrap();
        vec!["/bin/sh".to_string(), script.to_string_lossy().into_owned()]
    }

    fn fake_acp_agent(dir: &std::path::Path) -> Vec<String> {
        fake_acp_agent_tagged(dir, "s1")
    }

    /// A fake ACP agent whose prompt reply is a markdown snippet (heading +
    /// bullets), for exercising the transcript's markdown rendering. `\\n`
    /// in the format string is a JSON string escape; a lone `\n` frames a
    /// message.
    fn fake_acp_agent_markdown(dir: &std::path::Path) -> Vec<String> {
        let script = dir.join("fake-acp-md.sh");
        std::fs::write(
            &script,
            r###"#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  sid=$(printf '%s' "$line" | sed -n 's/.*"sessionId":"\([^"]*\)".*/\1/p')
  case "$line" in
    *'"method":"initialize"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":1,"agentCapabilities":{"sessionCapabilities":{"list":true}},"authMethods":[]}}\n' "$id" ;;
    *'"method":"session/new"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"sessionId":"s1"}}\n' "$id" ;;
    *'"method":"session/list"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"sessions":[{"sessionId":"s1","title":"work"}]}}\n' "$id" ;;
    *'"method":"session/prompt"'*)
      printf '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"%s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"## Summary\\n- parsed the file\\n- fixed the bug\\n\\nsee [docs](https://example.com)"}}}}\n' "$sid"
      printf '{"jsonrpc":"2.0","id":%s,"result":{"stopReason":"end_turn"}}\n' "$id" ;;
  esac
done
"###,
        )
        .unwrap();
        vec!["/bin/sh".to_string(), script.to_string_lossy().into_owned()]
    }

    /// Pump ticks until `f` holds — needed for state that only advances in
    /// `tick` (a started agent's session auto-opening).
    fn pump_until(app: &mut App, mut f: impl FnMut(&App) -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            app.tick();
            if f(app) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("condition not met within timeout");
    }

    fn wait_for(app: &App, mut f: impl FnMut(&App) -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            if f(app) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("condition not met within timeout");
    }

    fn buf_text(buf: &ratatui::buffer::Buffer) -> String {
        buf.content().iter().map(|c| c.symbol()).collect()
    }

    /// The open session id once a started agent's session auto-opens.
    fn open_id(app: &App) -> String {
        app.agent_view.open.as_ref().map(|(_, id)| id.clone()).expect("a session is open")
    }

    #[test]
    fn agents_sidebar_tree_lists_sessions_and_status_bar_shows_state() {
        let (dir, mut app) = test_app();
        app.shell = Shell::Agents;
        app.focus = Focus::Sidebar;
        // no agent: the sidebar prompts to start one
        assert!(buf_text(&render(&mut app)).contains("no agent running"));
        // start one; its live session auto-opens and, once session/list
        // round-trips, the tree shows its title
        app.start_acp(&fake_acp_agent(dir.path()));
        pump_until(&mut app, |a| a.acp.conn(0).is_some_and(|c| c.title("s1").is_some()));
        app.focus = Focus::Sidebar;
        let buf = render(&mut app);
        let text = buf_text(&buf);
        assert!(text.contains("work s1"), "session title in the tree: {text:?}");
        // "ready" sits on the bottom row (the status bar), not the sidebar
        let last_row: String =
            (0..buf.area.width).map(|x| buf[(x, buf.area.bottom() - 1)].symbol()).collect();
        assert!(last_row.contains("ready"), "status is in the status bar: {last_row:?}");
    }

    #[test]
    fn two_agents_group_in_the_tree_and_sessions_stay_separate() {
        let (dir, mut app) = test_app();
        app.start_acp(&fake_acp_agent_tagged(dir.path(), "s1"));
        pump_until(&mut app, |a| a.agent_view.open.is_some());
        app.start_acp(&fake_acp_agent_tagged(dir.path(), "s2"));
        pump_until(&mut app, |a| a.agent_view.open.as_ref().map(|(c, _)| *c) == Some(1));
        assert_eq!(app.acp.len(), 2, "two connections");

        // prompt only the second agent's open session
        let id = open_id(&app);
        app.acp.conn(1).unwrap().prompt(&id, "second only");
        pump_until(&mut app, |a| {
            a.acp
                .conn(1)
                .unwrap()
                .entries("s2")
                .iter()
                .any(|e| matches!(e, crate::acp::Entry::User(t) if t == "second only"))
        });
        // the first agent's session is untouched
        assert!(app.acp.conn(0).unwrap().entries("s1").is_empty(), "agent 1 untouched");

        // the sidebar tree shows both agents' sessions (wait out session/list)
        pump_until(&mut app, |a| {
            a.acp.conn(0).unwrap().title("s1").is_some()
                && a.acp.conn(1).unwrap().title("s2").is_some()
        });
        app.focus = Focus::Sidebar;
        let text = buf_text(&render(&mut app));
        assert!(
            text.contains("work s1") && text.contains("work s2"),
            "both sessions listed: {text:?}"
        );
    }

    #[test]
    fn acp_conversation_renders_transcript_tool_call_and_permission() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let (dir, mut app) = test_app();
        assert!(app.start_acp(&fake_acp_agent(dir.path())));
        assert_eq!(app.shell, Shell::Agents);
        pump_until(&mut app, |a| a.agent_view.open.is_some());
        let id = open_id(&app);

        // a prompt streams agent text and a tool-call row into the transcript
        app.acp.conn(0).unwrap().prompt(&id, "refactor the parser");
        pump_until(&mut app, |a| {
            a.acp
                .conn(0)
                .unwrap()
                .entries(&id)
                .iter()
                .any(|e| matches!(e, crate::acp::Entry::Agent(t) if t.contains("hello")))
        });
        let text = buf_text(&render(&mut app));
        assert!(text.contains("refactor the parser"), "user prompt shown");
        assert!(text.contains("hello world"), "agent text streamed in");
        assert!(text.contains("Read file"), "tool call row rendered");

        // the agent blocks on a permission request → the prompt renders
        pump_until(&mut app, |a| a.acp.conn(0).unwrap().pending_permission(&id).is_some());
        let text = buf_text(&render(&mut app));
        assert!(text.contains("permission"), "permission block shown: {text:?}");
        assert!(text.contains("Allow once") && text.contains("Reject"), "options shown");

        // pressing '1' selects the first option and the turn completes
        app.handle_key(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE));
        pump_until(&mut app, |a| {
            let c = a.acp.conn(0).unwrap();
            !c.turn_active(&id) && c.pending_permission(&id).is_none()
        });
    }

    #[test]
    fn mode_dropdown_shows_modes_and_tab_switches_the_active_one() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let (dir, mut app) = test_app();
        assert!(app.start_acp(&fake_acp_agent(dir.path())));
        pump_until(&mut app, |a| a.agent_view.open.is_some());
        let id = open_id(&app);
        wait_for(&app, |a| a.acp.conn(0).is_some_and(|c| !c.modes(&id).is_empty()));

        // the composer meta line names the active mode with a dropdown caret
        app.focus = Focus::Terminal;
        assert!(buf_text(&render(&mut app)).contains("Ask ▾"), "active mode + caret shown");

        // Shift+Tab opens the dropdown; both modes appear in the floating list
        app.handle_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT));
        assert_eq!(app.agent_view.mode_menu, Some(0), "menu opens on the active mode");
        let text = buf_text(&render(&mut app));
        assert!(text.contains("Ask") && text.contains("Code"), "both modes listed: {text:?}");

        // move to "Code" and apply it; the agent confirms the switch
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.agent_view.mode_menu, None, "menu closes on select");
        wait_for(&app, |a| a.acp.conn(0).unwrap().current_mode(&id).as_deref() == Some("code"));
        assert!(buf_text(&render(&mut app)).contains("Code ▾"), "meta line follows the switch");
    }

    #[test]
    fn mode_chip_hover_and_click_drive_the_dropdown() {
        use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
        let (dir, mut app) = test_app();
        assert!(app.start_acp(&fake_acp_agent(dir.path())));
        pump_until(&mut app, |a| a.agent_view.open.is_some());
        let id = open_id(&app);
        wait_for(&app, |a| a.acp.conn(0).is_some_and(|c| !c.modes(&id).is_empty()));
        app.focus = Focus::Terminal;
        render(&mut app); // records the chip's hit rect
        let moved = |c, r| MouseEvent {
            kind: MouseEventKind::Moved,
            column: c,
            row: r,
            modifiers: KeyModifiers::NONE,
        };
        let click = |c, r| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: c,
            row: r,
            modifiers: KeyModifiers::NONE,
        };

        // hovering the chip highlights it, clicking opens the dropdown
        let chip = app.layout.agent_mode_chip;
        assert!(chip.area() > 0, "chip rect recorded");
        assert!(app.handle_mouse(moved(chip.x, chip.y)));
        assert!(app.agent_view.mode_hover, "chip hovered");
        assert!(app.handle_mouse(click(chip.x, chip.y)));
        assert_eq!(app.agent_view.mode_menu, Some(0), "dropdown opens on the active mode");
        render(&mut app); // records the dropdown box rect

        // hovering the second row moves the highlight; clicking it picks "code"
        let menu = app.layout.agent_mode_menu;
        let row1 = menu.y + 2; // border row + second item
        assert!(app.handle_mouse(moved(menu.x + 2, row1)));
        assert_eq!(app.agent_view.mode_menu, Some(1), "row hover moves the highlight");
        assert!(app.handle_mouse(click(menu.x + 2, row1)));
        assert_eq!(app.agent_view.mode_menu, None, "menu closes on the pick");
        wait_for(&app, |a| a.acp.conn(0).unwrap().current_mode(&id).as_deref() == Some("code"));
    }

    #[test]
    fn agent_messages_render_as_markdown() {
        let (dir, mut app) = test_app();
        assert!(app.start_acp(&fake_acp_agent_markdown(dir.path())));
        pump_until(&mut app, |a| a.agent_view.open.is_some());
        let id = open_id(&app);
        app.acp.conn(0).unwrap().prompt(&id, "summarize");
        pump_until(&mut app, |a| {
            a.acp
                .conn(0)
                .unwrap()
                .entries(&id)
                .iter()
                .any(|e| matches!(e, crate::acp::Entry::Agent(t) if t.contains("Summary")))
        });
        let text = buf_text(&render(&mut app));
        // markdown is styled, not shown raw: bullets become glyphs and the
        // heading marker is stripped
        assert!(text.contains('•'), "bullets rendered as glyphs: {text:?}");
        assert!(text.contains("Summary") && text.contains("parsed the file"), "content shown");
        assert!(!text.contains("## Summary"), "raw heading marker consumed: {text:?}");
        assert!(!text.contains("- parsed"), "raw bullet dash consumed: {text:?}");
    }

    #[test]
    fn transcript_markdown_links_are_clickable() {
        use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
        let (dir, mut app) = test_app();
        assert!(app.start_acp(&fake_acp_agent_markdown(dir.path())));
        pump_until(&mut app, |a| a.agent_view.open.is_some());
        let id = open_id(&app);
        app.acp.conn(0).unwrap().prompt(&id, "summarize");
        pump_until(&mut app, |a| {
            a.acp
                .conn(0)
                .unwrap()
                .entries(&id)
                .iter()
                .any(|e| matches!(e, crate::acp::Entry::Agent(t) if t.contains("docs")))
        });
        let buf = render(&mut app);
        // the link label became an OSC 8 cell (native terminal click)
        assert!(
            buf.content().iter().any(|c| c.symbol().contains("\x1b]8;;https://example.com")),
            "transcript link is an OSC 8 cell"
        );
        // and a hitbox was recorded; clicking it opens the url
        let (hit, url) = app
            .link_hits
            .iter()
            .find(|(_, u)| u.contains("example.com"))
            .cloned()
            .expect("link hitbox recorded");
        // hovering the link previews its url in the corner chip
        let moved = MouseEvent {
            kind: MouseEventKind::Moved,
            column: hit.x,
            row: hit.y,
            modifiers: KeyModifiers::NONE,
        };
        assert!(app.handle_mouse(moved));
        assert_eq!(app.hovered_link.as_deref(), Some(url.as_str()), "url previewed on hover");
        assert!(buf_text(&render(&mut app)).contains(&url), "preview chip shows the url");
        // clicking it opens the url
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: hit.x,
            row: hit.y,
            modifiers: KeyModifiers::NONE,
        };
        assert!(app.handle_mouse(click));
        assert_eq!(app.status_msg.as_deref(), Some(format!("opened {url}").as_str()));
    }

    #[test]
    fn completion_popup_renders_labels_kinds_and_docs() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let (dir, mut app) = test_app();
        let path = dir.path().join("code.py");
        std::fs::write(&path, "x\n").unwrap();
        app.open_file(&path);
        app.shell = Shell::Code;
        app.focus = Focus::Terminal;
        // insert mode → the cursor draws, so the popup has an anchor
        app.handle_key(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        render(&mut app);
        let item = |label: &str, detail: Option<&str>| crate::lsp::CompletionItem {
            label: label.into(),
            kind: "Method",
            detail: detail.map(str::to_string),
            documentation: None,
            insert_text: label.into(),
            sort_text: label.into(),
        };
        app.completion = Some(crate::app::Completion {
            items: vec![
                item("TemplateResponse", Some("def TemplateResponse(name: str)")),
                item("get_template", None),
            ],
            filtered: vec![0, 1],
            selected: 0,
            anchor: 0,
        });
        let text = buf_text(&render(&mut app));
        assert!(text.contains("TemplateResponse"), "label shown: {text:?}");
        assert!(text.contains("get_template"), "second candidate shown");
        assert!(text.contains("Method"), "kind column shown");
        assert!(text.contains("def TemplateResponse"), "doc panel shows the signature");
    }

    #[test]
    fn bell_rings_on_permission_and_completion() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let (dir, mut app) = test_app();
        app.start_acp(&fake_acp_agent(dir.path()));
        pump_until(&mut app, |a| a.agent_view.open.is_some());
        let id = open_id(&app);
        app.take_bell(); // clear any startup edge

        app.acp.conn(0).unwrap().prompt(&id, "go");
        // the agent blocks on a permission → bell
        pump_until(&mut app, |a| a.acp.conn(0).unwrap().pending_permission(&id).is_some());
        assert!(app.take_bell(), "permission request rings the bell");

        // answering it lets the turn finish → working→idle edge → bell
        app.handle_key(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE));
        pump_until(&mut app, |a| !a.acp.conn(0).unwrap().turn_active(&id));
        assert!(app.take_bell(), "finishing the turn rings the bell");
    }

    #[test]
    fn acp_composer_edits_with_cursor_and_selection() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let (dir, mut app) = test_app();
        app.start_acp(&fake_acp_agent(dir.path()));
        pump_until(&mut app, |a| a.agent_view.open.is_some());
        let id = open_id(&app);
        let plain = |c| KeyEvent::new(c, KeyModifiers::NONE);
        let shift = |c| KeyEvent::new(c, KeyModifiers::SHIFT);
        let inp = |a: &App, id: &str| a.agent_view.ui.get(id).unwrap().input.clone();

        for c in "hello world".chars() {
            app.handle_key(plain(KeyCode::Char(c)));
        }
        // Home, then insert mid-line — the cursor isn't stuck at the end
        app.handle_key(plain(KeyCode::Home));
        assert_eq!(inp(&app, &id).cursor(), 0);
        app.handle_key(plain(KeyCode::Char('>')));
        assert_eq!(inp(&app, &id).text(), ">hello world");

        // cursor is right after the '>'; shift+End selects the rest
        app.handle_key(shift(KeyCode::End));
        assert_eq!(inp(&app, &id).selected_text().as_deref(), Some("hello world"));
        app.handle_key(plain(KeyCode::Char('x')));
        assert_eq!(inp(&app, &id).text(), ">x");
    }

    #[test]
    fn acp_conversation_navigates_like_git_arrows_and_esc() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let (dir, mut app) = test_app();
        app.start_acp(&fake_acp_agent(dir.path()));
        pump_until(&mut app, |a| a.agent_view.open.is_some());
        let id = open_id(&app);
        assert_eq!(app.focus, Focus::Terminal);

        // Up arrow scrolls the transcript (lines above the tail)
        let key = |c| KeyEvent::new(c, KeyModifiers::NONE);
        app.handle_key(key(KeyCode::Up));
        assert_eq!(app.agent_view.ui.get(&id).unwrap().scroll, 1, "Up scrolls back");
        app.handle_key(key(KeyCode::Down));
        assert_eq!(app.agent_view.ui.get(&id).unwrap().scroll, 0, "Down scrolls forward");

        // Esc steps back to the sidebar tree, like the git diff pane
        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.focus, Focus::Sidebar);
    }

    #[test]
    fn acp_composer_types_and_submits() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let (dir, mut app) = test_app();
        app.start_acp(&fake_acp_agent(dir.path()));
        pump_until(&mut app, |a| a.agent_view.open.is_some());
        let id = open_id(&app);
        // typing lands in the composer, visible in the frame
        for c in "hi".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        assert_eq!(app.agent_view.ui.get(&id).unwrap().input.text(), "hi");
        assert!(buf_text(&render(&mut app)).contains("hi"));
        // Enter submits and clears the composer
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.agent_view.ui.get(&id).unwrap().input.is_empty(), "composer cleared");
        pump_until(&mut app, |a| {
            a.acp
                .conn(0)
                .unwrap()
                .entries(&id)
                .iter()
                .any(|e| matches!(e, crate::acp::Entry::User(t) if t == "hi"))
        });
    }

    #[test]
    fn agent_auth_prompt_shows_and_number_key_signs_in() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let (dir, mut app) = test_app();
        // an agent that needs auth, then opens a session once signed in
        let script = dir.path().join("auth.sh");
        std::fs::write(
            &script,
            r#"#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":1,"authMethods":[{"id":"login","name":"Sign in with Google"}]}}\n' "$id" ;;
    *'"method":"authenticate"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id" ;;
    *'"method":"session/new"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"sessionId":"s1"}}\n' "$id" ;;
  esac
done
"#,
        )
        .unwrap();
        app.start_acp(&["/bin/sh".into(), script.to_string_lossy().into_owned()]);
        // the auth prompt appears in the main pane
        pump_until(&mut app, |a| a.acp_auth_target().is_some());
        let text = buf_text(&render(&mut app));
        assert!(text.contains("needs you to sign in"), "auth prompt shown: {text:?}");
        assert!(text.contains("Sign in with Google"), "method listed");

        // pressing 1 signs in and a session opens
        app.handle_key(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE));
        pump_until(&mut app, |a| a.agent_view.open.is_some());
        assert!(app.acp_auth_target().is_none(), "no longer awaiting auth");
    }

    #[test]
    fn agent_write_reflects_into_the_open_editor() {
        let (dir, mut app) = test_app();
        // an agent that, on prompt, writes a file through fs/write_text_file
        let script = dir.path().join("writer.sh");
        let target = dir.path().join("code.rs");
        std::fs::write(&target, "old\n").unwrap();
        std::fs::write(
            &script,
            format!(
                r#"#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  sid=$(printf '%s' "$line" | sed -n 's/.*"sessionId":"\([^"]*\)".*/\1/p')
  case "$line" in
    *'"method":"initialize"'*) printf '{{"jsonrpc":"2.0","id":%s,"result":{{"protocolVersion":1}}}}\n' "$id" ;;
    *'"method":"session/new"'*) printf '{{"jsonrpc":"2.0","id":%s,"result":{{"sessionId":"s1"}}}}\n' "$id" ;;
    *'"method":"session/prompt"'*)
      printf '{{"jsonrpc":"2.0","id":50,"method":"fs/write_text_file","params":{{"sessionId":"%s","path":"{}","content":"new from agent"}}}}\n' "$sid"
      IFS= read -r _ack
      printf '{{"jsonrpc":"2.0","id":%s,"result":{{"stopReason":"end_turn"}}}}\n' "$id" ;;
  esac
done
"#,
                target.display()
            ),
        )
        .unwrap();
        app.start_acp(&["/bin/sh".into(), script.to_string_lossy().into_owned()]);
        pump_until(&mut app, |a| a.agent_view.open.is_some());
        // open the target in the editor (clean buffer), then prompt the agent
        app.open_file(&target);
        assert_eq!(app.editor.as_ref().unwrap().text.to_string(), "old\n");
        let id = open_id(&app);
        app.acp.conn(0).unwrap().prompt(&id, "write the file");
        // the write lands on disk and reloads the clean open buffer
        pump_until(&mut app, |a| {
            a.editor.as_ref().is_some_and(|e| e.text.to_string().contains("new from agent"))
        });
        assert!(std::fs::read_to_string(&target).unwrap().contains("new from agent"));
    }

    #[test]
    fn dialogs_have_gray_base_and_plain_border() {
        let (_dir, mut app) = test_app();
        // agents shell: the code shell's home card shares the dialog chrome
        app.shell = Shell::Agents;
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
        assert!(cells.contains(&"┃"), "vertical thumb rendered");
        assert!(cells.contains(&"━"), "horizontal thumb rendered");
        // a short file shows neither
        let small = dir.path().join("small.txt");
        std::fs::write(&small, "hi\n").unwrap();
        app.open_file(&small);
        let buf = render(&mut app);
        let cells: Vec<&str> = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(!cells.contains(&"┃"), "no vertical thumb when it fits");
        assert!(!cells.contains(&"━"), "no horizontal thumb when it fits");
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
        assert!(cells.contains(&"┃"), "diff scrollbar thumb rendered");
    }

    #[test]
    fn open_paints_a_skeleton_then_resolves_colors() {
        let (dir, mut app) = test_app();
        let path = dir.path().join("skel.rs");
        std::fs::write(&path, "fn main() { let answer = 42; }\n").unwrap();
        app.open_file(&path);
        // the highlighter builds on a background thread and is only
        // received in tick() — right after open the frame is always the
        // ghost skeleton: real text in dim grey, no syntax colors
        assert!(app.editor.as_ref().unwrap().highlight_pending(), "parse in flight");
        let buf = render(&mut app);
        let ghosted: String = buf
            .content()
            .iter()
            .filter(|c| c.style().fg == Some(STATUS_DIM()))
            .map(|c| c.symbol())
            .collect();
        assert!(ghosted.contains("fn main"), "skeleton shows dim text: {ghosted:?}");
        // "keyword" is slot 10 of HIGHLIGHT_NAMES — the color `fn` gets
        let keyword = crate::editor::highlight::style_for(10).fg;
        assert!(keyword.is_some() && keyword != Some(STATUS_DIM()));
        assert!(
            !buf.content().iter().any(|c| c.style().fg == keyword),
            "no syntax colors while pending"
        );

        // once the parse lands, the same frame resolves into real colors
        app.editor.as_mut().unwrap().wait_for_highlighter();
        assert!(!app.editor.as_ref().unwrap().highlight_pending());
        let buf = render(&mut app);
        let colored: String =
            buf.content().iter().filter(|c| c.style().fg == keyword).map(|c| c.symbol()).collect();
        assert!(colored.contains("fn"), "keywords colored after resolve: {colored:?}");
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
        // spell scope comes from the highlight spans — wait out the
        // background parse (interactively a skeleton frame shows instead)
        app.editor.as_mut().unwrap().wait_for_highlighter();
        assert!(app.editor.as_ref().unwrap().spell_check, "spell on by default");
        let buf = render(&mut app);
        // misspelled comment words carry the UNDERCURL modifier in the
        // spell curl color
        let (r, g, b) = SPELL_CURL();
        let is_spell_curl = |c: &ratatui::buffer::Cell| {
            c.style().add_modifier.contains(crate::backend::UNDERCURL)
                && c.style().underline_color == Some(Color::Rgb(r, g, b))
        };
        let spell: String =
            buf.content().iter().filter(|c| is_spell_curl(c)).map(|c| c.symbol()).collect();
        assert!(spell.contains("teh"), "flagged 'teh': {spell:?}");
        assert!(spell.contains("mispeld"), "flagged 'mispeld': {spell:?}");
        assert!(!spell.contains("ok"), "code not spell-checked");

        // :spell toggles it off
        app.editor.as_mut().unwrap().spell_check = false;
        let buf = render(&mut app);
        assert!(!buf.content().iter().any(is_spell_curl), "toggled off");
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
        let mut app = App::new(dir.path().to_path_buf());
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
        let invisible_hit =
            buf.content().iter().any(|c| c.symbol() == "▒" && c.style().bg == Some(INVISIBLE_BG()));
        assert!(invisible_hit, "invisible char shown as ▒");
        // legitimate 'é' is NOT highlighted
        let accent_clean =
            buf.content().iter().any(|c| c.symbol() == "é" && c.style().bg != Some(INVISIBLE_BG()));
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
        // the diagnostic span carries the UNDERCURL modifier in the
        // error-red curl color, covering the whole range
        let curl_cells: Vec<&ratatui::buffer::Cell> = buf
            .content()
            .iter()
            .filter(|c| c.style().add_modifier.contains(crate::backend::UNDERCURL))
            .collect();
        assert!(!curl_cells.is_empty(), "undercurl cells rendered");
        let combined: String = curl_cells.iter().map(|c| c.symbol()).collect();
        assert_eq!(combined, "fn ");
        assert_eq!(
            curl_cells[0].style().underline_color,
            Some(Color::Rgb(240, 90, 105)),
            "error red curl"
        );
        // fancy mode: wavy, not the straight-underline fallback
        assert!(!curl_cells[0].style().add_modifier.contains(Modifier::UNDERLINED));
    }

    // the overlay only exists in debug builds; so does its test (`cargo
    // bench` compiles this target under the release profile)
    #[cfg(debug_assertions)]
    #[test]
    fn hitbox_debug_overlay_outlines_rects() {
        // call the overlay directly — flipping the env var would race the
        // other render tests running in parallel
        let (_dir, mut app) = test_app();
        unsafe { std::env::set_var("VIBIN_FANCY", "1") };
        let backend = TestBackend::new(100, 32);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw(f, &mut app);
                draw_hitbox_debug(f, &app);
            })
            .unwrap();
        let text = format!("{:?}", terminal.backend().buffer());
        assert!(text.contains("sidebar"), "sidebar hitbox labeled");
        assert!(text.contains("term"), "terminal hitbox labeled");
    }

    #[test]
    fn gutter_rule_joins_the_pane_border() {
        let (dir, mut app) = test_app();
        let path = dir.path().join("code.txt");
        std::fs::write(&path, "one\ntwo\nthree\n").unwrap();
        app.open_file(&path);
        let buf = render(&mut app);
        // find the rule column: the │ right of the line numbers (the pane
        // top border sits below the two menu-bar rows)
        let pane_y = 2u16;
        let row1 = 3u16;
        let x = (0..buf.area.width)
            .find(|&x| {
                buf[(x, row1)].symbol() == "│"
                    && x > SIDEBAR_WIDTH
                    && buf[(x, pane_y)].symbol() != "│"
            })
            .expect("gutter rule rendered");
        assert_eq!(buf[(x, pane_y)].symbol(), "┬", "joins the top border");
        assert_eq!(buf[(x, buf.area.bottom() - 2)].symbol(), "┴", "joins the bottom border");
    }

    #[test]
    fn sidebar_and_main_pane_keep_separate_borders() {
        let (_dir, mut app) = test_app();
        app.shell = Shell::Code;
        let buf = render(&mut app);
        let sidebar_edge = SIDEBAR_WIDTH - 1; // sidebar's own right border
        let pane_edge = SIDEBAR_WIDTH; // main pane's own left border
        // pane borders sit below the two menu-bar rows, above the status bar
        let (top, bottom) = (2u16, buf.area.bottom() - 2);
        let mid = (top + bottom) / 2;
        // each pane keeps its own frame: two adjacent vertical borders, not
        // one merged line — the sidebar reads as a separate layer
        assert_eq!(buf[(sidebar_edge, mid)].symbol(), "│", "sidebar right border");
        assert_eq!(buf[(pane_edge, mid)].symbol(), "│", "main pane left border");
        // both frames close their own corners rather than merging into ┬/┴
        assert_eq!(buf[(sidebar_edge, top)].symbol(), "╮", "sidebar top-right corner");
        assert_eq!(buf[(pane_edge, top)].symbol(), "╭", "main pane top-left corner");
        assert_eq!(buf[(sidebar_edge, bottom)].symbol(), "╯", "sidebar bottom-right corner");
        assert_eq!(buf[(pane_edge, bottom)].symbol(), "╰", "main pane bottom-left corner");
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
    fn hover_origin_span_gets_a_marker_tint() {
        let (dir, mut app) = test_app();
        let path = dir.path().join("code.txt");
        std::fs::write(&path, "let example = 1;\n").unwrap();
        app.open_file(&path);
        app.overlay = Some(Overlay::Hover(crate::app::HoverDoc {
            text: "docs".into(),
            scroll: 0,
            diagnostics: vec![],
        }));
        app.code_view.hover_doc_pos = Some((0, 6)); // inside "example"
        let buf = render(&mut app);
        let tinted: String = buf
            .content()
            .iter()
            .filter(|c| c.style().bg == Some(HOVER_SPAN_BG()))
            .map(|c| c.symbol())
            .collect();
        assert_eq!(tinted, "example", "exactly the hovered word: {tinted:?}");
        // popup closed → tint gone
        app.overlay = None;
        let buf = render(&mut app);
        assert!(!buf.content().iter().any(|c| c.style().bg == Some(HOVER_SPAN_BG())));
    }

    #[test]
    fn hover_footer_is_static_and_tracks_scroll() {
        let (_dir, mut app) = test_app();
        let long: String = (1..=40).map(|i| format!("doc line {i}\n\n")).collect();
        app.overlay = Some(Overlay::Hover(crate::app::HoverDoc {
            text: long,
            scroll: 0,
            diagnostics: vec![],
        }));
        let footer_row = |buf: &ratatui::buffer::Buffer, rect: Rect| -> String {
            let y = rect.bottom() - 1;
            (rect.x..rect.right()).map(|x| buf[(x, y)].symbol().to_string()).collect()
        };
        let buf = render(&mut app);
        let rect = app.layout.hover_rect;
        // static header: a rule row, no content on it
        let header: String =
            (rect.x..rect.right()).map(|x| buf[(x, rect.y)].symbol().to_string()).collect();
        assert!(header.contains("═"), "header rule rendered: {header:?}");
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
    fn hovering_a_link_shows_a_url_preview_chip() {
        let (_dir, mut app) = test_app();
        app.overlay = Some(Overlay::Hover(crate::app::HoverDoc {
            text: "docs: [example](https://example.com/page)".into(),
            scroll: 0,
            diagnostics: vec![],
        }));
        let _ = render(&mut app);
        let (hit, _) = app.link_hits[0].clone();
        // rest the pointer on the link label
        let changed = app.handle_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Moved,
            column: hit.x,
            row: hit.y,
            modifiers: crossterm::event::KeyModifiers::NONE,
        });
        assert!(changed, "hovering the link requests a redraw");
        let buf = render(&mut app);
        let text = format!("{buf:?}");
        assert!(text.contains("https://example.com/page"), "chip shows the URL");
        // moving off the link clears the preview
        let changed = app.handle_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Moved,
            column: app.layout.hover_rect.x,
            row: app.layout.hover_rect.y,
            modifiers: crossterm::event::KeyModifiers::NONE,
        });
        assert!(changed);
        assert_eq!(app.hovered_link, None);
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
                (rect.x..rect.right())
                    .any(|x| buf[(x, y)].symbol() == "f" && buf[(x + 1, y)].symbol() == "n")
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
                start_u16: 0,
                end_u16: 3,
                end_line: 0,
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
        let pos =
            buf.content().iter().position(|c| c.symbol().contains("]8;;https")).unwrap() as u16;
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
    fn bell_toggles_the_notification_pane() {
        use crate::app::ToastLevel;
        use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
        let (_dir, mut app) = test_app();
        app.notify(ToastLevel::Warn, "engine two is on fire");
        app.toasts.clear(); // the transient toast would show the text too
        let buf = render(&mut app);
        assert!(!format!("{buf:?}").contains("engine two"), "pane closed by default");
        // click the bell chip
        let bell = app.layout.menu_bell;
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: bell.x,
            row: bell.y,
            modifiers: KeyModifiers::NONE,
        };
        assert!(app.handle_mouse(click));
        assert!(app.notifications_open);
        let text = format!("{:?}", render(&mut app));
        assert!(text.contains("engine two is on fire"), "history shown:\n{text}");
        // history persists even after the toast itself expired
        app.toasts.clear();
        let text = format!("{:?}", render(&mut app));
        assert!(text.contains("engine two is on fire"));
        // click again: closed (re-read the rect — the unread badge
        // changes the chip's width between renders)
        render(&mut app);
        let bell = app.layout.menu_bell;
        assert!(app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: bell.x,
            row: bell.y,
            modifiers: KeyModifiers::NONE,
        }));
        assert!(!app.notifications_open);
    }

    #[test]
    fn notification_center_behaves_like_vscode() {
        use crate::app::ToastLevel;
        use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
        let (_dir, mut app) = test_app();
        // unread badge on the bell while the pane is closed
        app.notify(ToastLevel::Info, "first");
        app.notify(ToastLevel::Warn, "second");
        app.toasts.clear();
        let text = format!("{:?}", render(&mut app));
        assert!(text.contains(" 2 "), "unread count badge:\n{text}");
        // opening the pane marks everything read
        let bell = app.layout.menu_bell;
        let click_at = |x: u16, y: u16| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        };
        assert!(app.handle_mouse(click_at(bell.x, bell.y)));
        assert_eq!(app.notifications_seen, 2);
        // while open, plain notifications go straight to the pane, no toast
        app.notify(ToastLevel::Info, "third");
        assert!(app.toasts.is_empty(), "no toast while the center is open");
        assert_eq!(app.notifications.len(), 3);
        assert_eq!(app.notifications_seen, 3, "arrivals in an open pane are read");
        // …but buttoned questions still pop (the pane can't answer them)
        app.notify_actions(ToastLevel::Info, "pick", vec!["A".into()], None);
        assert_eq!(app.toasts.len(), 1);
        app.toasts.clear();
        // clear all empties the history
        render(&mut app);
        let clear = app.layout.notifications_clear;
        assert!(app.handle_mouse(click_at(clear.x, clear.y)));
        assert!(app.notifications.is_empty());
        let text = format!("{:?}", render(&mut app));
        assert!(text.contains("no notifications"));
    }

    #[test]
    fn pane_renders_pending_question_buttons() {
        use crate::app::ToastLevel;
        use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
        let (_dir, mut app) = test_app();
        app.notifications_open = true;
        app.notify_actions(ToastLevel::Warn, "deploy?", vec!["Yes".into(), "No".into()], None);
        render(&mut app);
        // the pane lists the question WITH its buttons
        let pane = app.layout.notifications;
        let (hit, ti, b) = app
            .toast_hits
            .iter()
            .find(|(r, ..)| pane.contains(ratatui::layout::Position::new(r.x, r.y)))
            .copied()
            .expect("button hitbox inside the pane");
        assert_eq!((ti, b), (0, Some(0)));
        assert!(app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: hit.x + 1,
            row: hit.y,
            modifiers: KeyModifiers::NONE,
        }));
        assert!(app.toasts.is_empty(), "clicking a pane button resolves the question");
        // resolved: the buttons leave the pane, the entry stays
        let text = format!("{:?}", render(&mut app));
        assert!(text.contains("deploy?"));
        assert!(!text.contains(" Yes "), "buttons gone after resolution:\n{text}");
    }

    #[test]
    fn notification_pane_renders_markdown_and_clickable_links() {
        use crate::app::ToastLevel;
        use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
        let (_dir, mut app) = test_app();
        app.notify(ToastLevel::Info, "**bold** and a [manual](https://example.com/man)");
        app.toasts.clear();
        app.notifications_open = true;
        let buf = render(&mut app);
        let text = format!("{buf:?}");
        assert!(!text.contains("**bold**"), "markdown is rendered, not raw");
        assert!(
            buf.content().iter().any(|c| c.symbol().contains("]8;;https://example.com/man")),
            "pane link becomes an OSC 8 cell"
        );
        // the link hitbox opens the url on click
        let (hit, url) = app
            .link_hits
            .iter()
            .find(|(_, u)| u.contains("example.com"))
            .cloned()
            .expect("link hitbox registered");
        assert!(app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: hit.x,
            row: hit.y,
            modifiers: KeyModifiers::NONE,
        }));
        assert_eq!(app.status_msg.as_deref(), Some(format!("opened {url}").as_str()));
        assert!(app.notifications_open, "click doesn't close the pane");
    }

    #[test]
    fn file_tree_devicons_follow_the_config_flag() {
        let (dir, mut app) = test_app();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
        app.tree = crate::filetree::FileTree::new(dir.path());
        app.shell = Shell::Code;
        let text = format!("{:?}", render(&mut app));
        assert!(text.contains(crate::devicons::FOLDER), "folder icon (default on)");
        assert!(text.contains('\u{e7a8}'), "rust icon on main.rs");
        // plain glyphs when disabled
        app.config.icons = false;
        let text = format!("{:?}", render(&mut app));
        assert!(text.contains("[▶]"), "plain fallback");
        assert!(!text.contains('\u{e7a8}'));
    }

    #[test]
    fn menu_bar_hover_opens_dropdown_and_click_runs_action() {
        use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
        let (_dir, mut app) = test_app();
        let moved = |x: u16, y: u16| MouseEvent {
            kind: MouseEventKind::Moved,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        };
        render(&mut app);
        // hovering the "View" label opens its dropdown
        let view = crate::app::MENU_BAR.iter().position(|(l, _)| *l == "View").unwrap();
        let label = app.layout.menu_items[view];
        assert!(app.handle_mouse(moved(label.x, label.y)), "hover opens");
        assert_eq!(app.menu_open, Some(view));
        let buf = render(&mut app);
        let text = format!("{buf:?}");
        assert!(text.contains("Agents"), "dropdown entries rendered");
        // sliding along the bar switches dropdowns without a click
        let file = app.layout.menu_items[0];
        assert!(app.handle_mouse(moved(file.x, file.y)));
        assert_eq!(app.menu_open, Some(0));
        assert!(app.handle_mouse(moved(label.x, label.y)));
        render(&mut app);
        // hover highlights a row, click runs its action (View → Git)
        let dd = app.layout.menu_dropdown;
        let git_row = crate::app::MENU_BAR[view].1.iter().position(|(l, _)| *l == "Git").unwrap();
        let row_y = dd.y + 1 + git_row as u16;
        assert!(app.handle_mouse(moved(dd.x + 2, row_y)));
        assert_eq!(app.menu_row, git_row);
        assert!(app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: dd.x + 2,
            row: row_y,
            modifiers: KeyModifiers::NONE,
        }));
        assert_eq!(app.menu_open, None, "click closes the menu");
        assert_eq!(app.shell, Shell::Git, "action ran");
        // moving away from bar and dropdown closes without running anything
        render(&mut app);
        let label = app.layout.menu_items[view];
        app.handle_mouse(moved(label.x, label.y));
        render(&mut app);
        assert!(app.handle_mouse(moved(40, 15)));
        assert_eq!(app.menu_open, None, "stray hover closes");
    }

    #[test]
    fn toast_markdown_renders_and_links_click_open() {
        use crate::app::ToastLevel;
        use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
        let (_dir, mut app) = test_app();
        app.notify(ToastLevel::Info, "see the [manual](https://example.com/man) for more");
        let buf = render(&mut app);
        // the link label is packed into an OSC 8 cell
        assert!(
            buf.content().iter().any(|c| c.symbol().contains("\x1b]8;;https://example.com/man")),
            "toast link becomes an OSC 8 cell"
        );
        let (hit, url) = app.link_hits[0].clone();
        assert_eq!(url, "https://example.com/man");
        // clicking the link opens it and keeps the toast up
        assert!(app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: hit.x,
            row: hit.y,
            modifiers: KeyModifiers::NONE,
        }));
        assert_eq!(app.status_msg.as_deref(), Some("opened https://example.com/man"));
        assert_eq!(app.toasts.len(), 1, "link click does not dismiss");
    }

    #[test]
    fn buttoned_toasts_stick_and_resolve_on_click() {
        use crate::app::ToastLevel;
        use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
        let (_dir, mut app) = test_app();
        app.notify_actions(
            ToastLevel::Info,
            "restart server?",
            vec!["Yes".into(), "No".into()],
            None,
        );
        // sticky: outlives the TTL
        app.toasts[0].born = std::time::Instant::now() - std::time::Duration::from_secs(60);
        app.tick();
        assert_eq!(app.toasts.len(), 1, "buttoned toast survives expiry");
        let buf = render(&mut app);
        let text = format!("{buf:?}");
        assert!(text.contains("restart server?"));
        assert!(text.contains(" Yes "));
        assert!(text.contains(" No "));
        // hover the second button, then click it
        let (rect, _, _) =
            *app.toast_hits.iter().find(|(_, t, b)| *t == 0 && *b == Some(1)).unwrap();
        let at =
            |kind| MouseEvent { kind, column: rect.x, row: rect.y, modifiers: KeyModifiers::NONE };
        assert!(app.handle_mouse(at(MouseEventKind::Moved)));
        assert_eq!(app.toast_hover, Some((0, 1)));
        assert!(app.handle_mouse(at(MouseEventKind::Down(MouseButton::Left))));
        assert!(app.toasts.is_empty(), "click resolves and removes the toast");
    }

    #[test]
    fn hovered_toasts_do_not_expire() {
        use crate::app::ToastLevel;
        use crossterm::event::{KeyModifiers, MouseEvent, MouseEventKind};
        let (_dir, mut app) = test_app();
        app.notify(ToastLevel::Info, "hover pins me");
        render(&mut app);
        let (rect, ..) = app.toast_hits[0];
        let moved = |x: u16, y: u16| MouseEvent {
            kind: MouseEventKind::Moved,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        };
        // pointer over the card: expiry pauses even far past the TTL
        app.handle_mouse(moved(rect.x + 1, rect.y));
        app.toasts[0].born = std::time::Instant::now() - std::time::Duration::from_secs(60);
        app.tick();
        assert_eq!(app.toasts.len(), 1, "hovered toast survives");
        // pointer leaves: the timer restarts, then runs out normally
        app.handle_mouse(moved(5, 20));
        app.tick();
        assert_eq!(app.toasts.len(), 1, "fresh TTL after unhover");
        app.toasts[0].born = std::time::Instant::now() - std::time::Duration::from_secs(60);
        app.tick();
        assert!(app.toasts.is_empty(), "expires once unhovered");
    }

    #[test]
    fn inter_hunk_bands_hover_and_expand_without_fold_mode() {
        use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
        let dir = tempfile::TempDir::new().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        let mut cfg = repo.config().unwrap();
        cfg.set_str("user.name", "T").unwrap();
        cfg.set_str("user.email", "t@e").unwrap();
        drop(cfg);
        let base: String = (1..=25).map(|i| format!("l{i}\n")).collect();
        std::fs::write(dir.path().join("f.txt"), &base).unwrap();
        crate::git::stage_all(&repo).unwrap();
        crate::git::commit(&repo, "base").unwrap();
        let changed = base.replace("l2\n", "L2\n").replace("l20\n", "L20\n");
        std::fs::write(dir.path().join("f.txt"), changed).unwrap();
        let mut app = App::new(dir.path().to_path_buf());
        app.lsp_enabled = false;
        app.shell = Shell::Git;
        app.git.refresh();
        app.refresh_git_pane(true);
        // default mode: the lines git omitted between hunks band up…
        let text = format!("{:?}", render(&mut app));
        assert!(text.contains("unchanged lines"), "inter-hunk band:\n{text}");
        assert!(!text.contains("l10"), "omitted lines hidden");
        // …with a hover state…
        let fold = app.git_view.pane.folds[0].clone();
        assert!(fold.band);
        let row = fold.row;
        let pane = app.layout.terminal_pane;
        let at = (pane.x + 4, pane.y + 1 + row as u16);
        assert!(app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Moved,
            column: at.0,
            row: at.1,
            modifiers: KeyModifiers::NONE,
        }));
        assert_eq!(app.git_view.gap_hover, Some(row));
        // …and a click expands the omitted lines from the file. With the
        // compact default the first band is the single leading line, so
        // the big middle region (containing l10) is the second band.
        let fold = app.git_view.pane.folds[1].clone();
        assert!(fold.band);
        let row = fold.row;
        assert!(app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: pane.x + 4,
            row: pane.y + 1 + row as u16,
            modifiers: KeyModifiers::NONE,
        }));
        let text = format!("{:?}", render(&mut app));
        assert!(text.contains("l10"), "omitted lines spliced in:\n{text}");
    }

    #[test]
    fn additions_only_regions_fold_independently() {
        use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
        let dir = tempfile::TempDir::new().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        let mut cfg = repo.config().unwrap();
        cfg.set_str("user.name", "T").unwrap();
        cfg.set_str("user.email", "t@e").unwrap();
        drop(cfg);
        let base: String = (1..=20).map(|i| format!("l{i}\n")).collect();
        std::fs::write(dir.path().join("f.txt"), &base).unwrap();
        crate::git::stage_all(&repo).unwrap();
        crate::git::commit(&repo, "base").unwrap();
        // two pure-insertion hunks: the old side never advances at the
        // change rows, which used to collide the region keys
        let changed = base.replace("l3\n", "l3\nNEW-A\n").replace("l15\n", "l15\nNEW-B\n");
        std::fs::write(dir.path().join("f.txt"), changed).unwrap();
        let mut app = App::new(dir.path().to_path_buf());
        app.lsp_enabled = false;
        app.shell = Shell::Git;
        app.git.refresh();
        app.refresh_git_pane(true);
        render(&mut app);
        let bands: Vec<crate::diff::FoldRow> =
            app.git_view.pane.folds.iter().filter(|f| f.band).cloned().collect();
        assert!(bands.len() >= 3, "{bands:?}");
        let keys: std::collections::HashSet<_> = bands.iter().map(|f| f.key).collect();
        assert_eq!(keys.len(), bands.len(), "region keys must be unique: {bands:?}");
        // clicking the middle band expands only its region
        let row = bands[1].row;
        let pane = app.layout.terminal_pane;
        assert!(app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: pane.x + 14,
            row: pane.y + 1 + row as u16,
            modifiers: KeyModifiers::NONE,
        }));
        let text = format!("{:?}", render(&mut app));
        assert!(text.contains("l8"), "middle region expanded:\n{text}");
        assert!(!text.contains("l18"), "trailing region stays folded:\n{text}");
        assert!(
            !text.contains("l1\\u{a0}") && !text.contains(" l1 "),
            "leading region stays folded"
        );
    }

    #[test]
    fn diff_folds_unchanged_regions_by_default() {
        let dir = tempfile::TempDir::new().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        let mut cfg = repo.config().unwrap();
        cfg.set_str("user.name", "T").unwrap();
        cfg.set_str("user.email", "t@e").unwrap();
        drop(cfg);
        std::fs::write(dir.path().join("f.txt"), "one\ntwo\nthree\nfour\nfive\n").unwrap();
        crate::git::stage_all(&repo).unwrap();
        crate::git::commit(&repo, "base").unwrap();
        std::fs::write(dir.path().join("f.txt"), "one\ntwo\nTHREE\nfour\nfive\n").unwrap();
        let mut app = App::new(dir.path().to_path_buf());
        app.lsp_enabled = false;
        app.shell = Shell::Git;
        app.git.refresh();
        app.refresh_git_pane(true);
        // default: compact — only the change, unchanged regions banded
        let text = format!("{:?}", render(&mut app));
        assert!(!text.contains("two"), "unchanged folded by default:\n{text}");
        assert!(text.contains("THREE"), "changes stay:\n{text}");
        assert!(text.contains("2 unchanged lines"), "counted gap bands:\n{text}");
        // z: everything expands (the whole file), z again refolds
        app.git.fold_all = false;
        app.refresh_git_pane(true);
        let text = format!("{:?}", render(&mut app));
        assert!(text.contains("two") && text.contains("five"), "z expands all:\n{text}");
        app.git.fold_all = true;
        app.refresh_git_pane(true);
        // clicking the first band expands just that region
        use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
        let row = app.git_view.pane.folds[0].row;
        let pane = app.layout.terminal_pane;
        assert!(app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: pane.x + 4,
            row: pane.y + 1 + row as u16,
            modifiers: KeyModifiers::NONE,
        }));
        let text = format!("{:?}", render(&mut app));
        assert!(text.contains("two"), "clicked region expanded:\n{text}");
        assert!(text.contains("unchanged line"), "the other region stays folded:\n{text}");
        // clicking a context line of the expanded region folds it back
        let row =
            app.git_view.pane.folds.iter().find(|f| !f.band).expect("expanded rows registered").row;
        assert!(app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: pane.x + 4,
            row: pane.y + 1 + row as u16,
            modifiers: KeyModifiers::NONE,
        }));
        let text = format!("{:?}", render(&mut app));
        assert!(!text.contains("two"), "region folded back:\n{text}");
    }

    #[test]
    fn status_bar_shows_push_pull_counts() {
        let dir = tempfile::TempDir::new().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        let mut cfg = repo.config().unwrap();
        cfg.set_str("user.name", "T").unwrap();
        cfg.set_str("user.email", "t@e").unwrap();
        drop(cfg);
        std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
        crate::git::stage(&repo, "a.txt").unwrap();
        crate::git::commit(&repo, "base").unwrap();
        // pin a stand-in upstream (remote "." = this repo) at "base", then
        // advance the local branch one commit past it → one to push
        {
            let head = repo.head().unwrap().peel_to_commit().unwrap();
            repo.branch("base", &head, false).unwrap();
        }
        let name = crate::git::head_branch(&repo).unwrap();
        let mut cfg = repo.config().unwrap();
        cfg.set_str(&format!("branch.{name}.remote"), ".").unwrap();
        cfg.set_str(&format!("branch.{name}.merge"), "refs/heads/base").unwrap();
        drop(cfg);
        std::fs::write(dir.path().join("b.txt"), "two\n").unwrap();
        crate::git::stage(&repo, "b.txt").unwrap();
        crate::git::commit(&repo, "ahead").unwrap();
        drop(repo);

        let mut app = App::new(dir.path().to_path_buf());
        app.lsp_enabled = false;
        app.shell = Shell::Git;
        app.git.refresh();
        assert_eq!(app.git.upstream, Some((1, 0)));
        let buf = render(&mut app);
        // the status bar (last row) carries the sync arrow and the
        // behind/ahead counts: ↺ 0↓ 1↑
        let bottom = buf.area.bottom() - 1;
        let bar: String =
            (0..buf.area.width).map(|x| buf[(x, bottom)].symbol().to_string()).collect();
        assert!(bar.contains('↺'), "sync glyph on the status bar: {bar:?}");
        assert!(bar.contains("0↓ 1↑"), "behind then ahead counts: {bar:?}");
    }

    #[test]
    fn staged_boundary_gets_a_separator_rule() {
        let dir = tempfile::TempDir::new().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        let mut cfg = repo.config().unwrap();
        cfg.set_str("user.name", "T").unwrap();
        cfg.set_str("user.email", "t@e").unwrap();
        drop(cfg);
        std::fs::write(dir.path().join("staged.txt"), "one\n").unwrap();
        std::fs::write(dir.path().join("pending.txt"), "two\n").unwrap();
        crate::git::stage(&repo, "staged.txt").unwrap();
        drop(repo);
        let mut app = App::new(dir.path().to_path_buf());
        app.lsp_enabled = false;
        app.shell = Shell::Git;
        app.git.refresh();
        let buf = render(&mut app);
        // the rule between the staged and unstaged sections joins the pane
        // borders: ├───…───┤
        let y = (1..buf.area.height - 1)
            .find(|&y| buf[(0, y)].symbol() == "├")
            .expect("separator junction on the left border");
        assert_eq!(buf[(SIDEBAR_WIDTH - 1, y)].symbol(), "┤", "right junction");
        assert_eq!(buf[(1, y)].symbol(), "─", "rule spans the row");
        // staged above the rule, unstaged below
        let row_text = |row: u16| -> String {
            (0..SIDEBAR_WIDTH).map(|x| buf[(x, row)].symbol().to_string()).collect()
        };
        assert!(row_text(y - 1).contains("staged.txt"));
        assert!(row_text(y + 1).contains("pending.txt"));
        // unstage everything: the separator disappears
        let repo = git2::Repository::open(dir.path()).unwrap();
        crate::git::unstage(&repo, "staged.txt").unwrap();
        drop(repo);
        app.git.refresh();
        let buf = render(&mut app);
        assert!(
            (1..buf.area.height - 1).all(|y| buf[(0, y)].symbol() != "├"),
            "rule gone without staged files"
        );
    }

    #[test]
    fn git_diff_gutter_rule_joins_the_pane_border() {
        let dir = tempfile::TempDir::new().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        let mut cfg = repo.config().unwrap();
        cfg.set_str("user.name", "T").unwrap();
        cfg.set_str("user.email", "t@e").unwrap();
        drop(cfg);
        std::fs::write(dir.path().join("f.txt"), "one\ntwo\nthree\n").unwrap();
        crate::git::stage_all(&repo).unwrap();
        crate::git::commit(&repo, "base").unwrap();
        std::fs::write(dir.path().join("f.txt"), "one\nTWO\nthree\n").unwrap();
        let mut app = App::new(dir.path().to_path_buf());
        app.lsp_enabled = false;
        app.shell = Shell::Git;
        app.git.refresh();
        app.refresh_git_pane(true);
        let buf = render(&mut app);
        // find the rule column: the bar right of the two line-number
        // columns (the pane top border sits below the two menu-bar rows)
        let pane_y = 2u16;
        let row1 = 3u16;
        // the first row may be a fold band (├ at the rule column), so
        // scan all pane rows for the rule
        let _ = row1;
        let x = (0..buf.area.width)
            .find(|&x| {
                x > SIDEBAR_WIDTH
                    && buf[(x, pane_y)].symbol() != "│"
                    && (3..buf.area.bottom() - 2).any(|y| matches!(buf[(x, y)].symbol(), "│" | "┃"))
            })
            .expect("diff gutter rule rendered");
        assert_eq!(buf[(x, pane_y)].symbol(), "┬", "joins the top border");
        assert_eq!(buf[(x, buf.area.bottom() - 2)].symbol(), "┴", "joins the bottom border");
    }

    #[test]
    fn toasts_render_and_expire() {
        use crate::app::ToastLevel;
        let (_dir, mut app) = test_app();
        app.notify(ToastLevel::Info, "toast says hi");
        app.notify(ToastLevel::Error, "toast says ouch");
        let buf = render(&mut app);
        let text = format!("{buf:?}");
        assert!(text.contains("toast says hi"), "info toast rendered");
        assert!(text.contains("toast says ouch"), "error toast rendered");
        // an expired toast is pruned by tick (and triggers a redraw)
        app.toasts[0].born = std::time::Instant::now() - std::time::Duration::from_secs(60);
        assert!(app.tick());
        assert_eq!(app.toasts.len(), 1);
        let buf = render(&mut app);
        let text = format!("{buf:?}");
        assert!(!text.contains("toast says hi"), "expired toast gone");
        assert!(text.contains("toast says ouch"), "fresh toast stays");
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
        // agents shell: the code shell's home card shares the dialog chrome
        app.shell = Shell::Agents;
        app.overlay = Some(Overlay::CommitPrompt("msg".into()));
        let buf = render(&mut app);
        assert!(bg_count(&buf, DIALOG_BG()) > 50);
        assert_eq!(border_count(&buf), 3);
    }
}
