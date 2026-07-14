//! Image preview for image files: decoded once, then rendered through the
//! terminal's pixel protocol (kitty graphics, sixel, iTerm2) or colored
//! half-block cells when no protocol is available. The picker (protocol +
//! font size) is negotiated once at startup while stdin is still quiet.

use std::path::{Path, PathBuf};

use ratatui_image::picker::Picker;
use ratatui_image::protocol::StatefulProtocol;

/// Probe the terminal for image support with one bounded write/read on raw
/// stdin (color.rs style). ratatui-image's own query parks a thread on a
/// blocking stdin read; when the terminal never answers, that thread stays
/// alive and steals keystrokes from the event loop — so we ask ourselves:
/// a kitty graphics probe (answered with `_Gi=31;OK`), the cell pixel size
/// (CSI 16 t), and sixel support (DA1 attribute 4).
///
/// `None` (no reply, or no usable cell size) keeps the half-block
/// fallback. The second value reports kitty *animation* support (`a=f`):
/// a two-frame throwaway image is transmitted, and only a terminal that
/// implements frame appending answers the `q=0` command with OK.
pub fn probe_picker() -> Option<(Picker, bool)> {
    use ratatui_image::FontSize;
    use ratatui_image::picker::ProtocolType;
    let reply = crate::color::raw_query(
        b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[16t\
          \x1b_Ga=T,U=1,q=2,i=48059,f=24,s=1,v=1;AAAA\x1b\\\
          \x1b_Ga=f,q=0,i=48059,f=24,s=1,v=1,z=40;AAAA\x1b\\\
          \x1b_Ga=d,d=I,q=2,i=48059\x1b\\",
    )?;
    // cell size reply: CSI 6 ; height ; width t
    let (w, h) = reply.split("\x1b[6;").nth(1).and_then(|rest| {
        let mut it = rest.split([';', 't']);
        let height: u16 = it.next()?.parse().ok()?;
        let width: u16 = it.next()?.parse().ok()?;
        (width > 0 && height > 0).then_some((width, height))
    })?;
    // deprecated in favor of from_query_stdio, whose stuck-thread problem
    // is exactly what this function exists to avoid
    #[allow(deprecated)]
    let mut picker = Picker::from_fontsize(FontSize::new(w, h));
    if reply.contains("_Gi=31;OK") {
        picker.set_protocol_type(ProtocolType::Kitty);
    } else if da1_lists_sixel(&reply) {
        picker.set_protocol_type(ProtocolType::Sixel);
    } else if std::env::var("TERM_PROGRAM")
        .is_ok_and(|v| v.contains("iTerm") || v.contains("WezTerm"))
        || std::env::var("LC_TERMINAL").is_ok_and(|v| v.contains("iTerm"))
    {
        picker.set_protocol_type(ProtocolType::Iterm2);
    }
    let kitty_anim =
        picker.protocol_type() == ProtocolType::Kitty && reply.contains("_Gi=48059;OK");
    Some((picker, kitty_anim))
}

/// DA1 reply (`ESC [ ? attrs... c`) with attribute 4 = sixel support.
fn da1_lists_sixel(reply: &str) -> bool {
    reply
        .split("\x1b[?")
        .nth(1)
        .and_then(|rest| rest.split('c').next())
        .is_some_and(|attrs| attrs.split(';').any(|a| a == "4"))
}

/// Animated GIFs beyond this many frames play only their first (a decoded
/// frame is a full RGBA canvas — a long 1080p GIF would eat gigabytes).
const MAX_ANIM_FRAMES: usize = 256;

/// Animation frames are downscaled to this long side at decode. Every
/// frame is transmitted to the terminal once and kept there, so frame
/// pixels are paid for in PTY bandwidth and terminal memory; a screen
/// recording at native resolution would freeze the whole terminal.
const MAX_ANIM_PX: u32 = 480;

/// One decoded frame, streamed from the worker as soon as it exists.
struct DecodedFrame {
    rgba: image::RgbaImage,
    delay: std::time::Duration,
    /// Source canvas size (before any downscale), for the status bar.
    source: (u32, u32),
}

/// Decode-thread → view messages. Frames stream one by one, so the first
/// one is on screen while the rest still decode.
enum Msg {
    Frame(DecodedFrame),
    Done,
    Failed,
}

/// Result of pumping the decode channel; see [`ImageView::poll`].
#[derive(PartialEq, Eq)]
pub enum Poll {
    /// Nothing new (still decoding, or already done).
    Pending,
    /// New frames landed — redraw.
    Ready,
    /// Decode failed before producing anything; fall back to hex.
    Failed,
}

/// How decoded frames reach the screen.
enum Playback {
    /// ratatui-image widget: app-side frame flipping via [`ImageView::tick`].
    Widget { frames: Vec<StatefulProtocol>, delays: Vec<std::time::Duration>, frame: usize },
    /// kitty animation protocol: frames live in the terminal, which owns
    /// playback; vibin renders only static placeholder cells.
    Anim(crate::kittyanim::KittyAnim),
}

/// What the UI should draw this frame.
pub enum Visual<'a> {
    /// Decode still running, nothing on screen yet.
    Loading,
    Widget(&'a mut StatefulProtocol),
    Anim(&'a mut crate::kittyanim::KittyAnim),
}

pub struct ImageView {
    pub path: PathBuf,
    /// Original file bytes, kept so `x` can flip to the hex viewer.
    pub data: Vec<u8>,
    playback: Playback,
    /// When the current widget frame went up, for `tick`.
    shown_at: std::time::Instant,
    /// Source dimensions in pixels, for the title line.
    pub width: u32,
    pub height: u32,
    /// Decoded frame dimensions (animations may be downscaled): the
    /// preview never renders more pixels than this.
    pub frame_px: (u32, u32),
    /// All frames arrived (or the decoder gave up partway).
    complete: bool,
    /// Live while the decode thread is; dropped once it finishes.
    rx: Option<std::sync::mpsc::Receiver<Msg>>,
}

impl ImageView {
    /// Start decoding `data` on a background thread — decoding a large
    /// image (or every frame of a GIF) takes whole seconds, and doing it
    /// on the event loop would freeze the app. Hands the bytes back when
    /// the magic bytes aren't an image format the `image` crate knows;
    /// [`poll`](Self::poll) streams the decoded frames in later.
    ///
    /// On kitty-protocol terminals everything renders through virtual
    /// placements (GPU-scaled placeholder cells; GIFs play terminal-side
    /// when the animation probe passed, else frame-flip). Other terminals
    /// go through the ratatui-image widget.
    pub fn from_data(
        picker: &Picker,
        kitty_anim: bool,
        path: &Path,
        data: Vec<u8>,
    ) -> Result<Self, Vec<u8>> {
        if image::guess_format(&data).is_err() {
            return Err(data);
        }
        let playback = if picker.protocol_type() == ratatui_image::picker::ProtocolType::Kitty {
            let animated = kitty_anim && data.starts_with(b"GIF8");
            Playback::Anim(crate::kittyanim::KittyAnim::new(animated))
        } else {
            Playback::Widget { frames: Vec::new(), delays: Vec::new(), frame: 0 }
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let worker_data = data.clone();
        std::thread::spawn(move || decode_stream(&worker_data, &tx));
        Ok(Self {
            path: path.to_path_buf(),
            data,
            playback,
            shown_at: std::time::Instant::now(),
            width: 0,
            height: 0,
            frame_px: (0, 0),
            complete: false,
            rx: Some(rx),
        })
    }

    /// True once the first frame arrived.
    pub fn ready(&self) -> bool {
        self.frame_count() > 0
    }

    /// Pump the decode channel (called from App::tick): drains every
    /// frame that arrived since last time and hands it to the playback
    /// path, then flushes any queued terminal transmissions.
    pub fn poll(&mut self, picker: &Picker) -> Poll {
        use std::sync::mpsc::TryRecvError;
        let mut result = Poll::Pending;
        if let Some(rx) = &self.rx {
            loop {
                match rx.try_recv() {
                    Ok(Msg::Frame(decoded)) => {
                        (self.width, self.height) = decoded.source;
                        self.frame_px = (decoded.rgba.width(), decoded.rgba.height());
                        match &mut self.playback {
                            Playback::Widget { frames, delays, .. } => {
                                delays.push(decoded.delay);
                                frames.push(picker.new_resize_protocol(
                                    image::DynamicImage::ImageRgba8(decoded.rgba),
                                ));
                                if frames.len() == 1 {
                                    self.shown_at = std::time::Instant::now();
                                }
                            }
                            Playback::Anim(anim) => anim.push_frame(&decoded.rgba, decoded.delay),
                        }
                        result = Poll::Ready;
                    }
                    // a decoder that failed mid-animation keeps its frames
                    Ok(Msg::Done) | Ok(Msg::Failed) | Err(TryRecvError::Disconnected) => {
                        self.rx = None;
                        self.complete = true;
                        if !self.ready() {
                            return Poll::Failed;
                        }
                        if let Playback::Anim(anim) = &mut self.playback {
                            anim.finish();
                        }
                        result = Poll::Ready;
                        break;
                    }
                    Err(TryRecvError::Empty) => break,
                }
            }
        }
        if let Playback::Anim(anim) = &mut self.playback {
            anim.pump();
        }
        result
    }

    pub fn frame_count(&self) -> usize {
        match &self.playback {
            Playback::Widget { frames, .. } => frames.len(),
            Playback::Anim(anim) => anim.frames_sent(),
        }
    }

    /// All frames decoded (frame_count is final).
    pub fn complete(&self) -> bool {
        self.complete
    }

    /// What to draw right now.
    pub fn visual(&mut self) -> Visual<'_> {
        match &mut self.playback {
            Playback::Widget { frames, frame, .. } => match frames.get_mut(*frame) {
                Some(protocol) => Visual::Widget(protocol),
                None => Visual::Loading,
            },
            Playback::Anim(anim) if anim.frames_sent() > 0 => Visual::Anim(anim),
            Playback::Anim(_) => Visual::Loading,
        }
    }

    /// Advance app-side animation when the current frame's delay is up
    /// (terminal-driven kitty animation never ticks). Returns true when
    /// a redraw is needed.
    pub fn tick(&mut self) -> bool {
        match &mut self.playback {
            Playback::Anim(anim) => anim.tick(),
            Playback::Widget { frames, delays, frame } => {
                if frames.len() < 2 || self.shown_at.elapsed() < delays[*frame] {
                    return false;
                }
                *frame = (*frame + 1) % frames.len();
                self.shown_at = std::time::Instant::now();
                true
            }
        }
    }

    pub fn file_name(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.display().to_string())
    }
}

/// Decode-thread body: still images send one frame, GIFs stream every
/// frame as it decodes. The image crate composites GIF disposal methods,
/// so every frame is a full canvas.
fn decode_stream(data: &[u8], tx: &std::sync::mpsc::Sender<Msg>) {
    if !data.starts_with(b"GIF8") {
        let Ok(img) = image::load_from_memory(data) else {
            let _ = tx.send(Msg::Failed);
            return;
        };
        let source = (img.width(), img.height());
        let _ = tx.send(Msg::Frame(DecodedFrame {
            rgba: img.into_rgba8(),
            delay: std::time::Duration::ZERO,
            source,
        }));
        let _ = tx.send(Msg::Done);
        return;
    }

    use image::AnimationDecoder;
    let Ok(decoder) = image::codecs::gif::GifDecoder::new(std::io::Cursor::new(data)) else {
        let _ = tx.send(Msg::Failed);
        return;
    };
    let mut sent = 0usize;
    for frame in decoder.into_frames().take(MAX_ANIM_FRAMES) {
        let Ok(frame) = frame else { break };
        // browser convention: near-zero delays mean "unset", play at 10fps
        let delay: std::time::Duration = frame.delay().into();
        let delay =
            if delay.as_millis() < 20 { std::time::Duration::from_millis(100) } else { delay };
        let mut img = image::DynamicImage::ImageRgba8(frame.into_buffer());
        let source = (img.width(), img.height());
        // downscale immediately (per frame, before the next one decodes)
        // to keep the peak at one full canvas, not the whole animation
        if img.width().max(img.height()) > MAX_ANIM_PX {
            img = img.resize(MAX_ANIM_PX, MAX_ANIM_PX, image::imageops::FilterType::Triangle);
        }
        if tx.send(Msg::Frame(DecodedFrame { rgba: img.into_rgba8(), delay, source })).is_err() {
            return; // view closed
        }
        sent += 1;
    }
    let _ = tx.send(if sent > 0 { Msg::Done } else { Msg::Failed });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn two_frame_gif() -> Vec<u8> {
        use image::codecs::gif::GifEncoder;
        use image::{Delay, Frame, RgbaImage};
        let mut out = Vec::new();
        let mut enc = GifEncoder::new(&mut out);
        for shade in [0u8, 255] {
            let buf = RgbaImage::from_pixel(2, 2, image::Rgba([shade, 0, 0, 255]));
            let frame = Frame::from_parts(buf, 0, 0, Delay::from_numer_denom_ms(40, 1));
            enc.encode_frame(frame).unwrap();
        }
        drop(enc);
        out
    }

    /// Spin until the whole decode streamed in.
    fn wait_complete(view: &mut ImageView, picker: &Picker) {
        for _ in 0..1000 {
            match view.poll(picker) {
                Poll::Failed => panic!("decode failed"),
                _ if view.complete() => return,
                _ => std::thread::sleep(std::time::Duration::from_millis(2)),
            }
        }
        panic!("decode timed out");
    }

    #[test]
    fn gif_streams_all_frames_still_png_gets_one() {
        let picker = Picker::halfblocks();
        let mut view =
            ImageView::from_data(&picker, false, Path::new("a.gif"), two_frame_gif()).unwrap();
        assert!(!view.ready(), "decode happens off-thread");
        wait_complete(&mut view, &picker);
        assert_eq!(view.frame_count(), 2);
        assert_eq!((view.width, view.height), (2, 2));
        let Playback::Widget { delays, .. } = &view.playback else {
            panic!("widget playback without kitty animation")
        };
        assert_eq!(*delays, vec![std::time::Duration::from_millis(40); 2]);

        let mut png = std::io::Cursor::new(Vec::new());
        image::RgbaImage::new(2, 2).write_to(&mut png, image::ImageFormat::Png).unwrap();
        let mut view =
            ImageView::from_data(&picker, false, Path::new("a.png"), png.into_inner()).unwrap();
        wait_complete(&mut view, &picker);
        assert_eq!(view.frame_count(), 1);
        assert!(!view.tick(), "still images never ask for redraws");
    }

    #[test]
    fn gif_with_kitty_anim_sends_frames_to_the_terminal() {
        let mut picker = Picker::halfblocks();
        picker.set_protocol_type(ratatui_image::picker::ProtocolType::Kitty);
        let mut view =
            ImageView::from_data(&picker, true, Path::new("a.gif"), two_frame_gif()).unwrap();
        wait_complete(&mut view, &picker);
        assert_eq!(view.frame_count(), 2);
        assert!(
            matches!(view.visual(), Visual::Anim(_)),
            "terminal-driven playback, no app-side frames"
        );
        assert!(!view.tick(), "the terminal owns the timing");
    }

    #[test]
    fn corrupt_data_with_image_magic_reports_failure() {
        let picker = Picker::halfblocks();
        // PNG magic, garbage body: from_data accepts, the decode fails
        let mut view = ImageView::from_data(
            &picker,
            false,
            Path::new("bad.png"),
            b"\x89PNG\r\n\x1a\nnope".to_vec(),
        )
        .unwrap();
        for _ in 0..1000 {
            match view.poll(&picker) {
                Poll::Failed => return,
                Poll::Ready => panic!("garbage decoded?"),
                Poll::Pending => std::thread::sleep(std::time::Duration::from_millis(2)),
            }
        }
        panic!("decode timed out");
    }
}
