//! Denise (OCS/ECS) and Lisa (AGA): bitplane serialisation, sprites, priorities,
//! collisions and colour output.
//!
//! Bitplane data arrives as Agnus writes BPLxDAT. When BPL1DAT is written the
//! data registers are serialised into a per-line buffer of plane bits indexed by
//! horizontal position in hires pixels. Register changes that affect colour output
//! (palette, BPLCONx, sprite registers) are recorded with their horizontal
//! position and replayed while composing the line, so copper effects land on the
//! correct pixel.
//!
//! Colours are kept as 24-bit RGB. OCS/ECS colour registers are 12-bit and are
//! expanded by repeating each nibble, which is also what an AGA write with
//! LOCT clear does.

/// Line buffer width in hires pixels.
pub const LINE_W: usize = 1024;
/// Framebuffer width (hires pixels) and the first lowres coordinate shown.
pub const FB_W: usize = 720;
pub const FB_X0_LORES: usize = 0x6d;
/// First displayed raster line and number of lines per field.
pub const FB_Y0: u16 = 0x1a;
pub const FB_LINES: usize = 286;
pub const FB_H: usize = FB_LINES * 2;

#[derive(Clone, Copy, Default, Debug)]
pub struct Sprite {
    pub pos: u16,
    pub ctl: u16,
    /// Shift register contents, MSB first (16, 32 or 64 bits wide on AGA).
    pub data: u64,
    pub datb: u64,
    pub armed: bool,
}

impl Sprite {
    /// Horizontal start in lowres pixels (9 bits).
    pub fn hstart(&self) -> u16 {
        (self.pos & 0xff) << 1 | (self.ctl & 1)
    }
    pub fn vstart(&self) -> u16 {
        (self.pos >> 8) | (self.ctl & 4) << 6
    }
    pub fn vstop(&self) -> u16 {
        (self.ctl >> 8) | (self.ctl & 2) << 7
    }
    pub fn attached(&self) -> bool {
        self.ctl & 0x80 != 0
    }
}

#[derive(Clone, Copy, Debug)]
struct Event {
    x: u16,
    reg: u16,
    /// Register value; for colours (reg $180) the palette index << 24 | RGB.
    val: u64,
}

const EV_COLOR: u16 = 0x180;

pub struct Denise {
    /// AGA Lisa rather than OCS/ECS Denise.
    pub aga: bool,
    pub color: [u32; 256],
    pub bplcon0: u16,
    pub bplcon1: u16,
    pub bplcon2: u16,
    /// ECS/AGA BPLCON3 (border blanking, colour bank, LOCT, sprite resolution).
    pub bplcon3: u16,
    /// AGA BPLCON4 (bitplane colour XOR, sprite colour banks).
    pub bplcon4: u16,
    pub bpldat: [u64; 8],
    pub spr: [Sprite; 8],
    pub clxcon: u16,
    pub clxdat: u16,
    pub diwstrt: u16,
    pub diwstop: u16,
    /// Sprite shift register width in bits (AGA FMODE).
    pub spr_width: u32,

    planes: Vec<u8>,
    events: Vec<Event>,
    /// Register state at the start of the current line (replayed with events).
    line_color: [u32; 256],
    line_bplcon0: u16,
    line_bplcon2: u16,
    line_bplcon4: u16,
    line_spr: [Sprite; 8],
    line_has_data: bool,

    pub fb: Vec<u32>,
}

/// 24-bit RGB to the framebuffer's RGBA byte order.
#[inline]
fn rgba(c: u32) -> u32 {
    let r = (c >> 16) & 0xff;
    let g = (c >> 8) & 0xff;
    let b = c & 0xff;
    r | g << 8 | b << 16 | 0xff00_0000
}

/// Expand a 12-bit colour register value to 24 bits.
#[inline]
fn rgb12(v: u16) -> u32 {
    let r = ((v >> 8) & 15) as u32;
    let g = ((v >> 4) & 15) as u32;
    let b = (v & 15) as u32;
    (r * 0x11) << 16 | (g * 0x11) << 8 | b * 0x11
}

impl Default for Denise {
    fn default() -> Self {
        Self::new()
    }
}

impl Denise {
    pub fn new() -> Self {
        Denise {
            aga: false,
            color: [0; 256],
            bplcon0: 0,
            bplcon1: 0,
            bplcon2: 0,
            bplcon3: 0x0c00,
            bplcon4: 0x0011,
            bpldat: [0; 8],
            spr: [Sprite::default(); 8],
            clxcon: 0,
            clxdat: 0,
            diwstrt: 0,
            diwstop: 0,
            spr_width: 16,
            planes: vec![0; LINE_W + 256],
            events: Vec::with_capacity(256),
            line_color: [0; 256],
            line_bplcon0: 0,
            line_bplcon2: 0,
            line_bplcon4: 0x0011,
            line_spr: [Sprite::default(); 8],
            line_has_data: false,
            fb: vec![0xff00_0000; FB_W * FB_H],
        }
    }

    pub fn with_aga(aga: bool) -> Self {
        Denise { aga, ..Self::new() }
    }

    pub fn reset(&mut self) {
        self.bplcon0 = 0;
        self.bplcon1 = 0;
        self.bplcon2 = 0;
        self.bplcon3 = 0x0c00;
        self.bplcon4 = 0x0011;
        self.clxcon = 0;
        self.spr_width = 16;
        for s in self.spr.iter_mut() {
            s.armed = false;
        }
    }

    /// BPU field: planes requested by BPLCON0 (AGA adds bit 4 for 8 planes).
    fn bpu(&self, bplcon0: u16) -> usize {
        let n = ((bplcon0 >> 12) & 7) as usize;
        if self.aga && bplcon0 & 0x10 != 0 {
            8
        } else {
            n
        }
    }

    /// Planes Denise displays. With BPU=7 on OCS/ECS, Agnus fetches only four
    /// planes but Denise shows six; planes 5 and 6 come from whatever was last
    /// written to BPL5DAT/BPL6DAT (an undocumented trick used by demos).
    pub fn display_planes(&self, bplcon0: u16) -> usize {
        let n = self.bpu(bplcon0);
        if !self.aga && n == 7 {
            6
        } else {
            n
        }
    }

    /// Planes fetched by bitplane DMA.
    pub fn nplanes(&self, bplcon0: u16) -> usize {
        let n = self.bpu(bplcon0);
        if !self.aga && n > 6 {
            4
        } else {
            n
        }
    }

    /// Record a register write that affects composition at hires position `x`.
    pub fn write(&mut self, reg: u16, val: u16, x: u16) {
        let mut ev = val as u64;
        match reg {
            0x100 => self.bplcon0 = val,
            0x102 => {
                self.bplcon1 = val;
                return;
            }
            0x104 => self.bplcon2 = val,
            0x10c => self.bplcon4 = val,
            0x140..=0x17e => {
                let n = ((reg - 0x140) >> 3) as usize;
                let s = &mut self.spr[n];
                match reg & 6 {
                    0 => s.pos = val,
                    2 => {
                        s.ctl = val;
                        s.armed = false;
                    }
                    4 => {
                        s.data = (val as u64) << 48;
                        s.armed = true;
                        ev = s.data;
                    }
                    _ => {
                        s.datb = (val as u64) << 48;
                        ev = s.datb;
                    }
                }
            }
            0x180..=0x1be => {
                let mut idx = ((reg - 0x180) >> 1) as usize;
                let rgb = if self.aga {
                    idx |= ((self.bplcon3 >> 13) as usize) << 5;
                    let old = self.color[idx];
                    if self.bplcon3 & 0x200 != 0 {
                        // LOCT: set the low nibbles only.
                        let r = ((val >> 8) & 15) as u32;
                        let g = ((val >> 4) & 15) as u32;
                        let b = (val & 15) as u32;
                        (old & 0xf0f0f0) | r << 16 | g << 8 | b
                    } else {
                        rgb12(val)
                    }
                } else {
                    rgb12(val & 0xfff)
                };
                self.color[idx] = rgb;
                self.events.push(Event { x, reg: EV_COLOR, val: (idx as u64) << 24 | rgb as u64 });
                return;
            }
            _ => return,
        }
        self.events.push(Event { x, reg, val: ev });
    }

    /// Wide sprite data from sprite DMA (`bits` = 16, 32 or 64, MSB-aligned in `v`).
    pub fn write_sprite_data(&mut self, n: usize, datb: bool, v: u64, x: u16) {
        let s = &mut self.spr[n];
        if datb {
            s.datb = v;
            self.events.push(Event { x, reg: 0x146 + n as u16 * 8, val: v });
        } else {
            s.data = v;
            s.armed = true;
            self.events.push(Event { x, reg: 0x144 + n as u16 * 8, val: v });
        }
    }

    /// BPLxDAT write of `bits` bits (16, 32 or 64; MSB-aligned in `val`). Writing
    /// BPL1DAT (plane 0) serialises all planes at `x0` (hires pixel position of
    /// the first output pixel before scrolling).
    pub fn write_bpldat(&mut self, plane: usize, val: u64, bits: u32, x0: usize) {
        self.bpldat[plane] = val;
        if plane != 0 {
            return;
        }
        // Pixel width in half-hires units: lores 4, hires 2, super-hires 1.
        let hw = if self.aga && self.bplcon0 & 0x40 != 0 {
            1
        } else if self.bplcon0 & 0x8000 != 0 {
            2
        } else {
            4
        };
        let n = self.display_planes(self.bplcon0);
        let c1 = self.bplcon1 as usize;
        // Scroll delays in super-hires pixels (AGA adds high and low bits).
        let (odd, even) = if self.aga {
            (
                (c1 >> 10 & 3) << 6 | (c1 & 15) << 2 | (c1 >> 8 & 3),
                (c1 >> 14 & 3) << 6 | (c1 >> 4 & 15) << 2 | (c1 >> 12 & 3),
            )
        } else {
            ((c1 & 15) << 2, (c1 >> 4 & 15) << 2)
        };
        self.line_has_data = true;
        for p in 0..n.max(1) {
            let delay = if p & 1 == 0 { odd } else { even } >> 1;
            let bit = 1u8 << p;
            let data = if p < n { self.bpldat[p] } else { 0 };
            let start = x0 + delay;
            for i in 0..bits as usize {
                let set = data & (1u64 << (63 - i)) != 0;
                // Super-hires pixels are half a buffer pixel: keep every other one.
                if hw == 1 && i & 1 == 1 {
                    continue;
                }
                let (x, w) = if hw == 1 { (start + i / 2, 1) } else { (start + i * hw / 2, hw / 2) };
                for k in 0..w {
                    if let Some(px) = self.planes.get_mut(x + k) {
                        if set {
                            *px |= bit;
                        } else {
                            *px &= !bit;
                        }
                    }
                }
            }
        }
    }

    /// Called at the start of each raster line.
    pub fn begin_line(&mut self) {
        if self.line_has_data {
            self.planes.fill(0);
            self.line_has_data = false;
        }
        self.events.clear();
        self.line_color = self.color;
        self.line_bplcon0 = self.bplcon0;
        self.line_bplcon2 = self.bplcon2;
        self.line_bplcon4 = self.bplcon4;
        self.line_spr = self.spr;
    }

    /// Sprite horizontal start in hires pixels (AGA adds a 70 ns position bit).
    fn spr_hstart(&self, s: &Sprite) -> usize {
        let h = (s.hstart() as usize) << 1;
        if self.aga {
            h | ((s.ctl >> 4) & 1) as usize
        } else {
            h
        }
    }

    /// Compose the finished line into the framebuffer.
    /// `vdiw` says whether the line is inside the vertical display window.
    pub fn end_line(&mut self, vpos: u16, vdiw: bool, interlace_field: Option<bool>) {
        if vpos < FB_Y0 || (vpos - FB_Y0) as usize >= FB_LINES {
            return;
        }
        let row = (vpos - FB_Y0) as usize * 2;

        let mut color = self.line_color;
        let mut bplcon0 = self.line_bplcon0;
        let mut bplcon2 = self.line_bplcon2;
        let mut bplcon4 = self.line_bplcon4;
        let mut spr = self.line_spr;
        let mut ev = 0usize;

        let hstart = (self.diwstrt & 0xff) as usize;
        let hstop = (self.diwstop & 0xff) as usize | 0x100;

        // Sprite resolution: hires pixels per sprite pixel (AGA SPRES).
        let spr_pw: u8 = if self.aga && matches!((self.bplcon3 >> 6) & 3, 2 | 3) { 1 } else { 2 };
        let spr_bits = if self.aga { self.spr_width as u8 } else { 16 };

        // Sprite shifters: remaining sprite pixels, phase within a pixel, data.
        let mut sh_cnt = [0u8; 8];
        let mut sh_ph = [0u8; 8];
        let mut sh_a = [0u64; 8];
        let mut sh_b = [0u64; 8];

        let mut ham: u32 = color[0];
        let mut out = [0u32; FB_W];

        let mut clx = 0u16;
        let clxcon = self.clxcon;
        let aga = self.aga;
        let pf2of = if aga { [0usize, 2, 4, 8, 16, 32, 64, 128][((self.bplcon3 >> 10) & 7) as usize] } else { 8 };

        let x_first = FB_X0_LORES * 2;
        for i in 0..FB_W {
            let x = x_first + i;
            while ev < self.events.len() && self.events[ev].x as usize <= x {
                let e = self.events[ev];
                match e.reg {
                    0x100 => bplcon0 = e.val as u16,
                    0x104 => bplcon2 = e.val as u16,
                    0x10c => bplcon4 = e.val as u16,
                    0x140..=0x17e => {
                        let n = ((e.reg - 0x140) >> 3) as usize;
                        match e.reg & 6 {
                            0 => spr[n].pos = e.val as u16,
                            2 => {
                                spr[n].ctl = e.val as u16;
                                spr[n].armed = false;
                            }
                            4 => {
                                spr[n].data = e.val;
                                spr[n].armed = true;
                            }
                            _ => spr[n].datb = e.val,
                        }
                    }
                    EV_COLOR => color[(e.val >> 24) as usize & 255] = e.val as u32 & 0xff_ffff,
                    _ => {}
                }
                ev += 1;
            }

            // Sprite comparators.
            for n in 0..8 {
                if spr[n].armed && self.spr_hstart(&spr[n]) == x {
                    sh_cnt[n] = spr_bits;
                    sh_ph[n] = 0;
                    sh_a[n] = spr[n].data;
                    sh_b[n] = spr[n].datb;
                }
            }

            let xl = x >> 1;
            let inside = vdiw && xl >= hstart && xl < hstop;
            let nplanes = self.display_planes(bplcon0);
            let pf = if inside { self.planes[x] & ((1u16 << nplanes) - 1) as u8 } else { 0 };

            // Sprite pixel: lowest numbered non-transparent sprite wins.
            let mut spr_col: Option<(usize, usize)> = None;
            let mut spr_bits_now = 0u8;
            let esprm = ((bplcon4 >> 4) & 15) as usize;
            let osprm = (bplcon4 & 15) as usize;
            for n in 0..8 {
                if sh_cnt[n] == 0 {
                    continue;
                }
                let a = (sh_a[n] >> 63) as usize;
                let b = (sh_b[n] >> 63) as usize;
                let v = a | b << 1;
                if v != 0 {
                    spr_bits_now |= 1 << n;
                }
                sh_ph[n] += 1;
                if sh_ph[n] == spr_pw {
                    sh_ph[n] = 0;
                    sh_a[n] <<= 1;
                    sh_b[n] <<= 1;
                    sh_cnt[n] -= 1;
                }
                if spr_col.is_some() {
                    continue;
                }
                let (bank_e, bank_o) = if aga { (esprm << 4, osprm << 4) } else { (16, 16) };
                if n & 1 == 0 && spr[n + 1].attached() && sh_cnt[n + 1] > 0 {
                    let a2 = (sh_a[n + 1] >> 63) as usize;
                    let b2 = (sh_b[n + 1] >> 63) as usize;
                    let v4 = v | a2 << 2 | b2 << 3;
                    if v4 != 0 {
                        spr_col = Some((n >> 1, bank_o + v4));
                    }
                } else if v != 0 {
                    if n & 1 == 1 && spr[n].attached() {
                        // Attached odd sprite pixel alone.
                        spr_col = Some((n >> 1, bank_o + (v << 2)));
                    } else {
                        let bank = if n & 1 == 0 { bank_e } else { bank_o };
                        spr_col = Some((n >> 1, bank + (n >> 1) * 4 + v));
                    }
                }
            }

            let dpf = bplcon0 & 0x400 != 0;
            let ham_mode = bplcon0 & 0x800 != 0 && !dpf;
            let xor = if aga { (bplcon4 >> 8) as usize } else { 0 };
            let c = if !inside {
                if self.bplcon3 & 0x20 != 0 {
                    // BRDRBLNK: border is black instead of COLOR00.
                    0
                } else {
                    color[0]
                }
            } else {
                // Playfield colour and visibility.
                let (pf_col, pf1_vis, pf2_vis) = if dpf {
                    let mut p1 = 0usize;
                    let mut p2 = 0usize;
                    for k in 0..4 {
                        p1 |= ((pf >> (2 * k)) as usize & 1) << k;
                        p2 |= ((pf >> (2 * k + 1)) as usize & 1) << k;
                    }
                    let pf2_front = bplcon2 & 0x40 != 0;
                    let col = if p2 != 0 && (pf2_front || p1 == 0) {
                        color[(p2 + pf2of) & 255]
                    } else if p1 != 0 {
                        color[p1]
                    } else {
                        color[0]
                    };
                    (col, p1 != 0, p2 != 0)
                } else if ham_mode && nplanes == 8 {
                    // HAM8: two control bits (planes 1-2) and six data bits.
                    let v = (pf >> 2) as u32;
                    ham = match pf & 3 {
                        0 => color[(pf as usize >> 2) ^ (xor >> 2)],
                        1 => (ham & 0xffff03) | v << 2,
                        2 => (ham & 0x03ffff) | v << 18,
                        _ => (ham & 0xff03ff) | v << 10,
                    };
                    (ham, pf != 0, false)
                } else if ham_mode && nplanes >= 5 {
                    let v = (pf & 15) as u32 * 0x11;
                    ham = match pf >> 4 & 3 {
                        0 => color[((pf & 15) as usize) ^ (xor & 15)],
                        1 => (ham & 0xffff00) | v,
                        2 => (ham & 0x00ffff) | v << 16,
                        _ => (ham & 0xff00ff) | v << 8,
                    };
                    (ham, pf != 0, false)
                } else if nplanes == 6 && !(aga && bplcon2 & 0x200 != 0) {
                    // Extra half-brite (AGA can disable it with KILLEHB).
                    let c = color[(pf as usize & 31) ^ (xor & 31)];
                    let c = if pf & 32 != 0 { (c >> 1) & 0x7f7f7f } else { c };
                    (c, pf != 0, false)
                } else {
                    let idx = if aga { pf as usize ^ xor } else { pf as usize & 31 };
                    (color[idx & 255], pf != 0, false)
                };
                match spr_col {
                    Some((pair, sc)) => {
                        let pf1p = (bplcon2 & 7) as usize;
                        let pf2p = ((bplcon2 >> 3) & 7) as usize;
                        let in_front = if dpf {
                            !((pf1_vis && pair >= pf1p) || (pf2_vis && pair >= pf2p))
                        } else {
                            !pf1_vis || pair < pf2p
                        };
                        if in_front {
                            color[sc & 255]
                        } else {
                            pf_col
                        }
                    }
                    None => pf_col,
                }
            };
            out[i] = rgba(c);

            if inside && clxcon != 0xffff {
                clx |= Self::collisions(clxcon, pf, spr_bits_now);
            }
        }
        self.clxdat |= clx;

        match interlace_field {
            None => {
                self.fb[row * FB_W..(row + 1) * FB_W].copy_from_slice(&out);
                self.fb[(row + 1) * FB_W..(row + 2) * FB_W].copy_from_slice(&out);
            }
            Some(long) => {
                let r = row + if long { 0 } else { 1 };
                self.fb[r * FB_W..(r + 1) * FB_W].copy_from_slice(&out);
            }
        }
    }

    fn collisions(clxcon: u16, pf: u8, spr_bits: u8) -> u16 {
        let enbp = ((clxcon >> 6) & 0x3f) as u8;
        let mvbp = (clxcon & 0x3f) as u8;
        let matched = !(pf ^ mvbp) & enbp;
        let odd_mask = enbp & 0b010101;
        let even_mask = enbp & 0b101010;
        let pf1 = matched & 0b010101 == odd_mask;
        let pf2 = matched & 0b101010 == even_mask;
        // Odd sprites only participate if enabled in CLXCON bits 12-15.
        let ensp = (clxcon >> 12) & 15;
        let mut group = [false; 4];
        for (g, item) in group.iter_mut().enumerate() {
            let even = spr_bits & (1 << (g * 2)) != 0;
            let odd = ensp & (1 << g) != 0 && spr_bits & (1 << (g * 2 + 1)) != 0;
            *item = even || odd;
        }
        let mut c = 0u16;
        if pf1 && pf2 {
            c |= 1;
        }
        for g in 0..4 {
            if group[g] {
                if pf1 {
                    c |= 1 << (1 + g);
                }
                if pf2 {
                    c |= 1 << (5 + g);
                }
            }
        }
        let pairs = [(0, 1), (0, 2), (0, 3), (1, 2), (1, 3), (2, 3)];
        for (k, &(a, b)) in pairs.iter().enumerate() {
            if group[a] && group[b] {
                c |= 1 << (9 + k);
            }
        }
        c
    }

    pub fn read_clxdat(&mut self) -> u16 {
        let v = self.clxdat | 0x8000;
        self.clxdat = 0;
        v
    }
}
