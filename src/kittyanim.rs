//! Image display through kitty virtual placements, with unicode
//! placeholder cells: pixels are transmitted once (zlib-deflated RGBA,
//! streamed as frames decode) and the terminal scales them to the
//! placeholder grid on the GPU — pane resizes and upscaling are free,
//! unlike the CPU resize-and-retransmit widget path.
//!
//! GIF playback comes in two flavors, best first:
//! - **Animated** (terminal passed the `a=f` probe): frames append to one
//!   image via the animation protocol and the terminal owns the timing —
//!   vibin's placeholder cells never change and the app sits idle.
//! - **Flip**: one image id per frame; each tick recolors the placeholder
//!   cells to the next frame's id (a cheap cell diff, no pixel traffic).
//!
//! Every runtime command carries q=2 since the event loop owns stdin and
//! could not read replies.

use std::io::Write as _;

use ratatui::buffer::{Buffer, CellDiffOption};
use ratatui::layout::Rect;

/// Image id of Animated mode, and the base id of Flip mode (frame n uses
/// BASE_ID + 1 + n). 24-bit so placeholders need no third diacritic; one
/// preview exists at a time, so ids are reused and the terminal frees the
/// previous image on retransmit.
const BASE_ID: u32 = 0xBEE42;

/// The kitty protocol addresses at most this many placeholder rows/cols.
const MAX_PLACEHOLDER: u16 = 297;

/// Display cap: the GPU could stretch the transmitted pixels across the
/// whole pane for free, but past 2× native it turns to mush. (2× matches
/// what a browser shows on a hidpi screen, where the terminal's font size
/// is reported in physical pixels.)
const MAX_UPSCALE: f64 = 2.0;

pub struct KittyAnim {
    /// Terminal-side playback via `a=f`; false = Flip mode.
    animated: bool,
    /// Canvas size in pixels, fixed by the first frame.
    canvas: (u32, u32),
    frames_sent: usize,
    /// Animated: playback started (loading mode, `s=2`).
    started: bool,
    /// Flip: per-frame delays, current frame, and when it went up.
    delays: Vec<std::time::Duration>,
    current: usize,
    shown_at: std::time::Instant,
    /// The cell grid the virtual placement(s) currently declare. Without
    /// an explicit `c=`/`r=` grid the terminal sizes the placement at the
    /// image's natural cell count and the placeholders merely window into
    /// it — big images crop, small ones huddle in the top-left corner.
    placed_grid: Option<(u16, u16)>,
    /// The grid the renderer last asked for (placements re-issue lazily).
    wanted_grid: Option<(u16, u16)>,
    /// Frames that already have a placement at `placed_grid` (Flip mode).
    placed_frames: usize,
    /// Buffered escape bytes, flushed to the tty by [`Self::pump`].
    out: Vec<u8>,
}

impl KittyAnim {
    pub fn new(animated: bool) -> Self {
        Self {
            animated,
            canvas: (0, 0),
            frames_sent: 0,
            started: false,
            delays: Vec::new(),
            current: 0,
            shown_at: std::time::Instant::now(),
            placed_grid: None,
            wanted_grid: None,
            placed_frames: 0,
            out: Vec::new(),
        }
    }

    pub fn frames_sent(&self) -> usize {
        self.frames_sent
    }

    /// The image id the placeholder cells reference right now.
    fn current_id(&self) -> u32 {
        if self.animated { BASE_ID } else { BASE_ID + 1 + self.current as u32 }
    }

    /// Advance Flip-mode playback when the current frame's delay is up.
    /// Returns true when a redraw (placeholder recolor) is needed.
    /// Animated mode never ticks — the terminal owns the timing.
    pub fn tick(&mut self) -> bool {
        if self.animated
            || self.frames_sent < 2
            || self.shown_at.elapsed() < self.delays[self.current]
        {
            return false;
        }
        self.current = (self.current + 1) % self.frames_sent;
        self.shown_at = std::time::Instant::now();
        true
    }

    /// Queue one decoded frame for transmission. The first frame sets the
    /// canvas; later frames must match it (GIF canvases are constant).
    pub fn push_frame(&mut self, rgba: &image::RgbaImage, delay: std::time::Duration) {
        use flate2::{Compression, write::ZlibEncoder};
        let (w, h) = (rgba.width(), rgba.height());
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::fast());
        let _ = enc.write_all(rgba.as_raw());
        let Ok(deflated) = enc.finish() else { return };
        let payload = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, deflated);

        let z = delay.as_millis().max(1);
        if self.frames_sent == 0 {
            self.canvas = (w, h);
        }
        // 4096 chars of base64 per chunk, as the protocol requires
        let mut chunks = payload.as_bytes().chunks(4096).peekable();
        let mut first = true;
        while let Some(chunk) = chunks.next() {
            let more = u8::from(chunks.peek().is_some());
            if first {
                first = false;
                if self.animated && self.frames_sent > 0 {
                    // append to the animation, terminal-side
                    let id = BASE_ID;
                    let _ = write!(
                        self.out,
                        "\x1b_Ga=f,q=2,i={id},f=32,o=z,s={w},v={h},z={z},m={more};"
                    );
                } else {
                    // a root image, data only (a=t): the sized virtual
                    // placement follows in refresh_placements once the
                    // renderer knows the cell grid
                    let id =
                        if self.animated { BASE_ID } else { BASE_ID + 1 + self.frames_sent as u32 };
                    let _ = write!(self.out, "\x1b_Ga=t,q=2,i={id},f=32,o=z,s={w},v={h},m={more};");
                }
            } else {
                let _ = write!(self.out, "\x1b_Gq=2,m={more};");
            }
            let _ = self.out.write_all(chunk);
            let _ = self.out.write_all(b"\x1b\\");
        }
        self.frames_sent += 1;
        self.delays.push(delay);

        if self.animated {
            if self.frames_sent == 1 {
                // the root frame's gap can only be set by editing it after
                let id = BASE_ID;
                let _ = write!(self.out, "\x1b_Ga=f,q=2,i={id},r=1,z={z}\x1b\\");
            } else if !self.started {
                // loading mode: play what's there, wait for the rest
                self.started = true;
                let id = BASE_ID;
                let _ = write!(self.out, "\x1b_Ga=a,q=2,i={id},s=2\x1b\\");
            }
        }
    }

    /// All frames are in: switch from loading mode to looping forever.
    pub fn finish(&mut self) {
        if self.animated && self.frames_sent > 1 {
            let id = BASE_ID;
            let _ = write!(self.out, "\x1b_Ga=a,q=2,i={id},s=3,v=1\x1b\\");
        }
    }

    /// (Re)issue virtual placements so every image id is displayed at the
    /// grid the renderer wants. A placement is only cells-to-image-grid
    /// bookkeeping — no pixels travel, so resizes are cheap.
    fn refresh_placements(&mut self) {
        let Some((cols, rows)) = self.wanted_grid else { return };
        let image_count = if self.animated { self.frames_sent.min(1) } else { self.frames_sent };
        let regrid = self.placed_grid != self.wanted_grid;
        let from = if regrid { 0 } else { self.placed_frames };
        for n in from..image_count {
            let id = if self.animated { BASE_ID } else { BASE_ID + 1 + n as u32 };
            if regrid && n < self.placed_frames {
                // drop the placement at the old grid first
                let _ = write!(self.out, "\x1b_Ga=d,d=i,q=2,i={id}\x1b\\");
            }
            let _ = write!(self.out, "\x1b_Ga=p,U=1,q=2,i={id},c={cols},r={rows}\x1b\\");
        }
        self.placed_grid = self.wanted_grid;
        self.placed_frames = image_count;
    }

    /// Flush queued transmissions to the terminal (main thread only).
    pub fn pump(&mut self) {
        self.refresh_placements();
        if self.out.is_empty() {
            return;
        }
        let mut stdout = std::io::stdout();
        let _ = stdout.write_all(&self.out);
        let _ = stdout.flush();
        self.out.clear();
    }

    /// The placeholder grid that fits `avail` cells: the image's aspect
    /// ratio in font-size pixels, GPU-upscaled up to [`MAX_UPSCALE`]
    /// (pixels were paid for once at transmission — display size wasn't).
    pub fn grid(&self, font: ratatui_image::FontSize, avail: (u16, u16)) -> (u16, u16) {
        let (avail_w, avail_h) =
            (avail.0.min(MAX_PLACEHOLDER) as f64, avail.1.min(MAX_PLACEHOLDER) as f64);
        let (px_w, px_h) = (self.canvas.0.max(1) as f64, self.canvas.1.max(1) as f64);
        let (cell_w, cell_h) = (font.width.max(1) as f64, font.height.max(1) as f64);
        let scale = (avail_w * cell_w / px_w).min(avail_h * cell_h / px_h).min(MAX_UPSCALE);
        let cols = ((px_w * scale / cell_w).round() as u16).clamp(1, avail_w as u16);
        let rows = ((px_h * scale / cell_h).round() as u16).clamp(1, avail_h as u16);
        (cols, rows)
    }

    /// Draw the placeholder cells: each row is written into its first
    /// cell (with a save/restore-cursor dance), the rest marked Skip so
    /// the diff never repaints them — the terminal composites the image
    /// wherever these cells sit.
    pub fn render(&mut self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        use std::fmt::Write as _;
        let width = area.width.min(MAX_PLACEHOLDER);
        let height = area.height.min(MAX_PLACEHOLDER);
        // the placement must declare this exact grid; the next pump()
        // (re)issues it whenever it changed
        self.wanted_grid = Some((width, height));
        // the placeholder cells reference the image by its id, carried in
        // the fg color — in Flip mode this is all that changes per frame
        let [_, r, g, b] = self.current_id().to_be_bytes();
        let id_color = format!("\x1b[38;2;{r};{g};{b}m");
        // inherited diacritics: cells after the first continue the row
        let row_tail: String =
            std::iter::repeat_n('\u{10EEEE}', width.saturating_sub(1) as usize).collect();
        let restore = format!("\x1b[u\x1b[{}C\x1b[{}B", width - 1, height - 1);
        for y in 0..height {
            let mut symbol = String::new();
            let _ = write!(symbol, "\x1b[s{id_color}\u{10EEEE}{}{}", diacritic(y), diacritic(0),);
            symbol.push_str(&row_tail);
            symbol.push_str(&restore);
            for x in 1..width {
                if let Some(cell) = buf.cell_mut((area.left() + x, area.top() + y)) {
                    cell.set_diff_option(CellDiffOption::Skip);
                }
            }
            if let Some(cell) = buf.cell_mut((area.left(), area.top() + y)) {
                cell.set_symbol(&symbol).set_diff_option(CellDiffOption::ForcedWidth(
                    std::num::NonZeroU16::new(1).expect("1 is nonzero"),
                ));
            }
        }
    }
}

impl Drop for KittyAnim {
    /// Free the image(s) terminal-side when the preview closes.
    fn drop(&mut self) {
        let mut stdout = std::io::stdout();
        if self.animated {
            let id = BASE_ID;
            let _ = write!(stdout, "\x1b_Ga=d,d=I,q=2,i={id}\x1b\\");
        } else {
            for n in 0..self.frames_sent as u32 {
                let id = BASE_ID + 1 + n;
                let _ = write!(stdout, "\x1b_Ga=d,d=I,q=2,i={id}\x1b\\");
            }
        }
        let _ = stdout.flush();
    }
}

fn diacritic(i: u16) -> char {
    *DIACRITICS.get(usize::from(i)).unwrap_or(&DIACRITICS[0])
}

/// Row/column diacritics from the kitty spec's rowcolumn-diacritics.txt
/// (via the ratatui-image crate, MIT).
static DIACRITICS: [char; 297] = [
    '\u{305}',
    '\u{30D}',
    '\u{30E}',
    '\u{310}',
    '\u{312}',
    '\u{33D}',
    '\u{33E}',
    '\u{33F}',
    '\u{346}',
    '\u{34A}',
    '\u{34B}',
    '\u{34C}',
    '\u{350}',
    '\u{351}',
    '\u{352}',
    '\u{357}',
    '\u{35B}',
    '\u{363}',
    '\u{364}',
    '\u{365}',
    '\u{366}',
    '\u{367}',
    '\u{368}',
    '\u{369}',
    '\u{36A}',
    '\u{36B}',
    '\u{36C}',
    '\u{36D}',
    '\u{36E}',
    '\u{36F}',
    '\u{483}',
    '\u{484}',
    '\u{485}',
    '\u{486}',
    '\u{487}',
    '\u{592}',
    '\u{593}',
    '\u{594}',
    '\u{595}',
    '\u{597}',
    '\u{598}',
    '\u{599}',
    '\u{59C}',
    '\u{59D}',
    '\u{59E}',
    '\u{59F}',
    '\u{5A0}',
    '\u{5A1}',
    '\u{5A8}',
    '\u{5A9}',
    '\u{5AB}',
    '\u{5AC}',
    '\u{5AF}',
    '\u{5C4}',
    '\u{610}',
    '\u{611}',
    '\u{612}',
    '\u{613}',
    '\u{614}',
    '\u{615}',
    '\u{616}',
    '\u{617}',
    '\u{657}',
    '\u{658}',
    '\u{659}',
    '\u{65A}',
    '\u{65B}',
    '\u{65D}',
    '\u{65E}',
    '\u{6D6}',
    '\u{6D7}',
    '\u{6D8}',
    '\u{6D9}',
    '\u{6DA}',
    '\u{6DB}',
    '\u{6DC}',
    '\u{6DF}',
    '\u{6E0}',
    '\u{6E1}',
    '\u{6E2}',
    '\u{6E4}',
    '\u{6E7}',
    '\u{6E8}',
    '\u{6EB}',
    '\u{6EC}',
    '\u{730}',
    '\u{732}',
    '\u{733}',
    '\u{735}',
    '\u{736}',
    '\u{73A}',
    '\u{73D}',
    '\u{73F}',
    '\u{740}',
    '\u{741}',
    '\u{743}',
    '\u{745}',
    '\u{747}',
    '\u{749}',
    '\u{74A}',
    '\u{7EB}',
    '\u{7EC}',
    '\u{7ED}',
    '\u{7EE}',
    '\u{7EF}',
    '\u{7F0}',
    '\u{7F1}',
    '\u{7F3}',
    '\u{816}',
    '\u{817}',
    '\u{818}',
    '\u{819}',
    '\u{81B}',
    '\u{81C}',
    '\u{81D}',
    '\u{81E}',
    '\u{81F}',
    '\u{820}',
    '\u{821}',
    '\u{822}',
    '\u{823}',
    '\u{825}',
    '\u{826}',
    '\u{827}',
    '\u{829}',
    '\u{82A}',
    '\u{82B}',
    '\u{82C}',
    '\u{82D}',
    '\u{951}',
    '\u{953}',
    '\u{954}',
    '\u{F82}',
    '\u{F83}',
    '\u{F86}',
    '\u{F87}',
    '\u{135D}',
    '\u{135E}',
    '\u{135F}',
    '\u{17DD}',
    '\u{193A}',
    '\u{1A17}',
    '\u{1A75}',
    '\u{1A76}',
    '\u{1A77}',
    '\u{1A78}',
    '\u{1A79}',
    '\u{1A7A}',
    '\u{1A7B}',
    '\u{1A7C}',
    '\u{1B6B}',
    '\u{1B6D}',
    '\u{1B6E}',
    '\u{1B6F}',
    '\u{1B70}',
    '\u{1B71}',
    '\u{1B72}',
    '\u{1B73}',
    '\u{1CD0}',
    '\u{1CD1}',
    '\u{1CD2}',
    '\u{1CDA}',
    '\u{1CDB}',
    '\u{1CE0}',
    '\u{1DC0}',
    '\u{1DC1}',
    '\u{1DC3}',
    '\u{1DC4}',
    '\u{1DC5}',
    '\u{1DC6}',
    '\u{1DC7}',
    '\u{1DC8}',
    '\u{1DC9}',
    '\u{1DCB}',
    '\u{1DCC}',
    '\u{1DD1}',
    '\u{1DD2}',
    '\u{1DD3}',
    '\u{1DD4}',
    '\u{1DD5}',
    '\u{1DD6}',
    '\u{1DD7}',
    '\u{1DD8}',
    '\u{1DD9}',
    '\u{1DDA}',
    '\u{1DDB}',
    '\u{1DDC}',
    '\u{1DDD}',
    '\u{1DDE}',
    '\u{1DDF}',
    '\u{1DE0}',
    '\u{1DE1}',
    '\u{1DE2}',
    '\u{1DE3}',
    '\u{1DE4}',
    '\u{1DE5}',
    '\u{1DE6}',
    '\u{1DFE}',
    '\u{20D0}',
    '\u{20D1}',
    '\u{20D4}',
    '\u{20D5}',
    '\u{20D6}',
    '\u{20D7}',
    '\u{20DB}',
    '\u{20DC}',
    '\u{20E1}',
    '\u{20E7}',
    '\u{20E9}',
    '\u{20F0}',
    '\u{2CEF}',
    '\u{2CF0}',
    '\u{2CF1}',
    '\u{2DE0}',
    '\u{2DE1}',
    '\u{2DE2}',
    '\u{2DE3}',
    '\u{2DE4}',
    '\u{2DE5}',
    '\u{2DE6}',
    '\u{2DE7}',
    '\u{2DE8}',
    '\u{2DE9}',
    '\u{2DEA}',
    '\u{2DEB}',
    '\u{2DEC}',
    '\u{2DED}',
    '\u{2DEE}',
    '\u{2DEF}',
    '\u{2DF0}',
    '\u{2DF1}',
    '\u{2DF2}',
    '\u{2DF3}',
    '\u{2DF4}',
    '\u{2DF5}',
    '\u{2DF6}',
    '\u{2DF7}',
    '\u{2DF8}',
    '\u{2DF9}',
    '\u{2DFA}',
    '\u{2DFB}',
    '\u{2DFC}',
    '\u{2DFD}',
    '\u{2DFE}',
    '\u{2DFF}',
    '\u{A66F}',
    '\u{A67C}',
    '\u{A67D}',
    '\u{A6F0}',
    '\u{A6F1}',
    '\u{A8E0}',
    '\u{A8E1}',
    '\u{A8E2}',
    '\u{A8E3}',
    '\u{A8E4}',
    '\u{A8E5}',
    '\u{A8E6}',
    '\u{A8E7}',
    '\u{A8E8}',
    '\u{A8E9}',
    '\u{A8EA}',
    '\u{A8EB}',
    '\u{A8EC}',
    '\u{A8ED}',
    '\u{A8EE}',
    '\u{A8EF}',
    '\u{A8F0}',
    '\u{A8F1}',
    '\u{AAB0}',
    '\u{AAB2}',
    '\u{AAB3}',
    '\u{AAB7}',
    '\u{AAB8}',
    '\u{AABE}',
    '\u{AABF}',
    '\u{AAC1}',
    '\u{FE20}',
    '\u{FE21}',
    '\u{FE22}',
    '\u{FE23}',
    '\u{FE24}',
    '\u{FE25}',
    '\u{FE26}',
    '\u{10A0F}',
    '\u{10A38}',
    '\u{1D185}',
    '\u{1D186}',
    '\u{1D187}',
    '\u{1D188}',
    '\u{1D189}',
    '\u{1D1AA}',
    '\u{1D1AB}',
    '\u{1D1AC}',
    '\u{1D1AD}',
    '\u{1D242}',
    '\u{1D243}',
    '\u{1D244}',
];
