use std::fmt::Write as _;
use std::time::Duration;

pub(crate) const RENDER_INTERVAL: Duration = Duration::from_millis(40);
pub(crate) const PIXEL_STAGGER: Duration = Duration::from_millis(110);
pub(crate) const DURATION: Duration = Duration::from_millis(950);
pub(crate) const ORDER: [usize; 8] = [0, 1, 2, 5, 8, 7, 6, 3];
pub(crate) const KITTY_OUTER_SPREAD: u16 = 1;

const BASE_OPACITY: f32 = 0.15;
const OPACITY_RANGE: f32 = 0.85;
const SIXEL_Y_OFFSET: u16 = 2;
const KITTY_CHUNK_SIZE: usize = 4096;
const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct Raster {
    pub(crate) width: u16,
    pub(crate) height: u16,
    pub(crate) rgba: Vec<u8>,
}

impl Raster {
    pub(crate) fn pixel(&self, x: u16, y: u16) -> [u8; 4] {
        let offset = (usize::from(y) * usize::from(self.width) + usize::from(x)) * 4;
        self.rgba[offset..offset + 4]
            .try_into()
            .expect("RGBA pixel")
    }
}

pub(crate) fn intensities(elapsed: Duration) -> [f32; 9] {
    let mut values = [0.0; 9];
    for (sequence, cell) in ORDER.into_iter().enumerate() {
        let delay = PIXEL_STAGGER * sequence as u32;
        if elapsed < delay {
            continue;
        }
        let phase =
            (elapsed - delay).as_secs_f32() % DURATION.as_secs_f32() / DURATION.as_secs_f32();
        let opacity = if phase <= 0.40 {
            BASE_OPACITY + OPACITY_RANGE * css_ease_in_out(phase / 0.40)
        } else if phase <= 0.55 {
            1.0
        } else {
            1.0 - OPACITY_RANGE * css_ease_in_out((phase - 0.55) / 0.45)
        };
        values[cell] = ((opacity - BASE_OPACITY) / OPACITY_RANGE).clamp(0.0, 1.0);
    }
    values
}

pub(crate) fn rasterize(
    values: [f32; 9],
    width: u16,
    height: u16,
    foreground: (u8, u8, u8),
    background: (u8, u8, u8),
    outer_spread: u16,
) -> Raster {
    let width = width.max(1);
    let height = height.max(1);
    let mut rgba = vec![0; usize::from(width) * usize::from(height) * 4];
    let side = width.min(height);
    let x_offset = (width - side) / 2;
    let y_offset = (height - side) / 2;
    let point_side = ((side / 5).max(1) | 1).saturating_sub(1).max(1);
    let point_offset = point_side / 2;

    for row in 0..3u16 {
        for column in 0..3u16 {
            let logical = usize::from(row * 3 + column);
            if logical == 4 {
                continue;
            }
            let intensity = values[logical].clamp(0.0, 1.0);
            if intensity == 0.0 {
                continue;
            }
            let color = blend(background, foreground, intensity);
            let center_x = (x_offset + ((2 * column + 1) * side) / 6)
                .saturating_add(column * outer_spread)
                .saturating_sub(outer_spread);
            let center_y = (y_offset + ((2 * row + 1) * side) / 6)
                .saturating_add(row * outer_spread)
                .saturating_sub(outer_spread);
            let start_x = center_x
                .saturating_sub(point_offset)
                .min(width - point_side);
            let start_y = center_y
                .saturating_sub(point_offset)
                .min(height - point_side);
            for y in start_y..start_y + point_side {
                for x in start_x..start_x + point_side {
                    let offset = (usize::from(y) * usize::from(width) + usize::from(x)) * 4;
                    rgba[offset..offset + 4].copy_from_slice(&[color.0, color.1, color.2, u8::MAX]);
                }
            }
        }
    }

    Raster {
        width,
        height,
        rgba,
    }
}

pub(crate) fn encode_sixel(raster: &Raster) -> String {
    let palette = palette(raster);
    let mut output = format!("\x1bP9;1;0q\"1;1;{};{}", raster.width, raster.height);
    for (index, &(red, green, blue)) in palette.iter().enumerate() {
        let _ = write!(
            output,
            "#{index};2;{};{};{}",
            percent(red),
            percent(green),
            percent(blue)
        );
    }

    for band_top in (0..raster.height).step_by(6) {
        let mut wrote_plane = false;
        for (index, &color) in palette.iter().enumerate() {
            if !band_contains(raster, band_top, color) {
                continue;
            }
            if wrote_plane {
                output.push('$');
            }
            wrote_plane = true;
            let _ = write!(output, "#{index}");
            for x in 0..raster.width {
                let mut mask = 0u8;
                for bit in 0..6u16 {
                    let Some(source_y) = band_top.checked_add(bit + SIXEL_Y_OFFSET) else {
                        continue;
                    };
                    if source_y < raster.height && color_at(raster, x, source_y) == Some(color) {
                        mask |= 1 << bit;
                    }
                }
                output.push(char::from(b'?' + mask));
            }
        }
        if band_top + 6 < raster.height {
            if wrote_plane {
                output.push('$');
            }
            output.push('-');
        }
    }
    output.push_str("\x1b\\");
    output
}

pub(crate) fn encode_kitty(raster: &Raster, image_id: u32) -> String {
    let payload = base64(&raster.rgba);
    let mut chunks = payload.as_bytes().chunks(KITTY_CHUNK_SIZE).peekable();
    let mut output = String::new();
    let mut first = true;
    while let Some(chunk) = chunks.next() {
        let more = u8::from(chunks.peek().is_some());
        if first {
            let _ = write!(
                output,
                "\x1b_Ga=T,t=d,f=32,s={},v={},c=1,q=2,i={image_id},m={more};",
                raster.width, raster.height
            );
            first = false;
        } else {
            let _ = write!(output, "\x1b_Gm={more};");
        }
        output.push_str(std::str::from_utf8(chunk).expect("base64 is ASCII"));
        output.push_str("\x1b\\");
    }
    output
}

pub(crate) fn kitty_delete_image(image_id: u32) -> String {
    format!("\x1b_Ga=d,d=I,i={image_id},q=2;\x1b\\")
}

fn css_ease_in_out(progress: f32) -> f32 {
    let progress = progress.clamp(0.0, 1.0);
    if progress == 0.0 || progress == 1.0 {
        return progress;
    }
    let (mut lower, mut upper) = (0.0, 1.0);
    for _ in 0..20 {
        let t = (lower + upper) / 2.0;
        if cubic_bezier_axis(t, 0.42, 0.58) < progress {
            lower = t;
        } else {
            upper = t;
        }
    }
    cubic_bezier_axis((lower + upper) / 2.0, 0.0, 1.0)
}

fn cubic_bezier_axis(t: f32, first: f32, second: f32) -> f32 {
    let inverse = 1.0 - t;
    3.0 * inverse * inverse * t * first + 3.0 * inverse * t * t * second + t * t * t
}

fn blend(from: (u8, u8, u8), to: (u8, u8, u8), intensity: f32) -> (u8, u8, u8) {
    let channel = |from: u8, to: u8| {
        (f32::from(from) + (f32::from(to) - f32::from(from)) * intensity).round() as u8
    };
    (
        channel(from.0, to.0),
        channel(from.1, to.1),
        channel(from.2, to.2),
    )
}

fn palette(raster: &Raster) -> Vec<(u8, u8, u8)> {
    let mut colors = Vec::with_capacity(8);
    for pixel in raster.rgba.chunks_exact(4) {
        let color = (pixel[0], pixel[1], pixel[2]);
        if pixel[3] >= 128 && !colors.contains(&color) {
            colors.push(color);
        }
    }
    colors
}

fn band_contains(raster: &Raster, band_top: u16, color: (u8, u8, u8)) -> bool {
    (band_top..raster.height.min(band_top + 6)).any(|y| {
        y.checked_add(SIXEL_Y_OFFSET).is_some_and(|source_y| {
            source_y < raster.height
                && (0..raster.width).any(|x| color_at(raster, x, source_y) == Some(color))
        })
    })
}

fn color_at(raster: &Raster, x: u16, y: u16) -> Option<(u8, u8, u8)> {
    let [red, green, blue, alpha] = raster.pixel(x, y);
    (alpha >= 128).then_some((red, green, blue))
}

fn percent(value: u8) -> u16 {
    u16::from(value) * 100 / 255
}

fn base64(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let first = chunk[0];
        let second = chunk.get(1).copied().unwrap_or(0);
        let third = chunk.get(2).copied().unwrap_or(0);
        output.push(char::from(BASE64[usize::from(first >> 2)]));
        output.push(char::from(
            BASE64[usize::from(((first & 0b11) << 4) | (second >> 4))],
        ));
        output.push(if chunk.len() > 1 {
            char::from(BASE64[usize::from(((second & 0b1111) << 2) | (third >> 6))])
        } else {
            '='
        });
        output.push(if chunk.len() > 2 {
            char::from(BASE64[usize::from(third & 0b11_1111)])
        } else {
            '='
        });
    }
    output
}
