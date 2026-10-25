//! MPEG-1 video (ISO 11172-2): the elementary stream goes in as it arrives,
//! and pictures come out in display order.
//!
//! I and P pictures are held back as references until the next one is
//! decoded; B pictures come out at once. Motion compensation clamps reads at
//! the picture edge, so damaged vectors cannot reach outside the frame.
//!
//! Time stamps from the system layer go in with [`Decoder::stamp`] and come
//! out on the pictures they belong to, in display order.

use super::vlc::{tables, ESCAPE, MB_BACKWARD, MB_ESCAPE, MB_FORWARD, MB_INTRA, MB_PATTERN, MB_QUANT, MB_STUFFING};
use super::Bits;
use std::collections::VecDeque;
use std::sync::OnceLock;

/// Scan position to coefficient position (row-major) in an 8x8 block.
const ZIGZAG: [u8; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57,
    50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

const DEFAULT_INTRA_QUANT: [u8; 64] = [
    8, 16, 19, 22, 26, 27, 29, 34, 16, 16, 22, 24, 27, 29, 34, 37, 19, 22, 26, 27, 29, 34, 34, 38, 22, 22, 26, 27, 29, 34, 37, 40, 22, 26, 27,
    29, 32, 35, 40, 48, 26, 27, 29, 32, 35, 40, 48, 58, 26, 27, 29, 34, 38, 46, 56, 69, 27, 29, 35, 38, 46, 56, 69, 83,
];

/// Pictures per second by picture_rate code.
pub const PICTURE_RATES: [f64; 9] = [0.0, 23.976, 24.0, 25.0, 29.97, 30.0, 50.0, 59.94, 60.0];

const SEQUENCE_HEADER: u8 = 0xb3;
const SEQUENCE_END: u8 = 0xb7;
const GROUP_START: u8 = 0xb8;
const PICTURE_START: u8 = 0x00;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SequenceInfo {
    pub width: u16,
    pub height: u16,
    pub aspect_ratio: u8,
    pub picture_rate: u8,
    /// In units of 400 bits/s.
    pub bit_rate: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TimeCode {
    pub drop_frame: bool,
    pub hours: u8,
    pub minutes: u8,
    pub seconds: u8,
    pub pictures: u8,
}

/// A picture in 4:2:0 Y, Cb and Cr planes, cropped to the sequence's size.
#[derive(Clone)]
pub struct Frame {
    pub width: usize,
    pub height: usize,
    pub y: Vec<u8>,
    pub cb: Vec<u8>,
    pub cr: Vec<u8>,
    pub temporal_reference: u16,
    /// The presentation time stamp (90 kHz), if the picture had one.
    pub pts: Option<u64>,
}

impl Frame {
    /// The picture as 0xRRGGBB pixels (ITU-R BT.601, studio range).
    pub fn to_rgb(&self) -> Vec<u32> {
        let (w, cw) = (self.width, self.width.div_ceil(2));
        let mut out = Vec::with_capacity(w * self.height);
        for y in 0..self.height {
            for x in 0..w {
                let c = (y / 2) * cw + x / 2;
                let l = (self.y[y * w + x] as i32 - 16) * 298;
                let (u, v) = (self.cb[c] as i32 - 128, self.cr[c] as i32 - 128);
                let r = ((l + 409 * v + 128) >> 8).clamp(0, 255) as u32;
                let g = ((l - 100 * u - 208 * v + 128) >> 8).clamp(0, 255) as u32;
                let b = ((l + 516 * u + 128) >> 8).clamp(0, 255) as u32;
                out.push(r << 16 | g << 8 | b);
            }
        }
        out
    }
}

pub enum Event {
    Sequence(SequenceInfo),
    Group(TimeCode),
    Picture(Frame),
}

/// A picture being decoded or kept as a reference: whole macroblocks.
struct Planes {
    y: Vec<u8>,
    cb: Vec<u8>,
    cr: Vec<u8>,
    temporal_reference: u16,
    pts: Option<u64>,
}

struct Sequence {
    info: SequenceInfo,
    mb_width: usize,
    mb_height: usize,
    intra_quant: [u8; 64],
    non_intra_quant: [u8; 64],
}

impl Sequence {
    fn planes(&self) -> Planes {
        let n = self.mb_width * self.mb_height * 256;
        Planes { y: vec![16; n], cb: vec![128; n / 4], cr: vec![128; n / 4], temporal_reference: 0, pts: None }
    }

    fn crop(&self, p: &Planes) -> Frame {
        let (w, h) = (self.info.width as usize, self.info.height as usize);
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        let crop = |plane: &[u8], stride: usize, w: usize, h: usize| (0..h).flat_map(|y| plane[y * stride..y * stride + w].iter().copied()).collect();
        Frame {
            width: w,
            height: h,
            y: crop(&p.y, self.mb_width * 16, w, h),
            cb: crop(&p.cb, self.mb_width * 8, cw, ch),
            cr: crop(&p.cr, self.mb_width * 8, cw, ch),
            temporal_reference: p.temporal_reference,
            pts: p.pts,
        }
    }
}

#[derive(Default)]
pub struct Decoder {
    data: Vec<u8>,
    /// The stream position of `data[0]`.
    base: u64,
    /// Time stamps by the stream position they apply from.
    stamps: VecDeque<(u64, u64)>,
    /// How far the search for the end of the unit at the front of `data` got.
    scan: usize,
    seq: Option<Sequence>,
    /// The two most recent I or P pictures; `future` is not displayed yet.
    past: Option<Planes>,
    future: Option<Planes>,
    spare: Option<Planes>,
    events: VecDeque<Event>,
}

/// The offset of the next start code prefix (00 00 01) at or after `from`.
fn find_start(data: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    while i + 3 < data.len() {
        // Skip ahead by three while the middle byte rules out a prefix here.
        if data[i + 2] > 1 {
            i += 3;
        } else if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            return Some(i);
        } else {
            i += 1;
        }
    }
    None
}

impl Decoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Stream bytes not yet decoded.
    pub fn buffered(&self) -> usize {
        self.data.len()
    }

    pub fn push(&mut self, data: &[u8]) {
        self.data.extend_from_slice(data);
    }

    /// A presentation time stamp for the first picture that starts `ahead`
    /// bytes or more past the data pushed so far (as a system stream packet
    /// gives it for the first picture that starts in the packet).
    pub fn stamp(&mut self, ahead: usize, pts: u64) {
        self.stamps.push_back((self.base + (self.data.len() + ahead) as u64, pts));
    }

    /// Drop stream bytes from the front of the buffer.
    fn consume(&mut self, n: usize) {
        self.data.drain(..n);
        self.base += n as u64;
    }

    /// The time stamp of the picture at the front of the buffer: the last
    /// one given for a position up to its start.
    fn take_stamp(&mut self) -> Option<u64> {
        let mut pts = None;
        while let Some(&(at, t)) = self.stamps.front() {
            if at > self.base {
                break;
            }
            pts = Some(t);
            self.stamps.pop_front();
        }
        pts
    }

    /// Forget the stream and the reference pictures.
    pub fn reset(&mut self) {
        *self = Decoder { seq: self.seq.take(), ..Default::default() };
    }

    /// The next sequence header, group of pictures or picture that the
    /// buffered stream holds completely.
    pub fn next_event(&mut self) -> Option<Event> {
        loop {
            if let Some(e) = self.events.pop_front() {
                return Some(e);
            }
            if !self.step() {
                return None;
            }
        }
    }

    /// The stream has ended: decode what is left as if a sequence end code
    /// followed it.
    pub fn end(&mut self) {
        self.push(&[0, 0, 1, SEQUENCE_END]);
    }

    /// Show the last reference picture (at the end of a sequence).
    fn flush(&mut self) {
        if let (Some(f), Some(seq)) = (self.future.take(), &self.seq) {
            self.events.push_back(Event::Picture(seq.crop(&f)));
            self.past = Some(f);
        }
    }

    /// Decode the unit at the front of the buffer, if all of it is there.
    fn step(&mut self) -> bool {
        let Some(start) = find_start(&self.data, 0) else {
            let keep = self.data.len().min(3);
            self.consume(self.data.len() - keep);
            self.scan = 0;
            return false;
        };
        if start > 0 {
            self.consume(start);
            self.scan = 0;
        }
        let code = self.data[3];
        if code == SEQUENCE_END {
            self.consume(4);
            self.flush();
            return true;
        }
        // A picture runs on through its slices (and any extension and user
        // data); other units end at the next start code.
        let mut at = self.scan.max(4);
        let end = loop {
            match find_start(&self.data, at) {
                None => {
                    self.scan = self.data.len().saturating_sub(3).max(4);
                    return false;
                }
                Some(p) => {
                    let next = self.data[p + 3];
                    if code != PICTURE_START || next == PICTURE_START || (next >= 0xb0 && next != 0xb2 && next != 0xb5) {
                        break p;
                    }
                    at = p + 4;
                }
            }
        };
        self.scan = 0;
        let data = std::mem::take(&mut self.data);
        match code {
            SEQUENCE_HEADER => self.sequence_header(&data[4..end]),
            GROUP_START => self.group(&data[4..end]),
            PICTURE_START => self.picture(&data[..end]),
            _ => {}
        }
        self.data = data;
        self.consume(end);
        true
    }

    fn sequence_header(&mut self, d: &[u8]) {
        let mut b = Bits::new(d);
        let width = b.read(12) as u16;
        let height = b.read(12) as u16;
        let aspect_ratio = b.read(4) as u8;
        let picture_rate = b.read(4) as u8;
        let bit_rate = b.read(18);
        b.skip(1 + 10 + 1); // marker, vbv_buffer_size, constrained_parameters_flag
        let mut intra_quant = DEFAULT_INTRA_QUANT;
        if b.bit() {
            for &z in &ZIGZAG {
                intra_quant[z as usize] = b.read(8) as u8;
            }
        }
        let mut non_intra_quant = [16; 64];
        if b.bit() {
            for &z in &ZIGZAG {
                non_intra_quant[z as usize] = b.read(8) as u8;
            }
        }
        if width == 0 || height == 0 || width > 768 || height > 576 {
            log::warn!("MPEG: unsupported picture size {width}x{height}");
            return;
        }
        let info = SequenceInfo { width, height, aspect_ratio, picture_rate, bit_rate };
        let (mb_width, mb_height) = ((width as usize).div_ceil(16), (height as usize).div_ceil(16));
        if self.seq.as_ref().is_some_and(|s| s.mb_width != mb_width || s.mb_height != mb_height) {
            self.past = None;
            self.future = None;
            self.spare = None;
        }
        self.seq = Some(Sequence { info, mb_width, mb_height, intra_quant, non_intra_quant });
        self.events.push_back(Event::Sequence(info));
    }

    fn group(&mut self, d: &[u8]) {
        let mut b = Bits::new(d);
        let drop_frame = b.bit();
        let hours = b.read(5) as u8;
        let minutes = b.read(6) as u8;
        b.skip(1);
        let seconds = b.read(6) as u8;
        let pictures = b.read(6) as u8;
        self.events.push_back(Event::Group(TimeCode { drop_frame, hours, minutes, seconds, pictures }));
    }

    fn picture(&mut self, d: &[u8]) {
        let pts = self.take_stamp();
        let Some(seq) = &self.seq else { return };
        let mut b = Bits::new(d);
        b.skip(32);
        let temporal_reference = b.read(10) as u16;
        let kind = b.read(3) as u8;
        b.skip(16); // vbv_delay
        let mut fwd = MotionParams::default();
        let mut bwd = MotionParams::default();
        for (m, used) in [(&mut fwd, kind == 2 || kind == 3), (&mut bwd, kind == 3)] {
            if used {
                m.full_pel = b.bit();
                let f_code = b.read(3);
                if f_code == 0 {
                    return;
                }
                m.r_size = f_code - 1;
            }
        }
        while b.bit() {
            b.skip(8); // extra_information_picture
        }
        // Without its reference pictures (after a seek) a picture cannot be made.
        let (fwd_ref, bwd_ref) = match kind {
            1 | 4 => (None, None),
            2 if self.future.is_some() => (self.future.as_ref(), None),
            3 if self.past.is_some() && self.future.is_some() => (self.past.as_ref(), self.future.as_ref()),
            _ => return,
        };
        let mut cur = self.spare.take().filter(|p| p.y.len() == seq.mb_width * seq.mb_height * 256).unwrap_or_else(|| seq.planes());
        cur.temporal_reference = temporal_reference;
        cur.pts = pts;
        let mut pic = PictureDecoder {
            seq,
            kind,
            fwd,
            bwd,
            fwd_ref,
            bwd_ref,
            cur: &mut cur,
            qscale: 1,
            dc_pred: [128; 3],
            mv_fwd: (0, 0),
            mv_bwd: (0, 0),
            mb_type: 0,
            block: [0; 64],
        };
        let mut next = find_start(d, b.pos().div_ceil(8));
        while let Some(p) = next {
            let code = d[p + 3];
            if (1..=0xaf).contains(&code) {
                let mut s = Bits::new(&d[p + 4..]);
                // A damaged slice ends at the error; the next slice carries on.
                pic.slice(&mut s, code as usize);
            }
            next = find_start(d, p + 4);
        }
        if kind == 3 {
            self.events.push_back(Event::Picture(seq.crop(&cur)));
            self.spare = Some(cur);
        } else {
            if let Some(f) = self.future.take() {
                self.events.push_back(Event::Picture(seq.crop(&f)));
                self.spare = self.past.replace(f);
            }
            self.future = Some(cur);
        }
    }
}

#[derive(Clone, Copy, Default)]
struct MotionParams {
    full_pel: bool,
    r_size: u32,
}

struct PictureDecoder<'a> {
    seq: &'a Sequence,
    /// picture_coding_type: 1 I, 2 P, 3 B, 4 D.
    kind: u8,
    fwd: MotionParams,
    bwd: MotionParams,
    fwd_ref: Option<&'a Planes>,
    bwd_ref: Option<&'a Planes>,
    cur: &'a mut Planes,
    qscale: u32,
    /// Y, Cb and Cr DC predictors.
    dc_pred: [i32; 3],
    /// Motion vector predictors (and the vectors of the last macroblock).
    mv_fwd: (i32, i32),
    mv_bwd: (i32, i32),
    /// The last macroblock's type, which skipped B macroblocks repeat.
    mb_type: u8,
    block: [i32; 64],
}

impl PictureDecoder<'_> {
    fn slice(&mut self, b: &mut Bits, vertical_position: usize) -> Option<()> {
        let t = tables();
        let (mb_width, count) = (self.seq.mb_width, self.seq.mb_width * self.seq.mb_height);
        self.qscale = b.read(5).max(1);
        while b.bit() {
            b.skip(8); // extra_information_slice
        }
        self.dc_pred = [128; 3];
        self.mv_fwd = (0, 0);
        self.mv_bwd = (0, 0);
        let mut addr = ((vertical_position - 1) * mb_width) as isize - 1;
        let mut first = true;
        // Macroblocks run until the next start code's 23 zero bits.
        while b.peek(23) != 0 {
            let mut inc = 0isize;
            loop {
                match t.mb_address_increment.read(b)? {
                    MB_STUFFING => {}
                    MB_ESCAPE => inc += 33,
                    v => {
                        inc += v as isize;
                        break;
                    }
                }
            }
            if addr + inc >= count as isize {
                return None;
            }
            if !first && inc > 1 {
                // Skipped macroblocks: P copies the reference picture, B
                // repeats the last macroblock's prediction.
                self.dc_pred = [128; 3];
                if self.kind == 2 {
                    self.mv_fwd = (0, 0);
                }
                for a in addr + 1..addr + inc {
                    self.predict(a as usize % mb_width, a as usize / mb_width, self.mb_type);
                }
            }
            first = false;
            addr += inc;
            if addr < 0 {
                return None;
            }
            self.macroblock(b, addr as usize % mb_width, addr as usize / mb_width)?;
        }
        Some(())
    }

    fn macroblock(&mut self, b: &mut Bits, mx: usize, my: usize) -> Option<()> {
        let t = tables();
        let ty = t.mb_type[self.kind as usize - 1].read(b)?;
        if ty & MB_QUANT != 0 {
            self.qscale = b.read(5).max(1);
        }
        let intra = ty & MB_INTRA != 0;
        if intra {
            self.mv_fwd = (0, 0);
            self.mv_bwd = (0, 0);
        } else {
            self.dc_pred = [128; 3];
            if ty & MB_FORWARD != 0 {
                let r = self.fwd.r_size;
                self.mv_fwd = (motion_component(b, r, self.mv_fwd.0)?, motion_component(b, r, self.mv_fwd.1)?);
            } else if self.kind == 2 {
                self.mv_fwd = (0, 0);
            }
            if ty & MB_BACKWARD != 0 {
                let r = self.bwd.r_size;
                self.mv_bwd = (motion_component(b, r, self.mv_bwd.0)?, motion_component(b, r, self.mv_bwd.1)?);
            }
        }
        self.mb_type = ty;
        let cbp = if ty & MB_PATTERN != 0 {
            t.coded_block_pattern.read(b)?
        } else if intra {
            0x3f
        } else {
            0
        };
        if !intra {
            self.predict(mx, my, ty);
        }
        for i in 0..6 {
            if cbp & (0x20 >> i) != 0 {
                self.block(b, i, intra, mx, my)?;
            }
        }
        if self.kind == 4 {
            b.skip(1); // end_of_macroblock
        }
        Some(())
    }

    /// Motion-compensated prediction of a macroblock.
    fn predict(&mut self, mx: usize, my: usize, ty: u8) {
        // P pictures always predict forwards (with a zero vector if none was sent).
        let (fwd, bwd) = if self.kind == 2 { (true, false) } else { (ty & MB_FORWARD != 0, ty & MB_BACKWARD != 0) };
        let mut average = false;
        for (on, r, mv, params) in [(fwd, self.fwd_ref, self.mv_fwd, self.fwd), (bwd, self.bwd_ref, self.mv_bwd, self.bwd)] {
            let (true, Some(r)) = (on, r) else { continue };
            let (vx, vy) = if params.full_pel { (mv.0 << 1, mv.1 << 1) } else { mv };
            let w = self.seq.mb_width * 16;
            let h = self.seq.mb_height * 16;
            predict_block(&mut self.cur.y, &r.y, w, h, mx * 16, my * 16, 16, vx, vy, average);
            // Chroma vectors are half the luma ones, rounded towards zero.
            let (cx, cy) = (vx / 2, vy / 2);
            predict_block(&mut self.cur.cb, &r.cb, w / 2, h / 2, mx * 8, my * 8, 8, cx, cy, average);
            predict_block(&mut self.cur.cr, &r.cr, w / 2, h / 2, mx * 8, my * 8, 8, cx, cy, average);
            average = true;
        }
    }

    fn block(&mut self, b: &mut Bits, i: usize, intra: bool, mx: usize, my: usize) -> Option<()> {
        let t = tables();
        self.block = [0; 64];
        let mut n = 0;
        if intra {
            let plane = i.saturating_sub(3);
            let size = t.dc_size[(plane > 0) as usize].read(b)? as u32;
            let diff = if size == 0 {
                0
            } else {
                let v = b.read(size) as i32;
                if v & 1 << (size - 1) != 0 {
                    v
                } else {
                    v + 1 - (1 << size)
                }
            };
            self.dc_pred[plane] += diff;
            self.block[0] = self.dc_pred[plane] * 8;
            n = 1;
        }
        let quant = if intra { &self.seq.intra_quant } else { &self.seq.non_intra_quant };
        while self.kind != 4 {
            if n > 0 && b.peek(2) == 0b10 {
                b.skip(2); // end_of_block
                break;
            }
            let c = t.dct_coeff.read(b)?;
            let (run, level) = if c == ESCAPE {
                let run = b.read(6) as usize;
                let level = match b.read(8) as i32 {
                    0 => b.read(8) as i32,
                    128 => b.read(8) as i32 - 256,
                    l if l > 128 => l - 256,
                    l => l,
                };
                (run, level)
            } else {
                if c == 1 && n > 0 {
                    b.skip(1); // "11": run 0, level 1 after the first coefficient
                }
                let level = (c & 0xff) as i32;
                ((c >> 8) as usize, if b.bit() { -level } else { level })
            };
            n += run;
            if n >= 64 {
                return None;
            }
            let pos = ZIGZAG[n] as usize;
            n += 1;
            // Dequantise (truncating towards zero), then force odd values
            // (mismatch control) and saturate.
            let q = quant[pos] as i32 * self.qscale as i32;
            let mag = level.abs();
            let mut v = if intra { (mag * q) >> 3 } else { ((2 * mag + 1) * q) >> 4 };
            if v & 1 == 0 && v != 0 {
                v -= 1;
            }
            self.block[pos] = if level < 0 { -v.min(2048) } else { v.min(2047) };
        }
        let (plane, w, x0, y0) = if i < 4 {
            (&mut self.cur.y, self.seq.mb_width * 16, mx * 16 + (i & 1) * 8, my * 16 + (i >> 1) * 8)
        } else {
            (if i == 4 { &mut self.cur.cb } else { &mut self.cur.cr }, self.seq.mb_width * 8, mx * 8, my * 8)
        };
        let mut out = [0i32; 64];
        idct(&self.block, &mut out);
        for y in 0..8 {
            let row = &mut plane[(y0 + y) * w + x0..][..8];
            for x in 0..8 {
                let v = if intra { out[y * 8 + x] } else { row[x] as i32 + out[y * 8 + x] };
                row[x] = v.clamp(0, 255) as u8;
            }
        }
        Some(())
    }
}

/// One motion vector component: the decoded difference added to the
/// predictor, wrapped into the range the f_code allows.
fn motion_component(b: &mut Bits, r_size: u32, pred: i32) -> Option<i32> {
    let code = tables().motion_code.read(b)? as i32;
    let f = 1 << r_size;
    let delta = if code != 0 && f != 1 {
        let r = b.read(r_size) as i32;
        let d = ((code.abs() - 1) << r_size) + r + 1;
        if code < 0 {
            -d
        } else {
            d
        }
    } else {
        code
    };
    let mut v = pred + delta;
    if v > (f << 4) - 1 {
        v -= f << 5;
    } else if v < -(f << 4) {
        v += f << 5;
    }
    Some(v)
}

/// Predict an `n`x`n` block at (x0, y0) of a `w`x`h` plane from `src`, moved
/// by a half-pel vector, or average with what is there (B pictures' second
/// direction).
#[allow(clippy::too_many_arguments)]
fn predict_block(dst: &mut [u8], src: &[u8], w: usize, h: usize, x0: usize, y0: usize, n: usize, vx: i32, vy: i32, average: bool) {
    let ix = x0 as i32 + (vx >> 1);
    let iy = y0 as i32 + (vy >> 1);
    let (hx, hy) = ((vx & 1) as usize, (vy & 1) as usize);
    let (wi, hi) = (w as i32 - 1, h as i32 - 1);
    let px = |x: i32, y: i32| src[y.clamp(0, hi) as usize * w + x.clamp(0, wi) as usize] as u32;
    for j in 0..n {
        let y = iy + j as i32;
        for i in 0..n {
            let x = ix + i as i32;
            let v = match (hx, hy) {
                (0, 0) => px(x, y),
                (1, 0) => (px(x, y) + px(x + 1, y) + 1) >> 1,
                (0, _) => (px(x, y) + px(x, y + 1) + 1) >> 1,
                _ => (px(x, y) + px(x + 1, y) + px(x, y + 1) + px(x + 1, y + 1) + 2) >> 2,
            };
            let d = &mut dst[(y0 + j) * w + x0 + i];
            *d = if average { ((*d as u32 + v + 1) >> 1) as u8 } else { v as u8 };
        }
    }
}

/// The 8x8 inverse DCT, separable in floating point (accurate enough for
/// IEEE 1180), skipping rows of zeros.
fn idct(input: &[i32; 64], out: &mut [i32; 64]) {
    static COS: OnceLock<[[f32; 8]; 8]> = OnceLock::new();
    let c = COS.get_or_init(|| {
        let mut c = [[0f32; 8]; 8];
        for (u, row) in c.iter_mut().enumerate() {
            let scale = if u == 0 { (0.125f64).sqrt() } else { 0.5 };
            for (x, v) in row.iter_mut().enumerate() {
                *v = (scale * ((2 * x + 1) as f64 * u as f64 * std::f64::consts::PI / 16.0).cos()) as f32;
            }
        }
        c
    });
    let mut tmp = [0f32; 64];
    for y in 0..8 {
        let row = &input[y * 8..y * 8 + 8];
        if row.iter().all(|&v| v == 0) {
            continue;
        }
        for x in 0..8 {
            tmp[y * 8 + x] = (0..8).map(|u| c[u][x] * row[u] as f32).sum();
        }
    }
    for x in 0..8 {
        for y in 0..8 {
            let s: f32 = (0..8).map(|v| c[v][y] * tmp[v * 8 + x]).sum();
            out[y * 8 + x] = s.round() as i32;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idct_of_a_dc_coefficient_is_flat() {
        let mut block = [0; 64];
        block[0] = 1024;
        let mut out = [0; 64];
        idct(&block, &mut out);
        assert!(out.iter().all(|&v| v == 128));
    }

    #[test]
    fn motion_vectors_wrap() {
        // f_code 1 (r_size 0): range -16..15; code +1 from 15 wraps to -16.
        let data = [0b0100_0000];
        let mut b = Bits::new(&data);
        assert_eq!(motion_component(&mut b, 0, 15), Some(-16));
    }

    #[test]
    fn units_wait_for_the_next_start_code() {
        let mut d = Decoder::new();
        // A 16x16 sequence header split across two pushes.
        let header = [0, 0, 1, 0xb3, 0x01, 0x00, 0x10, 0x13, 0xff, 0xff, 0xe0, 0x18];
        d.push(&header[..6]);
        assert!(d.next_event().is_none());
        d.push(&header[6..]);
        assert!(d.next_event().is_none(), "the header ends only at the next start code");
        d.push(&[0, 0, 1, 0xb7]);
        match d.next_event() {
            Some(Event::Sequence(s)) => assert_eq!((s.width, s.height, s.picture_rate), (16, 16, 3)),
            _ => panic!("no sequence header"),
        }
    }

    #[test]
    fn time_stamps_follow_their_pictures_into_display_order() {
        // A picture header without slices: temporal_reference, coding type,
        // vbv_delay, f_codes of 1 for P and B, and no extra information.
        let pic = |tr: u16, kind: u8| {
            let (mut bits, mut n): (u64, usize) = ((tr as u64) << 19 | (kind as u64) << 16 | 0xffff, 29);
            for _ in 0..[0, 0, 1, 2][kind as usize] {
                bits = bits << 4 | 1;
                n += 4;
            }
            n += 1;
            let bytes = n.div_ceil(8);
            bits <<= bytes * 8 - n + 1;
            let mut v = vec![0, 0, 1, PICTURE_START];
            v.extend_from_slice(&bits.to_be_bytes()[8 - bytes..]);
            v
        };
        let mut d = Decoder::new();
        d.push(&[0, 0, 1, 0xb3, 0x01, 0x00, 0x10, 0x13, 0xff, 0xff, 0xe0, 0x18]);
        d.stamp(0, 100);
        d.push(&pic(0, 1));
        d.stamp(0, 400);
        d.push(&pic(3, 1));
        d.stamp(0, 200);
        d.push(&pic(1, 3));
        // Inside the next picture, so it applies to none of these.
        d.stamp(4, 999);
        d.push(&pic(2, 3));
        d.end();
        let mut shown = vec![];
        while let Some(e) = d.next_event() {
            if let Event::Picture(f) = e {
                shown.push((f.temporal_reference, f.pts));
            }
        }
        assert_eq!(shown, [(0, Some(100)), (1, Some(200)), (2, None), (3, Some(400))]);
    }
}
