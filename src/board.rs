//! The Amiga motherboard: memory map, bus timing, Agnus DMA, copper and the
//! glue between CIAs, Paula, Denise, floppy drives and the keyboard.

use crate::a2091::{Dma, A2091};
use crate::bus::Bus;
use crate::chipset::audio::{Audio, AudioMem};
use crate::chipset::blitter::Blitter;
use crate::chipset::denise::{Denise, FB_H, FB_W};
use crate::chipset::{dma, int};
use crate::cia::{Cia, ICR_FLAG};
use crate::config::{Chipset, Config};
use crate::expansion::{registry, Card, Device, Expansion};
use crate::floppy::Drive;
use crate::keyboard::Keyboard;

pub const PAL_CCK_RATE: u32 = 3_546_895;
pub const NTSC_CCK_RATE: u32 = 3_579_545;
const LINE_CCKS: u16 = 227;
/// Colour clocks before a newly raised interrupt level reaches the CPU. Both
/// F1 GP Circuits (polls VPOSR for line 0, where the vertical blank interrupt
/// is raised) and Voyage (times a loop against CIA interrupts) need 3 or 4.
const IPL_DELAY_CCK: u64 = 3;
/// Colour clocks per MFM word at double density (16 bits * 2us).
const DISK_WORD_CCKS: u32 = 113;
const SPRITE_FIRST_LINE: u16 = 0x19;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CopState {
    Stopped,
    Fetch1,
    Fetch2,
    Wait,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SprSlot {
    None,
    Ctl,
    Data,
}

struct DiskDma {
    write: bool,
    words: u32,
}

/// Front-panel lights: what a real Amiga's LEDs would show.
#[derive(Clone, Copy, Debug, Default)]
pub struct Leds {
    /// Power LED at full brightness (CIA-A /LED low; it dims with the audio filter off).
    pub power: bool,
    /// Per floppy drive, if fitted: (motor on, disk DMA running on it).
    pub floppy: [Option<(bool, bool)>; 4],
    /// Hard disk activity counter, if a controller is fitted.
    pub hd: Option<u64>,
}

pub struct Board {
    pub cfg: Config,
    pub chip: Vec<u8>,
    pub chip_mask: u32,
    pub slow: Vec<u8>,
    /// A3000/A4000 motherboard fast RAM, ending at $08000000.
    pub mb_ram: Vec<u8>,
    /// Expansion boards on the autoconfig chain.
    pub exp: Expansion,
    /// A590/A2091 SCSI controller (on the chain), or the A3000's built-in SDMAC.
    pub scsi: Option<A2091>,
    /// A motherboard controller with registers at a fixed address (the
    /// A4000T's SCSI at $DD0040).
    pub builtin: Option<Box<dyn crate::expansion::Controller>>,
    /// Gayle gate array with IDE (A600/A1200).
    pub gayle: Option<crate::gayle::Gayle>,
    /// CD32 only.
    pub akiko: Option<crate::akiko::Akiko>,
    /// Battery-backed clock at $DC0000.
    pub rtc: Option<crate::rtc::Rtc>,
    pub rom: Vec<u8>,
    /// Extension ROM at $E00000 (AROS, CD32, CDTV).
    pub ext_rom: Vec<u8>,
    /// The emulator's own ROM at $F00000 (64 KB), empty when not needed.
    pub boot_rom: Vec<u8>,
    /// The CDTV's battery-backed RAM at $DC8000 (16 KB).
    pub nvram: Vec<u8>,
    /// UAE's virtual graphics card, when its VRAM board is fitted.
    pub rtg: Option<crate::rtg::Rtg>,
    /// The display with a genlocked board's video mixed in (the CD32 FMV
    /// cartridge), while it shows some.
    genlock: Option<Vec<u32>>,
    rom_mask: u32,
    pub overlay: bool,

    /// CPU clock (7.09 MHz PAL); the chipset runs at half this rate.
    pub cycles: u64,
    pub cck: u64,
    /// Last word on the custom chip bus (read back from write-only registers).
    last_custom: u16,
    /// Interrupt level the CPU sees, and the level Paula is changing to (since `ipl_since`).
    ipl_out: u8,
    ipl_pending: u8,
    ipl_since: u64,
    pub vpos: u16,
    pub hpos: u16,
    pub lof: bool,
    pub frame_done: bool,
    pub frame_count: u64,

    pub dmacon: u16,
    pub intena: u16,
    pub intreq: u16,
    pub adkcon: u16,
    copcon: u16,

    // Agnus bitplane state.
    bplpt: [u32; 8],
    /// AGA FMODE (bitplane and sprite fetch widths).
    fmode: u16,
    bpl1mod: i16,
    bpl2mod: i16,
    ddfstrt: u16,
    ddfstop: u16,
    diwstrt: u16,
    diwstop: u16,
    vdiw: bool,
    fetch_start: u16,
    fetch_end: u16,

    // Agnus sprite DMA state.
    sprpt: [u32; 8],
    spr_vstart: [u16; 8],
    spr_vstop: [u16; 8],
    spr_need_ctl: [bool; 8],
    spr_slot: [SprSlot; 8],

    // Copper.
    cop1lc: u32,
    cop2lc: u32,
    cop_pc: u32,
    cop_state: CopState,
    cop_ir1: u16,
    cop_ir2: u16,

    pub blitter: Blitter,
    pub denise: Denise,
    pub audio: Audio,

    // Disk controller.
    dskpt: u32,
    dsklen_prev: u16,
    dsksync: u16,
    disk_dma: Option<DiskDma>,
    disk_synced: bool,
    disk_acc: u32,
    disk_shift: u16,
    /// Xorshift state for the read line's noise when no drive is spinning.
    disk_noise: u32,
    disk_bits: u32,
    dskbyt: u16,
    pub drives: Vec<Drive>,
    ciab_prb_prev: u8,

    pub cia_a: Cia,
    pub cia_b: Cia,
    eclk: u8,
    pub keyboard: Keyboard,
    sp_was_output: bool,

    // Input devices.
    pub mouse_x: u8,
    pub mouse_y: u8,
    pub mouse_left: bool,
    pub mouse_right: bool,
    /// Joystick in port 2: up, down, left, right, fire, fire2.
    pub joy: [bool; 6],
    /// A joystick in port 1 instead of the mouse, and its state (as `joy`).
    pub port1_joystick: bool,
    pub joy1: [bool; 6],
    /// CD32 joypad's other buttons: play, reverse, forward, green, yellow
    /// (red and blue are fire and fire2).
    pub pad: [bool; 5],
    /// The pad buttons as last seen, for the CDTV's remote buttons.
    pad_prev: [bool; 5],
    /// The CDTV remote's mouse/joystick slider. In mouse mode the port 2
    /// joystick is the remote's direction pad and A/B buttons, and works the
    /// mouse port instead.
    pub remote: crate::cdtv_remote::Remote,
    /// The control that flips the slider is held (and was, last frame).
    pub remote_switch: bool,
    remote_switch_prev: bool,
    /// The CD32 joypad is plugged into port 2.
    pub cd32_pad: bool,
    /// The pad's shift register: bits left to send (8 = reloaded).
    pad_shift: u8,
    /// Last level of the shift clock (CIA-A PA7 as an output).
    pad_clock: bool,
    potgo: u16,
    /// POT0DAT/POT1DAT counters (Y<<8 | X), counting lines after POTGO START.
    pot: [u16; 2],
    pot_lines: u16,
    /// Current video standard (ECS can switch with BEAMCON0).
    pal: bool,
    /// ECS DIWHIGH value, if written since the last DIWSTRT/DIWSTOP.
    diwhigh: Option<u16>,

    pub serial_out: Vec<u8>,
    /// PC of the instruction being executed (for device tracing).
    pub cur_pc: u32,
    /// CPU speed relative to a 7 MHz 68000, in 1/16ths. Accesses that do not
    /// wait for the chip bus, and internal cycles, cost proportionally less.
    cpu_mult16: u32,
    cycle_frac: u32,
    /// A3000/A4000 Ramsey memory controller control register ($DE0003).
    ramsey_ctl: u8,
}

impl AudioMem for (&Vec<u8>, u32) {
    fn fetch(&mut self, addr: u32) -> u16 {
        let a = (addr & self.1 & !1) as usize;
        (self.0[a] as u16) << 8 | self.0[a + 1] as u16
    }
}

impl Board {
    pub fn new(cfg: Config, rom: Vec<u8>) -> Board {
        let chip_size = (cfg.chip_kb as usize * 1024).next_power_of_two();
        let rom_mask = rom.len() as u32 - 1;
        let mut drives = vec![];
        for i in 0..4 {
            drives.push(Drive::new(i < cfg.drives));
        }
        let cck_rate = if cfg.ntsc { NTSC_CCK_RATE } else { PAL_CCK_RATE };
        let mut b = Board {
            chip: vec![0; chip_size],
            chip_mask: chip_size as u32 - 1,
            slow: vec![0; cfg.slow_kb as usize * 1024],
            mb_ram: vec![0; cfg.mb_kb as usize * 1024],
            exp: Expansion::new(expansion_cards(&cfg)),
            scsi: (cfg.model == crate::config::Model::A3000).then(A2091::new_a3000),
            builtin: (cfg.model == crate::config::Model::A4000T)
                .then(|| Box::new(crate::a4091::A4000tScsi::new()) as Box<dyn crate::expansion::Controller>),
            gayle: match cfg.model {
                crate::config::Model::A600 | crate::config::Model::A1200 => Some(crate::gayle::Gayle::new()),
                crate::config::Model::A4000 | crate::config::Model::A4000T => Some(crate::gayle::Gayle::new_a4000()),
                _ => None,
            },
            akiko: (cfg.model == crate::config::Model::Cd32).then(crate::akiko::Akiko::new),
            rtc: cfg.rtc.map(|k| crate::rtc::Rtc::new(k, if cfg.ntsc { NTSC_CCK_RATE } else { PAL_CCK_RATE })),
            rom,
            rom_mask,
            ext_rom: vec![],
            boot_rom: vec![],
            nvram: if cfg.model == crate::config::Model::Cdtv { vec![0; 0x4000] } else { vec![] },
            rtg: None,
            genlock: None,
            overlay: true,
            cycles: 0,
            cck: 0,
            last_custom: 0xffff,
            ipl_out: 0,
            ipl_pending: 0,
            ipl_since: 0,
            vpos: 0,
            hpos: 0,
            lof: true,
            frame_done: false,
            frame_count: 0,
            dmacon: 0,
            intena: 0,
            intreq: 0,
            adkcon: 0,
            copcon: 0,
            bplpt: [0; 8],
            fmode: 0,
            bpl1mod: 0,
            bpl2mod: 0,
            ddfstrt: 0,
            ddfstop: 0,
            diwstrt: 0,
            diwstop: 0,
            vdiw: false,
            fetch_start: 0,
            fetch_end: 0,
            sprpt: [0; 8],
            spr_vstart: [0; 8],
            spr_vstop: [0; 8],
            spr_need_ctl: [false; 8],
            spr_slot: [SprSlot::None; 8],
            cop1lc: 0,
            cop2lc: 0,
            cop_pc: 0,
            cop_state: CopState::Stopped,
            cop_ir1: 0,
            cop_ir2: 0,
            blitter: Blitter::default(),
            denise: Denise::with_aga(cfg.model.chipset() == Chipset::Aga),
            audio: Audio::new(cck_rate, cfg.model.audio_lowpass_hz()),
            dskpt: 0,
            dsklen_prev: 0,
            dsksync: 0x4489,
            disk_dma: None,
            disk_synced: false,
            disk_acc: 0,
            disk_shift: 0,
            disk_noise: 0x2545_f491,
            disk_bits: 0,
            dskbyt: 0,
            drives,
            ciab_prb_prev: 0xff,
            cia_a: Cia::new(),
            cia_b: Cia::new(),
            eclk: 0,
            keyboard: Keyboard::new(),
            sp_was_output: false,
            mouse_x: 0,
            mouse_y: 0,
            mouse_left: false,
            mouse_right: false,
            port1_joystick: false,
            joy1: [false; 6],
            joy: [false; 6],
            pad: [false; 5],
            pad_prev: [false; 5],
            remote: Default::default(),
            remote_switch: false,
            remote_switch_prev: false,
            cd32_pad: cfg.model == crate::config::Model::Cd32,
            pad_shift: 8,
            pad_clock: false,
            potgo: 0,
            pot: [0; 2],
            pot_lines: 0,
            pal: !cfg.ntsc,
            diwhigh: None,
            serial_out: vec![],
            cur_pc: 0,
            cpu_mult16: ((cfg.cpu_speed * 16.0) as u32).max(16),
            cycle_frac: 0,
            ramsey_ctl: 0,
            cfg,
        };
        b.rtg = b.exp.cards.iter().position(|c| c.board == "uaegfx").map(crate::rtg::Rtg::new);
        if b.rtg.is_some() {
            b.boot_rom = crate::rtg::boot_rom();
        }
        b
    }

    pub fn leds(&self) -> Leds {
        let c = &self.cia_a;
        let pra = (c.pra & c.ddra) | !c.ddra;
        let mut floppy = [None; 4];
        for (i, d) in self.drives.iter().enumerate() {
            if d.present {
                floppy[i] = Some((d.motor, d.motor && d.selected && self.disk_dma.is_some()));
            }
        }
        let hd = match (&self.gayle, &self.scsi) {
            (Some(g), _) if g.drives.iter().any(|d| d.is_some()) => Some(g.activity),
            (_, Some(s)) => Some(s.activity),
            _ => None,
        };
        let io: u64 = self.exp.cards.iter().filter_map(|c| match &c.dev {
            Device::Io(d) => Some(d.activity()),
            _ => None,
        }).chain(self.builtin.as_ref().map(|d| d.activity())).sum();
        let hd = if io > 0 { Some(hd.unwrap_or(0).wrapping_add(io)) } else { hd };
        Leds { power: pra & 2 == 0, floppy, hd }
    }

    /// The picture to show: the graphics card's while its switch is on.
    pub fn framebuffer(&self) -> (&[u32], usize, usize) {
        if let Some(f) = self.rtg.as_ref().and_then(|r| r.framebuffer()) {
            return f;
        }
        (self.genlock.as_deref().unwrap_or(&self.denise.fb), FB_W, FB_H)
    }

    /// At the end of a frame, mix a board's video into the picture where
    /// Denise's ZD output marks it transparent. The video is doubled both
    /// ways (352x288 fills a PAL screen) and centred.
    fn mix_genlock(&mut self) {
        let overlay = self.exp.cards.iter().find_map(|c| match &c.dev {
            Device::Io(d) => d.overlay(),
            _ => None,
        });
        let Some(ov) = overlay else {
            self.genlock = None;
            self.denise.zd = None;
            return;
        };
        // ZD is recorded from the next frame on.
        let Some(zd) = self.denise.zd.as_ref() else {
            self.denise.zd = Some(vec![false; FB_W * FB_H]);
            return;
        };
        let out = self.genlock.get_or_insert_with(|| vec![0; FB_W * FB_H]);
        out.copy_from_slice(&self.denise.fb);
        let x0 = (FB_W as isize - 2 * ov.width as isize) / 2;
        let y0 = (FB_H as isize - 2 * ov.height as isize) / 2;
        for y in 0..FB_H {
            let vy = (y as isize - y0) >> 1;
            let row = (vy >= 0 && (vy as usize) < ov.height).then(|| &ov.pixels[vy as usize * ov.width..][..ov.width]);
            for x in 0..FB_W {
                if !zd[y * FB_W + x] {
                    continue;
                }
                let vx = (x as isize - x0) >> 1;
                out[y * FB_W + x] = match row {
                    Some(r) if vx >= 0 && (vx as usize) < ov.width => r[vx as usize],
                    _ => ov.border,
                };
            }
        }
    }

    fn ocs(&self) -> bool {
        self.cfg.model.chipset() == Chipset::Ocs
    }

    /// Change the CPU speed multiplier while running.
    pub fn set_cpu_speed(&mut self, speed: f32) {
        self.cfg.cpu_speed = speed.clamp(1.0, 64.0);
        self.cpu_mult16 = ((self.cfg.cpu_speed * 16.0) as u32).max(16);
    }

    /// Account for `n` CPU clocks that do not touch the chip bus.
    #[inline]
    fn cpu_cycles(&mut self, n: u32) {
        if self.cpu_mult16 == 16 {
            self.cycles += n as u64;
            return;
        }
        self.cycle_frac += n * 16;
        self.cycles += (self.cycle_frac / self.cpu_mult16) as u64;
        self.cycle_frac %= self.cpu_mult16;
    }

    fn aga(&self) -> bool {
        self.cfg.model.chipset() == Chipset::Aga
    }

    /// Words fetched per bitplane DMA slot (AGA FMODE 1x/2x/4x).
    fn bpl_fetch_words(&self) -> u32 {
        match self.fmode & 3 {
            0 => 1,
            3 => 4,
            _ => 2,
        }
    }

    /// Length in colour clocks of one bitplane fetch unit.
    fn bpl_unit(&self) -> u16 {
        let fw = self.bpl_fetch_words() as u16;
        let c0 = self.denise.bplcon0;
        if self.aga() && c0 & 0x40 != 0 {
            2 * fw
        } else if c0 & 0x8000 != 0 {
            4 * fw
        } else {
            8 * fw
        }
    }

    fn lines_per_frame(&self) -> u16 {
        match (!self.pal, self.lof) {
            (false, true) => 313,
            (false, false) => 312,
            (true, true) => 263,
            (true, false) => 262,
        }
    }

    /// Reset everything except memory contents (power-on or RESET line).
    fn reset_chips(&mut self) {
        if let Some(r) = &mut self.rtg {
            r.reset();
        }
        self.cia_a.reset();
        self.cia_b.reset();
        self.overlay = true;
        self.dmacon = 0;
        self.intena = 0;
        self.intreq = 0;
        self.adkcon = 0;
        self.copcon = 0;
        self.cop_state = CopState::Stopped;
        self.denise.reset();
        self.audio.reset();
        self.disk_dma = None;
        self.dsklen_prev = 0;
        self.blitter.pending = None;
        self.blitter.busy_until = None;
        self.ciab_prb_prev = 0xff;
        self.exp.reset();
        for d in self.controllers() {
            d.reset();
        }
        if let Some(s) = self.scsi.as_mut() {
            s.reset();
        }
        if let Some(g) = self.gayle.as_mut() {
            g.reset();
        }
        if let Some(a) = self.akiko.as_mut() {
            a.cd.reset();
        }
    }

    // ---- time -------------------------------------------------------------

    /// Run the chipset until it has caught up with the CPU clock.
    #[inline]
    pub fn sync(&mut self) {
        let target = self.cycles >> 1;
        while self.cck < target {
            self.step_cck();
        }
    }

    /// Wait for a free chip bus slot, then account for one bus cycle.
    fn chip_access(&mut self) {
        // Chip bus cycles start on a colour clock boundary. (Not only even
        // ones: Voyage's protection times a loop of 6-cycle-aligned DBF code
        // against CIA timers and only decrypts with 58-cycle iterations.)
        self.cycles = (self.cycles + 1) & !1;
        loop {
            self.sync();
            if !self.slot_busy() {
                break;
            }
            self.cycles += 4;
        }
        self.cycles += 4;
    }

    /// CIA accesses synchronise to the E clock (one tenth of the CPU clock).
    fn cia_access(&mut self) {
        self.cycles += 4;
        let rem = self.cycles % 10;
        self.cycles += (10 - rem) % 10 + 2;
        self.sync();
    }

    fn slot_busy(&self) -> bool {
        let h = self.hpos;
        if !self.bpl_fetch_active(h) {
            return false;
        }
        self.bpl_plane_at(h).is_some()
    }

    #[inline]
    fn bpl_fetch_active(&self, h: u16) -> bool {
        self.vdiw
            && h >= self.fetch_start
            && h < self.fetch_end
            && self.dmacon & (dma::BPL | dma::MASTER) == (dma::BPL | dma::MASTER)
    }

    /// Plane fetched at colour clock `h` inside the fetch window, if any.
    #[inline]
    fn bpl_plane_at(&self, h: u16) -> Option<usize> {
        // Plane fetched at each position of a fetch unit (1-based). Units longer
        // than 8 colour clocks (AGA 2x/4x) fetch in their first 8 slots.
        const SEQ8: [u8; 8] = [8, 4, 6, 2, 7, 3, 5, 1];
        const SEQ4: [u8; 4] = [4, 2, 3, 1];
        const SEQ2: [u8; 2] = [2, 1];
        let n = self.denise.nplanes(self.denise.bplcon0);
        let unit = self.bpl_unit();
        let off = ((h - self.fetch_start) % unit) as usize;
        let p = match unit {
            2 => SEQ2[off],
            4 => SEQ4[off],
            _ => SEQ8.get(off).copied().unwrap_or(0),
        } as usize;
        if p == 0 || p > n {
            None
        } else {
            Some(p - 1)
        }
    }

    fn update_fetch_window(&mut self) {
        let mask = if self.ocs() { 0xfc } else { 0xfe };
        let start = (self.ddfstrt & mask).clamp(0x18, 0xd8);
        let stop = (self.ddfstop & mask).clamp(start, 0xd8);
        // The last fetch unit starts at the first unit boundary at or after DDFSTOP.
        let unit = self.bpl_unit().max(8);
        let units = (stop - start).div_ceil(unit) + 1;
        self.fetch_start = start;
        self.fetch_end = (start + units * unit).min(0xe2);
    }

    // ---- per colour clock ---------------------------------------------------

    fn step_cck(&mut self) {
        let h = self.hpos;

        // Sprite DMA slots: two per sprite starting at 0x15.
        if (0x15..0x35).contains(&h) && (h - 0x15) & 1 == 0 {
            let n = ((h - 0x15) >> 2) as usize;
            self.sprite_dma(n, (h - 0x15) & 2 != 0);
        }

        // Bitplane DMA.
        let mut bus_used = false;
        if self.bpl_fetch_active(h) {
            if let Some(p) = self.bpl_plane_at(h) {
                let fw = self.bpl_fetch_words();
                let mut v = 0u64;
                for k in 0..fw {
                    v |= (self.chip_word(self.bplpt[p]) as u64) << (48 - 16 * k);
                    self.bplpt[p] = self.bplpt[p].wrapping_add(2);
                }
                self.write_bpldat(p, v, 16 * fw, h);
                bus_used = true;
            }
        }
        if h == self.fetch_end && self.vdiw && self.dmacon & (dma::BPL | dma::MASTER) == (dma::BPL | dma::MASTER) {
            let n = self.denise.nplanes(self.denise.bplcon0);
            // AGA BSCAN2 (scan doubling): every plane takes BPL1MOD and BPL2MOD on
            // alternate lines, counted from the window start line, instead of
            // odd planes BPL1MOD and even planes BPL2MOD.
            let (m1, m2) = if self.aga() && self.fmode & 0x4000 != 0 {
                let m = if ((self.diwstrt >> 8) ^ self.vpos) & 1 != 0 { self.bpl2mod } else { self.bpl1mod };
                (m, m)
            } else {
                (self.bpl1mod, self.bpl2mod)
            };
            for p in 0..n {
                let m = if p & 1 == 0 { m1 } else { m2 };
                self.bplpt[p] = self.bplpt[p].wrapping_add(m as i32 as u32);
            }
        }

        // Copper.
        if self.cop_state != CopState::Stopped && self.dmacon & (dma::COP | dma::MASTER) == (dma::COP | dma::MASTER) {
            self.copper_step(h, bus_used);
        }

        // Audio.
        let aud_en = if self.dmacon & dma::MASTER != 0 { (self.dmacon & dma::AUD) as u8 } else { 0 };
        let irq = {
            let mut mem = (&self.chip, self.chip_mask);
            self.audio.tick(&mut mem, aud_en, self.adkcon, (self.intreq >> 7) as u8 & 0xf)
        };
        if irq != 0 {
            self.intreq |= (irq as u16) << 7;
        }

        self.disk_tick();

        if let Some(t) = self.blitter.busy_until {
            if self.cck >= t {
                self.blitter.busy_until = None;
                self.intreq |= int::BLIT;
            }
        }

        self.eclk += 1;
        if self.eclk == 5 {
            self.eclk = 0;
            self.cia_a.tick();
            self.cia_b.tick();
            self.update_cia_irq();
        }

        self.cck += 1;
        self.hpos += 1;
        if self.hpos >= LINE_CCKS {
            self.end_line();
        }
    }

    fn end_line(&mut self) {
        let lace = self.denise.bplcon0 & 4 != 0;
        let field = if lace { Some(self.lof) } else { None };
        self.denise.end_line(self.vpos, self.vdiw, field);
        self.hpos = 0;
        self.vpos += 1;
        self.cia_b.tod_tick();
        if let Some(rtc) = &mut self.rtc {
            rtc.tick(LINE_CCKS as u32);
        }
        // Pot counters run while the capacitors charge; nothing analog is attached,
        // so they count to the maximum the hardware reaches within a frame.
        if self.pot_lines < 255 {
            self.pot_lines += 1;
            let n = self.pot_lines;
            self.pot = [n << 8 | n; 2];
        }
        if let Some(byte) = self.keyboard.tick(LINE_CCKS as u32) {
            self.cia_a.serial_in(byte);
        }
        if self.remote.mouse() {
            let rate = if self.cfg.ntsc { NTSC_CCK_RATE } else { PAL_CCK_RATE };
            let (dx, dy) = self.remote.tick(LINE_CCKS as f64 * 1e6 / rate as f64, self.joy);
            self.mouse_move(dx, dy);
        }
        if self.vpos >= self.lines_per_frame() {
            self.vpos = 0;
            self.lof = if lace { !self.lof } else { true };
            self.frame_done = true;
            self.frame_count += 1;
            self.intreq |= int::VERTB;
            self.cia_a.tod_tick();
            self.cdtv_buttons();
            self.mix_genlock();
            if let Some(r) = &mut self.rtg {
                if r.on {
                    let card = &self.exp.cards[r.card];
                    if let (Some(base), crate::expansion::Device::Ram(vram)) = (card.base, &card.dev) {
                        r.render(vram, base);
                    }
                }
            }
            self.cop_pc = self.cop1lc;
            self.cop_state = CopState::Fetch1;
            for n in 0..8 {
                self.spr_slot[n] = SprSlot::None;
            }
        }
        self.scsi_tick();
        self.akiko_tick();
        self.update_cia_irq();
        self.update_drive_sounds();
        self.start_line();
    }

    /// Pass the drives' mechanical state on to the drive sounds.
    fn update_drive_sounds(&mut self) {
        for (i, d) in self.drives.iter().enumerate() {
            self.audio.mech.set_floppy(i, d.present && d.motor, d.disk.is_some());
        }
        let mut seeks = [None; 8];
        if let Some(g) = self.gayle.as_mut() {
            seeks[0] = g.pending_seek.take();
        }
        if let Some(s) = self.scsi.as_mut() {
            for (slot, d) in seeks[1..].iter_mut().zip(s.disks.iter_mut().flatten()) {
                *slot = d.pending_seek.take();
            }
        }
        let mut rest = vec![];
        for d in self.controllers() {
            if let Some(disks) = d.disks() {
                rest.extend(disks.iter_mut().flatten().filter_map(|d| d.pending_seek.take()));
            }
        }
        for lba in seeks.into_iter().flatten().chain(rest) {
            self.audio.mech.hard_disk_access(lba);
        }
    }

    fn start_line(&mut self) {
        let (vstart, vstop) = match self.diwhigh {
            Some(hi) => ((self.diwstrt >> 8) | (hi & 7) << 8, (self.diwstop >> 8) | ((hi >> 8) & 7) << 8),
            None => (self.diwstrt >> 8, (self.diwstop >> 8) | if self.diwstop & 0x8000 == 0 { 0x100 } else { 0 }),
        };
        // The window closes at the end of every field, but (except on the A1000's
        // Agnus) a start line of 0 opens it again at once.
        if self.vpos == 0 || self.vpos == vstop {
            self.vdiw = false;
        }
        if self.vpos == vstart {
            self.vdiw = true;
        }
        if self.vpos == SPRITE_FIRST_LINE {
            self.spr_need_ctl = [true; 8];
        }
        self.denise.begin_line();
    }

    // ---- sprites -------------------------------------------------------------

    fn sprite_dma(&mut self, n: usize, second: bool) {
        if self.dmacon & (dma::SPR | dma::MASTER) != (dma::SPR | dma::MASTER) {
            return;
        }
        if self.vpos < SPRITE_FIRST_LINE || self.vpos + 1 >= self.lines_per_frame() {
            return;
        }
        if !second {
            self.spr_slot[n] = if self.spr_need_ctl[n] || self.vpos == self.spr_vstop[n] {
                SprSlot::Ctl
            } else if self.vpos >= self.spr_vstart[n] && self.vpos < self.spr_vstop[n] {
                SprSlot::Data
            } else {
                SprSlot::None
            };
        }
        let base = 0x140 + n as u16 * 8;
        match self.spr_slot[n] {
            SprSlot::None => {}
            SprSlot::Ctl => {
                // Wide sprites keep each control word in a full-width slot.
                let w = self.chip_word(self.sprpt[n]);
                self.sprpt[n] = self.sprpt[n].wrapping_add(self.denise.spr_width / 8);
                if second {
                    self.spr_need_ctl[n] = false;
                    self.custom_write_reg(base + 2, w);
                } else {
                    self.custom_write_reg(base, w);
                }
            }
            SprSlot::Data => {
                let words = self.denise.spr_width / 16;
                let mut v = 0u64;
                for k in 0..words {
                    v |= (self.chip_word(self.sprpt[n]) as u64) << (48 - 16 * k);
                    self.sprpt[n] = self.sprpt[n].wrapping_add(2);
                }
                // Memory order per line is DATA (low bit) then DATB (high bit).
                let x = self.denise_x();
                self.denise.write_sprite_data(n, second, v, x);
            }
        }
    }

    fn update_sprite_v(&mut self, n: usize) {
        let s = &self.denise.spr[n];
        self.spr_vstart[n] = s.vstart();
        self.spr_vstop[n] = s.vstop();
    }

    // ---- copper --------------------------------------------------------------

    fn copper_step(&mut self, h: u16, bus_used: bool) {
        match self.cop_state {
            CopState::Stopped => {}
            CopState::Wait => {
                if self.copper_compare() && (self.cop_ir2 & 0x8000 != 0 || !self.blitter.busy(self.cck)) {
                    self.cop_state = CopState::Fetch1;
                }
            }
            CopState::Fetch1 => {
                if h & 1 == 1 || bus_used || h >= LINE_CCKS - 1 {
                    return;
                }
                self.cop_ir1 = self.chip_word(self.cop_pc);
                self.cop_pc = self.cop_pc.wrapping_add(2);
                self.cop_state = CopState::Fetch2;
            }
            CopState::Fetch2 => {
                if h & 1 == 1 || bus_used || h >= LINE_CCKS - 1 {
                    return;
                }
                self.cop_ir2 = self.chip_word(self.cop_pc);
                self.cop_pc = self.cop_pc.wrapping_add(2);
                if self.cop_ir1 & 1 == 0 {
                    // MOVE
                    let reg = self.cop_ir1 & 0x1fe;
                    let limit = if self.copcon & 2 != 0 { 0x40 } else { 0x80 };
                    if reg < limit && (self.ocs() || self.copcon & 2 == 0) {
                        self.cop_state = CopState::Stopped;
                        return;
                    }
                    self.cop_state = CopState::Fetch1;
                    let v = self.cop_ir2;
                    self.custom_write_reg(reg, v);
                } else if self.cop_ir2 & 1 == 0 {
                    self.cop_state = CopState::Wait;
                } else {
                    // SKIP
                    if self.copper_compare() {
                        self.cop_pc = self.cop_pc.wrapping_add(4);
                    }
                    self.cop_state = CopState::Fetch1;
                }
            }
        }
    }

    fn copper_compare(&self) -> bool {
        let vp = (self.cop_ir1 >> 8) & 0xff;
        let hp = self.cop_ir1 & 0xfe;
        let ve = ((self.cop_ir2 >> 8) & 0x7f) | 0x80;
        let he = self.cop_ir2 & 0xfe;
        let v = self.vpos & 0xff;
        let h = self.hpos & 0xfe;
        let (cv, wv) = (v & ve, vp & ve);
        cv > wv || (cv == wv && (h & he) >= (hp & he))
    }

    // ---- disk ------------------------------------------------------------------

    fn disk_tick(&mut self) {
        self.disk_acc += 1;
        let dma_on = self.disk_dma.is_some() && self.dmacon & (dma::DSK | dma::MASTER) == (dma::DSK | dma::MASTER);
        let period = if self.cfg.turbo_floppy && dma_on { 4 } else { DISK_WORD_CCKS };
        if self.disk_acc < period {
            return;
        }
        self.disk_acc = 0;

        // Process one word time (16 bit cells) per tick.
        let sel = self.drives.iter().position(|d| d.selected && d.motor && d.present);
        for (i, d) in self.drives.iter_mut().enumerate() {
            if d.motor && Some(i) != sel {
                for _ in 0..16 {
                    d.rotate();
                }
            }
        }
        // With no spinning drive selected the read line only picks up noise, so a
        // read waiting for sync still finds one by chance and completes with
        // garbage. Kickstart 1.3's strap reads DF0 even when there is no drive
        // (a CDTV without its external floppy) and relies on this to move on.
        if sel.is_none() && !self.disk_dma.as_ref().is_some_and(|d| !d.write) {
            return;
        }

        let writing = dma_on && self.disk_dma.as_ref().is_some_and(|d| d.write);
        if let (Some(i), true) = (sel, writing) {
            let w = self.chip_word(self.dskpt);
            self.dskpt = self.dskpt.wrapping_add(2);
            if self.drives[i].write_word(w) {
                self.cia_b.set_icr(ICR_FLAG);
            }
            self.disk_dma_word_done();
            return;
        }

        let wordsync = self.adkcon & 0x400 != 0;
        for _ in 0..16 {
            let (bit, index) = match sel {
                Some(i) => self.drives[i].read_bit(),
                None => {
                    let x = &mut self.disk_noise;
                    *x ^= *x << 13;
                    *x ^= *x >> 17;
                    *x ^= *x << 5;
                    ((*x >> 16) as u16 & 1, false)
                }
            };
            if index {
                self.cia_b.set_icr(ICR_FLAG);
            }
            self.disk_shift = self.disk_shift << 1 | bit;
            self.disk_bits += 1;
            if self.disk_shift == self.dsksync {
                self.intreq |= int::DSKSYN;
                self.dskbyt |= 0x1000;
                if wordsync {
                    // Sync realigns the word counter. The first match starts DMA with
                    // the next word; later sync words are transferred like data.
                    if !self.disk_synced {
                        self.disk_synced = true;
                        self.disk_bits = 0;
                        continue;
                    }
                    self.disk_bits = 16;
                }
            }
            if self.disk_bits & 7 == 0 {
                self.dskbyt = (self.dskbyt & 0x1000) | 0x8000 | (self.disk_shift & 0xff);
            }
            if self.disk_bits >= 16 {
                self.disk_bits = 0;
                let w = self.disk_shift;
                let active = self.disk_dma.is_some()
                    && self.dmacon & (dma::DSK | dma::MASTER) == (dma::DSK | dma::MASTER);
                if active && (!wordsync || self.disk_synced) {
                    self.chip_write_word(self.dskpt, w);
                    self.dskpt = self.dskpt.wrapping_add(2);
                    self.disk_dma_word_done();
                }
            }
        }
    }

    fn disk_dma_word_done(&mut self) {
        if let Some(d) = self.disk_dma.as_mut() {
            d.words -= 1;
            if d.words == 0 {
                self.disk_dma = None;
                self.intreq |= int::DSKBLK;
            }
        }
    }

    fn write_dsklen(&mut self, v: u16) {
        static LOG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if *LOG.get_or_init(|| std::env::var_os("AMIGA_LOG_DISK").is_some()) {
            let d = &self.drives[0];
            eprintln!(
                "DSKLEN={v:04x} dskpt={:06x} sync={:04x} adkcon={:04x} dmacon={:04x} df0: sel={} motor={} cyl={} side={} frame={}",
                self.dskpt, self.dsksync, self.adkcon, self.dmacon, d.selected, d.motor, d.cyl, d.side, self.frame_count
            );
        }
        if v & 0x8000 == 0 {
            self.disk_dma = None;
        } else if self.dsklen_prev & 0x8000 != 0 {
            let words = (v & 0x3fff) as u32;
            if words > 0 {
                self.disk_dma = Some(DiskDma { write: v & 0x4000 != 0, words });
                self.disk_synced = false;
            }
        }
        self.dsklen_prev = v;
    }

    /// CIA-B port B drives the floppy control lines.
    fn update_drive_lines(&mut self) {
        let new = self.cia_b.pb_out();
        let old = self.ciab_prb_prev;
        self.ciab_prb_prev = new;
        let step = old & 1 != 0 && new & 1 == 0;
        let outward = new & 2 != 0;
        let side = if new & 4 == 0 { 1 } else { 0 };
        let motor = new & 0x80 == 0;
        let mut stepped = [false; 4];
        for (i, d) in self.drives.iter_mut().enumerate() {
            let bit = 0x08 << i;
            let sel = new & bit == 0;
            let was = old & bit == 0;
            if sel && !was {
                // The drive latches MTR on SEL's falling edge. Switching the motor
                // off resets the ID shift register; each further select with the
                // motor off shifts out the next ID bit.
                if !motor {
                    d.id_pos = if d.motor { 0xff } else { d.id_pos.wrapping_add(1) };
                }
                d.motor = motor;
            }
            d.selected = sel;
            d.side = side;
            if sel && step && d.present {
                // The head clacks even when it is already against track 0.
                stepped[i] = true;
                if outward {
                    d.cyl = d.cyl.saturating_sub(1);
                } else if d.cyl < 83 {
                    d.cyl += 1;
                }
                if d.disk.is_some() {
                    d.changed = false;
                }
            }
        }
        for (i, _) in stepped.iter().enumerate().filter(|s| *s.1) {
            self.audio.mech.floppy_step(i);
        }
    }

    /// The CD32 pad is in serial mode: port 2 pin 5 driven low.
    fn pad_serial(&self) -> bool {
        self.cd32_pad && self.potgo & 0x2000 != 0 && self.potgo & 0x1000 == 0
    }

    /// The joystick in port 2, unless it is the CDTV remote in mouse mode.
    fn joy2(&self) -> [bool; 6] {
        if self.remote.mouse() {
            [false; 6]
        } else {
            self.joy
        }
    }

    fn cia_a_inputs(&self) -> u8 {
        let mut v = 0xffu8;
        if self.joy2()[4] {
            v &= !0x80;
        }
        if self.mouse_left || self.remote.buttons[0] || self.port1_joystick && self.joy1[4] {
            v &= !0x40;
        }
        for d in self.drives.iter().filter(|d| d.selected && d.present) {
            if d.ready() {
                v &= !0x20;
            }
            if d.cyl == 0 {
                v &= !0x10;
            }
            if d.disk.as_ref().is_none_or(|k| k.write_protected) {
                v &= !0x08;
            }
            if d.changed {
                v &= !0x04;
            }
        }
        v
    }

    /// Highest enabled and requested interrupt level (0 = none).
    fn ipl_level(&self) -> u8 {
        if self.intena & int::INTEN == 0 {
            return 0;
        }
        let act = self.intena & self.intreq & 0x3fff;
        if act == 0 {
            return 0;
        }
        match 15 - act.leading_zeros() {
            13 => 6,
            11 | 12 => 5,
            7..=10 => 4,
            4..=6 => 3,
            3 => 2,
            _ => 1,
        }
    }

    fn update_cia_irq(&mut self) {
        // Expansion boards share Paula's INT2 input with CIA-A.
        if self.cia_a.irq()
            || self.scsi.as_ref().is_some_and(|s| s.irq())
            || self.gayle.as_ref().is_some_and(|g| g.irq())
            || self.exp.cards.iter().any(|c| matches!(&c.dev, Device::Io(d) if d.irq()))
            || self.builtin.as_ref().is_some_and(|d| d.irq())
            || self.akiko.as_ref().is_some_and(|a| a.cd.irq())
        {
            self.intreq |= int::PORTS;
        }
        if self.cia_b.irq() || self.gayle.as_ref().is_some_and(|g| g.irq6()) {
            self.intreq |= int::EXTER;
        }
    }

    fn cia_read(&mut self, addr: u32) -> u16 {
        let reg = ((addr >> 8) & 15) as u8;
        let mut v = 0xffffu16;
        if addr & 0x2000 == 0 {
            v = (v & 0x00ff) | (self.cia_b.read(reg) as u16) << 8;
        }
        if addr & 0x1000 == 0 {
            self.cia_a.pa_in = self.cia_a_inputs();
            v = (v & 0xff00) | self.cia_a.read(reg) as u16;
        }
        v
    }

    fn cia_write(&mut self, addr: u32, v: u16, hi: bool, lo: bool) {
        let reg = ((addr >> 8) & 15) as u8;
        if addr & 0x2000 == 0 && hi {
            self.cia_b.write(reg, (v >> 8) as u8);
            if reg == 1 || reg == 3 {
                self.update_drive_lines();
            }
        }
        if addr & 0x1000 == 0 && lo {
            self.cia_a.write(reg, v as u8);
            if reg == 0 || reg == 2 {
                let pa = self.cia_a.pa_out();
                // The CD32 pad shifts on each rising edge of PA7 driven as a clock.
                let clock = self.cia_a.ddra & 0x80 != 0 && pa & 0x80 != 0;
                if clock && !self.pad_clock && self.pad_serial() {
                    self.pad_shift = self.pad_shift.saturating_sub(1);
                }
                self.pad_clock = clock;
                if let Some(a) = self.akiko.as_mut() {
                    // The CD32 has no overlay bit: PA0 mutes CD audio (low =
                    // muted), and the boot ROM's first write clears the overlay.
                    a.cd.muted = pa & 1 == 0;
                    self.overlay &= pa & 1 != 0;
                } else {
                    self.overlay = pa & 1 != 0;
                }
                self.audio.filter_on = pa & 2 == 0;
            }
            if reg == 14 {
                let out = self.cia_a.sp_output_mode();
                if self.sp_was_output && !out {
                    self.keyboard.handshake();
                }
                self.sp_was_output = out;
            }
        }
        self.update_cia_irq();
    }

    // ---- chip memory -----------------------------------------------------------

    #[inline]
    fn chip_word(&self, addr: u32) -> u16 {
        let a = (addr & self.chip_mask & !1) as usize;
        (self.chip[a] as u16) << 8 | self.chip[a + 1] as u16
    }

    #[inline]
    fn chip_write_word(&mut self, addr: u32, v: u16) {
        let a = (addr & self.chip_mask & !1) as usize;
        self.chip[a] = (v >> 8) as u8;
        self.chip[a + 1] = v as u8;
    }

    fn write_bpldat(&mut self, plane: usize, v: u64, bits: u32, h: u16) {
        let x0 = (h as usize * 2 + 3) * 2;
        self.denise.write_bpldat(plane, v, bits, x0);
    }

    fn start_blit(&mut self, w: u32, h: u32) {
        if self.dmacon & (dma::BLT | dma::MASTER) != (dma::BLT | dma::MASTER) {
            self.blitter.pending = Some((w, h));
            return;
        }
        self.blitter.pending = None;
        let mut mem = (&mut self.chip, self.chip_mask);
        let dur = self.blitter.run(&mut mem, w, h);
        self.blitter.busy_until = Some(self.cck + dur);
    }

    // ---- custom registers ----------------------------------------------------

    pub fn custom_read(&mut self, reg: u16) -> u16 {
        let v = self.custom_read_reg(reg);
        self.last_custom = v;
        v
    }

    fn custom_read_reg(&mut self, reg: u16) -> u16 {
        match reg & 0x1fe {
            0x000 => self.blitter.ddat,
            0x002 => {
                let mut v = self.dmacon & 0x07ff;
                if self.blitter.busy(self.cck) {
                    v |= 0x4000;
                }
                if self.blitter.zero {
                    v |= 0x2000;
                }
                v
            }
            0x004 => {
                let id = match (self.cfg.model.chipset(), self.cfg.ntsc) {
                    (Chipset::Ocs, false) => 0x00,
                    (Chipset::Ocs, true) => 0x10,
                    (Chipset::Ecs, false) => 0x20,
                    (Chipset::Ecs, true) => 0x30,
                    (Chipset::Aga, false) => 0x22,
                    (Chipset::Aga, true) => 0x32,
                };
                (self.lof as u16) << 15 | id << 8 | (self.vpos >> 8) & 1
            }
            0x006 => (self.vpos & 0xff) << 8 | (self.hpos & 0xff),
            0x008 => 0,
            0x00a if self.port1_joystick => joy_dat(self.joy1),
            0x00a => (self.mouse_y as u16) << 8 | self.mouse_x as u16,
            0x00c => joy_dat(self.joy2()),
            0x00e => self.denise.read_clxdat(),
            0x010 => self.adkcon,
            0x012 => self.pot[0],
            0x014 => self.pot[1],
            0x016 => {
                let mut v = 0xff00u16;
                // Pins POTGO drives as outputs read back what is driven.
                for (out, dat) in [(0x8000, 0x4000), (0x2000, 0x1000), (0x0800, 0x0400), (0x0200, 0x0100)] {
                    if self.potgo & out != 0 && self.potgo & dat == 0 {
                        v &= !dat;
                    }
                }
                if self.mouse_right || self.remote.buttons[1] || self.port1_joystick && self.joy1[5] {
                    v &= !0x0400;
                }
                if self.pad_serial() {
                    // Pin 9 is the shift register's output: the buttons (low
                    // when pressed) blue, red, yellow, green, forward, reverse,
                    // play, then a 1 and a 0 that identify the pad.
                    let b = [self.pad[0], self.pad[1], self.pad[2], self.pad[3], self.pad[4], self.joy[4], self.joy[5]];
                    let high = match self.pad_shift {
                        0 => false,
                        1 => true,
                        n => !b[(n - 2).min(6) as usize],
                    };
                    v = v & !0x4000 | if high { 0x4000 } else { 0 };
                } else if self.joy2()[5] {
                    v &= !0x4000;
                }
                v
            }
            0x018 => 0x3000,
            0x01a => {
                let mut v = self.dskbyt;
                self.dskbyt &= 0x6fff;
                if self.disk_dma.is_some() {
                    v |= 0x4000;
                }
                v
            }
            0x01c => self.intena,
            0x01e => self.intreq,
            // DENISEID: OCS Denise has no ID register; ECS Denise (8373) reads $FC,
            // AGA Lisa (4203) $F8.
            0x07c => match self.cfg.model.chipset() {
                Chipset::Ocs => 0xffff,
                Chipset::Ecs => 0xfffc,
                Chipset::Aga => 0x00f8,
            },
            // Write-only and unused registers return whatever was last on the
            // chip bus. Dr. Mario keeps its random number seed in BPL7PTL.
            _ => self.last_custom,
        }
    }

    fn set_clr(reg: &mut u16, v: u16) {
        if v & 0x8000 != 0 {
            *reg |= v & 0x7fff;
        } else {
            *reg &= !v;
        }
    }

    fn denise_x(&self) -> u16 {
        self.hpos * 4 + 4
    }

    pub fn custom_write_reg(&mut self, reg: u16, v: u16) {
        let reg = reg & 0x1fe;
        self.last_custom = v;
        match reg {
            0x020 => self.dskpt = (self.dskpt & 0xffff) | (v as u32) << 16,
            0x022 => self.dskpt = (self.dskpt & 0xffff_0000) | (v & 0xfffe) as u32,
            0x024 => self.write_dsklen(v),
            0x02a => self.lof = v & 0x8000 != 0,
            0x02e => self.copcon = v,
            0x030 => {
                self.serial_out.push(v as u8);
                self.intreq |= int::TBE;
            }
            0x034 => {
                self.potgo = v;
                if !self.pad_serial() {
                    // Pin 5 high or floating: the pad's register stays loaded.
                    self.pad_shift = 8;
                }
                if v & 1 != 0 {
                    // START: discharge the pot capacitors and begin counting.
                    self.pot = [0; 2];
                    self.pot_lines = 0;
                }
            }
            0x036 => {
                self.mouse_y = (v >> 8) as u8 & 0xfc;
                self.mouse_x = v as u8 & 0xfc;
            }
            0x040 => self.blitter.con0 = v,
            0x042 => self.blitter.con1 = v,
            0x044 => self.blitter.afwm = v,
            0x046 => self.blitter.alwm = v,
            0x048 => self.blitter.cpt = (self.blitter.cpt & 0xffff) | (v as u32) << 16,
            0x04a => self.blitter.cpt = (self.blitter.cpt & 0xffff_0000) | (v & 0xfffe) as u32,
            0x04c => self.blitter.bpt = (self.blitter.bpt & 0xffff) | (v as u32) << 16,
            0x04e => self.blitter.bpt = (self.blitter.bpt & 0xffff_0000) | (v & 0xfffe) as u32,
            0x050 => self.blitter.apt = (self.blitter.apt & 0xffff) | (v as u32) << 16,
            0x052 => self.blitter.apt = (self.blitter.apt & 0xffff_0000) | (v & 0xfffe) as u32,
            0x054 => self.blitter.dpt = (self.blitter.dpt & 0xffff) | (v as u32) << 16,
            0x056 => self.blitter.dpt = (self.blitter.dpt & 0xffff_0000) | (v & 0xfffe) as u32,
            0x058 => {
                let h = match v >> 6 {
                    0 => 1024,
                    n => n as u32,
                };
                let w = match v & 0x3f {
                    0 => 64,
                    n => n as u32,
                };
                self.start_blit(w, h);
            }
            0x05a if !self.ocs() => self.blitter.con0 = (self.blitter.con0 & 0xff00) | (v & 0xff),
            0x05c if !self.ocs() => self.blitter.sizv = v & 0x7fff,
            0x05e if !self.ocs() => {
                let h = match self.blitter.sizv {
                    0 => 0x8000,
                    n => n as u32,
                };
                let w = match v & 0x7ff {
                    0 => 0x800,
                    n => n as u32,
                };
                self.start_blit(w, h);
            }
            0x060 => self.blitter.cmod = (v & 0xfffe) as i16,
            0x062 => self.blitter.bmod = (v & 0xfffe) as i16,
            0x064 => self.blitter.amod = (v & 0xfffe) as i16,
            0x066 => self.blitter.dmod = (v & 0xfffe) as i16,
            0x070 => self.blitter.cdat = v,
            0x072 => self.blitter.bdat = v,
            0x074 => self.blitter.adat = v,
            0x07e => self.dsksync = v,
            0x080 => self.cop1lc = (self.cop1lc & 0xffff) | (v as u32) << 16,
            0x082 => self.cop1lc = (self.cop1lc & 0xffff_0000) | (v & 0xfffe) as u32,
            0x084 => self.cop2lc = (self.cop2lc & 0xffff) | (v as u32) << 16,
            0x086 => self.cop2lc = (self.cop2lc & 0xffff_0000) | (v & 0xfffe) as u32,
            0x088 => {
                self.cop_pc = self.cop1lc;
                self.cop_state = CopState::Fetch1;
            }
            0x08a => {
                self.cop_pc = self.cop2lc;
                self.cop_state = CopState::Fetch1;
            }
            0x08e => {
                self.diwhigh = None;
                self.denise.diwhigh = None;
                self.diwstrt = v;
                self.denise.diwstrt = v;
            }
            0x090 => {
                self.diwhigh = None;
                self.denise.diwhigh = None;
                self.diwstop = v;
                self.denise.diwstop = v;
            }
            0x092 => {
                self.ddfstrt = v;
                self.update_fetch_window();
            }
            0x094 => {
                self.ddfstop = v;
                self.update_fetch_window();
            }
            0x096 => {
                Self::set_clr(&mut self.dmacon, v & 0x87ff);
                if let Some((w, h)) = self.blitter.pending {
                    if self.dmacon & (dma::BLT | dma::MASTER) == (dma::BLT | dma::MASTER) {
                        self.start_blit(w, h);
                    }
                }
            }
            0x098 => self.denise.clxcon = v,
            0x09a => Self::set_clr(&mut self.intena, v),
            0x09c => {
                Self::set_clr(&mut self.intreq, v);
                self.update_cia_irq();
            }
            0x09e => Self::set_clr(&mut self.adkcon, v),
            0x0a0..=0x0df => {
                let ch = ((reg - 0xa0) >> 4) as usize;
                let c = &mut self.audio.ch[ch];
                match reg & 0xf {
                    0x0 => c.lc = (c.lc & 0xffff) | ((v as u32) << 16),
                    0x2 => c.lc = (c.lc & 0xffff_0000) | (v & 0xfffe) as u32,
                    0x4 => c.len = v,
                    0x6 => c.per = v,
                    0x8 => c.vol = (v & 0x7f) as u8,
                    0xa => self.audio.write_dat(ch, v),
                    _ => {}
                }
            }
            0x0e0..=0x0fe if reg < 0x0f8 || self.aga() => {
                let p = ((reg - 0xe0) >> 2) as usize;
                if reg & 2 == 0 {
                    self.bplpt[p] = (self.bplpt[p] & 0xffff) | (v as u32) << 16;
                } else {
                    self.bplpt[p] = (self.bplpt[p] & 0xffff_0000) | (v & 0xfffe) as u32;
                }
            }
            0x100 => {
                let x = self.denise_x();
                self.denise.write(reg, v, x);
                self.update_fetch_window();
            }
            0x102 | 0x104 => {
                let x = self.denise_x();
                self.denise.write(reg, v, x);
            }
            0x106 if !self.ocs() => self.denise.bplcon3 = v,
            0x10c if self.aga() => {
                let x = self.denise_x();
                self.denise.write(reg, v, x);
            }
            0x1fc if self.aga() => {
                self.fmode = v;
                self.denise.spr_width = match (v >> 2) & 3 {
                    0 => 16,
                    3 => 64,
                    _ => 32,
                };
                self.update_fetch_window();
            }
            0x1dc if !self.ocs() => self.pal = v & 0x20 != 0,
            0x1e4 if !self.ocs() => {
                self.diwhigh = Some(v);
                self.denise.diwhigh = Some(v);
            }
            0x108 => self.bpl1mod = (v & 0xfffe) as i16,
            0x10a => self.bpl2mod = (v & 0xfffe) as i16,
            0x110..=0x11e if reg < 0x11c || self.aga() => {
                let p = ((reg - 0x110) >> 1) as usize;
                let h = self.hpos;
                self.write_bpldat(p, (v as u64) << 48, 16, h);
            }
            0x120..=0x13e => {
                let n = ((reg - 0x120) >> 2) as usize;
                if reg & 2 == 0 {
                    self.sprpt[n] = (self.sprpt[n] & 0xffff) | (v as u32) << 16;
                } else {
                    self.sprpt[n] = (self.sprpt[n] & 0xffff_0000) | (v & 0xfffe) as u32;
                }
            }
            0x140..=0x17e => {
                let x = self.denise_x();
                self.denise.write(reg, v, x);
                if reg & 4 == 0 {
                    self.update_sprite_v(((reg - 0x140) >> 3) as usize);
                }
            }
            0x180..=0x1be => {
                let x = self.denise_x();
                self.denise.write(reg, v, x);
            }
            _ => {}
        }
    }

    // ---- autoconfig -----------------------------------------------------------

    /// Add a board's cards to the autoconfig chain (before power-on or reset).
    /// Zorro II cards go ahead of Zorro III ones.
    /// The I/O boards on the chain, then the motherboard controller.
    pub fn controllers(&mut self) -> impl Iterator<Item = &mut Box<dyn crate::expansion::Controller>> {
        self.exp.io().chain(self.builtin.iter_mut())
    }

    pub fn add_cards(&mut self, cards: Vec<Card>) {
        for c in cards {
            let at = if c.ac.zorro3 {
                self.exp.cards.len()
            } else {
                self.exp.cards.iter().position(|x| x.ac.zorro3).unwrap_or(self.exp.cards.len())
            };
            self.exp.cards.insert(at, c);
        }
    }


    /// Ramsey (A3000/A4000 memory controller): control at $DE0003, revision at
    /// $DE0043. Fat Gary's registers in the same block read as zero.
    fn ramsey_read(&self, a: u32) -> u8 {
        match a & 0xffff {
            0x0003 => self.ramsey_ctl,
            0x0043 => 0x0f, // Ramsey-07
            _ => 0,
        }
    }

    fn ramsey_write(&mut self, a: u32, v: u8) {
        if a & 0xffff == 0x0003 {
            self.ramsey_ctl = v;
        }
    }

    // ---- SCSI controller --------------------------------------------------------

    fn scsi_tick(&mut self) {
        let now = self.cck * 2;
        while let Some(dma) = self.scsi.as_mut().and_then(|s| s.tick(now)) {
            match dma {
                Dma::ToMemory(addr, data) => {
                    for (i, b) in data.into_iter().enumerate() {
                        self.dma_poke(addr.wrapping_add(i as u32), b);
                    }
                }
                Dma::FromMemory(addr, n) => {
                    let data = (0..n as u32).map(|i| self.dma_peek(addr.wrapping_add(i))).collect();
                    self.scsi.as_mut().unwrap().dma_done(data);
                }
            }
        }
        for i in 0..self.exp.cards.len() {
            if !matches!(self.exp.cards[i].dev, Device::Io(_)) {
                continue;
            }
            let mut dev = std::mem::replace(&mut self.exp.cards[i].dev, Device::Busy);
            if let Device::Io(d) = &mut dev {
                d.tick(now, &mut DmaPort(self));
                d.take_cd_audio(&mut self.audio.cd);
            }
            self.exp.cards[i].dev = dev;
        }
        if let Some(mut d) = self.builtin.take() {
            d.tick(now, &mut DmaPort(self));
            self.builtin = Some(d);
        }
        self.update_cia_irq();
    }

    /// The CD32's CD drive: one scan line of its DMA and command processing.
    fn akiko_tick(&mut self) {
        let Some(mut a) = self.akiko.take() else { return };
        a.cd.tick_line(&mut DmaPort(self));
        // CD audio joins the mix (44.1 kHz from the disc).
        if !a.cd.samples.is_empty() {
            self.audio.cd.extend(a.cd.samples.drain(..));
        }
        self.akiko = Some(a);
    }

    fn dma_poke(&mut self, a: u32, v: u8) {
        match self.region(a, true) {
            Region::Chip(o) => self.chip[o] = v,
            Region::Slow(o) => self.slow[o] = v,
            Region::Mb(o) => self.mb_ram[o] = v,
            Region::Ram(i, o) => self.exp.ram(i).unwrap()[o] = v,
            _ => {}
        }
    }

    fn dma_peek(&self, a: u32) -> u8 {
        match self.region(a, false) {
            Region::Chip(o) => self.chip[o],
            Region::Slow(o) => self.slow[o],
            Region::Mb(o) => self.mb_ram[o],
            Region::Ram(i, o) => self.exp.ram_ref(i)[o],
            _ => 0,
        }
    }

    // ---- input helpers --------------------------------------------------------

    pub fn mouse_move(&mut self, dx: i32, dy: i32) {
        self.mouse_x = self.mouse_x.wrapping_add(dx as u8);
        self.mouse_y = self.mouse_y.wrapping_add(dy as u8);
    }

    pub fn debug_state(&self) -> String {
        format!(
            "bplcon0={:04x} bplcon1={:04x} bplcon2={:04x} ddf={:02x}-{:02x} fetch={:02x}-{:02x} diw={:04x}-{:04x} mod={}/{} dmacon={:04x} intena={:04x} intreq={:04x} cop1lc={:06x} cop2lc={:06x} fmode={:04x} bplcon3={:04x} bplcon4={:04x}",
            self.denise.bplcon0, self.denise.bplcon1, self.denise.bplcon2, self.ddfstrt, self.ddfstop,
            self.fetch_start, self.fetch_end, self.diwstrt, self.diwstop, self.bpl1mod, self.bpl2mod,
            self.dmacon, self.intena, self.intreq, self.cop1lc, self.cop2lc, self.fmode,
            self.denise.bplcon3, self.denise.bplcon4
        )
    }

    /// Side-effect free memory read for debuggers.
    pub fn peek16(&self, addr: u32) -> u16 {
        let b = |o: usize, v: &Vec<u8>| (v[o] as u16) << 8 | v[o + 1] as u16;
        match self.region(addr & !1, false) {
            Region::Chip(o) => b(o, &self.chip),
            Region::Slow(o) => b(o, &self.slow),
            Region::Mb(o) => b(o, &self.mb_ram),
            Region::Ram(i, o) => b(o, self.exp.ram_ref(i)),
            Region::Rom(o) => b(o, &self.rom),
            Region::ExtRom(o) => b(o, &self.ext_rom),
            Region::BootRom(o) => b(o, &self.boot_rom),
            Region::Scsi(o) if o >= 0x2000 => self.scsi.as_ref().unwrap().peek_rom16(o),
            Region::Io(i, o) => match &self.exp.cards[i].dev {
                Device::Io(d) => d.peek16(o),
                _ => 0,
            },
            _ => 0,
        }
    }

    pub fn take_audio(&mut self) -> Vec<f32> {
        std::mem::take(&mut self.audio.out)
    }
}

enum Region {
    Chip(usize),
    Slow(usize),
    /// RAM on expansion card .0.
    Ram(usize, usize),
    Rom(usize),
    ExtRom(usize),
    /// The emulator's own ROM at $F00000 (the graphics card driver).
    BootRom(usize),
    Custom,
    Cia,
    Autoconfig,
    Scsi(u32),
    /// An I/O board on the chain: (card, offset).
    Io(usize, u32),
    /// The motherboard controller: offset.
    Builtin(u32),
    /// The A600/A1200 PCMCIA slot's memory windows.
    Pcmcia,
    Gayle,
    Z3Config,
    Mb(usize),
    Ramsey,
    Akiko,
    /// The clock: one nibble per 4 bytes, in the low bits of the odd byte.
    Rtc,
    /// The CDTV's battery-backed RAM: offset.
    Nvram(usize),
    None,
}

impl Board {
    /// On the CDTV the CD32 pad's buttons stand in for the front panel and remote:
    /// play is play/pause, reverse and forward skip tracks, green is stop. They
    /// reach the machine as raw keys $72-$77, unless the ROM has put the drive in
    /// front-panel mode, where the drive acts on them itself. The remote
    /// switch flips the remote between joystick and mouse mode.
    fn cdtv_buttons(&mut self) {
        if self.cfg.model != crate::config::Model::Cdtv {
            return;
        }
        if self.remote_switch && !self.remote_switch_prev {
            self.remote.set_mouse(!self.remote.mouse());
            log::info!("CDTV remote: {} mode", if self.remote.mouse() { "mouse" } else { "joystick" });
        }
        self.remote_switch_prev = self.remote_switch;
        if self.pad == self.pad_prev {
            return;
        }
        const BUTTON: [u8; 4] = [1, 2, 3, 0];
        for i in 0..4 {
            let down = self.pad[i];
            if down == self.pad_prev[i] {
                continue;
            }
            let b = BUTTON[i];
            let drive = self.exp.cards.iter_mut().find_map(|c| match &mut c.dev {
                Device::Io(d) if d.front_panel_active() => Some(d),
                _ => None,
            });
            match drive {
                Some(d) if down => d.front_panel(b),
                Some(_) => {}
                None => self.keyboard.key(0x72 + b, down),
            }
        }
        self.pad_prev = self.pad;
    }

    /// The battery-backed or EEPROM memory that should outlive the session, with a
    /// short name for its file: the CDTV's bookmark RAM, the CD32's settings EEPROM.
    pub fn nvram_mut(&mut self) -> Option<(&'static str, &mut Vec<u8>)> {
        if let Some(a) = self.akiko.as_mut() {
            Some(("cd32", &mut a.nvram))
        } else if !self.nvram.is_empty() {
            Some(("cdtv", &mut self.nvram))
        } else {
            None
        }
    }

    #[inline]
    fn region(&self, a: u32, write: bool) -> Region {
        if a >> 24 != 0 && self.cfg.model.addr32() {
            return self.region32(a);
        }
        let a = a & 0xff_ffff;
        match a >> 16 {
            0x00..=0x1f => {
                if self.overlay && !write && a < 0x80000 {
                    Region::Rom((a & self.rom_mask) as usize)
                } else {
                    Region::Chip((a & self.chip_mask) as usize)
                }
            }
            0x60..=0x9f if self.exp.lookup24(a).is_none() && self.gayle.as_ref().is_some_and(|g| g.card_present()) => Region::Pcmcia,
            0x20..=0x9f => self.card_region(self.exp.lookup24(a)),
            0xa0..=0xa3 if self.gayle.as_ref().is_some_and(|g| g.card_present()) => Region::Pcmcia,
            0xb8 if self.akiko.is_some() => Region::Akiko,
            0xdc if a & 0x8000 != 0 && !self.nvram.is_empty() => Region::Nvram(a as usize & 0x3fff),
            0xdc if self.rtc.is_some() => Region::Rtc,
            0xa0..=0xbf => Region::Cia,
            0xc0..=0xd7 => {
                let off = (a - 0xc0_0000) as usize;
                if off < self.slow.len() {
                    Region::Slow(off)
                } else if self.ocs() {
                    // Unpopulated slow RAM space mirrors the custom chips; Kickstart
                    // relies on this to size the trapdoor expansion.
                    Region::Custom
                } else {
                    Region::None
                }
            }
            0xdf => Region::Custom,
            0xd8..=0xdb | 0xde if self.gayle.as_ref().is_some_and(|g| !g.a4000) => Region::Gayle,
            0xdd if self.builtin.is_some() && (a & 0xff40) == 0x0040 => Region::Builtin(a & 0x3f),
            0xdd if self.gayle.as_ref().is_some_and(|g| g.a4000) => Region::Gayle,
            0xde if self.cfg.model.addr32() => Region::Ramsey,
            0xe8 => Region::Autoconfig,
            0xdd if self.scsi.as_ref().is_some_and(|s| s.a3000) => Region::Scsi(a & 0xffff),
            0xe9..=0xef if self.exp.lookup24(a).is_some() => self.card_region(self.exp.lookup24(a)),
            0xf8..=0xff => Region::Rom((a & self.rom_mask) as usize),
            // The CDTV's extended ROM sits at $F00000.
            0xf0..=0xf7 if self.cfg.model == crate::config::Model::Cdtv && !self.ext_rom.is_empty() => {
                Region::ExtRom((a as usize - 0xf0_0000) % self.ext_rom.len())
            }
            0xf0 if !self.boot_rom.is_empty() => Region::BootRom(a as usize & 0xffff),
            0xe0..=0xe7 if !self.ext_rom.is_empty() && self.cfg.model != crate::config::Model::Cdtv => Region::ExtRom((a as usize - 0xe0_0000) % self.ext_rom.len()),
            0xe0..=0xe7 if self.rom.len() > 0x80000 => Region::Rom((a & self.rom_mask) as usize),
            _ => {
                if std::env::var_os("AMIGA_LOG_UNMAPPED").is_some() {
                    eprintln!("unmapped {} {a:06x} pc={:06x}", if write { "W" } else { "R" }, self.cur_pc);
                }
                Region::None
            }
        }
    }
}

/// JOYxDAT for a joystick (up, down, left, right): the direction switches
/// drive the mouse counter inputs.
fn joy_dat(j: [bool; 6]) -> u16 {
    let [up, down, left, right, ..] = j;
    let mut v = 0u16;
    if right {
        v |= 2;
    }
    if left {
        v |= 0x200;
    }
    if down ^ right {
        v |= 1;
    }
    if up ^ left {
        v |= 0x100;
    }
    v
}

/// The autoconfig chain a configuration asks for: the generic fast RAM sizes
/// (`--fast`, `--z3`, RP9 RAM), then the boards chosen with `--board`.
fn expansion_cards(cfg: &Config) -> Vec<Card> {
    let mut specs: Vec<(&str, registry::Options)> = vec![];
    let size = |kb: u32| registry::Options(vec![("ram".into(), format!("{kb}K"))]);
    if cfg.fast_kb > 0 {
        specs.push(("z2ram", size(cfg.fast_kb)));
    }
    for b in &cfg.boards {
        specs.push((b.id.as_str(), b.opts.clone()));
    }
    if cfg.z3_kb > 0 {
        specs.push(("z3ram", size(cfg.z3_kb)));
    }
    let mut cards: Vec<Card> = vec![];
    if cfg.model == crate::config::Model::Cdtv {
        // The CDTV's DMAC (CD controller) is first on the chain, at $E90000.
        let ac = crate::expansion::Autoconfig { zorro3: false, memory: false, diag_rom: None, size: 0x10000, manufacturer: 514, product: 3, serial: 0 };
        cards.push(Card { board: "cdtv-dmac", ac, base: None, state: crate::expansion::CardState::Waiting, dev: Device::Io(Box::new(crate::cdtv::CdtvDmac::new())) });
    }
    for (id, opts) in specs {
        match registry::find(id).map(|b| b.build(&opts)) {
            Some(Ok(p)) => cards.extend(p.cards),
            Some(Err(e)) => log::error!("{id}: {e}"),
            None => log::error!("unknown board {id}"),
        }
    }
    // Zorro II boards come first in the chain.
    cards.sort_by_key(|c| c.ac.zorro3);
    cards
}

/// Memory as DMA from an expansion board reaches it.
struct DmaPort<'a>(&'a mut Board);

impl crate::expansion::DmaMem for DmaPort<'_> {
    fn read8(&mut self, a: u32) -> u8 {
        self.0.dma_peek(a)
    }
    fn write8(&mut self, a: u32, v: u8) {
        self.0.dma_poke(a, v)
    }
}

impl Board {
    /// The region of a configured expansion card at (card, offset).
    #[inline]
    fn card_region(&self, hit: Option<(usize, u32)>) -> Region {
        match hit {
            Some((i, o)) => match self.exp.cards[i].dev {
                Device::Ram(_) => Region::Ram(i, o as usize),
                Device::A2091 => Region::Scsi(o),
                Device::Io(_) => Region::Io(i, o),
                Device::Busy => Region::None,
            },
            None => Region::None,
        }
    }

    /// Addresses above 16 MB on 32-bit machines.
    fn region32(&self, a: u32) -> Region {
        let mb_start = 0x0800_0000 - self.mb_ram.len() as u32;
        if (mb_start..0x0800_0000).contains(&a) {
            return Region::Mb((a - mb_start) as usize);
        }
        if let Some(c) = self.exp.lookup32(a) {
            return self.card_region(Some(c));
        }
        if a >> 24 == 0xff {
            return Region::Z3Config;
        }
        Region::None
    }
}

impl Bus for Board {
    fn read8(&mut self, addr: u32) -> u8 {
        match self.region(addr, false) {
            Region::Chip(o) => {
                self.chip_access();
                self.chip[o]
            }
            Region::Slow(o) => {
                self.chip_access();
                self.slow[o]
            }
            Region::Ram(i, o) => {
                self.cpu_cycles(4);
                self.exp.ram_ref(i)[o]
            }
            Region::Mb(o) => {
                self.cpu_cycles(4);
                self.mb_ram[o]
            }
            Region::Z3Config => {
                self.cpu_cycles(4);
                self.exp.z3_config_read(addr)
            }
            Region::Ramsey => {
                self.cpu_cycles(4);
                self.ramsey_read(addr)
            }
            Region::Akiko => {
                self.cpu_cycles(4);
                self.akiko.as_mut().map_or(0, |a| a.read8(addr))
            }
            Region::Rtc => {
                self.cpu_cycles(4);
                if addr & 1 == 0 {
                    0
                } else {
                    self.rtc.as_ref().unwrap().read((addr as usize >> 2) & 15)
                }
            }
            Region::Rom(o) => {
                self.cpu_cycles(4);
                self.rom[o]
            }
            Region::ExtRom(o) => {
                self.cpu_cycles(4);
                self.ext_rom[o]
            }
            Region::BootRom(o) => {
                self.cpu_cycles(4);
                self.boot_rom[o]
            }
            Region::Nvram(o) => {
                self.cpu_cycles(4);
                self.nvram[o]
            }
            Region::Custom => {
                self.chip_access();
                let w = self.custom_read((addr & 0x1fe) as u16);
                if addr & 1 == 0 {
                    (w >> 8) as u8
                } else {
                    w as u8
                }
            }
            Region::Cia => {
                self.cia_access();
                let w = self.cia_read(addr);
                if addr & 1 == 0 {
                    (w >> 8) as u8
                } else {
                    w as u8
                }
            }
            Region::Autoconfig => {
                self.cpu_cycles(4);
                self.exp.z2_config_read(addr)
            }
            Region::Gayle => {
                self.cpu_cycles(4);
                let v = self.gayle.as_mut().unwrap().read8(addr);
                self.update_cia_irq();
                v
            }
            Region::Builtin(o) => {
                self.cpu_cycles(4);
                let now = self.cycles;
                let v = self.builtin.as_mut().unwrap().read8(o, now);
                self.update_cia_irq();
                v
            }
            Region::Pcmcia => {
                self.cpu_cycles(4);
                self.gayle.as_ref().unwrap().card_read8(addr)
            }
            Region::Io(i, o) => {
                self.cpu_cycles(4);
                let now = self.cycles;
                let v = self.exp.io_at(i).read8(o, now);
                self.update_cia_irq();
                v
            }
            Region::Scsi(o) => {
                self.cpu_cycles(4);
                let now = self.cycles;
                let v = self.scsi.as_mut().unwrap().read8(o, now);
                if o < 0x2000 {
                    log::trace!("a2091 pc={:06x} read8( {o:02x} -> {v:x}", self.cur_pc);
                }
                self.update_cia_irq();
                v
            }
            Region::None => {
                self.cpu_cycles(4);
                0
            }
        }
    }

    fn read16(&mut self, addr: u32) -> u16 {
        let addr = addr & !1;
        match self.region(addr, false) {
            Region::Chip(o) => {
                self.chip_access();
                (self.chip[o] as u16) << 8 | self.chip[o + 1] as u16
            }
            Region::Slow(o) => {
                self.chip_access();
                (self.slow[o] as u16) << 8 | self.slow[o + 1] as u16
            }
            Region::Ram(i, o) => {
                self.cpu_cycles(4);
                let r = self.exp.ram_ref(i);
                (r[o] as u16) << 8 | r[o + 1] as u16
            }
            Region::Mb(o) => {
                self.cpu_cycles(4);
                (self.mb_ram[o] as u16) << 8 | self.mb_ram[o + 1] as u16
            }
            Region::Z3Config => {
                self.cpu_cycles(4);
                (self.exp.z3_config_read(addr) as u16) << 8 | 0xff
            }
            Region::Ramsey => {
                self.cpu_cycles(4);
                (self.ramsey_read(addr) as u16) << 8 | self.ramsey_read(addr + 1) as u16
            }
            Region::Akiko => {
                self.cpu_cycles(4);
                let a = self.akiko.as_mut().unwrap();
                (a.read8(addr) as u16) << 8 | a.read8(addr + 1) as u16
            }
            Region::Rtc => {
                self.cpu_cycles(4);
                self.rtc.as_ref().unwrap().read((addr as usize >> 2) & 15) as u16
            }
            Region::Rom(o) => {
                self.cpu_cycles(4);
                (self.rom[o] as u16) << 8 | self.rom[o + 1] as u16
            }
            Region::ExtRom(o) => {
                self.cpu_cycles(4);
                (self.ext_rom[o] as u16) << 8 | self.ext_rom[o + 1] as u16
            }
            Region::BootRom(o) => {
                self.cpu_cycles(4);
                (self.boot_rom[o] as u16) << 8 | self.boot_rom[o + 1] as u16
            }
            Region::Nvram(o) => {
                self.cpu_cycles(4);
                (self.nvram[o] as u16) << 8 | self.nvram[o + 1] as u16
            }
            Region::Custom => {
                self.chip_access();
                self.custom_read((addr & 0x1fe) as u16)
            }
            Region::Cia => {
                self.cia_access();
                self.cia_read(addr)
            }
            Region::Autoconfig => {
                self.cpu_cycles(4);
                (self.exp.z2_config_read(addr) as u16) << 8 | 0xff
            }
            Region::Gayle => {
                self.cpu_cycles(4);
                let v = self.gayle.as_mut().unwrap().read16(addr);
                self.update_cia_irq();
                v
            }
            Region::Builtin(o) => {
                self.cpu_cycles(4);
                let now = self.cycles;
                let v = self.builtin.as_mut().unwrap().read16(o, now);
                self.update_cia_irq();
                v
            }
            Region::Pcmcia => {
                self.cpu_cycles(4);
                let g = self.gayle.as_ref().unwrap();
                (g.card_read8(addr) as u16) << 8 | g.card_read8(addr | 1) as u16
            }
            Region::Io(i, o) => {
                self.cpu_cycles(4);
                let now = self.cycles;
                let v = self.exp.io_at(i).read16(o, now);
                self.update_cia_irq();
                v
            }
            Region::Scsi(o) => {
                self.cpu_cycles(4);
                let now = self.cycles;
                let v = self.scsi.as_mut().unwrap().read16(o, now);
                if o < 0x2000 {
                    log::trace!("a2091 pc={:06x} read16 {o:02x} -> {v:x}", self.cur_pc);
                }
                self.update_cia_irq();
                v
            }
            Region::None => {
                self.cpu_cycles(4);
                0
            }
        }
    }

    fn write8(&mut self, addr: u32, v: u8) {
        match self.region(addr, true) {
            Region::Chip(o) => {
                self.chip_access();
                self.chip[o] = v;
            }
            Region::Slow(o) => {
                self.chip_access();
                self.slow[o] = v;
            }
            Region::Ram(i, o) => {
                self.cpu_cycles(4);
                self.exp.ram(i).unwrap()[o] = v;
            }
            Region::Mb(o) => {
                self.cpu_cycles(4);
                self.mb_ram[o] = v;
            }
            Region::Z3Config => {
                self.cpu_cycles(4);
                self.exp.z3_config_write(addr, v as u16, false);
            }
            Region::Ramsey => {
                self.cpu_cycles(4);
                self.ramsey_write(addr, v);
            }
            Region::Akiko => {
                self.cpu_cycles(4);
                if let Some(a) = self.akiko.as_mut() {
                    a.write8(addr, v);
                }
            }
            Region::Rtc => {
                self.cpu_cycles(4);
                if addr & 1 != 0 {
                    self.rtc.as_mut().unwrap().write((addr as usize >> 2) & 15, v);
                }
            }
            Region::Nvram(o) => {
                self.cpu_cycles(4);
                self.nvram[o] = v;
            }
            Region::Custom => {
                self.chip_access();
                // The 68000 puts the byte on both halves of the data bus.
                self.custom_write_reg((addr & 0x1fe) as u16, (v as u16) << 8 | v as u16);
            }
            Region::Cia => {
                self.cia_access();
                let w = (v as u16) << 8 | v as u16;
                self.cia_write(addr, w, addr & 1 == 0, addr & 1 == 1);
            }
            Region::Autoconfig => {
                self.cpu_cycles(4);
                self.exp.z2_config_write(addr, v);
            }
            Region::Gayle => {
                self.cpu_cycles(4);
                self.gayle.as_mut().unwrap().write8(addr, v);
                self.update_cia_irq();
            }
            Region::Builtin(o) => {
                self.cpu_cycles(4);
                let now = self.cycles;
                self.builtin.as_mut().unwrap().write8(o, v, now);
                self.update_cia_irq();
            }
            Region::Pcmcia => {
                self.cpu_cycles(4);
                self.gayle.as_mut().unwrap().card_write8(addr, v);
            }
            Region::Io(i, o) => {
                self.cpu_cycles(4);
                let now = self.cycles;
                self.exp.io_at(i).write8(o, v, now);
                self.update_cia_irq();
            }
            Region::Scsi(o) => {
                self.cpu_cycles(4);
                let now = self.cycles;
                log::trace!("a2091 pc={:06x} write8( {o:02x} = {v:x}", self.cur_pc);
                self.scsi.as_mut().unwrap().write8(o, v, now);
                self.update_cia_irq();
            }
            Region::Rom(_) | Region::ExtRom(_) | Region::BootRom(_) | Region::None => self.cpu_cycles(4),
        }
    }

    fn write16(&mut self, addr: u32, v: u16) {
        let addr = addr & !1;
        match self.region(addr, true) {
            Region::Chip(o) => {
                self.chip_access();
                self.chip[o] = (v >> 8) as u8;
                self.chip[o + 1] = v as u8;
            }
            Region::Slow(o) => {
                self.chip_access();
                self.slow[o] = (v >> 8) as u8;
                self.slow[o + 1] = v as u8;
            }
            Region::Ram(i, o) => {
                self.cpu_cycles(4);
                let r = self.exp.ram(i).unwrap();
                r[o] = (v >> 8) as u8;
                r[o + 1] = v as u8;
            }
            Region::Mb(o) => {
                self.cpu_cycles(4);
                self.mb_ram[o] = (v >> 8) as u8;
                self.mb_ram[o + 1] = v as u8;
            }
            Region::Z3Config => {
                self.cpu_cycles(4);
                self.exp.z3_config_write(addr, v, true);
            }
            Region::Ramsey => {
                self.cpu_cycles(4);
                self.ramsey_write(addr, (v >> 8) as u8);
                self.ramsey_write(addr + 1, v as u8);
            }
            Region::Akiko => {
                self.cpu_cycles(4);
                if let Some(a) = self.akiko.as_mut() {
                    a.write8(addr, (v >> 8) as u8);
                    a.write8(addr + 1, v as u8);
                }
            }
            Region::Rtc => {
                self.cpu_cycles(4);
                self.rtc.as_mut().unwrap().write((addr as usize >> 2) & 15, v as u8);
            }
            Region::Nvram(o) => {
                self.cpu_cycles(4);
                self.nvram[o] = (v >> 8) as u8;
                self.nvram[o + 1] = v as u8;
            }
            Region::Custom => {
                self.chip_access();
                self.custom_write_reg((addr & 0x1fe) as u16, v);
            }
            Region::Cia => {
                self.cia_access();
                self.cia_write(addr, v, true, true);
            }
            Region::Autoconfig => {
                self.cpu_cycles(4);
                self.exp.z2_config_write(addr, (v >> 8) as u8);
            }
            Region::Gayle => {
                self.cpu_cycles(4);
                self.gayle.as_mut().unwrap().write16(addr, v);
                self.update_cia_irq();
            }
            Region::Builtin(o) => {
                self.cpu_cycles(4);
                let now = self.cycles;
                self.builtin.as_mut().unwrap().write16(o, v, now);
                self.update_cia_irq();
            }
            Region::Pcmcia => {
                self.cpu_cycles(4);
                let g = self.gayle.as_mut().unwrap();
                g.card_write8(addr, (v >> 8) as u8);
                g.card_write8(addr | 1, v as u8);
            }
            Region::Io(i, o) => {
                self.cpu_cycles(4);
                let now = self.cycles;
                self.exp.io_at(i).write16(o, v, now);
                self.update_cia_irq();
            }
            Region::Scsi(o) => {
                self.cpu_cycles(4);
                let now = self.cycles;
                log::trace!("a2091 pc={:06x} write16 {o:02x} = {v:x}", self.cur_pc);
                self.scsi.as_mut().unwrap().write16(o, v, now);
                self.update_cia_irq();
            }
            Region::Rom(_) | Region::ExtRom(_) | Region::BootRom(_) | Region::None => self.cpu_cycles(4),
        }
    }

    #[inline]
    fn idle(&mut self, cycles: u32) {
        self.cpu_cycles(cycles);
    }

    fn ipl(&mut self) -> u8 {
        // Paula's IPL outputs follow INTREQ/INTENA a little late, and the 68000
        // samples them before the end of an instruction: a new level only
        // reaches the CPU a couple of colour clocks after it is raised. Code
        // that polls VPOSR for line 0 relies on seeing that line before the
        // vertical blank interrupt is taken (F1 GP Circuits).
        let level = self.ipl_level();
        if level != self.ipl_pending {
            self.ipl_pending = level;
            self.ipl_since = self.cck;
        }
        if level < self.ipl_out || self.cck >= self.ipl_since + IPL_DELAY_CCK {
            self.ipl_out = level;
        }
        self.ipl_out
    }

    fn reset_devices(&mut self) {
        self.reset_chips();
    }
}

/// A complete machine: CPU plus board.
pub struct Amiga {
    pub cpu: crate::cpu::Cpu,
    pub board: Board,
    /// Ring buffer of recently executed PCs when tracing is enabled.
    pub pc_history: Option<std::collections::VecDeque<u32>>,
    /// Stop `run_frame` when the PC reaches this address.
    pub breakpoint: Option<u32>,
    /// The breakpoint only fires from this frame on.
    pub break_min_frame: u64,
    /// TRACE_RANGE only logs in these frames (inclusive).
    pub trace_frames: (u64, u64),
    pub break_hit: bool,
    /// Print registers every time the PC reaches this address.
    pub tracepoint: Option<u32>,
    /// Print every instruction executed within this address range.
    pub trace_range: Option<(u32, u32)>,
}

impl Amiga {
    pub fn new(cfg: Config, rom: Vec<u8>) -> Amiga {
        let mut cpu = crate::cpu::Cpu::with_model(cfg.cpu);
        cpu.fpu = cfg.fpu;
        let mut a = Amiga { cpu, board: Board::new(cfg, rom), pc_history: None, breakpoint: None, break_min_frame: 0, trace_frames: (0, u64::MAX), break_hit: false, tracepoint: None, trace_range: None };
        a.hard_reset();
        a
    }

    pub fn hard_reset(&mut self) {
        self.board.reset_chips();
        self.board.keyboard = Keyboard::new();
        self.cpu.reset(&mut self.board);
    }

    /// AmigaOS's ready and waiting tasks (exec's TaskReady and TaskWait lists):
    /// name, state, the signals waited for and received, and the saved stack
    /// pointer. Then Intuition's screens with their bitmaps.
    pub fn exec_tasks(&self) -> Vec<String> {
        let b = &self.board;
        let rl = |a: u32| (b.peek16(a) as u32) << 16 | b.peek16(a + 2) as u32;
        let name = |a: u32| -> String {
            let mut s = String::new();
            for i in 0..40 {
                let w = b.peek16((a + i) & !1);
                let c = if (a + i) & 1 == 0 { (w >> 8) as u8 } else { w as u8 };
                if c == 0 {
                    break;
                }
                s.push(c as char);
            }
            s
        };
        let exec = rl(4);
        let mut out = vec![];
        for (list, state) in [(406u32, "ready"), (420, "wait")] {
            let mut n = rl(exec + list);
            for _ in 0..100 {
                let next = rl(n);
                if next == 0 {
                    break;
                }
                out.push(format!(
                    "task {state:5} {:08x} {:24} sigwait={:08x} sigrecvd={:08x} sp={:08x}",
                    n,
                    name(rl(n + 10)),
                    rl(n + 22),
                    rl(n + 26),
                    rl(n + 54)
                ));
                n = next;
            }
        }
        // Intuition's screens, front to back.
        let mut lib = rl(exec + 378);
        for _ in 0..100 {
            if rl(lib) == 0 {
                break;
            }
            if name(rl(lib + 10)) == "intuition.library" {
                let mut s = rl(lib + 60);
                for _ in 0..20 {
                    if s == 0 {
                        break;
                    }
                    let bm = rl(s + 88);
                    out.push(format!(
                        "screen {:08x} {:24} {}x{} rp.bitmap={:08x} bpr={} rows={} flags={:02x} depth={} planes={:08x} {:08x}",
                        s,
                        name(rl(s + 22)),
                        b.peek16(s + 12),
                        b.peek16(s + 14),
                        bm,
                        b.peek16(bm),
                        b.peek16(bm + 2),
                        b.peek16(bm + 4) >> 8,
                        b.peek16(bm + 4) & 0xff,
                        rl(bm + 8),
                        rl(bm + 12)
                    ));
                    s = rl(s);
                }
            }
            lib = rl(lib);
        }
        out
    }

    pub fn disasm(&self, pc: u32, n: usize) -> Vec<String> {
        let mut out = vec![];
        let mut a = pc;
        for _ in 0..n {
            let (t, len) = crate::disasm::disasm(a, &|x| self.board.peek16(x));
            out.push(format!("{a:06x}  {t}"));
            a = a.wrapping_add(len);
        }
        out
    }

    /// Run until the end of the current video frame.
    pub fn run_frame(&mut self) {
        self.board.frame_done = false;
        while !self.board.frame_done {
            if self.tracepoint == Some(self.cpu.pc) {
                let c = &self.cpu;
                eprintln!(
                    "trace {:06x} f={} v={} h={} D={:08x?} A={:08x?}",
                    c.pc, self.board.frame_count, self.board.vpos, self.board.hpos, c.d, c.a
                );
            }
            if let Some((lo, hi)) = self.trace_range {
                let pc = self.cpu.pc;
                if (lo..hi).contains(&pc) && self.trace_frames.0 <= self.board.frame_count && self.board.frame_count <= self.trace_frames.1 {
                    let c = &self.cpu;
                    let (t, _) = crate::disasm::disasm(pc, &|x| self.board.peek16(x));
                    eprintln!(
                        "{pc:06x} {t:<32} v={} h={} cyc={} D={:08x?} A={:08x?}",
                        self.board.vpos, self.board.hpos, self.board.cycles, c.d, c.a
                    );
                }
            }
            if self.breakpoint == Some(self.cpu.pc) && !self.break_hit && self.board.frame_count >= self.break_min_frame {
                self.break_hit = true;
                return;
            }
            if let Some(h) = self.pc_history.as_mut() {
                if h.back() != Some(&self.cpu.pc) {
                    if h.len() >= 4096 {
                        h.pop_front();
                    }
                    h.push_back(self.cpu.pc);
                }
            }
            if self.cpu.pc == crate::rtg::TRAP_PC && self.board.rtg.is_some() {
                crate::rtg::trap(&mut self.cpu, &mut self.board);
                continue;
            }
            self.board.cur_pc = self.cpu.pc;
            self.cpu.step(&mut self.board);
            self.board.sync();
        }
    }
}
