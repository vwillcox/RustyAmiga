//! The card's blitter: Picasso96's BoardInfo drawing functions, done straight on
//! video RAM the way WinUAE's picasso96 module does them. rtg.library leaves
//! these to the card, so without them nothing but lines would be drawn.
//!
//! Pens and colours are stored as a 68k would write them (big-endian), except
//! 24-bit ones, which WinUAE stores low byte first.

use super::{
    bytes_per_pixel, read32, vram_base, RGBFB_A8B8G8R8, RGBFB_A8R8G8B8, RGBFB_B5G5R5PC, RGBFB_B8G8R8A8,
    RGBFB_R5G5B5, RGBFB_R5G5B5PC, RGBFB_R8G8B8A8,
};
use crate::board::Board;
use crate::bus::Bus;
use crate::cpu::Cpu;
use crate::expansion::Device;

// Draw modes.
const JAM2: u8 = 1;
const COMP: u8 = 2;
const INVERS: u8 = 4;

// Minterms that need more than `minterm`.
const OP_DST: u8 = 10;
const OP_SRC: u8 = 12;
const OP_SWAP: u8 = 16;

/// The card's video RAM.
fn vram(board: &mut Board) -> Option<&mut [u8]> {
    let card = board.rtg.as_ref()?.card;
    match &mut board.exp.cards[card].dev {
        Device::Ram(v) => Some(v.as_mut_slice()),
        _ => None,
    }
}

/// A RenderInfo: bitmap memory, bytes per row and format.
#[derive(Clone, Copy)]
struct RenderInfo {
    mem: u32,
    bpr: i32,
}

fn render_info(board: &mut Board, a: u32) -> RenderInfo {
    RenderInfo { mem: read32(board, a), bpr: board.read16(a + 4) as i16 as i32 }
}

/// Where a rectangle of a RenderInfo lies in VRAM: its offset and the row stride.
/// Like WinUAE's validatecoords, the width is clipped to the row, and a
/// rectangle outside VRAM is not drawn.
fn area(board: &Board, ri: RenderInfo, bpp: usize, x: u32, y: u32, w: &mut u32, h: u32) -> Option<(usize, usize)> {
    let (base, size) = vram_base(board)?;
    if *w == 0 || h == 0 || bpp == 0 || ri.bpr < 0 || [*w, h, x, y].iter().any(|&v| v > 32767) {
        return None;
    }
    let (x, y, stride) = (x as usize, y as usize, ri.bpr as usize);
    // A zero BytesPerRow repeats the same line.
    let row = match stride {
        0 if x != 0 => return None,
        0 => *w as usize * bpp,
        s => s,
    };
    if x * bpp >= row {
        return None;
    }
    if (x + *w as usize) * bpp > row {
        *w = (row / bpp - x) as u32;
    }
    if ri.mem < base {
        return None;
    }
    let start = (ri.mem - base) as usize;
    let end = start + (y + h as usize - 1) * stride + (x + *w as usize) * bpp;
    (end <= size as usize).then_some((start + y * stride + x * bpp, stride))
}

/// A pen or colour as the bytes of one pixel.
fn pixel(v: u32, bpp: usize) -> [u8; 4] {
    match bpp {
        1 => [v as u8, 0, 0, 0],
        2 => [(v >> 8) as u8, v as u8, 0, 0],
        3 => [v as u8, (v >> 8) as u8, (v >> 16) as u8, 0],
        _ => v.to_be_bytes(),
    }
}

/// The value of one pixel's bytes, the inverse of `pixel`.
fn value(p: &[u8]) -> u32 {
    match *p {
        [a] => a as u32,
        [a, b] => (a as u32) << 8 | b as u32,
        [a, b, c] => a as u32 | (b as u32) << 8 | (c as u32) << 16,
        [a, b, c, d] => u32::from_be_bytes([a, b, c, d]),
        _ => 0,
    }
}

/// The colour bits in each byte of a pixel, leaving out alpha and unused bits.
fn colour_mask(fmt: u32) -> [u8; 4] {
    match fmt {
        RGBFB_A8R8G8B8 | RGBFB_A8B8G8R8 => [0, 0xff, 0xff, 0xff],
        RGBFB_R8G8B8A8 | RGBFB_B8G8R8A8 => [0xff, 0xff, 0xff, 0],
        RGBFB_R5G5B5PC | RGBFB_B5G5R5PC => [0xff, 0x7f, 0, 0],
        RGBFB_R5G5B5 => [0x7f, 0xff, 0, 0],
        _ => [0xff; 4],
    }
}

/// One of the 16 minterms on a byte of source and destination; `m` keeps
/// the operations that invert the destination off the alpha bits.
fn minterm(op: u8, s: u8, d: u8, m: u8) -> u8 {
    match op {
        0 => 0,
        1 => !(s | d),
        2 => d & !s,
        3 => !s,
        4 => s & !d & m,
        5 => !d & m,
        6 => s ^ d,
        7 => !(s & d),
        8 => s & d,
        9 => !(s ^ d),
        10 => d,
        11 => !s | d,
        12 => s,
        13 => !d | s,
        14 => s | d,
        _ => 0xff,
    }
}

/// Write one pixel; on 8-bit screens only the bits in `mask` change.
fn put(d: &mut [u8], v: &[u8; 4], mask: u8) {
    if d.len() == 1 {
        d[0] = d[0] & !mask | v[0] & mask;
    } else {
        d.copy_from_slice(&v[..d.len()]);
    }
}

/// Complement one pixel's colour bits (COMP draw mode).
fn complement(d: &mut [u8], cm: &[u8; 4], mask: u8) {
    if d.len() == 1 {
        d[0] ^= mask;
    } else {
        for (p, m) in d.iter_mut().zip(cm) {
            *p ^= m;
        }
    }
}

fn rect(cpu: &Cpu) -> (u32, u32, u32, u32) {
    (cpu.d[0] & 0xffff, cpu.d[1] & 0xffff, cpu.d[2] & 0xffff, cpu.d[3] & 0xffff)
}

/// FillRect: a1 RenderInfo, d0-d3 rectangle, d4 pen, d5.b mask, d7 format.
pub fn fill_rect(cpu: &Cpu, board: &mut Board) -> u32 {
    let ri = render_info(board, cpu.a[1]);
    let (x, y, mut w, h) = rect(cpu);
    let mask = cpu.d[5] as u8;
    let bpp = bytes_per_pixel(cpu.d[7]) as usize;
    let Some((mut off, stride)) = area(board, ri, bpp, x, y, &mut w, h) else { return 1 };
    let pen = pixel(cpu.d[4], bpp);
    let vram = vram(board).unwrap();
    for _ in 0..h {
        for p in vram[off..off + w as usize * bpp].chunks_exact_mut(bpp) {
            put(p, &pen, mask);
        }
        off += stride;
    }
    1
}

/// InvertRect: a1 RenderInfo, d0-d3 rectangle, d4.b mask, d7 format.
pub fn invert_rect(cpu: &Cpu, board: &mut Board) -> u32 {
    let ri = render_info(board, cpu.a[1]);
    let (x, y, mut w, h) = rect(cpu);
    let bpp = bytes_per_pixel(cpu.d[7]) as usize;
    let mask = if bpp > 1 { 0xff } else { cpu.d[4] as u8 };
    let Some((mut off, stride)) = area(board, ri, bpp, x, y, &mut w, h) else { return 1 };
    let vram = vram(board).unwrap();
    for _ in 0..h {
        for p in &mut vram[off..off + w as usize * bpp] {
            *p ^= mask;
        }
        off += stride;
    }
    1
}

/// Copy a rectangle with a minterm, or (with a colour key) leaving out the
/// pixels of that colour.
#[allow(clippy::too_many_arguments)]
fn blit(
    board: &mut Board,
    src: RenderInfo,
    dst: RenderInfo,
    (sx, sy, dx, dy): (u32, u32, u32, u32),
    (mut w, h): (u32, u32),
    mask: u8,
    op: u8,
    fmt: u32,
    key: Option<u32>,
) -> u32 {
    let bpp = bytes_per_pixel(fmt) as usize;
    let dst = if dst.mem == src.mem { src } else { dst };
    let Some((soff, sstride)) = area(board, src, bpp, sx, sy, &mut w, h) else { return 1 };
    let Some((doff, dstride)) = area(board, dst, bpp, dx, dy, &mut w, h) else { return 1 };
    let width = w as usize * bpp;
    let cm = colour_mask(fmt);
    let key = key.map(|k| {
        let p = pixel(k, bpp);
        std::array::from_fn::<u8, 4, _>(|i| p[i] & cm[i])
    });
    let vram = vram(board).unwrap();
    let mut s = vec![0u8; width];
    // Go up the screen when copying down it, so overlapping areas come out right.
    let mut rows: Vec<usize> = (0..h as usize).collect();
    if doff > soff {
        rows.reverse();
    }
    for r in rows {
        let (so, d) = (soff + r * sstride, doff + r * dstride);
        s.copy_from_slice(&vram[so..so + width]);
        if op == OP_SWAP && key.is_none() {
            let m = if bpp == 1 { mask } else { 0xff };
            for i in 0..width {
                let (sv, dv) = (s[i], vram[d + i]);
                vram[d + i] = dv & !m | sv & m;
                vram[so + i] = sv & !m | dv & m;
            }
            continue;
        }
        let d = &mut vram[d..d + width];
        if let Some(k) = key {
            for (dp, sp) in d.chunks_exact_mut(bpp).zip(s.chunks_exact(bpp)) {
                if (0..bpp).any(|i| sp[i] & cm[i] != k[i]) {
                    dp.copy_from_slice(sp);
                }
            }
        } else if bpp == 1 && mask != 0xff {
            for (dp, &sv) in d.iter_mut().zip(&s) {
                *dp = *dp & !mask | minterm(op, sv, *dp, 0xff) & mask;
            }
        } else if op == OP_SRC {
            d.copy_from_slice(&s);
        } else if op != OP_DST {
            for (i, (dp, &sv)) in d.iter_mut().zip(&s).enumerate() {
                *dp = minterm(op, sv, *dp, cm[i % bpp]);
            }
        }
    }
    1
}

/// BlitRect: a1 RenderInfo, d0/d1 source, d2/d3 destination, d4/d5 size,
/// d6.b mask, d7 format.
pub fn blit_rect(cpu: &Cpu, board: &mut Board) -> u32 {
    let ri = render_info(board, cpu.a[1]);
    let (sx, sy, dx, dy) = rect(cpu);
    let size = (cpu.d[4] & 0xffff, cpu.d[5] & 0xffff);
    blit(board, ri, ri, (sx, sy, dx, dy), size, cpu.d[6] as u8, OP_SRC, cpu.d[7], None)
}

/// BlitRectNoMaskComplete: a1 source and a2 destination RenderInfo, d0-d5 as
/// BlitRect, d6.b minterm, d7 format.
pub fn blit_rect_no_mask_complete(cpu: &Cpu, board: &mut Board) -> u32 {
    let (src, dst) = (render_info(board, cpu.a[1]), render_info(board, cpu.a[2]));
    let (sx, sy, dx, dy) = rect(cpu);
    let size = (cpu.d[4] & 0xffff, cpu.d[5] & 0xffff);
    blit(board, src, dst, (sx, sy, dx, dy), size, 0xff, cpu.d[6] as u8, cpu.d[7], None)
}

/// BlitRectTransparent: as BlitRectNoMaskComplete, with d6 the colour to leave out.
pub fn blit_rect_transparent(cpu: &Cpu, board: &mut Board) -> u32 {
    let (src, dst) = (render_info(board, cpu.a[1]), render_info(board, cpu.a[2]));
    let (sx, sy, dx, dy) = rect(cpu);
    let size = (cpu.d[4] & 0xffff, cpu.d[5] & 0xffff);
    blit(board, src, dst, (sx, sy, dx, dy), size, 0xff, OP_SRC, cpu.d[7], Some(cpu.d[6]))
}

/// Expand a one-bit image into the rectangle with the draw mode's pens.
#[allow(clippy::too_many_arguments)]
fn expand(
    board: &mut Board,
    (off, stride): (usize, usize),
    (w, h): (u32, u32),
    bpp: usize,
    fmt: u32,
    mask: u8,
    mode: u8,
    (fg, bg): (u32, u32),
    bit: impl Fn(usize, usize) -> bool,
) {
    let (fg, bg) = (pixel(fg, bpp), pixel(bg, bpp));
    let cm = colour_mask(fmt);
    let invert = mode & INVERS != 0;
    let vram = vram(board).unwrap();
    for r in 0..h as usize {
        let row = off + r * stride;
        for (c, p) in vram[row..row + w as usize * bpp].chunks_exact_mut(bpp).enumerate() {
            let set = bit(r, c) != invert;
            match mode & 3 {
                JAM2 => put(p, if set { &fg } else { &bg }, mask),
                COMP if set => complement(p, &cm, mask),
                COMP => {}
                _ if set => put(p, &fg, mask),
                _ => {}
            }
        }
    }
}

/// BlitTemplate: a1 RenderInfo, a2 Template, d0-d3 rectangle, d4 mask, d7 format.
pub fn blit_template(cpu: &Cpu, board: &mut Board) -> u32 {
    let ri = render_info(board, cpu.a[1]);
    let t = cpu.a[2];
    let mem = read32(board, t);
    let bpr = board.read16(t + 4) as i16 as i32;
    let (xoff, mode) = (board.read8(t + 6) as usize, board.read8(t + 7));
    let pens = (read32(board, t + 8), read32(board, t + 12));
    let (x, y, mut w, h) = rect(cpu);
    let fmt = cpu.d[7];
    let bpp = bytes_per_pixel(fmt) as usize;
    let Some(at) = area(board, ri, bpp, x, y, &mut w, h) else { return 1 };
    // The template's bits, from XOffset on.
    let n = (xoff % 8 + w as usize).div_ceil(8);
    let mut bits = vec![0u8; n * h as usize];
    for r in 0..h as usize {
        let a = mem.wrapping_add((r as i32 * bpr) as u32).wrapping_add((xoff / 8) as u32);
        for i in 0..n {
            bits[r * n + i] = board.read8(a.wrapping_add(i as u32));
        }
    }
    let skip = xoff % 8;
    expand(board, at, (w, h), bpp, fmt, cpu.d[4] as u8, mode, pens, |r, c| {
        let b = skip + c;
        bits[r * n + b / 8] & 0x80 >> (b % 8) != 0
    });
    1
}

/// BlitPattern: a1 RenderInfo, a2 Pattern, d0-d3 rectangle, d4 mask, d7 format.
pub fn blit_pattern(cpu: &Cpu, board: &mut Board) -> u32 {
    let ri = render_info(board, cpu.a[1]);
    let p = cpu.a[2];
    let mem = read32(board, p);
    let (xoff, yoff) = (board.read16(p + 4) as usize, board.read16(p + 6) as usize);
    let pens = (read32(board, p + 8), read32(board, p + 12));
    let (size, mode) = (board.read8(p + 16), board.read8(p + 17));
    let (x, y, mut w, h) = rect(cpu);
    let fmt = cpu.d[7];
    let bpp = bytes_per_pixel(fmt) as usize;
    if size > 16 {
        return 1;
    }
    let Some(at) = area(board, ri, bpp, x, y, &mut w, h) else { return 1 };
    // One word per row, 2^Size rows, repeated; started XOffset/YOffset in.
    let lines: Vec<u16> = (0..1u32 << size).map(|i| board.read16(mem.wrapping_add(i * 2))).collect();
    let ymask = lines.len() - 1;
    expand(board, at, (w, h), bpp, fmt, cpu.d[4] as u8, mode, pens, |r, c| {
        let line = lines[(r + yoff) & ymask].rotate_left((xoff & 15) as u32);
        line & 0x8000 >> (c % 16) != 0
    });
    1
}

/// Colour indexes of a rectangle of a planar BitMap, row by row. Plane
/// pointers of 0 and -1 stand for planes of all zeros and all ones.
fn planar(board: &mut Board, bm: u32, (sx, sy): (u32, u32), (w, h): (u32, u32)) -> (Vec<u8>, u8) {
    let bpr = board.read16(bm) as u32;
    let depth = board.read8(bm + 5).min(8);
    let (w, h) = (w as usize, h as usize);
    let mut px = vec![0u8; w * h];
    let first = sx / 8;
    let n = ((sx as usize + w).div_ceil(8)) - first as usize;
    let mut row = vec![0u8; n];
    for k in 0..depth {
        let plane = read32(board, bm + 8 + k as u32 * 4);
        match plane {
            0 => {}
            0xffff_ffff => px.iter_mut().for_each(|p| *p |= 1 << k),
            _ => {
                for r in 0..h {
                    let a = plane.wrapping_add((sy + r as u32) * bpr + first);
                    for (i, b) in row.iter_mut().enumerate() {
                        *b = board.read8(a.wrapping_add(i as u32));
                    }
                    for c in 0..w {
                        let b = (sx % 8) as usize + c;
                        if row[b / 8] & 0x80 >> (b % 8) != 0 {
                            px[r * w + c] |= 1 << k;
                        }
                    }
                }
            }
        }
    }
    let depth_mask = if depth == 8 { 0xff } else { (1u8 << depth) - 1 };
    (px, depth_mask)
}

/// BlitPlanar2Chunky: a1 BitMap, a2 RenderInfo, d0/d1 source, d2/d3
/// destination, d4/d5 size, d6.b minterm, d7.b mask.
pub fn planar_to_chunky(cpu: &Cpu, board: &mut Board) -> u32 {
    let ri = render_info(board, cpu.a[2]);
    let (sx, sy, dx, dy) = rect(cpu);
    let (mut w, h) = (cpu.d[4] & 0xffff, cpu.d[5] & 0xffff);
    let (op, mask) = (cpu.d[6] as u8, cpu.d[7] as u8);
    let Some((off, stride)) = area(board, ri, 1, dx, dy, &mut w, h) else { return 1 };
    let (px, _) = planar(board, cpu.a[1], (sx, sy), (w, h));
    let vram = vram(board).unwrap();
    for r in 0..h as usize {
        let row = off + r * stride;
        for (d, &v) in vram[row..row + w as usize].iter_mut().zip(&px[r * w as usize..]) {
            *d = *d & !mask | minterm(op, v, *d, 0xff) & mask;
        }
    }
    1
}

/// BlitPlanar2Direct: as BlitPlanar2Chunky, with a3 the ColorIndexMapping
/// that gives each colour index its pixel value.
pub fn planar_to_direct(cpu: &Cpu, board: &mut Board) -> u32 {
    let ri = render_info(board, cpu.a[2]);
    let fmt = read32(board, cpu.a[2] + 8);
    let bpp = bytes_per_pixel(fmt) as usize;
    let (sx, sy, dx, dy) = rect(cpu);
    let (mut w, h) = (cpu.d[4] & 0xffff, cpu.d[5] & 0xffff);
    let (op, mask) = (cpu.d[6] as u8, cpu.d[7] as u8);
    let Some((off, stride)) = area(board, ri, bpp, dx, dy, &mut w, h) else { return 1 };
    let (px, depth_mask) = planar(board, cpu.a[1], (sx, sy), (w, h));
    let colours: Vec<u32> = (0..=depth_mask as u32).map(|i| read32(board, cpu.a[3] + 4 + i * 4)).collect();
    let cm = colour_mask(fmt);
    let alpha_free = value(&cm[..bpp]);
    let vram = vram(board).unwrap();
    for r in 0..h as usize {
        let row = off + r * stride;
        for (d, &p) in vram[row..row + w as usize * bpp].chunks_exact_mut(bpp).zip(&px[r * w as usize..]) {
            let (v, vi) = ((p & mask) as usize, ((p ^ mask) & depth_mask) as usize);
            let c = value(d);
            let out = match op {
                0 => colours[0],
                1 => !(colours[v] | c),
                2 => colours[vi] & c,
                3 => colours[vi],
                4 => colours[v] & (c ^ alpha_free),
                5 => c ^ alpha_free,
                6 => colours[v] ^ c,
                7 => !(colours[v] & c),
                8 => colours[v] & c,
                9 => !(colours[v] ^ c),
                10 => c,
                11 => colours[vi] | c,
                12 => colours[v],
                13 => colours[v] | (c ^ alpha_free),
                14 => colours[v] | c,
                _ => colours[depth_mask as usize],
            };
            d.copy_from_slice(&pixel(out, bpp)[..bpp]);
        }
    }
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixels_round_trip() {
        for bpp in 1..=4 {
            let v = 0x1234_5678u32 & (u32::MAX >> (32 - 8 * bpp));
            assert_eq!(value(&pixel(v, bpp)[..bpp]), v, "{bpp} bytes");
        }
        // 16-bit pens are written as a 68k would, 24-bit ones low byte first.
        assert_eq!(pixel(0xabcd, 2)[..2], [0xab, 0xcd]);
        assert_eq!(pixel(0x123456, 3)[..3], [0x56, 0x34, 0x12]);
    }

    #[test]
    fn minterms_match_their_names() {
        let (s, d) = (0b1100u8, 0b1010u8);
        assert_eq!(minterm(OP_SRC, s, d, 0xff), s);
        assert_eq!(minterm(OP_DST, s, d, 0xff), d);
        assert_eq!(minterm(6, s, d, 0xff), 0b0110); // EOR
        assert_eq!(minterm(2, s, d, 0xff), 0b0010); // ONLYDST
        assert_eq!(minterm(5, s, d, 0x0f), 0b0101); // NOTDST keeps alpha clear
    }
}
