//! Front-panel activity lights (power, floppy drives, hard disk) drawn below
//! the picture, so loading is visibly in progress.

use crate::board::Leds;

/// Height of the front-panel strip (activity lights) below the picture.
pub const PANEL_H: usize = 24;

/// 3x5 pixel glyphs for the panel labels.
fn glyph(c: char) -> [u8; 5] {
    match c {
        'P' => [0b110, 0b101, 0b110, 0b100, 0b100],
        'W' => [0b101, 0b101, 0b101, 0b111, 0b101],
        'R' => [0b110, 0b101, 0b110, 0b101, 0b101],
        'D' => [0b110, 0b101, 0b101, 0b101, 0b110],
        'F' => [0b111, 0b100, 0b110, 0b100, 0b100],
        'H' => [0b101, 0b101, 0b111, 0b101, 0b101],
        '0' => [0b111, 0b101, 0b101, 0b101, 0b111],
        '1' => [0b010, 0b110, 0b010, 0b010, 0b111],
        '2' => [0b111, 0b001, 0b111, 0b100, 0b111],
        '3' => [0b111, 0b001, 0b011, 0b001, 0b111],
        _ => [0; 5],
    }
}

fn fill(frame: &mut [u8], fw: usize, x: usize, y: usize, w: usize, h: usize, rgb: [u8; 3]) {
    let rows = frame.len() / 4 / fw;
    for yy in y..(y + h).min(rows) {
        for xx in x..(x + w).min(fw) {
            let o = (yy * fw + xx) * 4;
            frame[o..o + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 0xff]);
        }
    }
}

/// Draw `text` at 2x scale; returns the x position after it.
fn text(frame: &mut [u8], fw: usize, mut x: usize, y: usize, s: &str, rgb: [u8; 3]) -> usize {
    for c in s.chars() {
        let g = glyph(c);
        for (row, bits) in g.iter().enumerate() {
            for col in 0..3 {
                if bits & (4 >> col) != 0 {
                    fill(frame, fw, x + col * 2, y + row * 2, 2, 2, rgb);
                }
            }
        }
        x += 8;
    }
    x
}

/// Draw the front panel (power, floppy and hard disk lights) into the RGBA
/// `frame`, `fw` pixels wide, in the strip starting at row `y0`.
pub fn draw(frame: &mut [u8], fw: usize, y0: usize, leds: &Leds, hd_active: bool, tick: u64) {
    const LABEL: [u8; 3] = [0x90, 0x90, 0x90];
    fill(frame, fw, 0, y0, fw, PANEL_H, [0x1c, 0x1c, 0x1c]);
    let (ty, ly) = (y0 + 7, y0 + 8);
    let blink = (tick / 4) % 2 == 0;
    let mut x = 16;
    let light = |frame: &mut [u8], x: &mut usize, label: &str, rgb: [u8; 3]| {
        *x = text(frame, fw, *x, ty, label, LABEL) + 4;
        fill(frame, fw, *x, ly, 24, 8, rgb);
        *x += 24 + 24;
    };
    let power = if leds.power { [0xff, 0x30, 0x30] } else { [0x80, 0x18, 0x18] };
    light(frame, &mut x, "PWR", power);
    for (i, d) in leds.floppy.iter().enumerate() {
        let Some((motor, dma)) = *d else { continue };
        let rgb = match (motor, dma) {
            (_, true) if blink => [0xff, 0xe0, 0x30],
            (true, _) => [0x30, 0xff, 0x40],
            _ => [0x10, 0x30, 0x14],
        };
        light(frame, &mut x, &format!("DF{i}"), rgb);
    }
    if leds.hd.is_some() {
        let rgb = if hd_active && blink { [0xff, 0xa0, 0x20] } else if hd_active { [0xc0, 0x60, 0x10] } else { [0x30, 0x1c, 0x08] };
        light(frame, &mut x, "HD", rgb);
    }
}

