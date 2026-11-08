//! The finished video's frames, one at a time.
//!
//! Every format that writes pictures walks the finished video's clock the
//! same way: for each output frame, find the moment of the recording it
//! shows, the screen on show then, and the effects live on it, and dress
//! that screen's frame to the video's shape. This is that walk, so an MP4
//! and a GIF of the same edit show the same thing.
//!
//! Every screen of a multi-screen recording is decoded in step, since the
//! one on show can change at any frame; only the one on show is turned into
//! pixels.

use std::path::PathBuf;

use screenilux_core::dress::{Bubble, Cursor, Dresser, Scene};
use screenilux_core::edit::{EditList, Effect, Tool};
use screenilux_core::geometry::Rect;
use screenilux_core::look;
use screenilux_core::pointer::Track;
use screenilux_core::session::Focus;
use screenilux_platform::VideoFrame;
use screenilux_platform::decoder::VideoDecoder;

use crate::ExportError;

/// One screen of a recording.
#[derive(Debug, Clone)]
pub struct Screen {
    /// The name effects and the recording's own records call it by: the
    /// file it was recorded to, such as `screen1.mp4`.
    pub name: String,
    /// Where its video is.
    pub path: PathBuf,
    /// Where it was on the desktop, which is how the pointer is placed on it.
    pub desk: Rect,
}

pub use screenilux_core::edit::PointerStyle;
pub use screenilux_core::keys::Shortcut;
pub use screenilux_core::playback::Camera;

/// What a recording is made of, for drawing it out.
#[derive(Debug, Clone, Default)]
pub struct Recording {
    /// In the order the recording lists them; the first is shown when
    /// nothing says otherwise.
    pub screens: Vec<Screen>,
    pub pointer: Option<Track>,
    /// Which screen the pointer was on, over time.
    pub focus: Option<Focus>,
    /// Draw the pointer, when the recording left it out of the picture.
    pub cursor: Option<PointerStyle>,
    /// Shortcuts to show over the picture.
    pub shortcuts: Vec<Shortcut>,
    /// The file the recording's sound is in, if any: only one screen of a
    /// recording carries it.
    pub sound: Option<PathBuf>,
    /// The camera's own video, laid over the screens, when it was recorded
    /// apart from them and has not been left out.
    pub camera: Option<Camera>,
}

impl Recording {
    /// A recording of one screen, `width` by `height`, with nothing else
    /// known about it: a plain video.
    pub fn single(path: PathBuf, width: u32, height: u32) -> Self {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        Self {
            screens: vec![Screen {
                name,
                path: path.clone(),
                desk: Rect::new(0, 0, width, height),
            }],
            sound: Some(path),
            ..Self::default()
        }
    }
}

/// The shape of the finished video.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Shape {
    /// The recording's own shape.
    #[default]
    Wide,
    /// 9:16, for phones: stories, reels, shorts.
    Vertical,
    /// 1:1, for feeds.
    Square,
}

impl Shape {
    pub const ALL: [Self; 3] = [Self::Wide, Self::Vertical, Self::Square];

    pub fn label(self) -> &'static str {
        match self {
            Self::Wide => "Wide",
            Self::Vertical => "Vertical",
            Self::Square => "Square",
        }
    }

    /// The video's size in pixels, for a recording `source` big.
    pub fn size(self, source: (u32, u32)) -> (u32, u32) {
        match self {
            Self::Wide => ((source.0 & !1).max(2), (source.1 & !1).max(2)),
            Self::Vertical => (1080, 1920),
            Self::Square => (1080, 1080),
        }
    }
}

/// How long a click mark takes to fade, as in the editor.
const CLICK_LIFE: f64 = 0.55;

struct Feed {
    decoder: VideoDecoder,
    /// A frame read but not yet reached.
    pending: Option<(f64, Option<VideoFrame>)>,
    latest: Option<VideoFrame>,
    done: bool,
}

impl Feed {
    /// Read up to `s`, keeping the last frame at or before it. Frames are
    /// only turned into pixels when `convert` says this screen is on show.
    fn advance(&mut self, s: f64, convert: bool, tolerance: f64) -> Result<(), ExportError> {
        loop {
            if self.pending.is_none() {
                if self.done {
                    return Ok(());
                }
                match self
                    .decoder
                    .next_frame_if(|t| convert && t + tolerance >= s)
                    .map_err(|e| ExportError::Read(e.to_string()))?
                {
                    Some(read) => self.pending = Some(read),
                    None => {
                        self.done = true;
                        return Ok(());
                    }
                }
            }
            let t = self.pending.as_ref().map_or(f64::MAX, |(t, _)| *t);
            if t > s + 1e-6 {
                return Ok(());
            }
            if let Some((_, Some(frame))) = self.pending.take() {
                self.latest = Some(frame);
            }
        }
    }

    /// The frame to show: the last one reached, or failing that the next
    /// one, so the very first frame of a video is not blank.
    fn frame(&self) -> Option<&VideoFrame> {
        self.latest
            .as_ref()
            .or_else(|| self.pending.as_ref().and_then(|(_, f)| f.as_ref()))
    }
}

/// The walk over the finished video's frames.
pub(crate) struct Frames<'a> {
    recording: &'a Recording,
    edits: &'a EditList,
    feeds: Vec<Feed>,
    /// The camera's video, read in step with the screens.
    camera: Option<Feed>,
    dresser: Dresser,
    size: (u32, u32),
    step: f64,
    next: u64,
    output_len: f64,
    tolerance: f64,
}

impl<'a> Frames<'a> {
    pub fn open(
        recording: &'a Recording,
        edits: &'a EditList,
        shape: Shape,
        fps: f64,
    ) -> Result<Self, ExportError> {
        if recording.screens.is_empty() {
            return Err(ExportError::Read("the recording has no screens".into()));
        }
        let mut feeds = Vec::new();
        for screen in &recording.screens {
            let decoder = VideoDecoder::open(&screen.path)
                .map_err(|e| ExportError::Read(format!("{}: {e}", screen.name)))?;
            feeds.push(Feed {
                decoder,
                pending: None,
                latest: None,
                done: false,
            });
        }
        // A camera video that cannot be read costs the bubble, not the
        // export.
        let camera = recording.camera.as_ref().and_then(|cam| {
            VideoDecoder::open(&cam.path)
                .map_err(|e| tracing::warn!(error = %e, "leaving the camera out of the export"))
                .ok()
                .map(|decoder| Feed {
                    decoder,
                    pending: None,
                    latest: None,
                    done: false,
                })
        });
        let source_len = feeds
            .iter()
            .map(|f| f.decoder.duration())
            .fold(0.0, f64::max)
            .max(0.001);
        let output_len = edits.output_duration(source_len);
        if output_len <= 0.0 {
            return Err(ExportError::Empty);
        }
        let first = &feeds[0].decoder;
        let size = shape.size((first.width(), first.height()));
        let step = 1.0 / fps.max(1.0);
        Ok(Self {
            recording,
            edits,
            feeds,
            camera,
            dresser: Dresser::new(screenilux_core::dress::system_font()),
            size,
            step,
            next: 0,
            output_len,
            // A frame is turned into pixels when it is within about a frame
            // of where it is wanted; the rest cost a timestamp each.
            tolerance: 1.5 / fps.max(1.0),
        })
    }

    /// The video's size in pixels.
    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    /// How far along the walk is, 0 to 1.
    pub fn progress(&self) -> f32 {
        (self.next as f64 * self.step / self.output_len).clamp(0.0, 1.0) as f32
    }

    /// The next output frame as RGBA, with its time on the finished video's
    /// clock, or `None` once the video is done.
    pub fn next(&mut self) -> Result<Option<(f64, &[u8])>, ExportError> {
        let want = self.next as f64 * self.step;
        if want >= self.output_len - 1e-9 {
            return Ok(None);
        }
        self.next += 1;

        let s = self.edits.to_source(want);
        // A held frame keeps showing the moment its hold began; the clock
        // runs on underneath, so what is laid over it carries on.
        let held = self
            .edits
            .active_at(s)
            .find(|e| matches!(e.tool, Tool::Freeze))
            .map_or(s, |e| e.start.min(s));
        let shown = self.screen_at(s);
        for (i, feed) in self.feeds.iter_mut().enumerate() {
            feed.advance(held, i == shown, self.tolerance)?;
        }
        // The camera carries on through a held frame, as what is laid over
        // it does.
        if let Some(cam) = &mut self.camera {
            cam.advance(s, true, self.tolerance)?;
        }

        let all: Vec<Effect> = self.edits.active_at(s).cloned().collect();
        let screen = &self.recording.screens[shown];
        let live = look::for_screen(&all, Some(&screen.name));
        let track = self.recording.pointer.as_ref();
        let desk = screen.desk;
        let on_screen = |x: f64, y: f64| {
            let fx = ((x - f64::from(desk.left)) / f64::from(desk.width.max(1))) as f32;
            let fy = ((y - f64::from(desk.top)) / f64::from(desk.height.max(1))) as f32;
            ((0.0..=1.0).contains(&fx) && (0.0..=1.0).contains(&fy)).then_some((fx, fy))
        };
        // Following views move along the eased path, so a hand's tremor
        // does not shake the picture.
        let pointer = track
            .and_then(|t| t.smooth_at(held, 0.35))
            .and_then(|(x, y)| on_screen(x, y));
        let clicks: Vec<((f32, f32), f32)> = track
            .map(|t| {
                t.clicks_at(held, CLICK_LIFE)
                    .filter_map(|(c, age)| {
                        on_screen(f64::from(c.x), f64::from(c.y)).map(|p| (p, age as f32))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let cursor = match (&self.recording.cursor, track) {
            (Some(style), Some(t)) => {
                look::cursor_at(t, held, style).and_then(|((x, y), alpha)| {
                    on_screen(x, y).map(|at| Cursor {
                        at,
                        size: style.size,
                        alpha,
                    })
                })
            }
            _ => None,
        };
        let keys = look::shortcut_at(&self.recording.shortcuts, s);

        let feed = &self.feeds[shown];
        let frame = feed
            .frame()
            .or_else(|| self.feeds.iter().find_map(Feed::frame));
        let Some(frame) = frame else {
            return Err(ExportError::Read("no frame could be read".into()));
        };
        let camera = self
            .camera
            .as_ref()
            .and_then(Feed::frame)
            .zip(self.recording.camera.as_ref())
            .map(|(f, cam)| Bubble {
                rgba: &f.rgba,
                width: f.width,
                height: f.height,
                video: &cam.video,
                desk,
            });
        let scene = Scene {
            at: s,
            live: &live,
            pointer,
            clicks: &clicks,
            cursor,
            keys,
            camera,
        };
        let (w, h) = self.size;
        let out = self
            .dresser
            .dress(&frame.rgba, frame.width, frame.height, &scene, w, h);
        Ok(Some((want, out)))
    }

    /// Which screen is on show at `s`: one an effect asks for, else the one
    /// the pointer was on, else the first.
    fn screen_at(&self, s: f64) -> usize {
        let screens = &self.recording.screens;
        let by_name = |name: &str| screens.iter().position(|sc| sc.name == name);
        self.edits
            .active_at(s)
            .find_map(|e| match &e.tool {
                Tool::Screen { file } => by_name(file),
                _ => None,
            })
            .or_else(|| {
                self.recording
                    .focus
                    .as_ref()
                    .and_then(|f| f.screen_at(s))
                    .and_then(by_name)
            })
            .unwrap_or(0)
    }
}
