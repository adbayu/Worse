use super::{
    imageops, AnimationDecoder, Cursor, Frame, GifDecoder, ImageFormat, Path, Rgba8Pixel,
    RgbaImage, SharedPixelBuffer, WebPDecoder, ANIM_BUDGET, ANIM_FRAMES, ANIM_WIDTH, BLUR_BASE,
    FROST_SIGMA, FROST_WIDTH, MAX_FILE, MIN_DELAY_MS, STILL_WIDTH,
};

// ---- worker thread ------------------------------------------------------------------------------------------

pub(super) fn load(path: &Path, blur: u16) -> Result<Vec<Frame>, String> {
    let size = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    if size > MAX_FILE {
        return Err(format!(
            "the file is larger than {} MB",
            MAX_FILE / (1024 * 1024)
        ));
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let format =
        image::guess_format(&bytes).map_err(|_| "not an image this app can read".to_owned())?;
    let animated = match format {
        ImageFormat::Gif => true,
        ImageFormat::WebP => WebPDecoder::new(Cursor::new(&bytes)).is_ok_and(|d| d.has_animation()),
        _ => false,
    };
    if !animated {
        let image = image::load_from_memory_with_format(&bytes, format)
            .map_err(|e| e.to_string())?
            .into_rgba8();
        return Ok(vec![compose(&image, STILL_WIDTH, blur, 0)]);
    }
    // Frames are scaled as they are decoded; when they do not fit the budget the animation is decoded again
    // at a smaller size.
    let mut width = ANIM_WIDTH;
    loop {
        match animation(&bytes, format, width, blur)? {
            Some(frames) => return Ok(frames),
            None if width > 240 => width = width * 7 / 10,
            None => return Err("the animation is too long to keep in memory".to_owned()),
        }
    }
}

/// All frames at `width`, or `None` when they would take more than the budget.
fn animation(
    bytes: &[u8],
    format: ImageFormat,
    width: u32,
    blur: u16,
) -> Result<Option<Vec<Frame>>, String> {
    let frames = match format {
        ImageFormat::Gif => GifDecoder::new(Cursor::new(bytes))
            .map_err(|e| e.to_string())?
            .into_frames(),
        _ => WebPDecoder::new(Cursor::new(bytes))
            .map_err(|e| e.to_string())?
            .into_frames(),
    };
    let mut out = Vec::new();
    let mut used = 0usize;
    for frame in frames.take(ANIM_FRAMES) {
        let frame = frame.map_err(|e| e.to_string())?;
        let (numer, denom) = frame.delay().numer_denom_ms();
        let ms = numer.checked_div(denom).unwrap_or(0);
        // Browsers play "as fast as possible" frames at 10 a second; so does this.
        let delay_ms = if ms < 20 { 100 } else { ms.max(MIN_DELAY_MS) };
        let composed = compose(&frame.into_buffer(), width, blur, delay_ms);
        used += composed.wall.as_bytes().len() + composed.frost.as_bytes().len();
        if used > ANIM_BUDGET {
            return Ok(None);
        }
        out.push(composed);
    }
    Ok(Some(out))
}

/// Scales `image` to at most `max_width`, flattens it on black, blurs it as configured and makes the frosted
/// copy.
pub(super) fn compose(image: &RgbaImage, max_width: u32, blur: u16, delay_ms: u32) -> Frame {
    // A blurred picture needs fewer pixels: every 6 px of blur halves... roughly; never below 480 wide.
    let wanted = if blur == 0 {
        max_width
    } else {
        (max_width * 6 / (6 + u32::from(blur)))
            .max(480)
            .min(max_width)
    };
    let mut wall = fit(image, wanted);
    flatten(&mut wall);
    if blur > 0 {
        let sigma = f32::from(blur) * wall.width() as f32 / BLUR_BASE;
        if sigma >= 0.3 {
            wall = imageops::fast_blur(&wall, sigma);
        }
    }
    let frost = imageops::fast_blur(&fit(&wall, FROST_WIDTH), FROST_SIGMA);
    Frame {
        wall: buffer(&wall),
        frost: buffer(&frost),
        delay_ms,
    }
}

/// `image` no wider than `max_width` (never enlarged), keeping its shape.
fn fit(image: &RgbaImage, max_width: u32) -> RgbaImage {
    let (w, h) = image.dimensions();
    if w <= max_width || w == 0 || h == 0 {
        return image.clone();
    }
    let height = ((u64::from(h) * u64::from(max_width) / u64::from(w)) as u32).max(1);
    imageops::thumbnail(image, max_width, height)
}

/// Transparent pixels become black: the wallpaper is the bottom layer.
pub(super) fn flatten(image: &mut RgbaImage) {
    for pixel in image.pixels_mut() {
        let a = u16::from(pixel.0[3]);
        if a < 255 {
            for c in &mut pixel.0[..3] {
                *c = ((u16::from(*c) * a) / 255) as u8;
            }
            pixel.0[3] = 255;
        }
    }
}

fn buffer(image: &RgbaImage) -> SharedPixelBuffer<Rgba8Pixel> {
    SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(image.as_raw(), image.width(), image.height())
}
