//! The party parrot. The real GIF is embedded, decoded once, downscaled,
//! and converted to half-block pixel art (▀ with fg = upper pixel and
//! bg = lower pixel — one terminal cell shows two pixels, preserving the
//! square aspect since cells are ~twice as tall as wide).

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use std::sync::OnceLock;

const PARROT_GIF: &[u8] = include_bytes!("../assets/parrot.gif");

/// Rendered height in terminal rows (each row = 2 pixels) — matches the
/// wordmark's height so the two sit as one lockup.
pub const ROWS: u16 = 6;

pub fn width() -> u16 {
    frames().first().map(|f| f.width).unwrap_or(0)
}

pub struct Frame {
    pub lines: Vec<Line<'static>>,
    pub width: u16,
}

/// Decoded, downscaled animation frames. Empty if the embedded GIF is
/// somehow undecodable (the welcome screen then simply has no parrot).
pub fn frames() -> &'static [Frame] {
    // one cache per theme: outline tinting (see `themed`) bakes the
    // wash color into the spans, and it differs between light and dark
    static DARK: OnceLock<Vec<Frame>> = OnceLock::new();
    static LIGHT: OnceLock<Vec<Frame>> = OnceLock::new();
    let slot = if crate::color::is_light() { &LIGHT } else { &DARK };
    slot.get_or_init(|| decode().unwrap_or_default())
}

/// The artwork's outline strokes are near-black ink; on a terminal they
/// should read like the UI's own lines. Retint dark pixels toward the
/// theme's grey wash — soft on parchment, subtle on dark — leaving the
/// plumage untouched. Without OSC answers, the original color stands.
fn themed(c: (u8, u8, u8)) -> Color {
    let lum = (c.0 as u32 * 299 + c.1 as u32 * 587 + c.2 as u32 * 114) / 1000;
    if lum < 80
        && let Some((r, g, b)) = crate::color::wash(150)
    {
        return Color::Rgb(r, g, b);
    }
    Color::Rgb(c.0, c.1, c.2)
}

type Rgba = Option<(u8, u8, u8)>;

fn decode() -> Option<Vec<Frame>> {
    let mut options = gif::DecodeOptions::new();
    options.set_color_output(gif::ColorOutput::RGBA);
    let mut decoder = options.read_info(PARROT_GIF).ok()?;
    let (gw, gh) = (decoder.width() as usize, decoder.height() as usize);

    // composite partial frames onto a persistent canvas, honouring each
    // frame's disposal method — otherwise transparent areas ghost the
    // previous frames
    let mut canvas: Vec<Rgba> = vec![None; gw * gh];
    let mut composited: Vec<Vec<Rgba>> = Vec::new();
    while let Ok(Some(frame)) = decoder.read_next_frame() {
        let before = canvas.clone();
        let (fx, fy) = (frame.left as usize, frame.top as usize);
        let (fw, fh) = (frame.width as usize, frame.height as usize);
        for y in 0..fh {
            for x in 0..fw {
                let src = (y * fw + x) * 4;
                let px = &frame.buffer[src..src + 4];
                if px[3] >= 128 {
                    let (cx, cy) = (fx + x, fy + y);
                    if cx < gw && cy < gh {
                        canvas[cy * gw + cx] = Some((px[0], px[1], px[2]));
                    }
                }
            }
        }
        composited.push(canvas.clone());
        match frame.dispose {
            gif::DisposalMethod::Background => {
                // clear the frame's own rect back to transparent
                for y in fy..(fy + fh).min(gh) {
                    for x in fx..(fx + fw).min(gw) {
                        canvas[y * gw + x] = None;
                    }
                }
            }
            gif::DisposalMethod::Previous => canvas = before,
            _ => {}
        }
    }
    if composited.is_empty() {
        return None;
    }

    // crop to the union bounding box across all frames — the GIF canvas has
    // lots of transparent headroom that would otherwise offset the sprite —
    // while keeping the bob animation (union, not per-frame, so frames stay
    // mutually aligned)
    let (mut x0, mut y0, mut x1, mut y1) = (gw, gh, 0usize, 0usize);
    for canvas in &composited {
        for y in 0..gh {
            for x in 0..gw {
                if canvas[y * gw + x].is_some() {
                    x0 = x0.min(x);
                    y0 = y0.min(y);
                    x1 = x1.max(x + 1);
                    y1 = y1.max(y + 1);
                }
            }
        }
    }
    if x0 >= x1 || y0 >= y1 {
        return None;
    }
    let (bw, bh) = (x1 - x0, y1 - y0);
    let frames = composited
        .iter()
        .map(|canvas| {
            let cropped: Vec<Rgba> =
                (y0..y1).flat_map(|y| (x0..x1).map(move |x| canvas[y * gw + x])).collect();
            downscale(&cropped, bw, bh)
        })
        .collect();
    Some(frames)
}

/// Quadrant glyph for a 2x2 pixel mask (bit 8 = top-left, 4 = top-right,
/// 2 = bottom-left, 1 = bottom-right).
const QUADRANTS: [char; 16] =
    [' ', '▗', '▖', '▄', '▝', '▐', '▞', '▟', '▘', '▚', '▌', '▙', '▀', '▜', '▛', '█'];

/// Downscale to a pixel grid, then pack 2x2 pixel groups into quadrant
/// cells (two colors per cell). Compared to plain half-blocks this doubles
/// the horizontal resolution, and dominant-color sampling keeps the flat
/// pixel-art outlines crisp where averaging would smear them.
fn downscale(canvas: &[Rgba], gw: usize, gh: usize) -> Frame {
    let ph = ROWS as usize * 2;
    // horizontal pixels are half a cell wide, so double them to keep the
    // sprite's aspect ratio
    let pw = (gw * ph / gh.max(1)).max(1) * 2;
    let mut grid: Vec<Rgba> = (0..ph)
        .flat_map(|y| (0..pw).map(move |x| (x, y)))
        .map(|(x, y)| dominant(canvas, gw, gh, x, y, pw, ph))
        .collect();
    fill_interior_holes(&mut grid, pw, ph);
    let px = |x: usize, y: usize| grid[y * pw + x];

    let cols = pw / 2;
    let mut lines = Vec::with_capacity(ROWS as usize);
    for row in 0..ROWS as usize {
        let spans: Vec<Span> = (0..cols)
            .map(|col| {
                let quad = [
                    px(col * 2, row * 2),
                    px(col * 2 + 1, row * 2),
                    px(col * 2, row * 2 + 1),
                    px(col * 2 + 1, row * 2 + 1),
                ];
                quad_span(quad)
            })
            .collect();
        lines.push(Line::from(spans));
    }
    Frame { lines, width: cols as u16 }
}

/// Render a 2x2 pixel group as one quadrant cell. Opaque pixels split into
/// two color clusters: the first becomes the glyph's foreground mask, the
/// second the background. Transparent pixels stay unpainted.
fn quad_span(quad: [Rgba; 4]) -> Span<'static> {
    let opaque: Vec<(usize, (u8, u8, u8))> =
        quad.iter().enumerate().filter_map(|(i, p)| p.map(|c| (i, c))).collect();
    if opaque.is_empty() {
        return Span::raw(" ");
    }
    let rgb = themed;
    let bit = |i: usize| 8 >> i;
    let dist = |a: (u8, u8, u8), b: (u8, u8, u8)| {
        let d = |x: u8, y: u8| (x as i32 - y as i32).pow(2);
        d(a.0, b.0) + d(a.1, b.1) + d(a.2, b.2)
    };

    // with any transparency in the cell, paint opaque pixels in one color
    // (their average) so the terminal background shows through the rest
    if opaque.len() < 4 {
        let (mut r, mut g, mut b) = (0u32, 0u32, 0u32);
        for &(_, c) in &opaque {
            r += c.0 as u32;
            g += c.1 as u32;
            b += c.2 as u32;
        }
        let n = opaque.len() as u32;
        let mask: usize = opaque.iter().map(|&(i, _)| bit(i)).sum();
        return Span::styled(
            QUADRANTS[mask].to_string(),
            Style::default().fg(rgb(((r / n) as u8, (g / n) as u8, (b / n) as u8))),
        );
    }

    // fully opaque: split into the two most distant colors
    let mut seeds = (0, 1);
    let mut best = -1;
    for i in 0..4 {
        for j in (i + 1)..4 {
            let d = dist(quad[i].unwrap(), quad[j].unwrap());
            if d > best {
                best = d;
                seeds = (i, j);
            }
        }
    }
    if best <= 8 * 8 * 3 {
        // effectively one flat color
        let c = quad[0].unwrap();
        return Span::styled("█", Style::default().fg(rgb(c)));
    }
    let (a, b) = (quad[seeds.0].unwrap(), quad[seeds.1].unwrap());
    let mut mask = 0usize;
    let (mut fg_sum, mut fg_n) = ((0u32, 0u32, 0u32), 0u32);
    let (mut bg_sum, mut bg_n) = ((0u32, 0u32, 0u32), 0u32);
    for (i, pxl) in quad.iter().enumerate() {
        let c = pxl.unwrap();
        if dist(c, a) <= dist(c, b) {
            mask |= bit(i);
            fg_sum = (fg_sum.0 + c.0 as u32, fg_sum.1 + c.1 as u32, fg_sum.2 + c.2 as u32);
            fg_n += 1;
        } else {
            bg_sum = (bg_sum.0 + c.0 as u32, bg_sum.1 + c.1 as u32, bg_sum.2 + c.2 as u32);
            bg_n += 1;
        }
    }
    let avg = |s: (u32, u32, u32), n: u32| ((s.0 / n) as u8, (s.1 / n) as u8, (s.2 / n) as u8);
    Span::styled(
        QUADRANTS[mask].to_string(),
        Style::default().fg(rgb(avg(fg_sum, fg_n))).bg(rgb(avg(bg_sum, bg_n))),
    )
}

/// The GIF has thin transparent slivers inside the body (fine on the
/// meme's light background, black holes on a dark terminal). Transparent
/// pixels not connected to the sprite's border are interior holes: fill
/// them with the average of their opaque neighbours. The outer silhouette
/// stays transparent.
fn fill_interior_holes(grid: &mut [Rgba], pw: usize, ph: usize) {
    // flood-fill "outside" transparency from the border
    let mut outside = vec![false; pw * ph];
    let mut queue: Vec<usize> = Vec::new();
    for x in 0..pw {
        for y in [0, ph - 1] {
            queue.push(y * pw + x);
        }
    }
    for y in 0..ph {
        for x in [0, pw - 1] {
            queue.push(y * pw + x);
        }
    }
    while let Some(i) = queue.pop() {
        if outside[i] || grid[i].is_some() {
            continue;
        }
        outside[i] = true;
        let (x, y) = (i % pw, i / pw);
        if x > 0 {
            queue.push(i - 1);
        }
        if x + 1 < pw {
            queue.push(i + 1);
        }
        if y > 0 {
            queue.push(i - pw);
        }
        if y + 1 < ph {
            queue.push(i + pw);
        }
    }
    for i in 0..grid.len() {
        if grid[i].is_none() && !outside[i] {
            let (x, y) = (i % pw, i / pw);
            let mut sum = (0u32, 0u32, 0u32);
            let mut n = 0u32;
            let mut add = |j: usize| {
                if let Some((r, g, b)) = grid[j] {
                    sum = (sum.0 + r as u32, sum.1 + g as u32, sum.2 + b as u32);
                    n += 1;
                }
            };
            if x > 0 {
                add(i - 1);
            }
            if x + 1 < pw {
                add(i + 1);
            }
            if y > 0 {
                add(i - pw);
            }
            if y + 1 < ph {
                add(i + pw);
            }
            if let (Some(r), Some(g), Some(b)) =
                (sum.0.checked_div(n), sum.1.checked_div(n), sum.2.checked_div(n))
            {
                grid[i] = Some((r as u8, g as u8, b as u8));
            }
        }
    }
}

/// Most common color in the source box for target pixel (x, y); transparent
/// wins only when it covers at least half the box. Ties prefer darker colors
/// so outlines survive.
fn dominant(
    canvas: &[Rgba],
    gw: usize,
    gh: usize,
    x: usize,
    y: usize,
    pw: usize,
    ph: usize,
) -> Rgba {
    let (x0, x1) = (x * gw / pw, ((x + 1) * gw / pw).max(x * gw / pw + 1).min(gw));
    let (y0, y1) = (y * gh / ph, ((y + 1) * gh / ph).max(y * gh / ph + 1).min(gh));
    // quantized color key → (count, channel sums)
    type Bucket = (u32, (u32, u32, u32));
    let mut buckets: std::collections::HashMap<(u8, u8, u8), Bucket> =
        std::collections::HashMap::new();
    let mut transparent = 0u32;
    let mut total = 0u32;
    for sy in y0..y1 {
        for sx in x0..x1 {
            total += 1;
            match canvas[sy * gw + sx] {
                None => transparent += 1,
                Some((r, g, b)) => {
                    // quantize to 16 levels per channel to merge noise
                    let key = (r & 0xF0, g & 0xF0, b & 0xF0);
                    let entry = buckets.entry(key).or_insert((0, (0, 0, 0)));
                    entry.0 += 1;
                    entry.1.0 += r as u32;
                    entry.1.1 += g as u32;
                    entry.1.2 += b as u32;
                }
            }
        }
    }
    if transparent * 2 >= total.max(1) {
        return None;
    }
    buckets
        .into_iter()
        .max_by_key(|(key, (count, _))| {
            let darkness = 765 - (key.0 as u32 + key.1 as u32 + key.2 as u32);
            (*count, darkness)
        })
        .map(|(_, (count, (r, g, b)))| ((r / count) as u8, (g / count) as u8, (b / count) as u8))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_the_real_gif() {
        let frames = frames();
        assert!(frames.len() >= 5, "party parrot has many frames: {}", frames.len());
        for frame in frames {
            assert_eq!(frame.lines.len(), ROWS as usize);
            assert!(frame.width > 0);
        }
    }

    #[test]
    fn frames_are_animated_and_colored() {
        let frames = frames();
        let paint = |f: &Frame| {
            f.lines
                .iter()
                .flat_map(|l| l.spans.iter())
                .filter_map(|s| match s.style.fg {
                    Some(Color::Rgb(r, g, b)) => Some((r, g, b)),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        let first = paint(&frames[0]);
        assert!(!first.is_empty(), "frame has colored pixels");
        // the parrot changes hue between frames — that's the whole meme
        let later = paint(&frames[frames.len() / 2]);
        assert_ne!(first, later);
    }

    #[test]
    fn frames_do_not_accumulate_ghosting() {
        // If disposal were ignored, each frame would be the union of all
        // previous ones and opaque coverage would grow monotonically.
        let opaque = |f: &Frame| {
            f.lines.iter().flat_map(|l| l.spans.iter()).filter(|s| s.content != " ").count()
        };
        let counts: Vec<usize> = frames().iter().map(opaque).collect();
        let (min, max) = (*counts.iter().min().unwrap(), *counts.iter().max().unwrap());
        // poses legitimately vary in size (48..71 cells); ghosting would
        // push later frames toward the union of all silhouettes (~2x min)
        assert!(
            max as f64 <= min as f64 * 1.7,
            "frame coverage should stay similar across the loop: {counts:?}"
        );
        let grows_monotonically = counts.windows(2).all(|w| w[1] >= w[0]);
        assert!(!grows_monotonically, "coverage must not only grow: {counts:?}");
    }

    #[test]
    fn interior_holes_are_filled_but_border_transparency_kept() {
        // opaque square with a transparent center hole + open exterior
        let mut grid: Vec<Rgba> = vec![None; 25]; // 5x5
        for y in 1..4 {
            for x in 1..4 {
                grid[y * 5 + x] = Some((200, 100, 100));
            }
        }
        grid[2 * 5 + 2] = None; // interior hole
        fill_interior_holes(&mut grid, 5, 5);
        assert!(grid[2 * 5 + 2].is_some(), "interior hole filled");
        assert!(grid[0].is_none(), "exterior stays transparent");
        assert!(grid[24].is_none());
    }

    #[test]
    fn transparent_corners_stay_blank() {
        let frame = &frames()[0];
        let first_line = &frame.lines[0];
        let has_blank = first_line.spans.iter().any(|s| s.content == " ");
        assert!(has_blank, "background is transparent, not painted");
    }
}
