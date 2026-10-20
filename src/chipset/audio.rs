//! Paula audio: four 8-bit DMA channels, mixed to stereo (0+3 left, 1+2 right),
//! followed by a model of the Amiga's analog output stage.
//!
//! Each channel's DAC output is a zero-order hold of `sample * volume`. The
//! analog stage is simulated at the colour clock rate (3.55 MHz) so the filters
//! behave like the real RC networks, then box-filtered down to the host rate:
//!   * fixed one-pole RC low-pass (A500/A600/A2000: ~4.4 kHz, A1200/A4000: ~34 kHz)
//!   * optional "LED" filter: 2-pole Sallen-Key low-pass, 3.09 kHz, Q 0.660,
//!     enabled when CIA-A PRA bit 1 (the power LED) is low
//!   * one-pole ~5.2 Hz high-pass from the output coupling capacitors.

#[derive(Default, Clone, Debug)]
pub struct Channel {
    pub lc: u32,
    pub len: u16,
    pub per: u16,
    pub vol: u8,
    pub dat: u16,

    pt: u32,
    len_cnt: u32,
    per_cnt: u32,
    buf: u16,
    low_byte_next: bool,
    pub out: i8,
    dma: bool,
    manual: bool,
    /// Toggles between volume and period words when modulating both.
    mod_toggle: bool,
}

#[derive(Clone, Copy, Debug)]
struct OnePole {
    a: f64,
    y: f64,
}

impl OnePole {
    fn lowpass(fc: f64, fs: f64) -> Self {
        OnePole { a: 1.0 - (-2.0 * std::f64::consts::PI * fc / fs).exp(), y: 0.0 }
    }
    #[inline]
    fn lp(&mut self, x: f64) -> f64 {
        self.y += self.a * (x - self.y);
        self.y
    }
    /// High-pass as input minus the low-passed signal.
    #[inline]
    fn hp(&mut self, x: f64) -> f64 {
        x - self.lp(x)
    }
}

/// RBJ biquad low-pass (used for the 2-pole LED filter).
#[derive(Clone, Copy, Debug)]
struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    z1: f64,
    z2: f64,
}

impl Biquad {
    fn lowpass(fc: f64, q: f64, fs: f64) -> Self {
        let w = 2.0 * std::f64::consts::PI * fc / fs;
        let alpha = w.sin() / (2.0 * q);
        let cw = w.cos();
        let a0 = 1.0 + alpha;
        Biquad {
            b0: (1.0 - cw) / 2.0 / a0,
            b1: (1.0 - cw) / a0,
            b2: (1.0 - cw) / 2.0 / a0,
            a1: -2.0 * cw / a0,
            a2: (1.0 - alpha) / a0,
            z1: 0.0,
            z2: 0.0,
        }
    }
    #[inline]
    fn run(&mut self, x: f64) -> f64 {
        // Transposed direct form II.
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }
}

#[derive(Clone, Copy, Debug)]
struct Stage {
    rc: OnePole,
    led: Biquad,
    hp: OnePole,
}

pub struct Audio {
    pub ch: [Channel; 4],
    pub sample_rate: u32,
    /// Colour clocks per second (PAL 3546895, NTSC 3579545).
    pub cck_rate: u32,
    acc: u64,
    sum_l: f64,
    sum_r: f64,
    n: u32,
    /// Interleaved stereo output samples produced since last drained.
    pub out: Vec<f32>,
    pub filter_on: bool,
    /// 1.0 = hard-panned like real hardware, 0.0 = mono.
    pub stereo_separation: f32,
    stage: [Stage; 2],
    hp_rate: OnePole,
}

/// Something that can serve audio DMA fetches.
pub trait AudioMem {
    fn fetch(&mut self, addr: u32) -> u16;
}

impl Audio {
    /// `fixed_lp_hz` is the model's fixed RC low-pass cut-off.
    pub fn new(cck_rate: u32, fixed_lp_hz: f64) -> Self {
        let fs = cck_rate as f64;
        let stage = Stage {
            rc: OnePole::lowpass(fixed_lp_hz, fs),
            led: Biquad::lowpass(3090.53, 0.660, fs),
            hp: OnePole::lowpass(5.2, 48000.0),
        };
        Audio {
            ch: Default::default(),
            sample_rate: 48000,
            cck_rate,
            acc: 0,
            sum_l: 0.0,
            sum_r: 0.0,
            n: 0,
            out: Vec::with_capacity(4096),
            filter_on: false,
            stereo_separation: 1.0,
            stage: [stage; 2],
            hp_rate: OnePole::lowpass(5.2, 48000.0),
        }
    }

    /// Change the host output rate (recomputes the rate-dependent high-pass).
    pub fn set_sample_rate(&mut self, rate: u32) {
        self.sample_rate = rate;
        self.hp_rate = OnePole::lowpass(5.2, rate as f64);
        for s in self.stage.iter_mut() {
            s.hp = self.hp_rate;
        }
    }

    pub fn reset(&mut self) {
        for c in self.ch.iter_mut() {
            *c = Channel::default();
        }
    }

    /// CPU write to AUDxDAT.
    pub fn write_dat(&mut self, i: usize, v: u16) {
        let c = &mut self.ch[i];
        c.dat = v;
        if !c.dma && !c.manual {
            c.manual = true;
            c.buf = v;
            c.low_byte_next = false;
            c.per_cnt = 0;
        }
    }

    /// Advance one colour clock. `dma_en` holds the AUDxEN bits (already ANDed
    /// with DMAEN), `adkcon` the modulation bits. Returns channels requesting an
    /// interrupt.
    pub fn tick<M: AudioMem>(&mut self, mem: &mut M, dma_en: u8, adkcon: u16) -> u8 {
        let mut irq = 0u8;
        for i in 0..4 {
            let en = dma_en & (1 << i) != 0;
            let c = &mut self.ch[i];
            if en && !c.dma {
                c.dma = true;
                c.manual = false;
                c.pt = c.lc;
                c.len_cnt = if c.len == 0 { 0x10000 } else { c.len as u32 };
                c.buf = mem.fetch(c.pt);
                c.pt = c.pt.wrapping_add(2);
                c.len_cnt -= 1;
                if c.len_cnt == 0 {
                    c.pt = c.lc;
                    c.len_cnt = if c.len == 0 { 0x10000 } else { c.len as u32 };
                }
                c.low_byte_next = false;
                c.per_cnt = 0;
                irq |= 1 << i;
            } else if !en && c.dma {
                // DMA off: the state machine idles; the DAC keeps its last level.
                c.dma = false;
            }
            if !c.dma && !c.manual {
                continue;
            }
            if c.per_cnt > 0 {
                c.per_cnt -= 1;
                continue;
            }
            let per = if c.per == 0 { 0x10000 } else { c.per as u32 };
            c.per_cnt = per - 1;

            let mod_vol = i < 3 && adkcon & (1 << i) != 0;
            let mod_per = i < 3 && adkcon & (0x10 << i) != 0;
            let mut modulate = None;
            let word_done = if mod_vol || mod_per {
                // Modulator: each whole word goes to the next channel's VOL or PER.
                let to_vol = mod_vol && (!mod_per || !c.mod_toggle);
                c.mod_toggle = !c.mod_toggle;
                c.out = 0;
                modulate = Some((to_vol, c.buf));
                true
            } else if !c.low_byte_next {
                c.out = (c.buf >> 8) as i8;
                c.low_byte_next = true;
                false
            } else {
                c.out = c.buf as i8;
                c.low_byte_next = false;
                true
            };
            if let Some((to_vol, w)) = modulate {
                let next = &mut self.ch[i + 1];
                if to_vol {
                    next.vol = (w & 0x7f) as u8;
                } else {
                    next.per = w;
                }
            }
            if word_done {
                let c = &mut self.ch[i];
                if c.dma {
                    c.buf = mem.fetch(c.pt);
                    c.pt = c.pt.wrapping_add(2);
                    c.len_cnt -= 1;
                    if c.len_cnt == 0 {
                        c.pt = c.lc;
                        c.len_cnt = if c.len == 0 { 0x10000 } else { c.len as u32 };
                        irq |= 1 << i;
                    }
                } else {
                    // Manual mode: ask the CPU for more data.
                    c.buf = c.dat;
                    irq |= 1 << i;
                }
            }
        }

        // DAC + analog output stage, at the colour clock rate.
        let v = |c: &Channel| c.out as f64 * c.vol.min(64) as f64;
        let l = v(&self.ch[0]) + v(&self.ch[3]);
        let r = v(&self.ch[1]) + v(&self.ch[2]);
        let mut y = [l, r];
        for (k, s) in self.stage.iter_mut().enumerate() {
            let mut x = s.rc.lp(y[k]);
            if self.filter_on {
                x = s.led.run(x);
            } else {
                // Keep the LED filter state tracking so toggling does not click.
                s.led.run(x);
            }
            y[k] = x;
        }
        self.sum_l += y[0];
        self.sum_r += y[1];
        self.n += 1;
        self.acc += self.sample_rate as u64;
        if self.acc >= self.cck_rate as u64 {
            self.acc -= self.cck_rate as u64;
            // Full scale: two channels of 127 * 64.
            let scale = 1.0 / (self.n as f64 * 16256.0);
            let mut l = self.sum_l * scale;
            let mut r = self.sum_r * scale;
            self.sum_l = 0.0;
            self.sum_r = 0.0;
            self.n = 0;
            l = self.stage[0].hp.hp(l);
            r = self.stage[1].hp.hp(r);
            let sep = self.stereo_separation as f64;
            let (l, r) = (l * (0.5 + sep / 2.0) + r * (0.5 - sep / 2.0), r * (0.5 + sep / 2.0) + l * (0.5 - sep / 2.0));
            if self.out.len() < 96000 {
                self.out.push(l as f32);
                self.out.push(r as f32);
            }
        }
        irq
    }
}
