//! The Agnus blitter. Blits execute immediately when started; the busy flag and
//! completion interrupt are timed to approximate real hardware duration.

#[derive(Default, Clone, Debug)]
pub struct Blitter {
    pub con0: u16,
    pub con1: u16,
    pub afwm: u16,
    pub alwm: u16,
    pub apt: u32,
    pub bpt: u32,
    pub cpt: u32,
    pub dpt: u32,
    pub amod: i16,
    pub bmod: i16,
    pub cmod: i16,
    pub dmod: i16,
    pub adat: u16,
    pub bdat: u16,
    pub cdat: u16,
    pub ddat: u16,
    /// ECS BLTSIZV latch.
    pub sizv: u16,

    /// Blit waiting for blitter DMA to be enabled: (width words, height).
    pub pending: Option<(u32, u32)>,
    /// Colour clock at which the running blit finishes.
    pub busy_until: Option<u64>,
    pub zero: bool,
}

pub trait ChipMem {
    fn rd(&self, addr: u32) -> u16;
    fn wr(&mut self, addr: u32, v: u16);
}

impl ChipMem for (&mut Vec<u8>, u32) {
    #[inline]
    fn rd(&self, addr: u32) -> u16 {
        let a = (addr & self.1 & !1) as usize;
        (self.0[a] as u16) << 8 | self.0[a + 1] as u16
    }
    #[inline]
    fn wr(&mut self, addr: u32, v: u16) {
        let a = (addr & self.1 & !1) as usize;
        self.0[a] = (v >> 8) as u8;
        self.0[a + 1] = v as u8;
    }
}

#[inline]
fn minterm(lf: u8, a: u16, b: u16, c: u16) -> u16 {
    let mut r = 0u16;
    if lf & 0x80 != 0 {
        r |= a & b & c;
    }
    if lf & 0x40 != 0 {
        r |= a & b & !c;
    }
    if lf & 0x20 != 0 {
        r |= a & !b & c;
    }
    if lf & 0x10 != 0 {
        r |= a & !b & !c;
    }
    if lf & 0x08 != 0 {
        r |= !a & b & c;
    }
    if lf & 0x04 != 0 {
        r |= !a & b & !c;
    }
    if lf & 0x02 != 0 {
        r |= !a & !b & c;
    }
    if lf & 0x01 != 0 {
        r |= !a & !b & !c;
    }
    r
}

impl Blitter {
    pub fn busy(&self, now: u64) -> bool {
        self.pending.is_some() || self.busy_until.is_some_and(|t| now < t)
    }

    /// Execute a blit of `w` words by `h` lines. Returns its duration in colour clocks.
    pub fn run<M: ChipMem>(&mut self, mem: &mut M, w: u32, h: u32) -> u64 {
        if self.con1 & 1 != 0 {
            self.line(mem, h)
        } else {
            self.area(mem, w, h)
        }
    }

    fn area<M: ChipMem>(&mut self, mem: &mut M, w: u32, h: u32) -> u64 {
        let use_a = self.con0 & 0x800 != 0;
        let use_b = self.con0 & 0x400 != 0;
        let use_c = self.con0 & 0x200 != 0;
        let use_d = self.con0 & 0x100 != 0;
        let lf = self.con0 as u8;
        let ash = (self.con0 >> 12) as u32;
        let bsh = (self.con1 >> 12) as u32;
        let desc = self.con1 & 2 != 0;
        let fill = self.con1 & 0x18 != 0 && desc;
        let fill_excl = self.con1 & 0x10 != 0;
        let fci = self.con1 & 4 != 0;
        let step: u32 = if desc { (-2i32) as u32 } else { 2 };
        let modv = |m: i16| if desc { (-(m as i32)) as u32 } else { m as i32 as u32 };

        let mut zero = true;
        let mut a_old: u16 = 0;
        let mut b_old: u16 = 0;
        // The D write is pipelined one word behind the source reads, which
        // matters when the destination overlaps a source.
        let mut pending: Option<(u32, u16)> = None;
        for _y in 0..h {
            let mut fc = fci;
            for x in 0..w {
                if use_a {
                    self.adat = mem.rd(self.apt);
                    self.apt = self.apt.wrapping_add(step);
                }
                if use_b {
                    self.bdat = mem.rd(self.bpt);
                    self.bpt = self.bpt.wrapping_add(step);
                }
                if use_c {
                    self.cdat = mem.rd(self.cpt);
                    self.cpt = self.cpt.wrapping_add(step);
                }
                if let Some((addr, v)) = pending.take() {
                    mem.wr(addr, v);
                }
                let mut a = self.adat;
                if x == 0 {
                    a &= self.afwm;
                }
                if x == w - 1 {
                    a &= self.alwm;
                }
                let (ah, bh) = if desc {
                    (
                        ((((a as u32) << 16) | a_old as u32) >> (16 - ash)) as u16,
                        ((((self.bdat as u32) << 16) | b_old as u32) >> (16 - bsh)) as u16,
                    )
                } else {
                    (
                        ((((a_old as u32) << 16) | a as u32) >> ash) as u16,
                        ((((b_old as u32) << 16) | self.bdat as u32) >> bsh) as u16,
                    )
                };
                a_old = a;
                b_old = self.bdat;
                let mut d = minterm(lf, ah, bh, self.cdat);
                if fill {
                    let mut out = 0u16;
                    for bit in 0..16 {
                        let v = d & (1 << bit) != 0;
                        if fill_excl {
                            fc ^= v;
                            if fc {
                                out |= 1 << bit;
                            }
                        } else {
                            if fc || v {
                                out |= 1 << bit;
                            }
                            fc ^= v;
                        }
                    }
                    d = out;
                }
                if d != 0 {
                    zero = false;
                }
                self.ddat = d;
                if use_d {
                    pending = Some((self.dpt, d));
                    self.dpt = self.dpt.wrapping_add(step);
                }
            }
            if use_a {
                self.apt = self.apt.wrapping_add(modv(self.amod));
            }
            if use_b {
                self.bpt = self.bpt.wrapping_add(modv(self.bmod));
            }
            if use_c {
                self.cpt = self.cpt.wrapping_add(modv(self.cmod));
            }
            if use_d {
                self.dpt = self.dpt.wrapping_add(modv(self.dmod));
            }
        }
        if let Some((addr, v)) = pending {
            mem.wr(addr, v);
        }
        self.zero = zero;
        let channels = use_a as u64 + use_b as u64 + use_c as u64 + use_d as u64;
        let per_word = channels.max(2);
        (w as u64 * h as u64 * per_word).max(4)
    }

    fn line<M: ChipMem>(&mut self, mem: &mut M, h: u32) -> u64 {
        let lf = self.con0 as u8;
        let use_c = self.con0 & 0x200 != 0;
        let mut ash = (self.con0 >> 12) as u32;
        let mut bsh = (self.con1 >> 12) as u32;
        let sing = self.con1 & 2 != 0;
        let sud = self.con1 & 0x10 != 0;
        let sul = self.con1 & 0x08 != 0;
        let aul = self.con1 & 0x04 != 0;
        let mut sign = self.con1 & 0x40 != 0;
        let mut onedot = false;
        let mut zero = true;
        let mut cpt = self.cpt;
        let mut dpt = self.dpt;
        let mut apt = self.apt;
        let cmod = self.cmod as i32 as u32;

        for _ in 0..h {
            if use_c {
                self.cdat = mem.rd(cpt);
            }
            let a = (self.adat & self.afwm) >> ash;
            let b = if (self.bdat >> bsh) & 1 != 0 { 0xffff } else { 0 };
            let draw = !sing || !onedot;
            let d = minterm(lf, a, b, self.cdat);
            onedot = true;
            if d != 0 {
                zero = false;
            }

            // Error term and minor-axis step.
            let incx = |ash: &mut u32, p: &mut u32| {
                *ash += 1;
                if *ash == 16 {
                    *ash = 0;
                    *p = p.wrapping_add(2);
                }
            };
            let decx = |ash: &mut u32, p: &mut u32| {
                if *ash == 0 {
                    *ash = 15;
                    *p = p.wrapping_sub(2);
                } else {
                    *ash -= 1;
                }
            };
            if !sign {
                apt = apt.wrapping_add(self.amod as i32 as u32);
                if sud {
                    if sul {
                        cpt = cpt.wrapping_sub(cmod);
                    } else {
                        cpt = cpt.wrapping_add(cmod);
                    }
                    onedot = false;
                } else if sul {
                    decx(&mut ash, &mut cpt);
                } else {
                    incx(&mut ash, &mut cpt);
                }
            } else {
                apt = apt.wrapping_add(self.bmod as i32 as u32);
            }
            // Major-axis step.
            if sud {
                if aul {
                    decx(&mut ash, &mut cpt);
                } else {
                    incx(&mut ash, &mut cpt);
                }
            } else {
                if aul {
                    cpt = cpt.wrapping_sub(cmod);
                } else {
                    cpt = cpt.wrapping_add(cmod);
                }
                onedot = false;
            }
            sign = (apt as u16 as i16) < 0;
            bsh = (bsh + 15) & 15;

            // Line mode writes through D only when channel C is enabled.
            if draw && use_c {
                mem.wr(dpt, d);
            }
            self.ddat = d;
            dpt = cpt;
        }
        self.cpt = cpt;
        self.dpt = dpt;
        self.apt = apt;
        self.con0 = (self.con0 & 0x0fff) | (ash as u16) << 12;
        self.con1 = (self.con1 & 0x0fbf) | (bsh as u16) << 12 | if sign { 0x40 } else { 0 };
        self.zero = zero;
        h as u64 * 4
    }
}
