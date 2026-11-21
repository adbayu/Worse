//! Window wallpaper (Settings › Appearance › Background).
//!
//! A still image (PNG, JPEG, BMP, WebP) or an animation (GIF, animated WebP) drawn behind the UI. Decoding,
//! scaling and blurring run on a worker thread; the UI thread only swaps finished pixel buffers into `Look`.
//! Every frame is kept twice: the wallpaper itself, blurred as configured, and a small, heavily blurred copy
//! that glass panels show ("frost"). The dim is not baked in: Slint draws it as a veil of the canvas colour, so
//! the slider answers at once and follows the theme.
//!
//! An animation repaints the whole window on every frame, which is costly on the CPU renderer (about half a
//! core at 12 frames a second in a 1440x900 window). So it is paused while the window is hidden or not
//! focused or reduced motion is on, shows at most 10 frames a second on the CPU renderer (20 on the GPU; the
//! clock picks the frame, so the speed stays right), and its frames are scaled down until they fit a fixed
//! memory budget.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]

use std::cell::RefCell;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use harness_settings::AppearanceSettings;
use image::codecs::gif::GifDecoder;
use image::codecs::webp::WebPDecoder;
use image::{imageops, AnimationDecoder, ImageFormat, RgbaImage};
use slint::{ComponentHandle, Image, Rgba8Pixel, SharedPixelBuffer, Timer, TimerMode};

use crate::ui::{AppWindow, ArtClock, Look, Theme};

mod decode;

use decode::load;

/// Larger files are refused (a wallpaper is decoded whole).
const MAX_FILE: u64 = 96 * 1024 * 1024;
/// Widest still wallpaper kept (wider images are scaled down; the renderer scales to the window).
const STILL_WIDTH: u32 = 1920;
/// Widest animation frame kept, and the memory all frames of an animation may take together.
const ANIM_WIDTH: u32 = 800;
const ANIM_BUDGET: usize = 48 * 1024 * 1024;
/// Frames kept of one animation (longer ones play their first frames).
const ANIM_FRAMES: usize = 400;
/// Width of the frosted copy and its blur. At this size the blur spans about a seventh of the picture.
const FROST_WIDTH: u32 = 192;
const FROST_SIGMA: f32 = 7.0;
/// Shortest time a frame stays in the file's own timing (20 frames a second).
const MIN_DELAY_MS: u32 = 50;
/// Shortest time between two frames on screen with the CPU renderer (10 frames a second).
const CPU_FRAME_MS: u32 = 100;
/// The "blur" setting is in pixels of a window this wide.
const BLUR_BASE: f32 = 1440.0;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Spec {
    path: String,
    blur: u16,
}

/// One finished frame: wallpaper, frosted copy, how long it stays.
struct Frame {
    wall: SharedPixelBuffer<Rgba8Pixel>,
    frost: SharedPixelBuffer<Rgba8Pixel>,
    delay_ms: u32,
}

#[derive(Default)]
struct State {
    spec: Spec,
    frames: Vec<Frame>,
    /// End of each frame on the animation's own clock, in ms (running sum of the delays).
    ends: Vec<u64>,
    shown: usize,
    started: Option<Instant>,
    timer: Timer,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::default();
}

/// Newest load; a worker that finishes after a newer one started drops its result.
static GENERATION: AtomicU64 = AtomicU64::new(0);

/// Callbacks of the Background settings (choose / remove the wallpaper).
pub(crate) fn install(window: &AppWindow) {
    let look = window.global::<Look>();
    look.on_pick_wallpaper(|| {
        // The OS file picker blocks, so it runs on its own thread; the choice comes back as a settings write.
        std::thread::spawn(|| {
            let picked = rfd::FileDialog::new()
                .add_filter("Images", &["png", "jpg", "jpeg", "bmp", "webp", "gif"])
                .pick_file();
            if let Some(path) = picked {
                let path = path.to_string_lossy().into_owned();
                let _ = slint::invoke_from_event_loop(move || {
                    crate::settings::patch_setting("appearance.wallpaperPath", path.into());
                });
            }
        });
    });
    look.on_clear_wallpaper(|| {
        crate::settings::patch_setting("appearance.wallpaperPath", String::new().into());
    });
}

/// Mirrors the Background settings into `Look` and (re)loads the wallpaper when its file or blur changed.
pub(crate) fn apply(window: &AppWindow, appearance: &AppearanceSettings) {
    let look = window.global::<Look>();
    look.set_field(appearance.backdrop_field.as_str().into());
    look.set_field_motion(appearance.backdrop_motion.as_str().into());
    look.set_dim(f32::from(appearance.wallpaper_dim.min(95)) / 100.0);
    let spec = Spec {
        path: appearance.wallpaper_path.trim().to_owned(),
        blur: appearance.wallpaper_blur.min(40),
    };
    let changed = STATE.with(|cell| {
        let mut s = cell.borrow_mut();
        let changed = s.spec != spec;
        s.spec = spec.clone();
        changed
    });
    if !changed {
        return;
    }
    let generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    if spec.path.is_empty() {
        clear(window, "");
        return;
    }
    let name = Path::new(&spec.path)
        .file_name()
        .map_or_else(|| spec.path.clone(), |n| n.to_string_lossy().into_owned());
    look.set_wallpaper_name(name.as_str().into());
    look.set_wallpaper_note("loading".into());
    let weak = window.as_weak();
    std::thread::spawn(move || {
        let result = load(&PathBuf::from(&spec.path), spec.blur);
        let _ = slint::invoke_from_event_loop(move || {
            if GENERATION.load(Ordering::SeqCst) != generation {
                return;
            }
            let Some(window) = weak.upgrade() else {
                return;
            };
            match result {
                Ok(frames) => show(&window, frames),
                Err(error) => clear(&window, &error),
            }
        });
    });
}

/// No wallpaper: `note` says why when a load failed.
fn clear(window: &AppWindow, note: &str) {
    STATE.with(|cell| {
        let mut s = cell.borrow_mut();
        s.timer.stop();
        s.frames.clear();
        s.ends.clear();
        s.started = None;
    });
    let look = window.global::<Look>();
    look.set_has_wallpaper(false);
    look.set_wallpaper_animated(false);
    look.set_wallpaper(Image::default());
    look.set_frost(Image::default());
    look.set_wallpaper_note(note.into());
    if note.is_empty() {
        look.set_wallpaper_name(slint::SharedString::new());
    }
}

fn show(window: &AppWindow, frames: Vec<Frame>) {
    let Some(first) = frames.first() else {
        clear(window, "the image has no frames");
        return;
    };
    let look = window.global::<Look>();
    let animated = frames.len() > 1;
    look.set_wallpaper_animated(animated);
    look.set_wallpaper(Image::from_rgba8(first.wall.clone()));
    look.set_frost(Image::from_rgba8(first.frost.clone()));
    look.set_has_wallpaper(true);
    look.set_wallpaper_note(slint::SharedString::new());
    let first_delay = first.delay_ms;
    STATE.with(|cell| {
        let mut s = cell.borrow_mut();
        s.timer.stop();
        let mut end = 0u64;
        s.ends = frames
            .iter()
            .map(|f| {
                end += u64::from(f.delay_ms);
                end
            })
            .collect();
        s.frames = frames;
        s.shown = 0;
        s.started = animated.then(Instant::now);
    });
    if animated {
        arm(window, first_delay);
    }
}

/// Wakes once, when the frame on screen has had its time.
fn arm(window: &AppWindow, after_ms: u32) {
    let weak = window.as_weak();
    STATE.with(|cell| {
        cell.borrow().timer.start(
            TimerMode::SingleShot,
            Duration::from_millis(u64::from(after_ms.max(15))),
            move || {
                if let Some(window) = weak.upgrade() {
                    tick(&window);
                }
            },
        );
    });
}

fn tick(window: &AppWindow) {
    // Hidden or unfocused window, or reduced motion: hold the frame and look again later.
    let clock = window.global::<ArtClock>();
    let paused = !clock.get_visible()
        || !clock.get_focused()
        || window.global::<Theme>().get_reduce_motion();
    if paused {
        arm(window, 500);
        return;
    }
    // The frame is picked by the clock, not by counting ticks, so a slow repaint skips frames instead of
    // slowing the animation down.
    let next = STATE.with(|cell| {
        let mut s = cell.borrow_mut();
        let total = s.ends.last().copied().unwrap_or(0);
        let started = s.started?;
        if total == 0 || s.frames.len() < 2 {
            return None;
        }
        let at = u64::try_from(started.elapsed().as_millis()).unwrap_or(0) % total;
        let index = s
            .ends
            .partition_point(|end| *end <= at)
            .min(s.frames.len() - 1);
        let floor = if crate::gpu_rendering() {
            MIN_DELAY_MS
        } else {
            CPU_FRAME_MS
        };
        let wait = u32::try_from(s.ends[index] - at)
            .unwrap_or(MIN_DELAY_MS)
            .max(floor);
        let frame = (index != s.shown).then(|| {
            s.shown = index;
            (s.frames[index].wall.clone(), s.frames[index].frost.clone())
        });
        Some((frame, wait))
    });
    let Some((frame, wait)) = next else {
        return;
    };
    if let Some((wall, _frost)) = frame {
        // Only the wallpaper moves: panels show a flat veil over an animation (see `Frost`).
        window
            .global::<Look>()
            .set_wallpaper(Image::from_rgba8(wall));
    }
    arm(window, wait);
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::decode::{compose, flatten};
    use super::*;

    fn gradient(w: u32, h: u32) -> RgbaImage {
        RgbaImage::from_fn(w, h, |x, y| {
            image::Rgba([(x % 256) as u8, (y % 256) as u8, 128, 255])
        })
    }

    #[test]
    fn compose_scales_down_and_keeps_shape() {
        let frame = compose(&gradient(4000, 2000), STILL_WIDTH, 0, 0);
        assert_eq!((frame.wall.width(), frame.wall.height()), (1920, 960));
        assert_eq!(frame.frost.width(), FROST_WIDTH);
        // Small images are not enlarged.
        let small = compose(&gradient(300, 200), STILL_WIDTH, 0, 0);
        assert_eq!((small.wall.width(), small.wall.height()), (300, 200));
    }

    #[test]
    fn blur_uses_a_smaller_picture() {
        let frame = compose(&gradient(4000, 2000), STILL_WIDTH, 30, 0);
        assert_eq!(frame.wall.width(), 480);
        let light = compose(&gradient(4000, 2000), STILL_WIDTH, 6, 0);
        assert_eq!(light.wall.width(), 960);
    }

    #[test]
    fn transparent_pixels_become_opaque_black() {
        let mut image = RgbaImage::from_pixel(2, 1, image::Rgba([200, 100, 50, 0]));
        image.put_pixel(1, 0, image::Rgba([200, 100, 50, 255]));
        flatten(&mut image);
        assert_eq!(image.get_pixel(0, 0).0, [0, 0, 0, 255]);
        assert_eq!(image.get_pixel(1, 0).0, [200, 100, 50, 255]);
    }

    #[test]
    fn animated_gif_loads_all_frames_with_delays() {
        // Three 8x8 frames, 120 ms each, written with the encoder of the image crate.
        let mut bytes = Vec::new();
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut bytes);
            encoder
                .set_repeat(image::codecs::gif::Repeat::Infinite)
                .unwrap();
            for n in 0..3u8 {
                let frame = RgbaImage::from_pixel(8, 8, image::Rgba([n * 80, 0, 0, 255]));
                let delay = image::Delay::from_numer_denom_ms(120, 1);
                encoder
                    .encode_frame(image::Frame::from_parts(frame, 0, 0, delay))
                    .unwrap();
            }
        }
        let dir = std::env::temp_dir().join(format!("harness-wallpaper-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("anim.gif");
        std::fs::write(&path, &bytes).unwrap();
        let frames = load(&path, 0).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(frames.len(), 3);
        assert!(frames.iter().all(|f| f.delay_ms == 120));
        assert_eq!(frames[0].wall.width(), 8);
    }

    #[test]
    fn unreadable_files_are_errors() {
        let dir =
            std::env::temp_dir().join(format!("harness-wallpaper-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("note.txt");
        std::fs::write(&path, b"not an image").unwrap();
        assert!(load(&path, 0).is_err());
        assert!(load(&dir.join("missing.png"), 0).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
