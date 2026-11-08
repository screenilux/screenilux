//! Turning a captured screen into the frame that gets encoded.
//!
//! Order matters and is load-bearing: zoom crop and resize, then the trail,
//! the click rings, the camera bubble, and the cursor last so nothing covers
//! it. The trail, rings and cursor are positioned through the zoom transform
//! so they follow the magnified content; the camera deliberately is not — it
//! is a picture-in-picture pinned to the output frame, and must not drift or
//! grow when the screen zooms. Every overlay keeps a constant pixel size.
//!
//! Frames stay BGRA throughout, matching what the encoder wants. Overlays are
//! drawn into small premultiplied-RGBA pixmaps covering only the region they
//! touch, then blended in with the channel swap folded into the blend, so a
//! zoomed frame is the only full-frame work.

use std::collections::VecDeque;

use fast_image_resize::images::{Image, ImageRef};
use fast_image_resize::{PixelType, ResizeAlg, ResizeOptions, Resizer};
use tiny_skia::{Color, FillRule, Paint, PathBuilder, Pixmap, Stroke, Transform};

use crate::geometry::{Point, Rect};
use crate::ports::CameraFrame;

/// A camera frame together with where the bubble currently sits.
///
/// Placement is passed per render rather than stored on the frame: dragging
/// moves the bubble between camera frames, and a cached frame must still be
/// drawn at its newest position.
#[derive(Debug, Clone, Copy)]
pub struct CameraView<'a> {
    pub frame: &'a CameraFrame,
    pub rect: Rect,
}

/// Zoom limits and feel, carried over from the build these were tuned on.
pub const MIN_ZOOM: f64 = 1.0;
pub const MAX_ZOOM: f64 = 5.0;
const ZOOM_PER_STEP: f64 = 1.18;
/// Higher eases faster. Zoom settles quicker than the pan so the view does not
/// feel like it is sliding out from under the pointer.
const ZOOM_EASE: f64 = 9.0;
const PAN_EASE: f64 = 7.0;

const TRAIL_SECONDS: f64 = 0.45;
const TRAIL_MAX_POINTS: usize = 90;
const CLICK_SECONDS: f64 = 0.5;
const CLICK_MAX: usize = 12;

const CURSOR_OUTER: f32 = 9.0;
const CURSOR_INNER: f32 = 6.0;

/// Which overlays to draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectOptions {
    pub cursor: bool,
    pub zoom: bool,
    pub trail: bool,
    pub clicks: bool,
    pub camera: bool,
}

impl Default for EffectOptions {
    fn default() -> Self {
        Self {
            cursor: true,
            zoom: true,
            trail: false,
            clicks: false,
            camera: false,
        }
    }
}

impl EffectOptions {
    /// Whether any compositing is needed at all. When nothing is enabled the
    /// recorder passes captured frames straight to the encoder untouched.
    pub fn any(&self) -> bool {
        self.cursor || self.zoom || self.trail || self.clicks || self.camera
    }
}

/// The crop of the source that fills the output frame.
#[derive(Debug, Clone, Copy, PartialEq)]
struct View {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    scale: f64,
}

impl View {
    fn identity(width: u32, height: u32) -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            w: width as f64,
            h: height as f64,
            scale: 1.0,
        }
    }

    /// Screen-local point to output-frame point.
    fn map(&self, p: Point) -> (f32, f32) {
        (
            ((p.x as f64 - self.x) * self.scale) as f32,
            ((p.y as f64 - self.y) * self.scale) as f32,
        )
    }
}

pub struct Compositor {
    bounds: Rect,
    opts: EffectOptions,

    target_zoom: f64,
    zoom: f64,
    centre: (f64, f64),

    /// Positions with the session time they were sampled at.
    trail: VecDeque<(Point, f64)>,
    clicks: VecDeque<(Point, f64)>,
    now: f64,

    out: Vec<u8>,
    resizer: Resizer,
    overlay: Option<Pixmap>,
    camera_tile: Vec<u8>,
    camera_key: (u64, u32, u32),
}

impl Compositor {
    pub fn new(bounds: Rect, opts: EffectOptions) -> Self {
        let bytes = bounds.width as usize * bounds.height as usize * 4;
        Self {
            bounds,
            opts,
            target_zoom: 1.0,
            zoom: 1.0,
            centre: (bounds.width as f64 / 2.0, bounds.height as f64 / 2.0),
            trail: VecDeque::with_capacity(TRAIL_MAX_POINTS),
            clicks: VecDeque::with_capacity(CLICK_MAX),
            now: 0.0,
            out: vec![0u8; bytes],
            resizer: Resizer::new(),
            overlay: None,
            camera_tile: Vec::new(),
            camera_key: (u64::MAX, 0, 0),
        }
    }

    pub fn options(&self) -> EffectOptions {
        self.opts
    }

    pub fn zoom(&self) -> f64 {
        self.zoom
    }

    /// Does this screen contain the given virtual-desktop point?
    pub fn contains(&self, p: Point) -> bool {
        self.bounds.contains(p)
    }

    pub fn on_wheel(&mut self, steps: f64) {
        if self.opts.zoom {
            self.target_zoom =
                (self.target_zoom * ZOOM_PER_STEP.powf(steps)).clamp(MIN_ZOOM, MAX_ZOOM);
        }
    }

    pub fn on_reset(&mut self) {
        self.target_zoom = 1.0;
    }

    /// Record a click at a virtual-desktop point. Clicks on another screen are
    /// ignored rather than drawn off the edge of this one.
    pub fn on_click(&mut self, at: Point) {
        if !self.opts.clicks || !self.bounds.contains(at) {
            return;
        }
        if self.clicks.len() == CLICK_MAX {
            self.clicks.pop_front();
        }
        self.clicks.push_back((self.bounds.to_frame(at), self.now));
    }

    /// Composite one frame. `src` is BGRA at the monitor's size, `pointer` is
    /// in virtual-desktop coordinates, `dt` is seconds since the last call.
    pub fn render(
        &mut self,
        src: &[u8],
        pointer: Point,
        dt: f64,
        camera: Option<CameraView<'_>>,
    ) -> &[u8] {
        self.now += dt.max(0.0);
        let (w, h) = (self.bounds.width, self.bounds.height);
        let inside = self.bounds.contains(pointer);
        let local = self.bounds.to_frame(pointer);

        if self.opts.trail && inside {
            // Only record movement. Appending the same point every frame would
            // keep the trail permanently alive under a stationary pointer -
            // the round line caps render a zero-length segment as a dot, so it
            // would sit there as a blob that never fades.
            let moved = self.trail.back().map(|&(p, _)| p != local).unwrap_or(true);
            if moved {
                if self.trail.len() == TRAIL_MAX_POINTS {
                    self.trail.pop_front();
                }
                self.trail.push_back((local, self.now));
            }
        }
        self.expire();

        let view = self.advance_view(local, dt, inside);
        self.fill_output(src, &view);

        if self.opts.trail {
            self.draw_trail(&view);
        }
        if self.opts.clicks {
            self.draw_clicks(&view);
        }
        if self.opts.camera
            && let Some(cam) = camera
        {
            self.draw_camera(cam.frame, cam.rect);
        }
        if self.opts.cursor && inside {
            self.draw_cursor(view.map(local));
        }

        debug_assert_eq!(self.out.len(), w as usize * h as usize * 4);
        &self.out
    }

    fn expire(&mut self) {
        while let Some(&(_, t)) = self.trail.front() {
            if self.now - t > TRAIL_SECONDS {
                self.trail.pop_front();
            } else {
                break;
            }
        }
        while let Some(&(_, t)) = self.clicks.front() {
            if self.now - t > CLICK_SECONDS {
                self.clicks.pop_front();
            } else {
                break;
            }
        }
    }

    /// Ease the zoom and pan, then return the crop that fills the frame.
    fn advance_view(&mut self, pointer: Point, dt: f64, inside: bool) -> View {
        let (w, h) = (self.bounds.width as f64, self.bounds.height as f64);

        self.zoom += (self.target_zoom - self.zoom) * (dt * ZOOM_EASE).clamp(0.0, 1.0);
        if (self.zoom - self.target_zoom).abs() < 0.002 {
            self.zoom = self.target_zoom;
        }
        if self.zoom <= 1.001 {
            self.zoom = 1.0;
            self.centre = (w / 2.0, h / 2.0);
            return View::identity(self.bounds.width, self.bounds.height);
        }

        let (cw, ch) = (w / self.zoom, h / self.zoom);
        if inside {
            // Keep the view inside the screen, then ease toward the pointer.
            // While the pointer is on another screen the view simply holds,
            // rather than drifting to whichever edge is nearest it.
            let tx = (pointer.x as f64).clamp(cw / 2.0, w - cw / 2.0);
            let ty = (pointer.y as f64).clamp(ch / 2.0, h - ch / 2.0);
            let ease = (dt * PAN_EASE).clamp(0.0, 1.0);
            self.centre.0 += (tx - self.centre.0) * ease;
            self.centre.1 += (ty - self.centre.1) * ease;
        }
        let x = (self.centre.0 - cw / 2.0).clamp(0.0, (w - cw).max(0.0));
        let y = (self.centre.1 - ch / 2.0).clamp(0.0, (h - ch).max(0.0));
        View {
            x,
            y,
            w: cw,
            h: ch,
            scale: w / cw,
        }
    }

    fn fill_output(&mut self, src: &[u8], view: &View) {
        let (w, h) = (self.bounds.width, self.bounds.height);
        let bytes = self.out.len();
        if view.scale <= 1.0 {
            self.out.copy_from_slice(&src[..bytes]);
            return;
        }
        // A read-only view, so the source is never copied just to be read.
        let Ok(source) = ImageRef::new(w, h, src, PixelType::U8x4) else {
            self.out.copy_from_slice(&src[..bytes]);
            return;
        };
        let mut dst = Image::from_slice_u8(w, h, &mut self.out, PixelType::U8x4)
            .expect("output buffer is the frame size");
        let options = ResizeOptions::new()
            .resize_alg(ResizeAlg::Convolution(
                fast_image_resize::FilterType::Bilinear,
            ))
            .crop(view.x, view.y, view.w, view.h);
        if self.resizer.resize(&source, &mut dst, &options).is_err() {
            drop(dst);
            self.out.copy_from_slice(&src[..bytes]);
        }
    }

    // -- overlays ---------------------------------------------------------

    /// Draw into a scratch pixmap covering `rect`, then blend it in.
    fn with_overlay(&mut self, rect: (i32, i32, u32, u32), draw: impl FnOnce(&mut Pixmap)) {
        let (x, y, w, h) = rect;
        if w == 0 || h == 0 {
            return;
        }
        let reuse = self
            .overlay
            .as_ref()
            .is_some_and(|p| p.width() == w && p.height() == h);
        if !reuse {
            self.overlay = Pixmap::new(w, h);
        }
        let Some(pixmap) = self.overlay.as_mut() else {
            return;
        };
        pixmap.fill(Color::TRANSPARENT);
        draw(pixmap);
        blend_over_bgra(
            &mut self.out,
            self.bounds.width,
            self.bounds.height,
            pixmap,
            x,
            y,
        );
    }

    fn draw_trail(&mut self, view: &View) {
        if self.trail.len() < 2 {
            return;
        }
        let pts: Vec<(f32, f32)> = self.trail.iter().map(|&(p, _)| view.map(p)).collect();
        let Some(area) = bounding_box(&pts, 10.0, self.bounds.width, self.bounds.height) else {
            return;
        };
        let (ox, oy) = (area.0 as f32, area.1 as f32);
        let n = pts.len();

        self.with_overlay(area, |pixmap| {
            let mut paint = Paint::default();
            // amber, matching the build this was tuned on
            paint.set_color_rgba8(255, 200, 80, 140);
            paint.anti_alias = true;

            for i in 1..n {
                if pts[i - 1] == pts[i] {
                    continue;
                }
                // taper from thin at the oldest point to thick at the pointer
                let width = 1.0 + 7.0 * (i as f32 / n as f32);
                let mut pb = PathBuilder::new();
                pb.move_to(pts[i - 1].0 - ox, pts[i - 1].1 - oy);
                pb.line_to(pts[i].0 - ox, pts[i].1 - oy);
                if let Some(path) = pb.finish() {
                    let stroke = Stroke {
                        width,
                        line_cap: tiny_skia::LineCap::Round,
                        ..Stroke::default()
                    };
                    pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
                }
            }
        });
    }

    fn draw_clicks(&mut self, view: &View) {
        let now = self.now;
        let rings: Vec<((f32, f32), f32, f32)> = self
            .clicks
            .iter()
            .filter_map(|&(p, t)| {
                let age = (now - t) / CLICK_SECONDS;
                if !(0.0..=1.0).contains(&age) {
                    return None;
                }
                // expands fast then settles, fading out as it goes
                let radius = 8.0 + 46.0 * age.sqrt();
                let alpha = 0.75 * (1.0 - age);
                Some((view.map(p), radius as f32, alpha as f32))
            })
            .collect();

        for (centre, radius, alpha) in rings {
            let pad = radius + 4.0;
            let Some(area) = bounding_box(&[centre], pad, self.bounds.width, self.bounds.height)
            else {
                continue;
            };
            let (ox, oy) = (area.0 as f32, area.1 as f32);
            self.with_overlay(area, |pixmap| {
                let mut paint = Paint::default();
                paint.set_color_rgba8(255, 220, 90, (alpha * 255.0) as u8);
                paint.anti_alias = true;
                let mut pb = PathBuilder::new();
                pb.push_circle(centre.0 - ox, centre.1 - oy, radius);
                if let Some(path) = pb.finish() {
                    let stroke = Stroke {
                        width: 3.0,
                        ..Stroke::default()
                    };
                    pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
                }
            });
        }
    }

    fn draw_cursor(&mut self, at: (f32, f32)) {
        let pad = CURSOR_OUTER + 2.0;
        let Some(area) = bounding_box(&[at], pad, self.bounds.width, self.bounds.height) else {
            return;
        };
        let (ox, oy) = (area.0 as f32, area.1 as f32);
        self.with_overlay(area, |pixmap| {
            let (cx, cy) = (at.0 - ox, at.1 - oy);

            let mut outline = Paint::default();
            outline.set_color_rgba8(0, 0, 0, 255);
            outline.anti_alias = true;
            let mut pb = PathBuilder::new();
            pb.push_circle(cx, cy, CURSOR_OUTER);
            if let Some(path) = pb.finish() {
                let stroke = Stroke {
                    width: 2.0,
                    ..Stroke::default()
                };
                pixmap.stroke_path(&path, &outline, &stroke, Transform::identity(), None);
            }

            let mut fill = Paint::default();
            fill.set_color_rgba8(255, 255, 255, 255);
            fill.anti_alias = true;
            let mut pb = PathBuilder::new();
            pb.push_circle(cx, cy, CURSOR_INNER);
            if let Some(path) = pb.finish() {
                pixmap.fill_path(&path, &fill, FillRule::Winding, Transform::identity(), None);
            }
        });
    }

    /// The camera bubble, in output-frame coordinates and unaffected by zoom.
    fn draw_camera(&mut self, cam: &CameraFrame, bubble: Rect) {
        let x = bubble.left - self.bounds.left;
        let y = bubble.top - self.bounds.top;
        // Entirely on another screen: nothing to draw here.
        if x + bubble.width as i32 <= 0
            || y + bubble.height as i32 <= 0
            || x >= self.bounds.width as i32
            || y >= self.bounds.height as i32
        {
            return;
        }

        self.scale_camera(cam, bubble.width, bubble.height);
        if self.camera_tile.is_empty() {
            return;
        }
        let tile = std::mem::take(&mut self.camera_tile);
        let (bw, bh) = (bubble.width, bubble.height);

        self.with_overlay((x, y, bw, bh), |pixmap| {
            // A round bubble: the photo clipped to a circle, or to a pill
            // if the bubble is not square.
            let mut pb = PathBuilder::new();
            if bw == bh {
                let r = bw as f32 / 2.0;
                pb.push_circle(r, r, r);
            } else {
                let radius = bw.min(bh) as f32 / 2.0;
                push_rounded_rect(&mut pb, 0.0, 0.0, bw as f32, bh as f32, radius);
            }
            let Some(path) = pb.finish() else { return };

            let Some(mut mask) = Pixmap::new(bw, bh) else {
                return;
            };
            let rows = mask.data_mut().as_chunks_mut::<4>().0.iter_mut();
            for (dst, src) in rows.zip(tile.as_chunks::<4>().0) {
                // tile is BGRA, the pixmap wants premultiplied RGBA; opaque, so
                // premultiplying is a no-op beyond the channel swap
                dst[0] = src[2];
                dst[1] = src[1];
                dst[2] = src[0];
                dst[3] = 255;
            }

            let mut paint = Paint {
                anti_alias: true,
                ..Paint::default()
            };
            paint.shader = tiny_skia::Pattern::new(
                mask.as_ref(),
                tiny_skia::SpreadMode::Pad,
                tiny_skia::FilterQuality::Nearest,
                1.0,
                Transform::identity(),
            );
            pixmap.fill_path(
                &path,
                &paint,
                FillRule::Winding,
                Transform::identity(),
                None,
            );

            let mut border = Paint::default();
            // A faint dark rim so the bubble still has an edge on a white
            // page, the same as the preview window draws. Not a light ring:
            // a white band round a face looks like a sticker.
            border.set_color_rgba8(0, 0, 0, 60);
            border.anti_alias = true;
            let stroke = Stroke {
                width: 2.0,
                ..Stroke::default()
            };
            pixmap.stroke_path(&path, &border, &stroke, Transform::identity(), None);
        });

        self.camera_tile = tile;
    }

    /// Scale the camera frame to the bubble, centre-cropped to its aspect.
    /// Cached on the frame's sequence number so N screens scale it once.
    fn scale_camera(&mut self, cam: &CameraFrame, w: u32, h: u32) {
        if self.camera_key == (cam.seq, w, h) && !self.camera_tile.is_empty() {
            return;
        }
        let want = w as f64 / h as f64;
        let have = cam.width as f64 / cam.height as f64;
        let (cw, ch) = if have > want {
            ((cam.height as f64 * want) as u32, cam.height)
        } else {
            (cam.width, (cam.width as f64 / want) as u32)
        };
        let (cx, cy) = ((cam.width - cw) / 2, (cam.height - ch) / 2);

        let Ok(source) = ImageRef::new(cam.width, cam.height, &cam.pixels, PixelType::U8x4) else {
            return;
        };
        self.camera_tile.clear();
        self.camera_tile.resize(w as usize * h as usize * 4, 0);
        let Ok(mut dst) = Image::from_slice_u8(w, h, &mut self.camera_tile, PixelType::U8x4) else {
            return;
        };
        let options = ResizeOptions::new()
            .resize_alg(ResizeAlg::Convolution(
                fast_image_resize::FilterType::Bilinear,
            ))
            .crop(cx as f64, cy as f64, cw as f64, ch as f64);
        if self.resizer.resize(&source, &mut dst, &options).is_ok() {
            self.camera_key = (cam.seq, w, h);
        } else {
            self.camera_tile.clear();
        }
    }
}

/// Bounding box of some points plus padding, clipped to the frame.
fn bounding_box(
    pts: &[(f32, f32)],
    pad: f32,
    width: u32,
    height: u32,
) -> Option<(i32, i32, u32, u32)> {
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for &(x, y) in pts {
        x0 = x0.min(x);
        y0 = y0.min(y);
        x1 = x1.max(x);
        y1 = y1.max(y);
    }
    let x0 = ((x0 - pad).floor() as i32).max(0);
    let y0 = ((y0 - pad).floor() as i32).max(0);
    let x1 = ((x1 + pad).ceil() as i32).min(width as i32);
    let y1 = ((y1 + pad).ceil() as i32).min(height as i32);
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    Some((x0, y0, (x1 - x0) as u32, (y1 - y0) as u32))
}

fn push_rounded_rect(pb: &mut PathBuilder, x: f32, y: f32, w: f32, h: f32, r: f32) {
    let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
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
}

/// Alpha-blend a premultiplied RGBA pixmap onto a BGRA frame at `(ox, oy)`.
///
/// The source is already premultiplied, so this is a plain `src + dst*(1-a)`
/// with the channel swap folded in — no un-premultiply round trip.
fn blend_over_bgra(out: &mut [u8], width: u32, height: u32, pixmap: &Pixmap, ox: i32, oy: i32) {
    let (pw, ph) = (pixmap.width() as i32, pixmap.height() as i32);
    let src = pixmap.data();
    for row in 0..ph {
        let dy = oy + row;
        if dy < 0 || dy >= height as i32 {
            continue;
        }
        for col in 0..pw {
            let dx = ox + col;
            if dx < 0 || dx >= width as i32 {
                continue;
            }
            let s = ((row * pw + col) * 4) as usize;
            let a = src[s + 3];
            if a == 0 {
                continue;
            }
            let d = ((dy * width as i32 + dx) * 4) as usize;
            if a == 255 {
                out[d] = src[s + 2];
                out[d + 1] = src[s + 1];
                out[d + 2] = src[s];
                out[d + 3] = 255;
                continue;
            }
            let inv = 255 - a as u32;
            out[d] = (src[s + 2] as u32 + out[d] as u32 * inv / 255).min(255) as u8;
            out[d + 1] = (src[s + 1] as u32 + out[d + 1] as u32 * inv / 255).min(255) as u8;
            out[d + 2] = (src[s] as u32 + out[d + 2] as u32 * inv / 255).min(255) as u8;
            out[d + 3] = 255;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: u32 = 640;
    const H: u32 = 480;

    fn screen() -> Rect {
        Rect::new(0, 0, W, H)
    }

    fn flat(value: u8) -> Vec<u8> {
        vec![value; W as usize * H as usize * 4]
    }

    /// A recognisable gradient, so a zoom is detectable from the pixels.
    fn gradient() -> Vec<u8> {
        let mut v = vec![0u8; W as usize * H as usize * 4];
        for y in 0..H as usize {
            for x in 0..W as usize {
                let i = (y * W as usize + x) * 4;
                v[i] = (x % 256) as u8;
                v[i + 1] = (y % 256) as u8;
                v[i + 2] = 128;
                v[i + 3] = 255;
            }
        }
        v
    }

    fn opts(cursor: bool, zoom: bool, trail: bool, clicks: bool, camera: bool) -> EffectOptions {
        EffectOptions {
            cursor,
            zoom,
            trail,
            clicks,
            camera,
        }
    }

    fn px(frame: &[u8], x: u32, y: u32) -> [u8; 4] {
        let i = ((y * W + x) * 4) as usize;
        [frame[i], frame[i + 1], frame[i + 2], frame[i + 3]]
    }

    fn differs(a: &[u8], b: &[u8]) -> usize {
        a.as_chunks::<4>()
            .0
            .iter()
            .zip(b.as_chunks::<4>().0)
            .filter(|(p, q)| p != q)
            .count()
    }

    fn camera_frame(w: u32, h: u32) -> CameraFrame {
        CameraFrame {
            pixels: vec![200u8; w as usize * h as usize * 4],
            width: w,
            height: h,
            seq: 1,
        }
    }

    fn view(frame: &CameraFrame, rect: Rect) -> CameraView<'_> {
        CameraView { frame, rect }
    }

    #[test]
    fn output_is_always_the_monitor_size() {
        let mut c = Compositor::new(screen(), opts(true, true, true, true, false));
        let out = c.render(&flat(10), Point::new(10, 10), 1.0 / 30.0, None);
        assert_eq!(out.len(), W as usize * H as usize * 4);
    }

    #[test]
    fn nothing_enabled_means_nothing_to_do() {
        assert!(!opts(false, false, false, false, false).any());
        assert!(opts(true, false, false, false, false).any());
    }

    #[test]
    fn the_cursor_marks_the_pointer_and_nothing_else() {
        let src = flat(10);
        let mut c = Compositor::new(screen(), opts(true, false, false, false, false));
        let out = c
            .render(&src, Point::new(320, 240), 1.0 / 30.0, None)
            .to_vec();

        assert_ne!(
            px(&out, 320, 240),
            px(&src, 320, 240),
            "cursor should be drawn"
        );
        assert_eq!(px(&out, 10, 10), px(&src, 10, 10), "far corner untouched");
        // only a small disc changes
        assert!(differs(&src, &out) < 500, "cursor painted too much");
    }

    #[test]
    fn the_cursor_is_not_drawn_when_the_pointer_is_on_another_screen() {
        let src = flat(10);
        let mut c = Compositor::new(screen(), opts(true, false, false, false, false));
        let out = c.render(&src, Point::new(2000, 240), 1.0 / 30.0, None);
        assert_eq!(
            differs(&src, out),
            0,
            "another screen's pointer must not appear"
        );
    }

    #[test]
    fn zoom_eases_in_rather_than_snapping() {
        let src = gradient();
        let mut c = Compositor::new(screen(), opts(false, true, false, false, false));
        c.on_wheel(4.0);
        let after_one = {
            c.render(&src, Point::new(320, 240), 1.0 / 30.0, None);
            c.zoom()
        };
        assert!(after_one > 1.0 && after_one < 1.9, "eased to {after_one}");

        for _ in 0..90 {
            c.render(&src, Point::new(320, 240), 1.0 / 30.0, None);
        }
        assert!(
            (c.zoom() - 1.18f64.powf(4.0)).abs() < 0.01,
            "should settle at the target, got {}",
            c.zoom()
        );
    }

    #[test]
    fn zoom_clamps_at_both_ends() {
        let mut c = Compositor::new(screen(), opts(false, true, false, false, false));
        for _ in 0..50 {
            c.on_wheel(1.0);
        }
        assert_eq!(c.target_zoom, MAX_ZOOM);
        for _ in 0..100 {
            c.on_wheel(-1.0);
        }
        assert_eq!(c.target_zoom, MIN_ZOOM);
    }

    #[test]
    fn zooming_actually_magnifies_the_content() {
        let src = gradient();
        let mut c = Compositor::new(screen(), opts(false, true, false, false, false));
        let plain = c
            .render(&src, Point::new(320, 240), 1.0 / 30.0, None)
            .to_vec();

        c.on_wheel(6.0);
        let mut zoomed = Vec::new();
        for _ in 0..90 {
            zoomed = c
                .render(&src, Point::new(320, 240), 1.0 / 30.0, None)
                .to_vec();
        }
        assert!(c.zoom() > 2.0, "expected a real zoom, got {}", c.zoom());
        assert!(
            differs(&plain, &zoomed) > (W * H / 4) as usize,
            "a zoomed frame should look substantially different"
        );
    }

    #[test]
    fn resetting_returns_to_one_to_one() {
        let src = gradient();
        let mut c = Compositor::new(screen(), opts(false, true, false, false, false));
        c.on_wheel(5.0);
        for _ in 0..90 {
            c.render(&src, Point::new(320, 240), 1.0 / 30.0, None);
        }
        c.on_reset();
        for _ in 0..90 {
            c.render(&src, Point::new(320, 240), 1.0 / 30.0, None);
        }
        assert_eq!(c.zoom(), 1.0);
    }

    #[test]
    fn a_click_ring_appears_then_expires() {
        let src = flat(10);
        let mut c = Compositor::new(screen(), opts(false, false, false, true, false));
        c.on_click(Point::new(300, 200));

        let shortly = c.render(&src, Point::new(0, 0), 0.05, None).to_vec();
        assert!(differs(&src, &shortly) > 0, "a ring should be visible");

        // step past the ring's life
        for _ in 0..20 {
            c.render(&src, Point::new(0, 0), 0.05, None);
        }
        let later = c.render(&src, Point::new(0, 0), 0.05, None);
        assert_eq!(differs(&src, later), 0, "the ring should have expired");
    }

    #[test]
    fn a_click_on_another_screen_is_ignored() {
        let src = flat(10);
        let mut c = Compositor::new(screen(), opts(false, false, false, true, false));
        c.on_click(Point::new(5000, 200));
        let out = c.render(&src, Point::new(0, 0), 0.05, None);
        assert_eq!(differs(&src, out), 0);
    }

    #[test]
    fn the_trail_follows_the_pointer_and_fades() {
        let src = flat(10);
        let mut c = Compositor::new(screen(), opts(false, false, true, false, false));
        for i in 0..10 {
            c.render(&src, Point::new(100 + i * 20, 240), 1.0 / 60.0, None);
        }
        let drawn = c
            .render(&src, Point::new(300, 240), 1.0 / 60.0, None)
            .to_vec();
        assert!(differs(&src, &drawn) > 0, "a trail should be visible");

        // hold still well past the trail's life
        for _ in 0..40 {
            c.render(&src, Point::new(300, 240), 0.05, None);
        }
        let faded = c.render(&src, Point::new(300, 240), 0.05, None);
        assert_eq!(
            differs(&src, faded),
            0,
            "the trail should expire once the pointer stops"
        );
    }

    #[test]
    fn the_camera_bubble_lands_where_it_was_placed() {
        let src = flat(10);
        let bubble = Rect::new(300, 200, 160, 120);
        let cam = camera_frame(640, 480);
        let mut c = Compositor::new(screen(), opts(false, false, false, false, true));
        let out = c.render(&src, Point::new(0, 0), 1.0 / 30.0, None).to_vec();
        assert_eq!(differs(&src, &out), 0, "no camera frame means no bubble");

        let out = c
            .render(&src, Point::new(0, 0), 1.0 / 30.0, Some(view(&cam, bubble)))
            .to_vec();
        assert_ne!(
            px(&out, 380, 260),
            px(&src, 380, 260),
            "centre of the bubble"
        );
        assert_eq!(px(&out, 50, 50), px(&src, 50, 50), "outside the bubble");
    }

    #[test]
    fn a_bubble_on_another_screen_is_not_drawn_here() {
        let src = flat(10);
        let cam = camera_frame(640, 480);
        let bubble = Rect::new(2500, 200, 160, 120);
        let mut c = Compositor::new(screen(), opts(false, false, false, false, true));
        let out = c.render(&src, Point::new(0, 0), 1.0 / 30.0, Some(view(&cam, bubble)));
        assert_eq!(differs(&src, out), 0);
    }

    #[test]
    fn a_bubble_straddling_an_edge_is_clipped_not_dropped() {
        let src = flat(10);
        let cam = camera_frame(640, 480);
        let bubble = Rect::new(W as i32 - 60, 200, 160, 120);
        let mut c = Compositor::new(screen(), opts(false, false, false, false, true));
        let out = c
            .render(&src, Point::new(0, 0), 1.0 / 30.0, Some(view(&cam, bubble)))
            .to_vec();
        assert!(
            differs(&src, &out) > 0,
            "the visible sliver should be drawn"
        );
        assert_ne!(px(&out, W - 30, 260), px(&src, W - 30, 260));
    }

    /// The camera is a picture-in-picture: zooming the screen must not move
    /// or resize it, unlike every other overlay.
    #[test]
    fn zoom_does_not_move_or_scale_the_camera_bubble() {
        let src = gradient();
        let bubble = Rect::new(400, 300, 160, 120);
        let cam = camera_frame(640, 480);
        let mut c = Compositor::new(screen(), opts(false, true, false, false, true));

        let unzoomed = c
            .render(
                &src,
                Point::new(100, 100),
                1.0 / 30.0,
                Some(view(&cam, bubble)),
            )
            .to_vec();
        let footprint = |frame: &[u8]| -> Vec<(u32, u32)> {
            let mut on = Vec::new();
            for y in (0..H).step_by(4) {
                for x in (0..W).step_by(4) {
                    if px(frame, x, y) != px(&src, x, y) {
                        on.push((x, y));
                    }
                }
            }
            on
        };
        assert!(
            !footprint(&unzoomed).is_empty(),
            "the bubble should be drawn"
        );

        c.on_wheel(6.0);
        let mut zoomed = Vec::new();
        for _ in 0..90 {
            zoomed = c
                .render(
                    &src,
                    Point::new(100, 100),
                    1.0 / 30.0,
                    Some(view(&cam, bubble)),
                )
                .to_vec();
        }
        assert!(c.zoom() > 2.0);

        // compare the bubble's own corners rather than the whole frame, since
        // the background legitimately changed everywhere
        let corner = |frame: &[u8], x: u32, y: u32| px(frame, x, y);
        for (x, y) in [(405u32, 305u32), (550, 415)] {
            assert_ne!(
                corner(&zoomed, x, y),
                corner(&src, x, y),
                "the bubble should still cover ({x},{y}) while zoomed"
            );
        }
    }

    #[test]
    fn rendering_is_stable_across_many_frames() {
        let src = gradient();
        let cam = camera_frame(640, 480);
        let bubble = Rect::new(10, 10, 160, 120);
        let mut c = Compositor::new(screen(), opts(true, true, true, true, true));
        c.on_wheel(3.0);
        for i in 0..120 {
            if i % 20 == 0 {
                c.on_click(Point::new(100 + i, 100));
            }
            let out = c.render(
                &src,
                Point::new(50 + i * 3, 100 + (i % 50)),
                1.0 / 30.0,
                Some(view(&cam, bubble)),
            );
            assert_eq!(out.len(), W as usize * H as usize * 4);
        }
    }

    #[test]
    fn a_zero_delta_does_not_wind_the_clock_backwards() {
        let src = flat(10);
        let mut c = Compositor::new(screen(), opts(true, true, true, true, false));
        c.render(&src, Point::new(10, 10), 0.0, None);
        c.render(&src, Point::new(10, 10), -1.0, None);
        assert!(c.now >= 0.0);
    }
}
