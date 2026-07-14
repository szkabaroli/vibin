//! Terminal color-capability handling. Terminals without 24-bit color
//! support (notably Apple Terminal.app) garble RGB escape sequences, so on
//! those we quantize every RGB color in the finished frame to the nearest
//! xterm-256 palette entry — one pass, no changes to drawing code.

use ratatui::buffer::Buffer;
use ratatui::style::Color;

/// Does the hosting terminal understand 24-bit color?
/// `VIBIN_TRUECOLOR=1|0` overrides detection.
pub fn supports_truecolor() -> bool {
    match std::env::var("VIBIN_TRUECOLOR").as_deref() {
        Ok("1") => return true,
        Ok("0") => return false,
        _ => {}
    }
    // Apple Terminal sets TERM_PROGRAM but never COLORTERM
    std::env::var("COLORTERM")
        .map(|v| v.contains("truecolor") || v.contains("24bit"))
        .unwrap_or(false)
}

/// Can we use the fancy glyph set (eighth-block hairlines, slant caps)?
/// Terminals without truecolor (Terminal.app) also tend to lack font
/// coverage for these, rendering them at wrong widths from fallback fonts.
/// `VIBIN_FANCY=1|0` overrides.
pub fn fancy_glyphs() -> bool {
    static FANCY: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FANCY.get_or_init(|| match std::env::var("VIBIN_FANCY").as_deref() {
        Ok("1") => true,
        Ok("0") => false,
        _ => supports_truecolor(),
    })
}

/// Replace every RGB color in the buffer with its nearest xterm-256 index.
pub fn quantize_buffer(buf: &mut Buffer) {
    let area = buf.area;
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            let cell = &mut buf[(x, y)];
            if let Color::Rgb(r, g, b) = cell.fg {
                cell.fg = Color::Indexed(nearest_indexed(r, g, b));
            }
            if let Color::Rgb(r, g, b) = cell.bg {
                cell.bg = Color::Indexed(nearest_indexed(r, g, b));
            }
            #[allow(clippy::single_match)]
            match cell.underline_color {
                Color::Rgb(r, g, b) => {
                    cell.underline_color = Color::Indexed(nearest_indexed(r, g, b));
                }
                _ => {}
            }
        }
    }
}

/// Nearest xterm-256 palette index for an RGB color: the 6x6x6 color cube
/// (16..=231) and the grayscale ramp (232..=255).
pub fn nearest_indexed(r: u8, g: u8, b: u8) -> u8 {
    // candidate from the color cube
    let level = |c: u8| -> u8 {
        // cube levels: 0, 95, 135, 175, 215, 255
        if c < 48 {
            0
        } else if c < 115 {
            1
        } else {
            ((c as u16 - 35) / 40) as u8
        }
    };
    let cube_value = |i: u8| -> u8 { if i == 0 { 0 } else { 55 + i * 40 } };
    let (cr, cg, cb) = (level(r), level(g), level(b));
    let cube_idx = 16 + 36 * cr + 6 * cg + cb;
    let cube_rgb = (cube_value(cr), cube_value(cg), cube_value(cb));

    // candidate from the grayscale ramp (232..=255 → 8, 18, …, 238)
    let gray_avg = (r as u16 + g as u16 + b as u16) / 3;
    let gray_step = ((gray_avg.saturating_sub(3)) / 10).min(23) as u8;
    let gray_idx = 232 + gray_step;
    let gray_value = 8 + gray_step * 10;

    let dist = |c: (u8, u8, u8)| -> u32 {
        let d = |a: u8, b: u8| (a as i32 - b as i32).pow(2) as u32;
        d(c.0, r) + d(c.1, g) + d(c.2, b)
    };
    if dist(cube_rgb) <= dist((gray_value, gray_value, gray_value)) { cube_idx } else { gray_idx }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;
    use ratatui::style::Style;

    #[test]
    fn primaries_map_to_expected_indices() {
        assert_eq!(nearest_indexed(0, 0, 0), 16); // cube black
        assert_eq!(nearest_indexed(255, 255, 255), 231); // cube white
        assert_eq!(nearest_indexed(255, 0, 0), 196); // pure red
        assert_eq!(nearest_indexed(0, 255, 0), 46); // pure green
        assert_eq!(nearest_indexed(0, 0, 255), 21); // pure blue
    }

    #[test]
    fn grays_use_the_gray_ramp() {
        let idx = nearest_indexed(128, 128, 128);
        assert!((232..=255).contains(&idx), "mid gray → ramp, got {idx}");
        // slightly tinted colors stay in the cube
        let idx = nearest_indexed(150, 100, 100);
        assert!((16..=231).contains(&idx));
    }

    #[test]
    fn quantize_rewrites_all_rgb_colors() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 4, 2));
        buf.set_string(
            0,
            0,
            "test",
            Style::default().fg(Color::Rgb(255, 110, 140)).bg(Color::Rgb(40, 42, 48)),
        );
        quantize_buffer(&mut buf);
        for x in 0..4 {
            let cell = &buf[(x, 0)];
            assert!(matches!(cell.fg, Color::Indexed(_)), "fg quantized");
            assert!(matches!(cell.bg, Color::Indexed(_)), "bg quantized");
        }
        // non-RGB colors are left alone
        let cell = &buf[(0, 1)];
        assert_eq!(cell.fg, Color::Reset);
    }
}

use std::sync::{OnceLock, RwLock};

// Live theme state: seeded at startup by [`detect_terminal_bg`], updated
// mid-session by color-scheme events (crossterm mode 2031) in main's loop.
static TERMINAL_BG: RwLock<Option<(u8, u8, u8)>> = RwLock::new(None);
static SCHEME_LIGHT: RwLock<Option<bool>> = RwLock::new(None);
/// The terminal's own color palette (OSC 4), queried at startup and after
/// scheme changes. Slots 0-15 are the ANSI colors; higher slots (like
/// 238, the mid grey used for line numbers) are the 256-color extension,
/// which themes may override.
static ANSI16: RwLock<[Option<(u8, u8, u8)>; 256]> = RwLock::new([None; 256]);
/// The terminal's selection/highlight background (OSC 17), if reported.
static SELECTION_BG: RwLock<Option<(u8, u8, u8)>> = RwLock::new(None);
/// The terminal's default foreground (OSC 10), if reported.
static TERMINAL_FG: RwLock<Option<(u8, u8, u8)>> = RwLock::new(None);

/// Whether the UI should use the light palette. Precedence:
/// VIBIN_THEME=light/dark, then live scheme-change events from the
/// terminal, then the OSC 11 background's luminance; dark when unknown.
pub fn is_light() -> bool {
    static ENV: OnceLock<Option<bool>> = OnceLock::new();
    let forced = ENV.get_or_init(|| match std::env::var("VIBIN_THEME").as_deref() {
        Ok("light") => Some(true),
        Ok("dark") => Some(false),
        _ => None,
    });
    if let Some(forced) = *forced {
        return forced;
    }
    if let Some(light) = *SCHEME_LIGHT.read().unwrap() {
        return light;
    }
    terminal_bg()
        .is_some_and(|(r, g, b)| (r as u32 * 299 + g as u32 * 587 + b as u32 * 114) / 1000 > 128)
}

/// The terminal's default background color, if it answered OSC 11.
/// [`detect_terminal_bg`] must have run first (it is called at startup).
pub fn terminal_bg() -> Option<(u8, u8, u8)> {
    *TERMINAL_BG.read().unwrap()
}

/// Record a new terminal background (from a mid-session re-query).
pub fn set_terminal_bg(rgb: (u8, u8, u8)) {
    *TERMINAL_BG.write().unwrap() = Some(rgb);
}

/// Record the scheme the terminal reported (a mode 2031 event).
pub fn set_scheme_light(light: bool) {
    *SCHEME_LIGHT.write().unwrap() = Some(light);
}

/// The terminal's color for ANSI palette slot `i`, if it answered OSC 4.
pub fn ansi16(i: usize) -> Option<(u8, u8, u8)> {
    ANSI16.read().unwrap().get(i).copied().flatten()
}

/// Record a palette color (startup query or mid-session re-query).
pub fn set_ansi16(i: usize, rgb: (u8, u8, u8)) {
    if let Some(slot) = ANSI16.write().unwrap().get_mut(i) {
        *slot = Some(rgb);
    }
}

/// The terminal's native selection background, if it answered OSC 17.
pub fn selection_bg() -> Option<(u8, u8, u8)> {
    *SELECTION_BG.read().unwrap()
}

pub fn set_selection_bg(rgb: (u8, u8, u8)) {
    *SELECTION_BG.write().unwrap() = Some(rgb);
}

/// The terminal's default foreground, if it answered OSC 10.
pub fn terminal_fg() -> Option<(u8, u8, u8)> {
    *TERMINAL_FG.read().unwrap()
}

pub fn set_terminal_fg(rgb: (u8, u8, u8)) {
    *TERMINAL_FG.write().unwrap() = Some(rgb);
}

/// Theme-native grey: the terminal foreground washed into its background
/// at `weight`/256 — how the theme itself would mix a dim surface or dim
/// text. None until both OSC 10 and 11 answered.
pub fn wash(weight: u32) -> Option<(u8, u8, u8)> {
    let (f, b) = (terminal_fg()?, terminal_bg()?);
    let mix = |a: u8, c: u8| ((a as u32 * weight + c as u32 * (256 - weight)) / 256) as u8;
    Some((mix(f.0, b.0), mix(f.1, b.1), mix(f.2, b.2)))
}

/// Query the terminal background via OSC 11. Must run while raw mode is
/// active and BEFORE the crossterm event loop starts reading stdin — the
/// reply arrives on stdin and we consume it here. Terminals that never
/// answer just hit the short timeout. (Mid-session re-queries instead go
/// through crossterm's query batching, which owns stdin by then.)
pub fn detect_terminal_bg() {
    let Some(reply) = raw_color_query() else {
        return;
    };
    if let Some(rgb) = parse_osc11(&reply) {
        set_terminal_bg(rgb);
    }
    for i in (0..16).chain([238]) {
        if let Some(rgb) = parse_osc4(&reply, i) {
            set_ansi16(i, rgb);
        }
    }
    if let Some(rgb) = parse_osc_dynamic(&reply, 17) {
        set_selection_bg(rgb);
    }
    if let Some(rgb) = parse_osc_dynamic(&reply, 10) {
        set_terminal_fg(rgb);
    }
}

#[cfg(not(unix))]
fn raw_color_query() -> Option<String> {
    None
}

/// One combined write of the OSC 11 + OSC 4 queries, one raw read of the
/// replies (5 expected). Startup-only; see detect_terminal_bg.
#[cfg(unix)]
fn raw_color_query() -> Option<String> {
    let mut query = b"\x1b]10;?\x07\x1b]11;?\x07\x1b]17;?\x07".to_vec();
    for i in (0..16).chain([238]) {
        query.extend_from_slice(format!("\x1b]4;{i};?\x07").as_bytes());
    }
    raw_query(&query)
}

#[cfg(not(unix))]
pub(crate) fn raw_query(_query: &[u8]) -> Option<String> {
    None
}

/// One raw write of `query` + a DA1 probe, one bounded read of the replies.
/// Startup-only: must run while raw mode is active and BEFORE the crossterm
/// event loop starts reading stdin (the replies arrive there).
#[cfg(unix)]
pub(crate) fn raw_query(query: &[u8]) -> Option<String> {
    use std::io::Write;
    use std::os::fd::AsRawFd;
    use std::time::{Duration, Instant};
    let mut out = std::io::stdout();
    let mut query = query.to_vec();
    // DA1 sentinel: every terminal answers it, and answers in order — when
    // its reply (CSI ? ... c) arrives, all replies we will ever get are
    // already in the buffer. No fixed reply count, no wasted timeout.
    query.extend_from_slice(b"\x1b[c");
    out.write_all(&query).ok()?;
    out.flush().ok()?;
    let fd = std::io::stdin().as_raw_fd();
    let mut buf: Vec<u8> = Vec::new();
    let deadline = Instant::now() + Duration::from_millis(150);
    // On timeout, keep whatever replies did arrive — a terminal that skips
    // one query (e.g. OSC 17) must not cost us the answers it DID give.
    loop {
        let remain = deadline.saturating_duration_since(Instant::now());
        if remain.is_zero() {
            break;
        }
        let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
        let ready = unsafe { libc::poll(&mut pfd, 1, remain.as_millis() as i32) };
        if ready <= 0 {
            break;
        }
        let mut chunk = [0u8; 64];
        let n = unsafe { libc::read(fd, chunk.as_mut_ptr().cast(), chunk.len()) };
        if n <= 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n as usize]);
        // done when the DA1 sentinel reply (ESC [ ? ... c) is complete
        if let Some(pos) = buf.windows(3).position(|w| w == b"\x1b[?")
            && buf[pos..].contains(&b'c')
        {
            break;
        }
    }
    if buf.is_empty() { None } else { Some(String::from_utf8_lossy(&buf).into_owned()) }
}

/// Parse an OSC 11 reply: `]11;rgb:RRRR/GGGG/BBBB` (components are 1-4 hex
/// digits; the top 8 bits carry the color).
fn parse_osc11(reply: &str) -> Option<(u8, u8, u8)> {
    parse_osc_dynamic(reply, 11)
}

/// Parse any dynamic-color reply (OSC 10..=19): `]N;rgb:R/G/B`.
fn parse_osc_dynamic(reply: &str, n: u8) -> Option<(u8, u8, u8)> {
    let marker = format!("]{n};");
    parse_rgb_spec(reply.split(marker.as_str()).nth(1)?)
}

/// Parse an OSC 4 reply for palette slot `i`: `]4;I;rgb:RRRR/GGGG/BBBB`.
fn parse_osc4(reply: &str, i: usize) -> Option<(u8, u8, u8)> {
    let marker = format!("]4;{i};");
    parse_rgb_spec(reply.split(marker.as_str()).nth(1)?)
}

fn parse_rgb_spec(spec: &str) -> Option<(u8, u8, u8)> {
    let rgb = spec.trim_start_matches("rgba:").trim_start_matches("rgb:");
    let mut parts = rgb.split(['/', '\x07', '\x1b']).filter(|p| !p.is_empty());
    let mut comp = || -> Option<u8> {
        let field: String = parts.next()?.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
        let v = u32::from_str_radix(&field, 16).ok()?;
        Some(match field.len() {
            1 => (v * 17) as u8,
            2 => v as u8,
            3 => (v >> 4) as u8,
            4 => (v >> 8) as u8,
            _ => return None,
        })
    };
    Some((comp()?, comp()?, comp()?))
}

#[cfg(test)]
mod osc11_tests {
    use super::{parse_osc4, parse_osc11};

    #[test]
    fn parses_combined_osc11_and_osc4_replies() {
        // one buffer holding a background reply plus four palette replies,
        // as the startup query receives them
        let buf = "\x1b]11;rgb:0d0d/1111/1717\x07\
\x1b]4;1;rgb:ff00/5050/5050\x07\
\x1b]4;2;rgb:3a00/d900/6d00\x07\
\x1b]4;3;rgb:d2/99/22\x07\
\x1b]4;4;rgb:2f00/6f00/ed00\x07";
        assert_eq!(parse_osc11(buf), Some((0x0d, 0x11, 0x17)));
        assert_eq!(parse_osc4(buf, 1), Some((0xff, 0x50, 0x50)));
        assert_eq!(parse_osc4(buf, 2), Some((0x3a, 0xd9, 0x6d)));
        assert_eq!(parse_osc4(buf, 3), Some((0xd2, 0x99, 0x22)));
        assert_eq!(parse_osc4(buf, 4), Some((0x2f, 0x6f, 0xed)));
        assert_eq!(parse_osc4(buf, 5), None, "unqueried slot stays empty");
    }

    #[test]
    fn parses_common_reply_formats() {
        // 16-bit components, BEL-terminated (Ghostty, xterm)
        assert_eq!(parse_osc11("\x1b]11;rgb:1e1e/2828/3232\x07"), Some((0x1e, 0x28, 0x32)));
        // ST-terminated
        assert_eq!(parse_osc11("\x1b]11;rgb:0000/0000/0000\x1b\\"), Some((0, 0, 0)));
        // 8-bit components
        assert_eq!(parse_osc11("\x1b]11;rgb:ff/ff/ff\x07"), Some((255, 255, 255)));
        // rgba variant (some terminals)
        assert_eq!(parse_osc11("\x1b]11;rgba:1000/2000/3000\x07"), Some((0x10, 0x20, 0x30)));
        // garbage does not parse
        assert_eq!(parse_osc11("nonsense"), None);
    }
}
