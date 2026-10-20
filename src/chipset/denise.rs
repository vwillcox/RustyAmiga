//! Denise: bitplane serialisation, sprites, priorities, collisions and colour output.
//!
//! Bitplane data arrives as Agnus writes BPLxDAT. When BPL1DAT is written the six
//! data registers are serialised into a per-line buffer of plane bits indexed by
//! horizontal position in hires pixels. Register changes that affect colour output
//! (palette, BPLCON0/2, sprite registers) are recorded with their horizontal
//! position and replayed while composing the line, so copper effects land on the
//! correct pixel.

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
    pub data: u16,
    pub datb: u16,
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
    val: u16,
}

pub struct Denise {
    pub color: [u16; 32],
    pub bplcon0: u16,
    pub bplcon1: u16,
    pub bplcon2: u16,
    pub bpldat: [u16; 6],
    pub spr: [Sprite; 8],
    pub clxcon: u16,
    pub clxdat: u16,
    /// ECS BPLCON3 (border blanking, sprites in border).
    pub bplcon3: u16,
    pub diwstrt: u16,
    pub diwstop: u16,

    planes: Vec<u8>,
    events: Vec<Event>,
    /// Register state at the start of the current line (replayed with events).
    line_color: [u16; 32],
    line_bplcon0: u16,
    line_bplcon2: u16,
    line_spr: [Sprite; 8],
    line_has_data: bool,

    pub fb: Vec<u32>,
    rgb: [u32; 4096],
}

#[inline]
fn rgba(c: u16) -> u32 {
    let r = ((c >> 8) & 15) as u32 * 17;
    let g = ((c >> 4) & 15) as u32 * 17;
    let b = (c & 15) as u32 * 17;
    r | g << 8 | b << 16 | 0xff00_0000
}

impl Default for Denise {
    fn default() -> Self {
        Self::new()
    }
}

impl Denise {
    pub fn new() -> Self {
        let mut rgb = [0u32; 4096];
        for (i, v) in rgb.iter_mut().enumerate() {
            *v = rgba(i as u16);
        }
        Denise {
            color: [0; 32],
            bplcon0: 0,
            bplcon1: 0,
            bplcon2: 0,
            bpldat: [0; 6],
            spr: [Sprite::default(); 8],
            clxcon: 0,
            clxdat: 0,
            bplcon3: 0,
            diwstrt: 0,
            diwstop: 0,
            planes: vec![0; LINE_W + 64],
            events: Vec::with_capacity(256),
            line_color: [0; 32],
            line_bplcon0: 0,
            line_bplcon2: 0,
            line_spr: [Sprite::default(); 8],
            line_has_data: false,
            fb: vec![0xff00_0000; FB_W * FB_H],
            rgb,
        }
    }

    pub fn reset(&mut self) {
        self.bplcon0 = 0;
        self.bplcon1 = 0;
        self.bplcon2 = 0;
        self.clxcon = 0;
        for s in self.spr.iter_mut() {
            s.armed = false;
        }
    }

    /// Planes Denise displays. With BPU=7 on OCS/ECS, Agnus fetches only four
    /// planes but Denise shows six; planes 5 and 6 come from whatever was last
    /// written to BPL5DAT/BPL6DAT (an undocumented trick used by demos).
    pub fn display_planes(bplcon0: u16) -> usize {
        if (bplcon0 >> 12) & 7 == 7 {
            6
        } else {
            Self::nplanes(bplcon0)
        }
    }

    /// Planes fetched by bitplane DMA.
    pub fn nplanes(bplcon0: u16) -> usize {
        let n = ((bplcon0 >> 12) & 7) as usize;
        if n > 6 {
            4
        } else {
            n
        }
    }

    /// Record a register write that affects composition at hires position `x`.
    pub fn write(&mut self, reg: u16, val: u16, x: u16) {
        match reg {
            0x100 => self.bplcon0 = val,
            0x102 => {
                self.bplcon1 = val;
                return;
            }
            0x104 => self.bplcon2 = val,
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
                        s.data = val;
                        s.armed = true;
                    }
                    _ => s.datb = val,
                }
            }
            0x180..=0x1be => self.color[((reg - 0x180) >> 1) as usize] = val & 0xfff,
            _ => return,
        }
        self.events.push(Event { x, reg, val });
    }

    /// BPLxDAT write. Writing BPL1DAT (plane 0) serialises all planes at `x0`
    /// (hires pixel position of the first output pixel before scrolling).
    pub fn write_bpldat(&mut self, plane: usize, val: u16, x0: usize) {
        self.bpldat[plane] = val;
        if plane != 0 {
            return;
        }
        let hires = self.bplcon0 & 0x8000 != 0;
        let n = Self::display_planes(self.bplcon0);
        let pw = if hires { 1 } else { 2 };
        let odd_delay = ((self.bplcon1 & 15) * 2) as usize;
        let even_delay = (((self.bplcon1 >> 4) & 15) * 2) as usize;
        self.line_has_data = true;
        for p in 0..n.max(1) {
            let delay = if p & 1 == 0 { odd_delay } else { even_delay };
            let bit = 1u8 << p;
            let data = if p < n { self.bpldat[p] } else { 0 };
            let start = x0 + delay;
            for i in 0..16 {
                let set = data & (0x8000 >> i) != 0;
                for k in 0..pw {
                    let x = start + i * pw + k;
                    if x < self.planes.len() {
                        if set {
                            self.planes[x] |= bit;
                        } else {
                            self.planes[x] &= !bit;
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
        self.line_spr = self.spr;
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
        let mut spr = self.line_spr;
        let mut ev = 0usize;

        let hstart = (self.diwstrt & 0xff) as usize;
        let hstop = (self.diwstop & 0xff) as usize | 0x100;

        // Sprite shifters: remaining lowres pixels, data, datb.
        let mut sh_cnt = [0u8; 8];
        let mut sh_a = [0u16; 8];
        let mut sh_b = [0u16; 8];

        let mut ham: u16 = color[0];
        let mut out = [0u32; FB_W];

        let mut clx = 0u16;
        let clxcon = self.clxcon;

        let x_first = FB_X0_LORES * 2;
        for i in 0..FB_W {
            let x = x_first + i;
            while ev < self.events.len() && self.events[ev].x as usize <= x {
                let e = self.events[ev];
                match e.reg {
                    0x100 => bplcon0 = e.val,
                    0x104 => bplcon2 = e.val,
                    0x140..=0x17e => {
                        let n = ((e.reg - 0x140) >> 3) as usize;
                        match e.reg & 6 {
                            0 => spr[n].pos = e.val,
                            2 => {
                                spr[n].ctl = e.val;
                                spr[n].armed = false;
                            }
                            4 => {
                                spr[n].data = e.val;
                                spr[n].armed = true;
                            }
                            _ => spr[n].datb = e.val,
                        }
                    }
                    0x180..=0x1be => color[((e.reg - 0x180) >> 1) as usize] = e.val & 0xfff,
                    _ => {}
                }
                ev += 1;
            }

            let xl = x >> 1;
            // Sprite comparators run at lowres resolution.
            if x & 1 == 0 {
                for n in 0..8 {
                    if spr[n].armed && spr[n].hstart() as usize == xl {
                        sh_cnt[n] = 16;
                        sh_a[n] = spr[n].data;
                        sh_b[n] = spr[n].datb;
                    }
                }
            }

            let inside = vdiw && xl >= hstart && xl < hstop;
            let nplanes = Self::display_planes(bplcon0);
            let pf = if inside { self.planes[x] & ((1u8 << nplanes) - 1) } else { 0 };

            // Sprite pixel: lowest numbered non-transparent sprite wins.
            let mut spr_col: Option<(usize, u16)> = None;
            let mut spr_bits = 0u8;
            for n in 0..8 {
                if sh_cnt[n] == 0 {
                    continue;
                }
                let a = (sh_a[n] >> 15) & 1;
                let b = (sh_b[n] >> 15) & 1;
                let v = a | b << 1;
                if v != 0 {
                    spr_bits |= 1 << n;
                }
                if x & 1 == 1 {
                    sh_a[n] <<= 1;
                    sh_b[n] <<= 1;
                    sh_cnt[n] -= 1;
                }
                if spr_col.is_some() {
                    continue;
                }
                if n & 1 == 0 && spr[n + 1].attached() && sh_cnt[n + 1] > 0 {
                    let a2 = (sh_a[n + 1] >> 15) & 1;
                    let b2 = (sh_b[n + 1] >> 15) & 1;
                    let v4 = v | a2 << 2 | b2 << 3;
                    if v4 != 0 {
                        spr_col = Some((n >> 1, 16 + v4));
                    }
                } else if v != 0 {
                    if n & 1 == 1 && spr[n].attached() {
                        // Attached odd sprite pixel alone.
                        spr_col = Some((n >> 1, 16 + (v << 2)));
                    } else {
                        spr_col = Some((n >> 1, 16 + (n as u16 >> 1) * 4 + v));
                    }
                }
            }

            let dpf = bplcon0 & 0x400 != 0;
            let ham_mode = bplcon0 & 0x800 != 0 && !dpf;
            let c = if !inside {
                if self.bplcon3 & 0x20 != 0 {
                    // BRDRBLNK: border is black instead of COLOR00.
                    0
                } else {
                    color[0]
                }
            } else {
                // Playfield colour and visibility.
                let (pf_col, pf1_vis, pf2_vis, pf2_front) = if dpf {
                    let p1 = (pf & 1) | ((pf >> 1) & 2) | ((pf >> 2) & 4);
                    let p2 = ((pf >> 1) & 1) | ((pf >> 2) & 2) | ((pf >> 3) & 4);
                    let pf2_front = bplcon2 & 0x40 != 0;
                    let col = if p2 != 0 && (pf2_front || p1 == 0) {
                        color[8 + p2 as usize]
                    } else if p1 != 0 {
                        color[p1 as usize]
                    } else {
                        color[0]
                    };
                    (col, p1 != 0, p2 != 0, pf2_front)
                } else if ham_mode && nplanes >= 5 {
                    let v = (pf & 15) as u16;
                    ham = match pf >> 4 {
                        0 => color[v as usize],
                        1 => (ham & 0xff0) | v,
                        2 => (ham & 0x0ff) | v << 8,
                        _ => (ham & 0xf0f) | v << 4,
                    };
                    (ham, pf != 0, false, false)
                } else if nplanes == 6 && !ham_mode {
                    // Extra half-brite.
                    let c = color[(pf & 31) as usize];
                    let c = if pf & 32 != 0 { (c >> 1) & 0x777 } else { c };
                    (c, pf != 0, false, false)
                } else {
                    (color[pf as usize & 31], pf != 0, false, false)
                };
                match spr_col {
                    Some((pair, sc)) => {
                        let pf1p = (bplcon2 & 7) as usize;
                        let pf2p = ((bplcon2 >> 3) & 7) as usize;
                        let in_front = if dpf {
                            let front_pf_hides = (pf1_vis && pair >= pf1p) || (pf2_vis && pair >= pf2p);
                            let _ = pf2_front;
                            !front_pf_hides
                        } else {
                            !pf1_vis || pair < pf2p
                        };
                        if in_front {
                            color[sc as usize]
                        } else {
                            pf_col
                        }
                    }
                    None => pf_col,
                }
            };
            out[i] = self.rgb[c as usize];

            if inside && clxcon != 0xffff {
                clx |= Self::collisions(clxcon, pf, spr_bits);
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
