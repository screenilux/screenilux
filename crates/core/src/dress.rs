//! Dressing a frame for export: the effects drawn into the pixels.
//!
//! The editor's preview paints effects over the picture on the GPU, which is
//! free per frame and right for scrubbing, but leaves nothing in a file. This
//! is the same picture made on the CPU for the export, one output frame at a
//! time: the part a zoom shows, cut to the shape of the video (wide, vertical
//! or square), set on its backdrop, with the regions, captions, click marks,
//! pointer and shortcuts drawn over it.
//!
//! Where things go comes from [`look`](crate::look), which the preview uses
//! too, so the two cannot disagree about what a zoom shows or when an effect
//! is at full strength. Sizes of things laid over the picture (captions,
//! labels, rings) are given against a 720-pixel-tall picture and scaled,
//! which is about the size the preview shows them at.

use ab_glyph::{Font, FontArc, PxScale, ScaleFont, point};
use fast_image_resize::images::{Image, ImageRef};
use fast_image_resize::{FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer};
use tiny_skia::{
    Color, FillRule, GradientStop, LinearGradient, Mask, Paint, Path, PathBuilder, Pixmap,
    PixmapPaint, Point, Rect, SpreadMode, Stroke, Transform,
};

use crate::edit::{Area, Backdrop, Effect, Place, Tool};
use crate::look;

/// The app's light palette, which is what an exported video is dressed in.
mod colour {
    pub const STAGE: [u8; 3] = [0xE1, 0xF7, 0xF7];
    pub const SIGNAL: [u8; 3] = [0x06, 0x9A, 0x95];
    pub const ACTION: [u8; 3] = [0x04, 0x73, 0x6F];
    pub const BAR: [u8; 3] = [0x01, 0x26, 0x25];
    pub const ON_DARK: [u8; 3] = [0xF3, 0xFF, 0xFE];
}

/// How many blocks across a blur at no strength is cut into, as in the
/// preview. More strength, fewer and bigger blocks.
const BLUR_STEPS: u32 = 22;

/// The pointer as drawn into a video that left it out when it was recorded.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cursor {
    /// Where it is, as a fraction of the screen.
    pub at: (f32, f32),
    /// How big, 1 being the size of an ordinary pointer.
    pub size: f32,
    /// How visible, 0 to 1, so it can fade when it rests.
    pub alpha: f32,
}

/// Everything that decides how one output frame looks, besides the frame.
#[derive(Debug, Clone, Copy)]
pub struct Scene<'a> {
    /// Seconds into the original recording.
    pub at: f64,
    /// The effects live on this screen at `at`.
    pub live: &'a [&'a Effect],
    /// Where the pointer is on this screen, smoothed. A following zoom and a
    /// vertical frame keep it in view.
    pub pointer: Option<(f32, f32)>,
    /// Clicks to mark: where on the screen, and how far through fading (0
    /// just clicked, 1 gone).
    pub clicks: &'a [((f32, f32), f32)],
    /// The pointer to draw, when the recording left it out.
    pub cursor: Option<Cursor>,
    /// A shortcut to show, and how far in it has faded (0 to 1).
    pub keys: Option<(&'a str, f32)>,
    /// The camera bubble, when the camera was recorded to a video of its own.
    pub camera: Option<Bubble<'a>>,
}

/// The camera's picture at this moment, and where its bubble goes.
#[derive(Clone, Copy)]
pub struct Bubble<'a> {
    /// RGBA, `width` by `height`.
    pub rgba: &'a [u8],
    pub width: u32,
    pub height: u32,
    /// Where the bubble sat while recording.
    pub video: &'a crate::session::CameraVideo,
    /// Where the screen on show sat on the desktop.
    pub desk: crate::geometry::Rect,
}

impl std::fmt::Debug for Bubble<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Bubble")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("desk", &self.desk)
            .finish()
    }
}

/// Draws output frames. Holds what is worth keeping between them: the font,
/// the resizer and the picture being drawn into.
pub struct Dresser {
    font: Option<FontArc>,
    resizer: Resizer,
    canvas: Option<Pixmap>,
    picture: Vec<u8>,
    /// The camera's picture, scaled to its bubble.
    face: Vec<u8>,
}

impl Dresser {
    /// `font` is a TrueType or OpenType font to write captions and labels
    /// in; without one they are left out. See [`system_font`].
    pub fn new(font: Option<Vec<u8>>) -> Self {
        Self {
            font: font.and_then(|bytes| FontArc::try_from_vec(bytes).ok()),
            resizer: Resizer::new(),
            canvas: None,
            picture: Vec::new(),
            face: Vec::new(),
        }
    }

    /// One output frame, `width` by `height`, as RGBA, from `frame`: an RGBA
    /// picture of one screen, `frame_w` by `frame_h`.
    pub fn dress(
        &mut self,
        frame: &[u8],
        frame_w: u32,
        frame_h: u32,
        scene: &Scene,
        width: u32,
        height: u32,
    ) -> &[u8] {
        let width = width.max(2);
        let height = height.max(2);
        if self
            .canvas
            .as_ref()
            .is_none_or(|c| c.width() != width || c.height() != height)
        {
            self.canvas = Pixmap::new(width, height);
        }
        let mut canvas = self.canvas.take().expect("a canvas of a valid size");
        let (w, h) = (width as f32, height as f32);
        let ui = w.min(h) / 720.0;

        // Where the picture sits: the whole frame, or inset on a backdrop.
        let backdrop = scene.live.iter().find_map(|e| match &e.tool {
            Tool::Backdrop { pad, style } => {
                Some((*pad, *style, look::ramp(scene.at, e.start, e.end)))
            }
            _ => None,
        });
        let inset = match backdrop {
            Some((pad, _, t)) if t > 0.0 => w.min(h) * pad.clamp(0.0, 0.3) * t,
            _ => 0.0,
        };
        let pic = Rect::from_xywh(inset, inset, w - 2.0 * inset, h - 2.0 * inset)
            .filter(|r| r.width() >= 8.0 && r.height() >= 8.0)
            .unwrap_or_else(|| Rect::from_xywh(0.0, 0.0, w, h).expect("a valid frame"));
        let inset = pic.x();

        // Which part of the screen is shown: the zoom, then cut to the
        // picture's shape around the pointer, or what the zoom is on.
        let zoomed = look::lens(scene.live, scene.at, scene.pointer);
        let centre = scene.pointer.unwrap_or(zoomed.centre());
        let shown = look::fit(
            zoomed,
            frame_w as f32 / frame_h.max(1) as f32,
            pic.width() / pic.height(),
            centre,
        );
        let (pw, ph) = (
            pic.width().round().max(1.0) as u32,
            pic.height().round().max(1.0) as u32,
        );
        self.scale(frame, frame_w, frame_h, shown, pw, ph);

        if inset > 0.5 {
            let (_, style, _) = backdrop.expect("an inset means a backdrop");
            fill_backdrop(&mut canvas, style);
            let radius = (inset * 0.35).clamp(0.0, 18.0 * ui);
            let outline = rounded(
                pic.x() - 1.0,
                pic.y() - 1.0,
                pic.width() + 2.0,
                pic.height() + 2.0,
                radius,
            );
            fill(&mut canvas, &outline, [0, 0, 0], 40.0 / 255.0);
            if let Some(picture) = Pixmap::from_vec(
                std::mem::take(&mut self.picture),
                tiny_skia::IntSize::from_wh(pw, ph).expect("a picture of a valid size"),
            ) {
                let mut mask = Mask::new(width, height).expect("a mask the canvas's size");
                mask.fill_path(
                    &rounded(pic.x(), pic.y(), pic.width(), pic.height(), radius),
                    FillRule::Winding,
                    true,
                    Transform::identity(),
                );
                canvas.draw_pixmap(
                    pic.x().round() as i32,
                    pic.y().round() as i32,
                    picture.as_ref(),
                    &PixmapPaint::default(),
                    Transform::identity(),
                    Some(&mask),
                );
                self.picture = picture.take();
            }
        } else {
            let n = canvas.data().len();
            canvas.data_mut().copy_from_slice(&self.picture[..n]);
        }

        // Positions on the screen, as positions on the canvas.
        let to_canvas = |a: Area| {
            look::place(a, shown).and_then(|v| {
                Rect::from_xywh(
                    pic.x() + v.x * pic.width(),
                    pic.y() + v.y * pic.height(),
                    v.w * pic.width(),
                    v.h * pic.height(),
                )
            })
        };
        let point_on = |p: (f32, f32)| {
            look::spot(p, shown)
                .map(|(x, y)| (pic.x() + x * pic.width(), pic.y() + y * pic.height()))
        };

        for effect in scene.live {
            let t = look::ramp(scene.at, effect.start, effect.end);
            if t <= 0.0 {
                continue;
            }
            let Some(on) = effect.tool.area().and_then(to_canvas) else {
                continue;
            };
            match &effect.tool {
                Tool::Hide { .. } => {
                    fill(
                        &mut canvas,
                        &rounded(on.x(), on.y(), on.width(), on.height(), 4.0 * ui),
                        colour::BAR,
                        1.0,
                    );
                }
                Tool::Blur { strength, .. } => pixelate(&mut canvas, on, *strength),
                Tool::Spotlight { dim, .. } => {
                    veil_around(&mut canvas, pic, on, dim.clamp(0.0, 1.0) * t);
                    ring(&mut canvas, on, 6.0 * ui, 1.5 * ui, colour::SIGNAL, 1.0);
                }
                Tool::Callout { text, .. } => {
                    veil_around(&mut canvas, pic, on, 96.0 / 255.0 * t);
                    ring(&mut canvas, on, 6.0 * ui, 2.0 * ui, colour::SIGNAL, 1.0);
                    if !text.is_empty() {
                        let centre = (on.x() + on.width() / 2.0, on.y() - 10.0 * ui);
                        self.label(
                            &mut canvas,
                            text,
                            15.0 * ui,
                            centre,
                            colour::ACTION,
                            1.0,
                            ui,
                        );
                    }
                }
                _ => {}
            }
        }

        for effect in scene.live {
            if let Tool::Caption { text, place } = &effect.tool
                && !text.is_empty()
            {
                self.caption(&mut canvas, pic, text, *place);
            }
        }

        if scene
            .live
            .iter()
            .any(|e| matches!(e.tool, Tool::MarkClicks))
        {
            let grow = pic.width().min(pic.height()) * 0.04;
            for &(at, age) in scene.clicks {
                if let Some(c) = point_on(at) {
                    let fade = (1.0 - age).clamp(0.0, 1.0);
                    circle(
                        &mut canvas,
                        c,
                        grow * (0.35 + age),
                        2.5 * ui * fade.max(0.05),
                        colour::SIGNAL,
                        fade,
                    );
                }
            }
        }

        // The camera sits over the picture, not in it: a zoom does not move
        // or magnify it. Under the pointer, so it never hides it.
        if let Some(cam) = &scene.camera
            && let Some(at) = cam.video.placed(
                scene.at,
                cam.desk,
                (pic.x(), pic.y(), pic.width(), pic.height()),
            )
        {
            self.bubble(&mut canvas, cam, at, ui);
        }

        if let Some(cursor) = scene.cursor
            && cursor.alpha > 0.0
            && let Some(at) = point_on(cursor.at)
        {
            // The pointer is magnified with the picture, as it would be if it
            // had been recorded into it.
            let magnified = 1.0 / shown.w.max(0.05);
            let size = h / 1080.0 * 22.0 * cursor.size * magnified.clamp(1.0, 4.0).sqrt();
            pointer(&mut canvas, at, size, cursor.alpha);
        }

        if let Some((keys, t)) = scene.keys
            && t > 0.0
        {
            let centre = (w / 2.0, h - h * 0.11);
            self.label(
                &mut canvas,
                keys,
                26.0 * ui,
                centre,
                [0x01, 0x14, 0x13],
                t * 0.86,
                ui,
            );
        }

        self.canvas = Some(canvas);
        self.canvas.as_ref().expect("just put back").data()
    }

    /// The camera's picture in a pill at `at` (`x, y, width, height`), its
    /// middle cropped to the pill's shape.
    fn bubble(&mut self, canvas: &mut Pixmap, cam: &Bubble, at: (f32, f32, f32, f32), ui: f32) {
        let (x, y) = (at.0.round(), at.1.round());
        let (bw, bh) = (at.2.round().max(2.0) as u32, at.3.round().max(2.0) as u32);
        let want = f64::from(bw) / f64::from(bh);
        let (fw, fh) = (f64::from(cam.width), f64::from(cam.height.max(1)));
        let (cw, ch) = if fw / fh > want {
            (fh * want, fh)
        } else {
            (fw, fw / want)
        };
        self.face.resize(bw as usize * bh as usize * 4, 0);
        let scaled = ImageRef::new(cam.width, cam.height, cam.rgba, PixelType::U8x4)
            .ok()
            .and_then(|src| {
                let mut dst = Image::from_slice_u8(bw, bh, &mut self.face, PixelType::U8x4).ok()?;
                let options = ResizeOptions::new()
                    .resize_alg(ResizeAlg::Convolution(FilterType::Bilinear))
                    .crop((fw - cw) / 2.0, (fh - ch) / 2.0, cw, ch);
                self.resizer.resize(&src, &mut dst, &options).ok()
            });
        if scaled.is_none() {
            return;
        }
        for px in self.face.as_chunks_mut::<4>().0 {
            px[3] = 255;
        }
        let Some(face) = tiny_skia::IntSize::from_wh(bw, bh)
            .and_then(|size| Pixmap::from_vec(std::mem::take(&mut self.face), size))
        else {
            return;
        };
        let pill = rounded(x, y, bw as f32, bh as f32, bw.min(bh) as f32 / 2.0);
        let mut photo = Paint {
            anti_alias: true,
            ..Paint::default()
        };
        photo.shader = tiny_skia::Pattern::new(
            face.as_ref(),
            SpreadMode::Pad,
            tiny_skia::FilterQuality::Nearest,
            1.0,
            Transform::from_translate(x, y),
        );
        canvas.fill_path(
            &pill,
            &photo,
            FillRule::Winding,
            Transform::identity(),
            None,
        );
        // A faint dark rim, so it has an edge on a white page.
        canvas.stroke_path(
            &pill,
            &paint([0, 0, 0], 60.0 / 255.0),
            &Stroke {
                width: (2.0 * ui).max(1.0),
                ..Stroke::default()
            },
            Transform::identity(),
            None,
        );
        self.face = face.take();
    }

    /// The shown part of `frame`, scaled to `w` by `h` into `self.picture`.
    fn scale(&mut self, frame: &[u8], fw: u32, fh: u32, shown: Area, w: u32, h: u32) {
        self.picture.resize(w as usize * h as usize * 4, 0);
        let crop = (
            f64::from(shown.x) * f64::from(fw),
            f64::from(shown.y) * f64::from(fh),
            (f64::from(shown.w) * f64::from(fw)).max(1.0),
            (f64::from(shown.h) * f64::from(fh)).max(1.0),
        );
        let resized = ImageRef::new(fw, fh, frame, PixelType::U8x4)
            .ok()
            .and_then(|src| {
                let mut dst =
                    Image::from_slice_u8(w, h, &mut self.picture, PixelType::U8x4).ok()?;
                let options = ResizeOptions::new()
                    .resize_alg(ResizeAlg::Convolution(FilterType::Bilinear))
                    .crop(crop.0, crop.1, crop.2, crop.3);
                self.resizer.resize(&src, &mut dst, &options).ok()
            });
        if resized.is_none() {
            self.picture.fill(0);
        }
        // Whatever the source said about alpha, a video frame is opaque.
        for px in self.picture.as_chunks_mut::<4>().0 {
            px[3] = 255;
        }
    }

    /// Words in a rounded box centred on `centre`.
    #[allow(clippy::too_many_arguments)]
    fn label(
        &self,
        canvas: &mut Pixmap,
        text: &str,
        size: f32,
        centre: (f32, f32),
        back: [u8; 3],
        alpha: f32,
        ui: f32,
    ) {
        let Some(font) = &self.font else { return };
        let width = measure(font, text, size);
        let (pad_x, pad_y) = (10.0 * ui, 5.0 * ui);
        let line = line_height(font, size);
        let bw = width + 2.0 * pad_x;
        let bh = line + 2.0 * pad_y;
        let (x, y) = (centre.0 - bw / 2.0, centre.1 - bh / 2.0);
        fill(canvas, &rounded(x, y, bw, bh, 8.0 * ui), back, alpha);
        write(
            canvas,
            font,
            text,
            size,
            x + pad_x,
            y + pad_y,
            colour::ON_DARK,
            alpha,
        );
    }

    /// A caption across the picture, where it says to sit.
    fn caption(&self, canvas: &mut Pixmap, pic: Rect, text: &str, place: Place) {
        let Some(font) = &self.font else { return };
        let size = (pic.height() * 0.045).clamp(13.0, 30.0 * pic.height() / 720.0);
        let lines = wrap(font, text, size, pic.width() * 0.8);
        let line = line_height(font, size);
        let width = lines
            .iter()
            .map(|l| measure(font, l, size))
            .fold(0.0, f32::max);
        let (pad_x, pad_y) = (size * 0.8, size * 0.45);
        let bw = width + 2.0 * pad_x;
        let bh = line * lines.len() as f32 + 2.0 * pad_y;
        let cy = match place {
            Place::Bottom => pic.bottom() - pic.height() * 0.08 - bh / 2.0 + pad_y,
            Place::Centre => pic.y() + pic.height() / 2.0,
            Place::Top => pic.y() + pic.height() * 0.08 + bh / 2.0 - pad_y,
        };
        let (x, y) = (pic.x() + pic.width() / 2.0 - bw / 2.0, cy - bh / 2.0);
        fill(
            canvas,
            &rounded(x, y, bw, bh, 10.0 * pic.height() / 720.0),
            [0, 0, 0],
            190.0 / 255.0,
        );
        for (i, l) in lines.iter().enumerate() {
            let lx = pic.x() + pic.width() / 2.0 - measure(font, l, size) / 2.0;
            write(
                canvas,
                font,
                l,
                size,
                lx,
                y + pad_y + line * i as f32,
                colour::ON_DARK,
                1.0,
            );
        }
    }
}

/// A font from the system to write captions in: Segoe UI on Windows, Noto
/// Sans or DejaVu Sans elsewhere, the faces the app itself is drawn in.
pub fn system_font() -> Option<Vec<u8>> {
    let candidates: &[&str] = if cfg!(windows) {
        &[
            r"C:\Windows\Fonts\seguisb.ttf",
            r"C:\Windows\Fonts\segoeui.ttf",
            r"C:\Windows\Fonts\arial.ttf",
        ]
    } else {
        &[
            "/usr/share/fonts/truetype/noto/NotoSans-Medium.ttf",
            "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
            "/usr/share/fonts/noto/NotoSans-Regular.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            "/usr/share/fonts/TTF/DejaVuSans.ttf",
            "/usr/share/fonts/dejavu/DejaVuSans.ttf",
        ]
    };
    candidates.iter().find_map(|p| std::fs::read(p).ok())
}

// -- shapes ---------------------------------------------------------------

fn paint(rgb: [u8; 3], alpha: f32) -> Paint<'static> {
    let mut paint = Paint::default();
    paint.set_color(Color::from_rgba8(
        rgb[0],
        rgb[1],
        rgb[2],
        (alpha.clamp(0.0, 1.0) * 255.0) as u8,
    ));
    paint.anti_alias = true;
    paint
}

fn rounded(x: f32, y: f32, w: f32, h: f32, r: f32) -> Path {
    let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.quad_to(x + w, y, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.quad_to(x + w, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.quad_to(x, y + h, x, y + h - r);
    pb.line_to(x, y + r);
    pb.quad_to(x, y, x + r, y);
    pb.close();
    pb.finish().unwrap_or_else(|| {
        PathBuilder::from_rect(Rect::from_xywh(x, y, w.max(1.0), h.max(1.0)).expect("a rect"))
    })
}

fn fill(canvas: &mut Pixmap, path: &Path, rgb: [u8; 3], alpha: f32) {
    canvas.fill_path(
        path,
        &paint(rgb, alpha),
        FillRule::Winding,
        Transform::identity(),
        None,
    );
}

fn ring(canvas: &mut Pixmap, on: Rect, radius: f32, width: f32, rgb: [u8; 3], alpha: f32) {
    let half = width / 2.0;
    let path = rounded(
        on.x() - half,
        on.y() - half,
        on.width() + width,
        on.height() + width,
        radius,
    );
    let stroke = Stroke {
        width,
        ..Stroke::default()
    };
    canvas.stroke_path(
        &path,
        &paint(rgb, alpha),
        &stroke,
        Transform::identity(),
        None,
    );
}

fn circle(canvas: &mut Pixmap, c: (f32, f32), r: f32, width: f32, rgb: [u8; 3], alpha: f32) {
    if let Some(path) = PathBuilder::from_circle(c.0, c.1, r.max(0.5)) {
        let stroke = Stroke {
            width,
            ..Stroke::default()
        };
        canvas.stroke_path(
            &path,
            &paint(rgb, alpha),
            &stroke,
            Transform::identity(),
            None,
        );
    }
}

/// Darken the picture everywhere but `hole`.
fn veil_around(canvas: &mut Pixmap, pic: Rect, hole: Rect, alpha: f32) {
    let panes = [
        Rect::from_ltrb(pic.left(), pic.top(), pic.right(), hole.top()),
        Rect::from_ltrb(pic.left(), hole.bottom(), pic.right(), pic.bottom()),
        Rect::from_ltrb(pic.left(), hole.top(), hole.left(), hole.bottom()),
        Rect::from_ltrb(hole.right(), hole.top(), pic.right(), hole.bottom()),
    ];
    for pane in panes.into_iter().flatten() {
        canvas.fill_rect(pane, &paint([0, 0, 0], alpha), Transform::identity(), None);
    }
}

/// A blur that cannot be undone: the area cut into blocks, each one the
/// average of what was under it. Like the preview, more strength means
/// fewer and bigger blocks.
fn pixelate(canvas: &mut Pixmap, on: Rect, strength: f32) {
    let steps = (BLUR_STEPS as f32 * (1.05 - strength.clamp(0.0, 1.0)))
        .round()
        .clamp(3.0, BLUR_STEPS as f32) as u32;
    let (cw, ch) = (canvas.width() as i32, canvas.height() as i32);
    let x0 = (on.left().floor() as i32).clamp(0, cw);
    let y0 = (on.top().floor() as i32).clamp(0, ch);
    let x1 = (on.right().ceil() as i32).clamp(0, cw);
    let y1 = (on.bottom().ceil() as i32).clamp(0, ch);
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let cols = steps.max(1) as i32;
    let rows = ((steps as f32 * (y1 - y0) as f32 / (x1 - x0).max(1) as f32).round() as i32)
        .clamp(2, steps as i32);
    let stride = cw as usize * 4;
    let data = canvas.data_mut();
    for row in 0..rows {
        let by0 = y0 + (y1 - y0) * row / rows;
        let by1 = (y0 + (y1 - y0) * (row + 1) / rows).max(by0 + 1);
        for col in 0..cols {
            let bx0 = x0 + (x1 - x0) * col / cols;
            let bx1 = (x0 + (x1 - x0) * (col + 1) / cols).max(bx0 + 1);
            let mut sum = [0u64; 4];
            let mut n = 0u64;
            for y in by0..by1.min(ch) {
                for x in bx0..bx1.min(cw) {
                    let i = y as usize * stride + x as usize * 4;
                    for (c, s) in sum.iter_mut().enumerate() {
                        *s += u64::from(data[i + c]);
                    }
                    n += 1;
                }
            }
            if n == 0 {
                continue;
            }
            let avg = sum.map(|s| (s / n) as u8);
            for y in by0..by1.min(ch) {
                for x in bx0..bx1.min(cw) {
                    let i = y as usize * stride + x as usize * 4;
                    data[i..i + 4].copy_from_slice(&avg);
                }
            }
        }
    }
}

fn fill_backdrop(canvas: &mut Pixmap, style: Backdrop) {
    let (w, h) = (canvas.width() as f32, canvas.height() as f32);
    let stage = colour::STAGE;
    match style {
        Backdrop::Solid => canvas.fill(Color::from_rgba8(stage[0], stage[1], stage[2], 255)),
        // The blurred style draws the gradient for now, as the preview does.
        Backdrop::Gradient | Backdrop::Blurred => {
            let mix =
                |a: u8, b: u8, f: f32| (f32::from(a) + (f32::from(b) - f32::from(a)) * f) as u8;
            let top = [
                mix(stage[0], colour::SIGNAL[0], 0.35),
                mix(stage[1], colour::SIGNAL[1], 0.35),
                mix(stage[2], colour::SIGNAL[2], 0.35),
            ];
            let shader = LinearGradient::new(
                Point::from_xy(0.0, 0.0),
                Point::from_xy(0.0, h),
                vec![
                    GradientStop::new(0.0, Color::from_rgba8(top[0], top[1], top[2], 255)),
                    GradientStop::new(1.0, Color::from_rgba8(stage[0], stage[1], stage[2], 255)),
                ],
                SpreadMode::Pad,
                Transform::identity(),
            );
            let mut paint = Paint::default();
            if let Some(shader) = shader {
                paint.shader = shader;
            }
            if let Some(all) = Rect::from_xywh(0.0, 0.0, w, h) {
                canvas.fill_rect(all, &paint, Transform::identity(), None);
            }
        }
    }
}

/// An arrow pointer with its tip at `tip`, `size` pixels tall: white with a
/// dark edge, so it reads on any background.
fn pointer(canvas: &mut Pixmap, tip: (f32, f32), size: f32, alpha: f32) {
    // The classic arrow, on a 20-unit-tall grid.
    const SHAPE: [(f32, f32); 7] = [
        (0.0, 0.0),
        (0.0, 17.0),
        (4.2, 13.2),
        (7.0, 19.6),
        (9.8, 18.4),
        (7.0, 12.2),
        (12.4, 12.2),
    ];
    let k = size / 20.0;
    let mut pb = PathBuilder::new();
    for (i, (x, y)) in SHAPE.iter().enumerate() {
        let p = (tip.0 + x * k, tip.1 + y * k);
        if i == 0 {
            pb.move_to(p.0, p.1);
        } else {
            pb.line_to(p.0, p.1);
        }
    }
    pb.close();
    let Some(path) = pb.finish() else { return };
    fill(canvas, &path, [255, 255, 255], alpha);
    let stroke = Stroke {
        width: (1.4 * k).max(1.0),
        line_join: tiny_skia::LineJoin::Round,
        ..Stroke::default()
    };
    canvas.stroke_path(
        &path,
        &paint([0x10, 0x10, 0x10], alpha),
        &stroke,
        Transform::identity(),
        None,
    );
}

// -- text -----------------------------------------------------------------

fn line_height(font: &FontArc, size: f32) -> f32 {
    let s = font.as_scaled(PxScale::from(size));
    s.ascent() - s.descent() + s.line_gap()
}

fn measure(font: &FontArc, text: &str, size: f32) -> f32 {
    let s = font.as_scaled(PxScale::from(size));
    let mut width = 0.0;
    let mut last = None;
    for c in text.chars() {
        let id = s.glyph_id(c);
        if let Some(prev) = last {
            width += s.kern(prev, id);
        }
        width += s.h_advance(id);
        last = Some(id);
    }
    width
}

/// `text` broken into lines no wider than `max`, at spaces.
fn wrap(font: &FontArc, text: &str, size: f32, max: f32) -> Vec<String> {
    let mut lines = Vec::new();
    for para in text.lines() {
        let mut line = String::new();
        for word in para.split_whitespace() {
            let tried = if line.is_empty() {
                word.to_string()
            } else {
                format!("{line} {word}")
            };
            if !line.is_empty() && measure(font, &tried, size) > max {
                lines.push(std::mem::replace(&mut line, word.to_string()));
            } else {
                line = tried;
            }
        }
        lines.push(line);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// One line of text with its top left at (`x`, `y`).
#[allow(clippy::too_many_arguments)]
fn write(
    canvas: &mut Pixmap,
    font: &FontArc,
    text: &str,
    size: f32,
    x: f32,
    y: f32,
    rgb: [u8; 3],
    alpha: f32,
) {
    let s = font.as_scaled(PxScale::from(size));
    let baseline = y + s.ascent();
    let (cw, ch) = (canvas.width() as i32, canvas.height() as i32);
    let stride = cw as usize * 4;
    let data = canvas.data_mut();
    let mut caret = x;
    let mut last = None;
    for c in text.chars() {
        let id = s.glyph_id(c);
        if let Some(prev) = last {
            caret += s.kern(prev, id);
        }
        let glyph = id.with_scale_and_position(size, point(caret, baseline));
        caret += s.h_advance(id);
        last = Some(id);
        let Some(outline) = font.outline_glyph(glyph) else {
            continue;
        };
        let bounds = outline.px_bounds();
        outline.draw(|gx, gy, cover| {
            let px = bounds.min.x as i32 + gx as i32;
            let py = bounds.min.y as i32 + gy as i32;
            if px < 0 || py < 0 || px >= cw || py >= ch {
                return;
            }
            let a = (cover * alpha).clamp(0.0, 1.0);
            let i = py as usize * stride + px as usize * 4;
            for (k, &v) in rgb.iter().enumerate() {
                let d = f32::from(data[i + k]);
                data[i + k] = (d + (f32::from(v) - d) * a) as u8;
            }
            let d = f32::from(data[i + 3]);
            data[i + 3] = (d + (255.0 - d) * a) as u8;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::EditList;

    /// A 160x90 frame, left half black and right half white.
    fn halves() -> Vec<u8> {
        let (w, h) = (160usize, 90usize);
        let mut f = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 4;
                let v = if x < w / 2 { 0 } else { 255 };
                f[i..i + 4].copy_from_slice(&[v, v, v, 255]);
            }
        }
        f
    }

    fn at(out: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * w + x) * 4) as usize;
        [out[i], out[i + 1], out[i + 2], out[i + 3]]
    }

    fn scene<'a>(live: &'a [&'a Effect]) -> Scene<'a> {
        Scene {
            at: 2.5,
            live,
            pointer: None,
            clicks: &[],
            cursor: None,
            keys: None,
            camera: None,
        }
    }

    #[test]
    fn the_camera_is_laid_over_the_picture_where_it_was() {
        let video = crate::session::CameraVideo {
            file: "camera.mp4".into(),
            width: 40,
            height: 40,
            moves: vec![crate::session::BubbleMove {
                t: 0.0,
                left: 100,
                top: 30,
            }],
        };
        // A red camera over the black-and-white halves.
        let face: Vec<u8> = [255u8, 0, 0, 255].repeat(64 * 64);
        let mut s = scene(&[]);
        s.camera = Some(Bubble {
            rgba: &face,
            width: 64,
            height: 64,
            video: &video,
            desk: crate::geometry::Rect::new(0, 0, 160, 90),
        });
        let mut d = Dresser::new(None);
        let out = d.dress(&halves(), 160, 90, &s, 160, 90).to_vec();
        assert_eq!(
            at(&out, 160, 120, 50),
            [255, 0, 0, 255],
            "the middle is the camera"
        );
        assert_eq!(
            at(&out, 160, 101, 31)[1],
            255,
            "a round bubble leaves its corner"
        );
        assert_eq!(at(&out, 160, 10, 45)[0], 0, "the rest is the picture");
    }

    #[test]
    fn with_nothing_on_the_frame_comes_through_as_it_was() {
        let mut d = Dresser::new(None);
        let out = d.dress(&halves(), 160, 90, &scene(&[]), 160, 90).to_vec();
        assert_eq!(at(&out, 160, 10, 45)[0], 0);
        assert_eq!(at(&out, 160, 150, 45)[0], 255);
    }

    #[test]
    fn a_zoom_fills_the_frame_with_what_it_is_on() {
        let mut edits = EditList::default();
        let id = edits.add(
            0.0,
            5.0,
            Tool::Zoom {
                area: Area::new(0.75, 0.4, 0.1, 0.1),
                follow: false,
                scale: 4.0,
            },
        );
        let e = edits.get(id).unwrap().clone();
        let mut d = Dresser::new(None);
        let out = d.dress(&halves(), 160, 90, &scene(&[&e]), 160, 90).to_vec();
        // Zoomed into the white half: white all the way across.
        assert_eq!(at(&out, 160, 5, 45)[0], 255);
        assert_eq!(at(&out, 160, 155, 45)[0], 255);
    }

    #[test]
    fn a_blur_leaves_no_detail_inside_it() {
        // A fine checkerboard, then blurred all over: every block is one
        // colour, so no pixel differs from its right-hand neighbour much.
        let (w, h) = (160u32, 90u32);
        let mut f = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let v = if (x + y) % 2 == 0 { 0 } else { 255 };
                let i = ((y * w + x) * 4) as usize;
                f[i..i + 4].copy_from_slice(&[v, v, v, 255]);
            }
        }
        let mut edits = EditList::default();
        let id = edits.add(0.0, 5.0, Tool::blur(Area::new(0.0, 0.0, 1.0, 1.0)));
        let e = edits.get(id).unwrap().clone();
        let mut d = Dresser::new(None);
        let out = d.dress(&f, w, h, &scene(&[&e]), w, h).to_vec();
        let differs = (0..w - 1)
            .filter(|&x| {
                (i16::from(at(&out, w, x, 40)[0]) - i16::from(at(&out, w, x + 1, 40)[0])).abs() > 40
            })
            .count();
        assert!(differs < 30, "{differs} sharp edges left in a blurred row");
    }

    #[test]
    fn a_vertical_video_is_cut_around_the_pointer() {
        let mut d = Dresser::new(None);
        // Pointer on the white half: a 9:16 slice of the white side.
        let mut s = scene(&[]);
        s.pointer = Some((0.85, 0.5));
        let out = d.dress(&halves(), 160, 90, &s, 90, 160).to_vec();
        assert_eq!(at(&out, 90, 45, 80)[0], 255);
        // On the black half instead.
        s.pointer = Some((0.1, 0.5));
        let out = d.dress(&halves(), 160, 90, &s, 90, 160).to_vec();
        assert_eq!(at(&out, 90, 45, 80)[0], 0);
    }

    #[test]
    fn a_backdrop_insets_the_picture() {
        let mut edits = EditList::default();
        let id = edits.add(
            0.0,
            5.0,
            Tool::Backdrop {
                pad: 0.1,
                style: Backdrop::Solid,
            },
        );
        let e = edits.get(id).unwrap().clone();
        let mut d = Dresser::new(None);
        let out = d.dress(&halves(), 160, 90, &scene(&[&e]), 160, 90).to_vec();
        // The corner is the backdrop's colour, not the picture's.
        assert_eq!(&at(&out, 160, 1, 1)[..3], &colour::STAGE);
    }

    #[test]
    fn a_hide_covers_its_area() {
        let mut edits = EditList::default();
        let id = edits.add(
            0.0,
            5.0,
            Tool::Hide {
                area: Area::new(0.6, 0.2, 0.3, 0.6),
            },
        );
        let e = edits.get(id).unwrap().clone();
        let mut d = Dresser::new(None);
        let out = d.dress(&halves(), 160, 90, &scene(&[&e]), 160, 90).to_vec();
        assert_eq!(&at(&out, 160, 120, 45)[..3], &colour::BAR);
    }

    #[test]
    fn captions_are_written_when_there_is_a_font() {
        let Some(font) = system_font() else {
            eprintln!("skipped: no system font");
            return;
        };
        let mut edits = EditList::default();
        let id = edits.add(
            0.0,
            5.0,
            Tool::Caption {
                text: "Hello".into(),
                place: Place::Centre,
            },
        );
        let e = edits.get(id).unwrap().clone();
        let mut d = Dresser::new(Some(font));
        let before = d.dress(&halves(), 160, 90, &scene(&[]), 640, 360).to_vec();
        let after = d
            .dress(&halves(), 160, 90, &scene(&[&e]), 640, 360)
            .to_vec();
        let changed = before
            .chunks(4)
            .zip(after.chunks(4))
            .filter(|(a, b)| a != b)
            .count();
        assert!(changed > 200, "the caption changed only {changed} pixels");
    }

    #[test]
    fn the_pointer_is_drawn_where_it_is() {
        let mut d = Dresser::new(None);
        let mut s = scene(&[]);
        s.cursor = Some(Cursor {
            at: (0.25, 0.5),
            size: 2.0,
            alpha: 1.0,
        });
        let out = d.dress(&halves(), 160, 90, &s, 640, 360).to_vec();
        // Just below and right of the tip, on the black half, it is white.
        let p = at(&out, 640, 163, 186);
        assert!(p[0] > 200, "{p:?}");
    }
}
