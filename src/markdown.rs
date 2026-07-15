//! A small reusable markdown → styled-Lines renderer. Used by the LSP
//! hover popup; intended to also render .md files later.
//!
//! Supported: headings, fenced code blocks, inline code, **bold**,
//! *italic*, ~~strikethrough~~, [links](url), bare URL autolinks, bullet
//! lists, blockquotes, tables, and horizontal rules (---, ***, ___).

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// Hover/preview markdown colors, derived from the terminal theme with
/// the same machinery as the editor: palette slots for accents, fg/bg
/// washes for surfaces. Fallbacks match the old dark constants, plus
/// light variants.
fn wash(weight: u32) -> Option<Color> {
    crate::color::wash(weight).map(|(r, g, b)| Color::Rgb(r, g, b))
}
fn slot(i: usize, dark: (u8, u8, u8), light: (u8, u8, u8)) -> Color {
    let (r, g, b) =
        crate::color::ansi16(i).unwrap_or(if crate::color::is_light() { light } else { dark });
    Color::Rgb(r, g, b)
}
fn adaptive(dark: Color, light: Color) -> Color {
    if crate::color::is_light() { light } else { dark }
}
#[allow(non_snake_case)]
fn CODE_BG() -> Color {
    wash(16).unwrap_or_else(|| adaptive(Color::Rgb(26, 28, 34), Color::Rgb(232, 234, 238)))
}
#[allow(non_snake_case)]
fn HEADING_FG() -> Color {
    wash(252).unwrap_or_else(|| adaptive(Color::Rgb(240, 244, 250), Color::Rgb(24, 26, 32)))
}
#[allow(non_snake_case)]
pub fn LINK_FG() -> Color {
    slot(12, (110, 175, 255), (9, 105, 218))
}
/// Same color as the hover popup's header/footer rules (ui::DIALOG_BORDER),
/// so every horizontal line inside a popover reads as one family.
#[allow(non_snake_case)]
fn RULE_FG() -> Color {
    wash(110).unwrap_or_else(|| adaptive(Color::Rgb(96, 100, 112), Color::Rgb(152, 156, 166)))
}
#[allow(non_snake_case)]
fn QUOTE_FG() -> Color {
    wash(140).unwrap_or_else(|| adaptive(Color::Rgb(150, 156, 168), Color::Rgb(104, 108, 118)))
}
#[allow(non_snake_case)]
fn BULLET_FG() -> Color {
    slot(12, (110, 175, 255), (9, 105, 218))
}
#[allow(non_snake_case)]
fn DONE_FG() -> Color {
    slot(10, (126, 200, 120), (26, 143, 40))
}

thread_local! {
    /// Render width, used to pad code blocks into solid panels.
    static WIDTH_HINT: std::cell::Cell<usize> = const { std::cell::Cell::new(60) };
    /// Default language for inline `code` and unlabeled fences — the
    /// language of the source file the docs belong to (hover popups).
    static LANG_HINT: std::cell::Cell<&'static str> = const { std::cell::Cell::new("") };
}

/// A hyperlink found during rendering: rendered line index, char column
/// within that line, label text, and target url.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MdLink {
    pub line: usize,
    pub col: usize,
    pub text: String,
    pub url: String,
}

/// Render markdown into styled lines. `width` is used for horizontal rules
/// and code-block panels. (The hover popup uses render_with_links; this
/// simpler entry point is for future .md file rendering.)
#[allow(dead_code)]
pub fn render(text: &str, width: usize) -> Vec<Line<'static>> {
    render_with_links(text, width).0
}

/// Like render(), also reporting where hyperlinks ended up so callers can
/// emit OSC 8 overlays for terminals that support clickable links.
/// render_with_links with a default language: inline `code` spans and
/// fences without a language tag highlight as `lang` — hover docs read
/// like the source file they describe.
pub fn render_with_links_lang(
    text: &str,
    width: usize,
    lang: &'static str,
) -> (Vec<Line<'static>>, Vec<MdLink>) {
    LANG_HINT.with(|l| l.set(lang));
    let out = render_with_links(text, width);
    LANG_HINT.with(|l| l.set(""));
    out
}

pub fn render_with_links(text: &str, width: usize) -> (Vec<Line<'static>>, Vec<MdLink>) {
    WIDTH_HINT.with(|w| w.set(width));
    let mut links: Vec<MdLink> = Vec::new();
    let mut out = Vec::new();
    let mut code_block: Option<(String, Vec<String>)> = None;
    // indexed so a header row can peek at the next line for a table delimiter
    let src: Vec<&str> = text.lines().collect();
    let mut i = 0;
    while i < src.len() {
        let raw = src[i];
        let trimmed = raw.trim_start();
        if trimmed.starts_with("```") {
            match code_block.take() {
                Some((lang, block)) => out.extend(code_lines(&lang, &block)),
                None => {
                    let mut lang = trimmed.trim_start_matches('`').trim().to_lowercase();
                    if lang.is_empty() {
                        lang = LANG_HINT.with(|l| l.get()).to_string();
                    }
                    code_block = Some((lang, Vec::new()));
                }
            }
            i += 1;
            continue;
        }
        if let Some((_, block)) = &mut code_block {
            block.push(raw.to_string());
            i += 1;
            continue;
        }
        if is_rule(trimmed) {
            out.push(Line::from(Span::styled(
                "─".repeat(width.max(1)),
                Style::default().fg(RULE_FG()),
            )));
            i += 1;
            continue;
        }
        if let Some(rest) = heading_text(trimmed) {
            out.push(Line::from(Span::styled(
                rest.to_string(),
                Style::default().fg(HEADING_FG()).add_modifier(Modifier::BOLD),
            )));
            i += 1;
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("> ") {
            let mut spans = vec![Span::styled("▎ ", Style::default().fg(RULE_FG()))];
            spans.extend(restyle(
                inline_links(rest, 2, out.len(), &mut links),
                Style::default().fg(QUOTE_FG()),
            ));
            out.push(Line::from(spans));
            i += 1;
            continue;
        }
        // a GFM task-list item: a bullet whose content opens with [ ] or [x]
        if let Some((checked, rest)) =
            trimmed.strip_prefix("- ").or_else(|| trimmed.strip_prefix("* ")).and_then(|r| {
                r.strip_prefix("[ ] ").map(|t| (false, t)).or_else(|| {
                    r.strip_prefix("[x] ").or_else(|| r.strip_prefix("[X] ")).map(|t| (true, t))
                })
            })
        {
            let indent = raw.len() - trimmed.len();
            let fancy = crate::color::fancy_glyphs();
            let (glyph, fg) = match (fancy, checked) {
                (true, true) => ("✔ ", DONE_FG()),
                (true, false) => ("▢ ", BULLET_FG()),
                (false, true) => ("[x] ", DONE_FG()),
                (false, false) => ("[ ] ", BULLET_FG()),
            };
            let mut spans =
                vec![Span::raw(" ".repeat(indent)), Span::styled(glyph, Style::default().fg(fg))];
            spans.extend(inline_links(rest, indent + glyph.chars().count(), out.len(), &mut links));
            out.push(Line::from(spans));
            i += 1;
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("- ").or_else(|| trimmed.strip_prefix("* ")) {
            let indent = raw.len() - trimmed.len();
            let mut spans = vec![
                Span::raw(" ".repeat(indent)),
                Span::styled("• ", Style::default().fg(BULLET_FG())),
            ];
            spans.extend(inline_links(rest, indent + 2, out.len(), &mut links));
            out.push(Line::from(spans));
            i += 1;
            continue;
        }
        // a GFM table: a `| … |` header row whose next line is a `|---|---|`
        // delimiter with one cell per header column
        if trimmed.contains('|')
            && let Some(aligns) = src.get(i + 1).and_then(|d| parse_delimiter(d))
            && split_row(trimmed).len() == aligns.len()
        {
            let mut rows: Vec<&str> = vec![raw];
            let mut j = i + 2;
            while let Some(r) = src.get(j) {
                if r.trim().is_empty() || !r.contains('|') {
                    break;
                }
                rows.push(r);
                j += 1;
            }
            out.extend(table_lines(&rows, &aligns, width));
            i = j;
            continue;
        }
        let line_idx = out.len();
        out.push(Line::from(inline_links(raw, 0, line_idx, &mut links)));
        i += 1;
    }
    // unterminated fence: render what we have
    if let Some((lang, block)) = code_block {
        out.extend(code_lines(&lang, &block));
    }
    let out = collapse_rule_gaps(out, &mut links);
    let out = wrap_lines(out, &mut links, width);
    (out, links)
}

/// Drop blank rows adjacent to horizontal rules: the rule IS the
/// separator, so `text\n\n---\n\nmore` renders as three rows, not five.
fn collapse_rule_gaps(lines: Vec<Line<'static>>, links: &mut Vec<MdLink>) -> Vec<Line<'static>> {
    let is_rule =
        |l: &Line| l.width() > 0 && l.spans.iter().all(|sp| sp.content.chars().all(|c| c == '─'));
    let is_blank = |l: &Line| l.width() == 0;
    let mut keep = vec![true; lines.len()];
    for i in 0..lines.len() {
        if !is_blank(&lines[i]) {
            continue;
        }
        let prev = lines[..i].iter().rev().find(|l| !is_blank(l));
        let next = lines[i + 1..].iter().find(|l| !is_blank(l));
        if prev.is_some_and(is_rule) || next.is_some_and(is_rule) {
            keep[i] = false;
        }
    }
    // remap link line indices past the removed rows
    let mut new_index = vec![0usize; lines.len()];
    let mut n = 0;
    for (i, &k) in keep.iter().enumerate() {
        new_index[i] = n;
        if k {
            n += 1;
        }
    }
    for link in links.iter_mut() {
        link.line = new_index[link.line];
    }
    lines.into_iter().zip(keep).filter_map(|(l, k)| k.then_some(l)).collect()
}

/// Word-wrap rendered lines to `width`, splitting styled spans at word
/// boundaries and remapping link coordinates onto the wrapped rows. Done
/// here (not via ratatui's Paragraph::wrap) so scroll math and link
/// overlays keep counting real rows. Code-panel rows (fully backgrounded)
/// are exempt — code clips rather than reflows.
fn wrap_lines(
    lines: Vec<Line<'static>>,
    links: &mut Vec<MdLink>,
    width: usize,
) -> Vec<Line<'static>> {
    if width == 0 {
        return lines;
    }
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut remapped: Vec<MdLink> = Vec::new();
    for (idx, line) in lines.into_iter().enumerate() {
        let line_links: Vec<MdLink> = links.iter().filter(|l| l.line == idx).cloned().collect();
        let is_code_panel =
            !line.spans.is_empty() && line.spans.iter().all(|sp| sp.style.bg.is_some());
        if line.width() <= width || is_code_panel {
            for mut l in line_links {
                l.line = out.len();
                remapped.push(l);
            }
            out.push(line);
            continue;
        }
        let cells: Vec<(char, Style)> = line
            .spans
            .iter()
            .flat_map(|sp| {
                let st = sp.style;
                sp.content.chars().map(move |c| (c, st)).collect::<Vec<_>>()
            })
            .collect();
        // greedy word wrap over the styled cells
        let mut rows: Vec<(usize, usize)> = Vec::new();
        let mut start = 0;
        while start < cells.len() {
            if !rows.is_empty() {
                while start < cells.len() && cells[start].0 == ' ' {
                    start += 1;
                }
            }
            if start >= cells.len() {
                break;
            }
            let mut end = (start + width).min(cells.len());
            if end < cells.len()
                && let Some(space) = (start..end).rev().find(|&i| cells[i].0 == ' ')
                && space > start
            {
                end = space;
            }
            rows.push((start, end));
            start = end;
        }
        for &(s0, e0) in &rows {
            let mut spans: Vec<Span<'static>> = Vec::new();
            for &(c, st) in &cells[s0..e0] {
                match spans.last_mut() {
                    Some(last) if last.style == st => last.content.to_mut().push(c),
                    _ => spans.push(Span::styled(c.to_string(), st)),
                }
            }
            for l in &line_links {
                let len = l.text.chars().count();
                // links split by the wrap are dropped (same policy the
                // hover overlay already applies to clipped links)
                if l.col >= s0 && l.col + len <= e0 {
                    let mut nl = l.clone();
                    nl.line = out.len();
                    nl.col = l.col - s0;
                    remapped.push(nl);
                }
            }
            out.push(Line::from(spans));
        }
    }
    *links = remapped;
    out
}

/// Inline `code` → spans on the panel background, syntax-highlighted with
/// the hinted language (the hovered file's) when one is set.
fn inline_code_spans(code: &str) -> Vec<Span<'static>> {
    use crate::editor::highlight::{line_spans, style_for};
    let lang = LANG_HINT.with(|l| l.get());
    let plain = || vec![Span::styled(code.to_string(), Style::default().bg(CODE_BG()))];
    if lang.is_empty() {
        return plain();
    }
    let hl = line_spans(lang, code);
    if hl.is_empty() {
        return plain();
    }
    let mut styles = vec![Style::default().bg(CODE_BG()); code.chars().count()];
    for span in &hl {
        let s_chars = code.get(..span.start).map(|t| t.chars().count()).unwrap_or(0);
        let e_chars = code.get(..span.end).map(|t| t.chars().count()).unwrap_or(s_chars);
        let style = style_for(span.highlight).bg(CODE_BG());
        if style.fg.is_some() {
            for slot in styles.iter_mut().take(e_chars.min(code.chars().count())).skip(s_chars) {
                *slot = style;
            }
        }
    }
    let mut spans: Vec<Span<'static>> = Vec::new();
    for (c, st) in code.chars().zip(styles) {
        match spans.last_mut() {
            Some(last) if last.style == st => last.content.to_mut().push(c),
            _ => spans.push(Span::styled(c.to_string(), st)),
        }
    }
    spans
}

/// Fenced code block → syntax-highlighted lines via tree-sitter when the
/// fence names a language we know; plain code tint otherwise.
fn code_lines(lang: &str, block: &[String]) -> Vec<Line<'static>> {
    use crate::editor::highlight::{cached_config_for_lang, highlight_source, style_for};

    let pad_to = |_text: &str, spans: &mut Vec<Span<'static>>| {
        let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
        spans.push(Span::styled(
            " ".repeat(WIDTH_HINT.with(|w| w.get()).saturating_sub(used)),
            Style::default().bg(CODE_BG()),
        ));
    };
    let plain = || -> Vec<Line<'static>> {
        block
            .iter()
            .map(|l| {
                let mut spans = vec![Span::styled(l.clone(), Style::default().bg(CODE_BG()))];
                pad_to(l, &mut spans);
                Line::from(spans)
            })
            .collect()
    };
    if lang.is_empty() {
        return plain();
    }
    // diff blocks: +/- lines in the theme's green/red, no grammar needed
    if lang == "diff" {
        return block
            .iter()
            .map(|l| {
                let fg = match l.chars().next() {
                    Some('+') => Some(slot(2, (110, 190, 110), (40, 130, 60))),
                    Some('-') => Some(slot(1, (210, 120, 120), (180, 60, 60))),
                    _ => None,
                };
                let mut style = Style::default().bg(CODE_BG());
                if let Some(fg) = fg {
                    style = style.fg(fg);
                }
                let mut spans = vec![Span::styled(l.clone(), style)];
                pad_to(l, &mut spans);
                Line::from(spans)
            })
            .collect();
    }
    let Some(config) = cached_config_for_lang(lang) else {
        return plain();
    };
    let source = block.join(
        "
",
    );
    let spans = highlight_source(&config, &source);
    let mut lines = Vec::with_capacity(block.len());
    let mut line_start = 0usize; // byte offset of the current line
    for text in block {
        let line_end = line_start + text.len();
        // background-only base: plain tokens keep the terminal's
        // default foreground, exactly as the editor renders them
        let mut styles = vec![Style::default().bg(CODE_BG()); text.chars().count()];
        for span in spans.iter().filter(|s| s.start < line_end && s.end > line_start) {
            let s = span.start.saturating_sub(line_start);
            let e = (span.end - line_start).min(text.len());
            let s_chars = text.get(..s).map(|t| t.chars().count()).unwrap_or(0);
            let e_chars = text.get(..e).map(|t| t.chars().count()).unwrap_or(s_chars);
            let style = style_for(span.highlight).bg(CODE_BG());
            let upto = e_chars.min(styles.len());
            for slot in styles.iter_mut().take(upto).skip(s_chars) {
                if style.fg.is_some() {
                    *slot = style;
                }
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
            segments.push(Span::raw(""));
        }
        pad_to(text, &mut segments);
        lines.push(Line::from(segments));
        line_start = line_end + 1; // + newline
    }
    lines
}

/// Column alignment from a table's delimiter row.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Align {
    Left,
    Center,
    Right,
}

/// Split a table row into trimmed cells, dropping the outer `|` fence:
/// `| a | b |` and `a | b` both yield `["a", "b"]`.
fn split_row(line: &str) -> Vec<String> {
    let t = line.trim();
    let t = t.strip_prefix('|').unwrap_or(t);
    let t = t.strip_suffix('|').unwrap_or(t);
    t.split('|').map(|c| c.trim().to_string()).collect()
}

/// Parse a `|---|:--:|---:|` delimiter row into per-column alignments, or
/// None when the line isn't a valid delimiter (each cell is dashes with
/// optional leading/trailing `:`).
fn parse_delimiter(line: &str) -> Option<Vec<Align>> {
    if !line.contains('-') {
        return None;
    }
    let cells = split_row(line);
    if cells.is_empty() {
        return None;
    }
    let mut aligns = Vec::with_capacity(cells.len());
    for cell in &cells {
        let c = cell.trim();
        if c.is_empty() || !c.contains('-') || !c.chars().all(|ch| ch == '-' || ch == ':') {
            return None;
        }
        aligns.push(match (c.starts_with(':'), c.ends_with(':')) {
            (true, true) => Align::Center,
            (false, true) => Align::Right,
            _ => Align::Left,
        });
    }
    Some(aligns)
}

/// Pad (or truncate with `…`) a cell to `w` columns, honoring alignment.
fn pad_cell(text: &str, w: usize, align: Align) -> String {
    let len = text.chars().count();
    let shown: String = if len > w {
        if w == 0 {
            String::new()
        } else {
            let mut s: String = text.chars().take(w - 1).collect();
            s.push('…');
            s
        }
    } else {
        text.to_string()
    };
    let space = w.saturating_sub(shown.chars().count());
    match align {
        Align::Left => format!("{shown}{}", " ".repeat(space)),
        Align::Right => format!("{}{shown}", " ".repeat(space)),
        Align::Center => {
            let l = space / 2;
            format!("{}{shown}{}", " ".repeat(l), " ".repeat(space - l))
        }
    }
}

/// Render a GFM table (header row + body `rows`, minus the delimiter) as a
/// box-drawn table. Columns size to their content, capped so the whole table
/// fits `width`; overflowing cells truncate with `…`. Box-drawing glyphs when
/// the terminal has them, ASCII `+-|` otherwise.
fn table_lines(rows: &[&str], aligns: &[Align], width: usize) -> Vec<Line<'static>> {
    let ncols = aligns.len().max(1);
    let grid: Vec<Vec<String>> = rows
        .iter()
        .map(|r| {
            let mut cells = split_row(r);
            cells.resize(ncols, String::new());
            cells
        })
        .collect();
    if grid.is_empty() {
        return Vec::new();
    }

    // natural column widths, then shrink the widest until the table fits
    let mut col_w = vec![1usize; ncols];
    for row in &grid {
        for (j, cell) in row.iter().take(ncols).enumerate() {
            col_w[j] = col_w[j].max(cell.chars().count());
        }
    }
    let chrome = (ncols + 1) + 2 * ncols; // separators + one space of padding each side
    let budget = width.saturating_sub(chrome).max(ncols);
    while col_w.iter().sum::<usize>() > budget {
        let widest = col_w.iter().enumerate().max_by_key(|(_, w)| **w).map(|(j, _)| j).unwrap();
        if col_w[widest] <= 1 {
            break;
        }
        col_w[widest] -= 1;
    }

    let fancy = crate::color::fancy_glyphs();
    let g = |a: char, b: char| if fancy { a } else { b };
    let (h, v) = (g('─', '-'), g('│', '|'));
    let border = Style::default().fg(RULE_FG());
    let rule = |left: char, mid: char, right: char| {
        let mut s = String::new();
        s.push(left);
        for (j, w) in col_w.iter().enumerate() {
            s.push_str(&h.to_string().repeat(w + 2));
            s.push(if j + 1 < col_w.len() { mid } else { right });
        }
        Line::from(Span::styled(s, border))
    };
    let content = |cells: &[String], header: bool| {
        let text = if header {
            Style::default().fg(HEADING_FG()).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        let mut spans = vec![Span::styled(v.to_string(), border)];
        for (j, cell) in cells.iter().take(ncols).enumerate() {
            spans.push(Span::styled(format!(" {} ", pad_cell(cell, col_w[j], aligns[j])), text));
            spans.push(Span::styled(v.to_string(), border));
        }
        Line::from(spans)
    };

    let mut out = vec![
        rule(g('┌', '+'), g('┬', '+'), g('┐', '+')),
        content(&grid[0], true),
        rule(g('├', '+'), g('┼', '+'), g('┤', '+')),
    ];
    for row in &grid[1..] {
        out.push(content(row, false));
    }
    out.push(rule(g('└', '+'), g('┴', '+'), g('┘', '+')));
    out
}

/// A thematic break: 3+ of the same char among -, *, _ (spaces allowed).
fn is_rule(line: &str) -> bool {
    let chars: Vec<char> = line.chars().filter(|c| !c.is_whitespace()).collect();
    chars.len() >= 3 && matches!(chars[0], '-' | '*' | '_') && chars.iter().all(|c| *c == chars[0])
}

fn heading_text(line: &str) -> Option<&str> {
    let hashes = line.chars().take_while(|c| *c == '#').count();
    if (1..=6).contains(&hashes) {
        line.strip_prefix(&"#".repeat(hashes)).map(|r| r.trim_start()).filter(|r| !r.is_empty())
    } else {
        None
    }
}

fn restyle(spans: Vec<Span<'static>>, base: Style) -> Vec<Span<'static>> {
    spans
        .into_iter()
        .map(|s| {
            let style = base.patch(s.style);
            Span::styled(s.content, style)
        })
        .collect()
}

/// If a bare URL starts at `chars[i]`, the char index just past it — else
/// None. Recognizes `http://`, `https://`, and `www.`; consumes to the next
/// space/delimiter, then trims trailing sentence punctuation and an unbalanced
/// closing paren (so `(see https://x.io/a).` links just the URL).
fn autolink_end(chars: &[char], i: usize) -> Option<usize> {
    let has = |p: &str| p.chars().enumerate().all(|(k, c)| chars.get(i + k) == Some(&c));
    let scheme = if has("https://") {
        8
    } else if has("http://") {
        7
    } else if has("www.") {
        4
    } else {
        return None;
    };
    let mut end = i;
    while end < chars.len() {
        let c = chars[end];
        if c.is_whitespace() || matches!(c, '<' | '>' | '"' | '`' | '|' | '\\') {
            break;
        }
        end += 1;
    }
    // need at least one char past the scheme to be a real link
    if end <= i + scheme {
        return None;
    }
    loop {
        match chars.get(end - 1) {
            Some('.' | ',' | ';' | ':' | '!' | '?') => end -= 1,
            Some(')') => {
                let opens = chars[i..end].iter().filter(|&&c| c == '(').count();
                let closes = chars[i..end].iter().filter(|&&c| c == ')').count();
                if closes > opens {
                    end -= 1;
                } else {
                    break;
                }
            }
            _ => break,
        }
    }
    (end > i + scheme).then_some(end)
}

/// Inline markdown: `code`, **bold**, *italic*, ~~strikethrough~~, [text](url),
/// and bare URLs.
#[allow(dead_code)]
fn inline(text: &str) -> Vec<Span<'static>> {
    inline_links(text, 0, 0, &mut Vec::new())
}

/// Inline renderer that also records hyperlink positions. `offset` is the
/// char column where this text starts on its rendered line.
fn inline_links(
    text: &str,
    offset: usize,
    line_idx: usize,
    links: &mut Vec<MdLink>,
) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let mut buf = String::new();
    let mut bold = false;
    let mut italic = false;
    let mut strike = false;
    let mut i = 0;

    let flush = |buf: &mut String,
                 spans: &mut Vec<Span<'static>>,
                 bold: bool,
                 italic: bool,
                 strike: bool| {
        if buf.is_empty() {
            return;
        }
        let mut style = Style::default();
        if bold {
            style = style.add_modifier(Modifier::BOLD);
        }
        if italic {
            style = style.add_modifier(Modifier::ITALIC);
        }
        if strike {
            style = style.add_modifier(Modifier::CROSSED_OUT);
        }
        spans.push(Span::styled(std::mem::take(buf), style));
    };

    while i < chars.len() {
        // `code`
        if chars[i] == '`'
            && let Some(end) = chars[i + 1..].iter().position(|c| *c == '`')
        {
            flush(&mut buf, &mut spans, bold, italic, strike);
            let code: String = chars[i + 1..i + 1 + end].iter().collect();
            spans.extend(inline_code_spans(&code));
            i += end + 2;
            continue;
        }
        // [text](url) — show the text, drop the url
        if chars[i] == '['
            && let Some(close) = chars[i + 1..].iter().position(|c| *c == ']')
        {
            let after = i + 1 + close + 1;
            if chars.get(after) == Some(&'(')
                && let Some(paren) = chars[after + 1..].iter().position(|c| *c == ')')
            {
                flush(&mut buf, &mut spans, bold, italic, strike);
                let label: String = chars[i + 1..i + 1 + close].iter().collect();
                let url: String = chars[after + 1..after + 1 + paren].iter().collect();
                let col: usize =
                    offset + spans.iter().map(|s| s.content.chars().count()).sum::<usize>();
                links.push(MdLink { line: line_idx, col, text: label.clone(), url });
                spans.push(Span::styled(
                    label,
                    Style::default().fg(LINK_FG()).add_modifier(Modifier::UNDERLINED),
                ));
                i = after + 1 + paren + 1;
                continue;
            }
        }
        // ~~ strikethrough toggle
        if chars[i] == '~' && chars.get(i + 1) == Some(&'~') {
            flush(&mut buf, &mut spans, bold, italic, strike);
            strike = !strike;
            i += 2;
            continue;
        }
        // ** bold toggle
        if chars[i] == '*' && chars.get(i + 1) == Some(&'*') {
            flush(&mut buf, &mut spans, bold, italic, strike);
            bold = !bold;
            i += 2;
            continue;
        }
        // * italic toggle
        if chars[i] == '*' {
            flush(&mut buf, &mut spans, bold, italic, strike);
            italic = !italic;
            i += 1;
            continue;
        }
        // bare URL autolink (http(s):// or www.), at a word boundary
        if (i == 0 || !chars[i - 1].is_alphanumeric())
            && let Some(end) = autolink_end(&chars, i)
        {
            flush(&mut buf, &mut spans, bold, italic, strike);
            let label: String = chars[i..end].iter().collect();
            let url =
                if label.starts_with("www.") { format!("https://{label}") } else { label.clone() };
            let col: usize =
                offset + spans.iter().map(|s| s.content.chars().count()).sum::<usize>();
            links.push(MdLink { line: line_idx, col, text: label.clone(), url });
            spans.push(Span::styled(
                label,
                Style::default().fg(LINK_FG()).add_modifier(Modifier::UNDERLINED),
            ));
            i = end;
            continue;
        }
        buf.push(chars[i]);
        i += 1;
    }
    flush(&mut buf, &mut spans, bold, italic, strike);
    if spans.is_empty() {
        spans.push(Span::raw(""));
    }
    spans
}

#[cfg(test)]
mod tests {
    #[test]
    fn blank_rows_around_rules_collapse() {
        let lines = super::render("above\n\n---\n\nbelow", 20);
        let texts: Vec<String> =
            lines.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect()).collect();
        assert_eq!(texts.len(), 3, "text, rule, text — no blank rows: {texts:?}");
        assert!(texts[0].contains("above"));
        assert!(texts[1].chars().all(|c| c == '─'));
        assert!(texts[2].contains("below"));
        // blank rows between paragraphs (no rule involved) survive
        let lines = super::render("one\n\ntwo", 20);
        assert_eq!(lines.len(), 3, "paragraph gap kept");
    }

    #[test]
    fn inline_code_highlights_with_hinted_language() {
        // with a rust hint, `let` inside inline code gets the keyword color
        let (lines, _) = super::render_with_links_lang("see `let x = 1;` here", 60, "rust");
        let spans = &lines[0].spans;
        let let_span = spans
            .iter()
            .find(|sp| sp.content.as_ref() == "let")
            .expect("`let` split into its own span");
        assert!(let_span.style.fg.is_some(), "keyword colored: {:?}", let_span.style);
        assert_eq!(let_span.style.bg, Some(CODE_BG()), "still on the code panel");
        // without a hint, inline code stays a single plain span
        let (lines, _) = super::render_with_links("see `let x = 1;` here", 60);
        let spans = &lines[0].spans;
        assert!(
            spans.iter().any(|sp| sp.content.as_ref() == "let x = 1;"),
            "unhinted inline code is one plain span"
        );
    }

    #[test]
    fn long_paragraphs_word_wrap_and_links_survive() {
        let text =
            format!("{} [docs](https://example.com) {}", "word ".repeat(20), "tail ".repeat(20));
        let (lines, links) = super::render_with_links(&text, 40);
        assert!(lines.len() > 1, "paragraph wrapped into multiple rows");
        for line in &lines {
            assert!(line.width() <= 40, "row within width: {}", line.width());
        }
        // no mid-word breaks: full rows end exactly at a word boundary
        for line in &lines[..lines.len() - 1] {
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            if text.chars().count() == 40 {
                assert!(
                    text.ends_with(' ') || text.ends_with(char::is_alphanumeric),
                    "row is a clean cut: {text:?}"
                );
            }
        }
        // the link survived wrapping and points at its label on the row
        let link = links.iter().find(|l| l.url == "https://example.com").expect("link kept");
        let row: String = lines[link.line].spans.iter().map(|s| s.content.as_ref()).collect();
        let at: String = row.chars().skip(link.col).take(4).collect();
        assert_eq!(at, "docs", "link col points at its label");
    }

    use super::*;

    fn text_of(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn horizontal_rules_become_lines() {
        for rule in ["---", "___", "***", "- - -", "_____"] {
            let lines = render(rule, 10);
            assert_eq!(text_of(&lines[0]), "─".repeat(10), "rule {rule:?}");
        }
        // not rules: rendered as text (word-wrapped at width 10)
        let lines = render("-- too short", 10);
        let joined: String = lines.iter().map(text_of).collect::<Vec<_>>().join(" ");
        assert!(joined.contains("too short"), "{joined:?}");
    }

    #[test]
    fn headings_are_bold_without_hashes() {
        let lines = render("## Section title", 40);
        assert_eq!(text_of(&lines[0]), "Section title");
        assert!(lines[0].spans[0].style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn code_fences_hide_markers_and_syntax_highlight() {
        let lines = render("```rust\nfn main() {}\n```\nafter", 40);
        assert_eq!(lines.len(), 2);
        assert_eq!(text_of(&lines[0]).trim_end(), "fn main() {}");
        // "fn" is a rust keyword → keyword purple, not the plain code tint
        let kw = lines[0].spans.iter().find(|s| s.content.contains("fn")).unwrap();
        assert_eq!(kw.style.fg, Some(Color::Rgb(183, 148, 244)));
        assert_eq!(kw.style.bg, Some(CODE_BG()), "code block sits on its own panel");
        assert_eq!(text_of(&lines[1]), "after");
    }

    #[test]
    fn unknown_fence_language_stays_plain() {
        let lines = render("```klingon\nqapla' code\n```", 40);
        assert_eq!(lines[0].spans[0].style.fg, None, "code keeps the default fg");
        assert_eq!(lines[0].spans[0].style.bg, Some(CODE_BG()), "panel background set");
    }

    #[test]
    fn unterminated_fence_still_renders() {
        let lines = render("```rust\nlet x = 1;", 40);
        assert_eq!(text_of(&lines[0]).trim_end(), "let x = 1;");
    }

    #[test]
    fn inline_styles() {
        let lines = render("a **bold** and `code` and *it* end", 60);
        let line = &lines[0];
        let bold = line.spans.iter().find(|s| s.content == "bold").unwrap();
        assert!(bold.style.add_modifier.contains(Modifier::BOLD));
        let code = line.spans.iter().find(|s| s.content == "code").unwrap();
        assert_eq!(code.style.fg, None, "inline code keeps the default fg");
        assert_eq!(code.style.bg, Some(CODE_BG()), "inline code gets the panel bg");
        assert_eq!(code.style.bg, Some(CODE_BG()), "inline code is a chip");
        let italic = line.spans.iter().find(|s| s.content == "it").unwrap();
        assert!(italic.style.add_modifier.contains(Modifier::ITALIC));
    }

    #[test]
    fn strikethrough_is_crossed_out() {
        let lines = render("keep ~~drop this~~ and **~~both~~** done", 60);
        let line = &lines[0];
        let struck = line.spans.iter().find(|s| s.content == "drop this").unwrap();
        assert!(struck.style.add_modifier.contains(Modifier::CROSSED_OUT), "struck: {struck:?}");
        // the ~~ markers are gone, the text stays
        let text = text_of(line);
        assert!(text.contains("drop this") && !text.contains('~'), "markers stripped: {text:?}");
        // strikethrough composes with bold
        let both = line.spans.iter().find(|s| s.content == "both").unwrap();
        assert!(
            both.style.add_modifier.contains(Modifier::CROSSED_OUT)
                && both.style.add_modifier.contains(Modifier::BOLD),
            "bold + strike compose: {both:?}"
        );
    }

    #[test]
    fn links_show_label_only() {
        let lines = render("see [the docs](https://example.com) here", 60);
        let text = text_of(&lines[0]);
        assert!(text.contains("the docs"));
        assert!(!text.contains("example.com"));
        let link = lines[0].spans.iter().find(|s| s.content == "the docs").unwrap();
        assert!(link.style.add_modifier.contains(Modifier::UNDERLINED));
    }

    #[test]
    fn bare_urls_autolink() {
        // a plain URL becomes a clickable link (label == url)
        let (lines, links) = render_with_links("visit https://example.com now", 60);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].text, "https://example.com");
        assert_eq!(links[0].url, "https://example.com");
        assert_eq!(links[0].col, 6); // after "visit "
        let underlined =
            lines[0].spans.iter().find(|s| s.content == "https://example.com").unwrap();
        assert!(underlined.style.add_modifier.contains(Modifier::UNDERLINED));

        // trailing sentence punctuation and an unbalanced paren stay out
        let (_l, links) = render_with_links("see (https://ex.io/a).", 60);
        assert_eq!(links[0].url, "https://ex.io/a", "trailing ) and . trimmed");

        // www. gets an https scheme while the label keeps the bare form
        let (_l, links) = render_with_links("at www.example.com today", 60);
        assert_eq!(links[0].text, "www.example.com");
        assert_eq!(links[0].url, "https://www.example.com");

        // glued to a preceding word: not linked. markdown links still win
        let (_l, links) = render_with_links("gluedhttps://no.io but [x](https://y.io)", 60);
        assert_eq!(links.len(), 1, "glued url not linked; md link kept: {links:?}");
        assert_eq!(links[0].url, "https://y.io");
    }

    #[test]
    fn links_report_positions_and_urls() {
        let (lines, links) = render_with_links(
            "intro\nsee [docs](https://example.com) now\n- has [b](https://b.io)",
            60,
        );
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].line, 1);
        assert_eq!(links[0].col, 4); // after "see "
        assert_eq!(links[0].text, "docs");
        assert_eq!(links[0].url, "https://example.com");
        assert_eq!(links[1].line, 2);
        assert_eq!(links[1].col, 6); // after "• has "
        // the rendered text at that position is the label
        let text: String = lines[1].spans.iter().map(|s| s.content.as_ref()).collect::<String>();
        assert_eq!(&text[4..8], "docs");
    }

    #[test]
    fn bullets_and_quotes() {
        let lines = render("- item one\n> quoted words", 40);
        assert!(text_of(&lines[0]).starts_with("• "));
        assert!(text_of(&lines[1]).contains("quoted words"));
    }

    #[test]
    fn task_list_markers_become_checkboxes() {
        let lines = render("- [ ] todo item\n- [x] done item\n- plain bullet", 40);
        let done = text_of(&lines[0]);
        let todo = text_of(&lines[1]);
        // the literal [ ]/[x] markers are consumed, not shown
        assert!(!done.contains("[ ]") && !todo.contains("[x]"));
        assert!(done.starts_with("▢ ") || done.starts_with("[ ] "));
        assert!(todo.starts_with("✔ ") || todo.starts_with("[x] "));
        assert!(done.ends_with("todo item") && todo.ends_with("done item"));
        // a plain bullet still renders as a bullet, not a checkbox
        assert!(text_of(&lines[2]).starts_with("• "));
    }

    #[test]
    fn unclosed_backtick_is_literal() {
        let lines = render("has ` one tick", 40);
        assert!(text_of(&lines[0]).contains("` one tick"));
    }

    #[test]
    fn tables_render_with_box_borders() {
        unsafe { std::env::set_var("VIBIN_FANCY", "1") };
        let md = "| Name | Age |\n| --- | --- |\n| Ada | 36 |\n| Bo | 7 |";
        let lines = render(md, 40);
        let texts: Vec<String> = lines.iter().map(text_of).collect();
        // top border, header, separator, two body rows, bottom border
        assert_eq!(texts.len(), 6, "table lines: {texts:?}");
        assert!(texts[0].starts_with('┌') && texts[0].ends_with('┐'), "top: {:?}", texts[0]);
        assert!(texts[0].contains('┬'), "top has a column joint");
        assert!(texts[1].contains('│') && texts[1].contains("Name") && texts[1].contains("Age"));
        assert!(texts[2].starts_with('├') && texts[2].contains('┼') && texts[2].ends_with('┤'));
        assert!(texts[3].contains("Ada") && texts[3].contains("36"));
        assert!(texts[5].starts_with('└') && texts[5].contains('┴') && texts[5].ends_with('┘'));
        // the raw pipe/dash markup is gone; header is bold
        assert!(!texts.iter().any(|t| t.contains("---")), "delimiter row not shown");
        let header = &lines[1];
        assert!(
            header.spans.iter().any(
                |s| s.content.contains("Name") && s.style.add_modifier.contains(Modifier::BOLD)
            ),
            "header cells are bold"
        );
        // every row is the same width (aligned columns)
        let w = lines[0].width();
        assert!(lines.iter().all(|l| l.width() == w), "uniform width {w}");
    }

    #[test]
    fn wide_table_fits_the_width() {
        unsafe { std::env::set_var("VIBIN_FANCY", "1") };
        let md = "| A | B |\n|---|---|\n| a very long cell that overflows | short |";
        let lines = render(md, 24);
        for l in &lines {
            assert!(l.width() <= 24, "row within width: {} ({:?})", l.width(), text_of(l));
        }
        // the overflowing cell was truncated with an ellipsis
        assert!(lines.iter().any(|l| text_of(l).contains('…')), "long cell truncated");
    }

    #[test]
    fn table_alignment_from_delimiter() {
        // right-aligned column pads on the left
        let cell = super::pad_cell("7", 4, super::Align::Right);
        assert_eq!(cell, "   7");
        let cell = super::pad_cell("hi", 6, super::Align::Center);
        assert_eq!(cell, "  hi  ");
        let cell = super::pad_cell("hi", 6, super::Align::Left);
        assert_eq!(cell, "hi    ");
    }

    #[test]
    fn pipe_line_without_a_delimiter_is_not_a_table() {
        // a lone piped line renders as text, not a bordered table
        let lines = render("use a | b pipe here", 40);
        assert!(text_of(&lines[0]).contains("a | b pipe"), "plain text: {:?}", text_of(&lines[0]));
        assert!(!lines.iter().any(|l| text_of(l).contains('┌')), "no table drawn");
    }
}
