//! Mechanical drive noises, synthesized and mixed into the audio output: the
//! whirr of a spinning floppy (with a tape-transport flavour), the clack of the
//! floppy head stepping between tracks (a seek is a rapid chug of them), and a
//! hard disk's spindle whine and seek clicks. Nothing here affects emulation.

use std::f32::consts::TAU;

/// A decaying two-pole resonator, struck to make clicks and thumps.
#[derive(Clone, Copy, Default)]
struct Resonator {
    a1: f32,
    a2: f32,
    y1: f32,
    y2: f32,
}

impl Resonator {
    /// `freq` in Hz; the ring dies down by 1/e in `decay` seconds.
    fn new(freq: f32, decay: f32, rate: f32) -> Self {
        let r = (-1.0 / (decay * rate)).exp();
        Resonator { a1: 2.0 * r * (TAU * freq / rate).cos(), a2: -r * r, y1: 0.0, y2: 0.0 }
    }

    fn run(&mut self, x: f32) -> f32 {
        let y = x + self.a1 * self.y1 + self.a2 * self.y2;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

/// Band-pass biquad (constant peak gain) for shaping noise.
#[derive(Clone, Copy, Default)]
struct BandPass {
    b0: f32,
    a1: f32,
    a2: f32,
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
}

impl BandPass {
    fn new(freq: f32, q: f32, rate: f32) -> Self {
        let w = TAU * freq / rate;
        let alpha = w.sin() / (2.0 * q);
        let a0 = 1.0 + alpha;
        BandPass { b0: alpha / a0, a1: -2.0 * w.cos() / a0, a2: (1.0 - alpha) / a0, ..Default::default() }
    }

    fn run(&mut self, x: f32) -> f32 {
        let y = self.b0 * x - self.b0 * self.x2 - self.a1 * self.y1 - self.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

/// Struck resonators making one kind of click.
#[derive(Clone)]
struct Clicker {
    parts: Vec<(Resonator, f32)>,
    /// Length of the noise burst a strike feeds in.
    burst_len: u32,
    /// Samples of burst still to feed in, and its loudness.
    burst: u32,
    level: f32,
}

impl Clicker {
    /// `parts`: (frequency, decay in seconds, peak level). Each strike drives
    /// the resonators with `burst` seconds of noise.
    fn new(parts: &[(f32, f32, f32)], burst: f32, rate: f32) -> Self {
        let burst_len = ((burst * rate) as u32).max(1);
        let parts = parts
            .iter()
            .map(|&(f, d, w)| {
                // Scale each resonator so a strike peaks at its level.
                let mut r = Resonator::new(f, d, rate);
                let mut peak = 0f32;
                for i in 0..burst_len + (d * rate) as u32 {
                    peak = peak.max(r.run(if i < burst_len { 0.5 } else { 0.0 }).abs());
                }
                (Resonator::new(f, d, rate), w / peak.max(1e-6))
            })
            .collect();
        Clicker { parts, burst_len, burst: 0, level: 0.0 }
    }

    fn strike(&mut self, level: f32) {
        self.level = level;
        self.burst = self.burst_len;
    }

    fn run(&mut self, noise: f32) -> f32 {
        let x = if self.burst > 0 {
            self.burst -= 1;
            self.level * (0.5 + 0.5 * noise)
        } else {
            0.0
        };
        self.parts.iter_mut().map(|(r, w)| r.run(x * *w)).sum()
    }
}

#[derive(Clone)]
struct Floppy {
    motor: bool,
    disk: bool,
    /// Spindle speed, 0..1.
    spin: f32,
    phase: f32,
    wobble: f32,
    hiss: BandPass,
    whirr: BandPass,
    step: Clicker,
}

#[derive(Clone)]
struct HardDrive {
    spin: f32,
    phase: f32,
    whine: BandPass,
    seek: Clicker,
    last_lba: u32,
    /// Samples until the next queued seek may sound, and seeks waiting.
    busy: u32,
    queued: Vec<f32>,
}

pub struct DriveSounds {
    pub enabled: bool,
    rate: f32,
    floppies: [Floppy; 4],
    hd: Option<HardDrive>,
    seed: u32,
}

/// Overall level of the mechanical sounds against Paula's full scale.
const VOLUME: f32 = 0.35;

impl DriveSounds {
    pub fn new(rate: u32) -> Self {
        let r = rate as f32;
        let floppy = Floppy {
            motor: false,
            disk: false,
            spin: 0.0,
            phase: 0.0,
            wobble: 0.0,
            hiss: BandPass::new(2400.0, 0.8, r),
            whirr: BandPass::new(420.0, 1.4, r),
            // The head stepper's clack: a low thump, the carriage body and a
            // sharp tick.
            step: Clicker::new(&[(170.0, 0.018, 1.0), (950.0, 0.009, 0.8), (2700.0, 0.003, 0.5)], 0.002, r),
        };
        DriveSounds { enabled: true, rate: r, floppies: std::array::from_fn(|_| floppy.clone()), hd: None, seed: 0x1234_5678 }
    }

    /// Change the output rate (rebuilds the filters; drive states are kept).
    pub fn set_rate(&mut self, rate: u32) {
        let mut fresh = DriveSounds::new(rate);
        fresh.enabled = self.enabled;
        for (f, old) in fresh.floppies.iter_mut().zip(&self.floppies) {
            f.motor = old.motor;
            f.disk = old.disk;
        }
        if self.hd.is_some() {
            fresh.set_hard_disk(true);
        }
        *self = fresh;
    }

    /// Whether the machine has a hard disk (it spins up when this is set).
    pub fn set_hard_disk(&mut self, present: bool) {
        if !present {
            self.hd = None;
            return;
        }
        if self.hd.is_none() {
            let r = self.rate;
            self.hd = Some(HardDrive {
                spin: 0.0,
                phase: 0.0,
                whine: BandPass::new(4200.0, 6.0, r),
                // The voice-coil actuator's knock and the arm's tick.
                seek: Clicker::new(&[(240.0, 0.007, 1.0), (1400.0, 0.004, 0.6), (3800.0, 0.0015, 0.4)], 0.0012, r),
                last_lba: 0,
                busy: 0,
                queued: vec![],
            });
        }
    }

    pub fn set_floppy(&mut self, unit: usize, motor: bool, disk: bool) {
        if let Some(f) = self.floppies.get_mut(unit) {
            f.motor = motor;
            f.disk = disk;
        }
    }

    /// The head of floppy `unit` stepped one track.
    pub fn floppy_step(&mut self, unit: usize) {
        if let Some(f) = self.floppies.get_mut(unit) {
            f.step.strike(1.0);
        }
    }

    /// The hard disk was asked for block `lba`: far seeks knock, nearby ones
    /// tick, and sequential transfers make no extra noise.
    pub fn hard_disk_access(&mut self, lba: u32) {
        let Some(hd) = self.hd.as_mut() else { return };
        let dist = lba.abs_diff(hd.last_lba);
        hd.last_lba = lba;
        if dist <= 64 {
            return;
        }
        // Louder for longer seeks, on a log scale up to about a gigabyte.
        let level = 0.25 + 0.75 * ((dist as f32).ln() / (2_000_000f32).ln()).min(1.0);
        if hd.queued.len() < 3 {
            hd.queued.push(level);
        }
    }

    fn noise(&mut self) -> f32 {
        // xorshift32, deterministic so headless recordings repeat.
        let mut x = self.seed;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.seed = x;
        (x as f32 / u32::MAX as f32) * 2.0 - 1.0
    }

    /// The next output sample (mono, added to both channels).
    pub fn sample(&mut self) -> f32 {
        if !self.enabled {
            return 0.0;
        }
        let rate = self.rate;
        let mut out = 0.0;
        for i in 0..4 {
            let n1 = self.noise();
            let n2 = self.noise();
            let f = &mut self.floppies[i];
            // Spin up in about a quarter second, run down in about half.
            let target = if f.motor { 1.0 } else { 0.0 };
            let step = if f.motor { 4.0 } else { 2.0 } / rate;
            f.spin += (target - f.spin).clamp(-step, step);
            let mut s = f.step.run(n1) * 0.35;
            if f.spin > 0.0 {
                // The motor tone with its harmonics, rising with speed.
                f.phase = (f.phase + 110.0 * f.spin / rate).fract();
                let p = TAU * f.phase;
                let tone = p.sin() + 0.5 * (2.0 * p).sin() + 0.3 * (3.0 * p).sin();
                // The disk turns five times a second: a slow wow on the sound.
                f.wobble = (f.wobble + 5.0 * f.spin / rate).fract();
                let wow = 1.0 + 0.3 * (TAU * f.wobble).sin();
                // Like a tape transport: a broad whirr, and with a disk in, the
                // hiss of the medium rubbing the head pads.
                let whirr = f.whirr.run(n2) * 1.6;
                let hiss = if f.disk { f.hiss.run(n1) * 0.7 } else { 0.0 };
                s += f.spin * f.spin * wow * (0.02 * tone + 0.09 * whirr + 0.05 * hiss);
            }
            out += s;
        }
        let n = self.noise();
        if let Some(hd) = self.hd.as_mut() {
            hd.spin = (hd.spin + 1.0 / (3.0 * rate)).min(1.0);
            hd.phase = (hd.phase + 90.0 * hd.spin / rate).fract();
            let hum = (TAU * hd.phase).sin() * 0.004 + hd.whine.run(n) * 0.012;
            if hd.busy > 0 {
                hd.busy -= 1;
            } else if !hd.queued.is_empty() {
                let level = hd.queued.remove(0);
                hd.seek.strike(level);
                // A seek and settle takes several milliseconds.
                hd.busy = (rate * 0.009) as u32;
            }
            out += hd.spin * hum + hd.seek.run(n) * 0.3;
        }
        out * VOLUME
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peak(d: &mut DriveSounds, n: usize) -> f32 {
        (0..n).map(|_| d.sample().abs()).fold(0.0, f32::max)
    }

    #[test]
    fn silent_until_something_happens() {
        let mut d = DriveSounds::new(48000);
        assert_eq!(peak(&mut d, 4800), 0.0);
        d.floppy_step(0);
        let click = peak(&mut d, 4800);
        assert!(click > 0.02 && click < 1.0, "step click peak {click}");
        d.set_floppy(1, true, true);
        let whirr = peak(&mut d, 48000);
        assert!(whirr > 0.005 && whirr < 0.5, "whirr peak {whirr}");
    }

    #[test]
    fn hard_disk_seeks_click_and_sequential_reads_do_not() {
        let mut d = DriveSounds::new(48000);
        d.set_hard_disk(true);
        let _ = peak(&mut d, 48000 * 3);
        let hum = peak(&mut d, 4800);
        d.hard_disk_access(10);
        assert!(peak(&mut d, 4800) <= hum * 1.5, "sequential read clicked");
        d.hard_disk_access(500_000);
        assert!(peak(&mut d, 4800) > hum * 3.0, "seek did not click");
    }
}
