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
use crate::floppy::Drive;
use crate::keyboard::Keyboard;

pub const PAL_CCK_RATE: u32 = 3_546_895;
pub const NTSC_CCK_RATE: u32 = 3_579_545;
const LINE_CCKS: u16 = 227;
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

pub struct Board {
    pub cfg: Config,
    pub chip: Vec<u8>,
    pub chip_mask: u32,
    pub slow: Vec<u8>,
    pub fast: Vec<u8>,
    fast_base: Option<u32>,
    autoconfig_done: bool,
    /// A590/A2091 SCSI controller, if fitted (configured after the fast RAM).
    pub scsi: Option<A2091>,
    /// Gayle gate array with IDE (A600/A1200).
    pub gayle: Option<crate::gayle::Gayle>,
    scsi_ac_done: bool,
    pub rom: Vec<u8>,
    /// Extension ROM at $E00000 (AROS, CD32, CDTV).
    pub ext_rom: Vec<u8>,
    rom_mask: u32,
    pub overlay: bool,

    /// CPU clock (7.09 MHz PAL); the chipset runs at half this rate.
    pub cycles: u64,
    pub cck: u64,
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
        Board {
            chip: vec![0; chip_size],
            chip_mask: chip_size as u32 - 1,
            slow: vec![0; cfg.slow_kb as usize * 1024],
            fast: vec![0; cfg.fast_kb as usize * 1024],
            fast_base: None,
            autoconfig_done: false,
            scsi: None,
            gayle: matches!(cfg.model, crate::config::Model::A600 | crate::config::Model::A1200).then(crate::gayle::Gayle::new),
            scsi_ac_done: false,
            rom,
            rom_mask,
            ext_rom: vec![],
            overlay: true,
            cycles: 0,
            cck: 0,
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
            joy: [false; 6],
            potgo: 0,
            pot: [0; 2],
            pot_lines: 0,
            pal: !cfg.ntsc,
            diwhigh: None,
            serial_out: vec![],
            cur_pc: 0,
            cfg,
        }
    }

    pub fn framebuffer(&self) -> (&[u32], usize, usize) {
        (&self.denise.fb, FB_W, FB_H)
    }

    fn ocs(&self) -> bool {
        self.cfg.model.chipset() == Chipset::Ocs
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
        self.fast_base = None;
        self.autoconfig_done = false;
        self.scsi_ac_done = false;
        if let Some(s) = self.scsi.as_mut() {
            s.reset();
        }
        if let Some(g) = self.gayle.as_mut() {
            g.reset();
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
        // Chip bus cycles start on an even colour clock.
        self.cycles = (self.cycles + 3) & !3;
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
            for p in 0..n {
                let m = if p & 1 == 0 { self.bpl1mod } else { self.bpl2mod };
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
            self.audio.tick(&mut mem, aud_en, self.adkcon)
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
        if self.vpos >= self.lines_per_frame() {
            self.vpos = 0;
            self.lof = if lace { !self.lof } else { true };
            self.frame_done = true;
            self.frame_count += 1;
            self.intreq |= int::VERTB;
            self.cia_a.tod_tick();
            self.cop_pc = self.cop1lc;
            self.cop_state = CopState::Fetch1;
            for n in 0..8 {
                self.spr_slot[n] = SprSlot::None;
            }
        }
        self.scsi_tick();
        self.update_cia_irq();
        self.start_line();
    }

    fn start_line(&mut self) {
        let (vstart, vstop) = match self.diwhigh {
            Some(hi) => ((self.diwstrt >> 8) | (hi & 7) << 8, (self.diwstop >> 8) | ((hi >> 8) & 7) << 8),
            None => (self.diwstrt >> 8, (self.diwstop >> 8) | if self.diwstop & 0x8000 == 0 { 0x100 } else { 0 }),
        };
        if self.vpos == vstart {
            self.vdiw = true;
        }
        if self.vpos == vstop {
            self.vdiw = false;
        }
        if self.vpos == 0 {
            self.vdiw = false;
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
        let Some(i) = sel else { return };

        let writing = dma_on && self.disk_dma.as_ref().is_some_and(|d| d.write);
        if writing {
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
            let (bit, index) = self.drives[i].read_bit();
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
        for (i, d) in self.drives.iter_mut().enumerate() {
            let bit = 0x08 << i;
            let sel = new & bit == 0;
            let was = old & bit == 0;
            if sel && !was {
                d.motor = motor;
            }
            d.selected = sel;
            d.side = side;
            if sel && step && d.present {
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
    }

    fn cia_a_inputs(&self) -> u8 {
        let mut v = 0xffu8;
        if self.joy[4] {
            v &= !0x80;
        }
        if self.mouse_left {
            v &= !0x40;
        }
        for d in self.drives.iter().filter(|d| d.selected && d.present) {
            v &= !0x20; // /RDY: present 3.5" DD drive (ID mode reads all ones)
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

    fn update_cia_irq(&mut self) {
        // Expansion boards share Paula's INT2 input with CIA-A.
        if self.cia_a.irq() || self.scsi.as_ref().is_some_and(|s| s.irq()) || self.gayle.as_ref().is_some_and(|g| g.irq()) {
            self.intreq |= int::PORTS;
        }
        if self.cia_b.irq() {
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
                self.overlay = pa & 1 != 0;
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
            0x00a => (self.mouse_y as u16) << 8 | self.mouse_x as u16,
            0x00c => {
                let [up, down, left, right, ..] = self.joy;
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
            0x00e => self.denise.read_clxdat(),
            0x010 => self.adkcon,
            0x012 => self.pot[0],
            0x014 => self.pot[1],
            0x016 => {
                let mut v = 0xff00u16;
                if self.mouse_right {
                    v &= !0x0400;
                }
                if self.joy[5] {
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
                Chipset::Aga => 0xfff8,
            },
            _ => 0xffff,
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
                self.diwstrt = v;
                self.denise.diwstrt = v;
            }
            0x090 => {
                self.diwhigh = None;
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
            0x1e4 if !self.ocs() => self.diwhigh = Some(v),
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

    // ---- autoconfig (Zorro II fast RAM, then the SCSI controller) --------------

    fn autoconfig_regs(&self) -> Option<[u8; 16]> {
        if !self.fast.is_empty() && !self.autoconfig_done {
            // er_Type: Zorro II, memory, link to free pool; size code.
            let size_code: u8 = match self.fast.len() / 1024 {
                64 => 1,
                128 => 2,
                256 => 3,
                512 => 4,
                1024 => 5,
                2048 => 6,
                4096 => 7,
                _ => 0,
            };
            return Some([
                0xc0 | 0x20 | size_code, // er_Type (not inverted)
                0x01,                    // er_Product
                0x80,                    // er_Flags: memory space preferred
                0x00,
                0x07, // manufacturer hi
                0xdb, // manufacturer lo
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            ]);
        }
        if self.scsi.is_some() && !self.scsi_ac_done {
            return Some(A2091::autoconfig_regs());
        }
        None
    }

    fn autoconfig_read(&self, addr: u32) -> u8 {
        let Some(regs) = self.autoconfig_regs() else {
            return 0xff;
        };
        let off = (addr & 0xff) as usize;
        let idx = off / 4;
        let nibble_hi = off & 2 == 0;
        let byte = if idx < 16 { regs[idx] } else { 0 };
        let nib = if nibble_hi { byte >> 4 } else { byte & 15 };
        // All registers except er_Type are read inverted.
        let nib = if idx == 0 { nib } else { !nib & 15 };
        nib << 4 | 0x0f
    }

    fn autoconfig_write(&mut self, addr: u32, v: u8) {
        if !self.fast.is_empty() && !self.autoconfig_done {
            match addr & 0xff {
                0x48 => {
                    self.fast_base = Some((v as u32 >> 4) << 20 | (self.fast_base.unwrap_or(0) & 0x0f_0000));
                    self.autoconfig_done = true;
                }
                0x4a => {
                    self.fast_base = Some((self.fast_base.unwrap_or(0) & 0xf0_0000) | (v as u32 >> 4) << 16);
                }
                0x4c => self.autoconfig_done = true,
                _ => {}
            }
        } else if let Some(s) = self.scsi.as_mut().filter(|_| !self.scsi_ac_done) {
            match addr & 0xff {
                0x48 => {
                    s.base = Some((v as u32 >> 4) << 20 | (s.base.unwrap_or(0) & 0x0f_0000));
                    self.scsi_ac_done = true;
                    log::debug!("a2091 configured at {:06x}", s.base.unwrap());
                }
                0x4a => s.base = Some((s.base.unwrap_or(0) & 0xf0_0000) | (v as u32 >> 4) << 16),
                0x4c => self.scsi_ac_done = true,
                _ => {}
            }
        }
    }

    // ---- SCSI controller --------------------------------------------------------

    fn scsi_tick(&mut self) {
        if self.scsi.is_none() {
            return;
        }
        let now = self.cck * 2;
        while let Some(dma) = self.scsi.as_mut().unwrap().tick(now) {
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
        self.update_cia_irq();
    }

    fn dma_poke(&mut self, a: u32, v: u8) {
        match self.region(a, true) {
            Region::Chip(o) => self.chip[o] = v,
            Region::Slow(o) => self.slow[o] = v,
            Region::Fast(o) => self.fast[o] = v,
            _ => {}
        }
    }

    fn dma_peek(&self, a: u32) -> u8 {
        match self.region(a, false) {
            Region::Chip(o) => self.chip[o],
            Region::Slow(o) => self.slow[o],
            Region::Fast(o) => self.fast[o],
            _ => 0,
        }
    }

    fn fast_offset(&self, a: u32) -> Option<usize> {
        let base = self.fast_base?;
        if !self.autoconfig_done {
            return None;
        }
        let off = a.wrapping_sub(base) as usize;
        (off < self.fast.len()).then_some(off)
    }

    // ---- input helpers --------------------------------------------------------

    pub fn mouse_move(&mut self, dx: i32, dy: i32) {
        self.mouse_x = self.mouse_x.wrapping_add(dx as u8);
        self.mouse_y = self.mouse_y.wrapping_add(dy as u8);
    }

    pub fn debug_state(&self) -> String {
        format!(
            "bplcon0={:04x} bplcon1={:04x} bplcon2={:04x} ddf={:02x}-{:02x} fetch={:02x}-{:02x} diw={:04x}-{:04x} mod={}/{} dmacon={:04x} intena={:04x} intreq={:04x} cop1lc={:06x}",
            self.denise.bplcon0, self.denise.bplcon1, self.denise.bplcon2, self.ddfstrt, self.ddfstop,
            self.fetch_start, self.fetch_end, self.diwstrt, self.diwstop, self.bpl1mod, self.bpl2mod,
            self.dmacon, self.intena, self.intreq, self.cop1lc
        )
    }

    /// Side-effect free memory read for debuggers.
    pub fn peek16(&self, addr: u32) -> u16 {
        let b = |o: usize, v: &Vec<u8>| (v[o] as u16) << 8 | v[o + 1] as u16;
        match self.region(addr & !1, false) {
            Region::Chip(o) => b(o, &self.chip),
            Region::Slow(o) => b(o, &self.slow),
            Region::Fast(o) => b(o, &self.fast),
            Region::Rom(o) => b(o, &self.rom),
            Region::ExtRom(o) => b(o, &self.ext_rom),
            Region::Scsi(o) if o >= 0x2000 => self.scsi.as_ref().unwrap().peek_rom16(o),
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
    Fast(usize),
    Rom(usize),
    ExtRom(usize),
    Custom,
    Cia,
    Autoconfig,
    Scsi(u32),
    Gayle,
    None,
}

impl Board {
    #[inline]
    fn region(&self, a: u32, write: bool) -> Region {
        let a = a & 0xff_ffff;
        match a >> 16 {
            0x00..=0x1f => {
                if self.overlay && !write && a < 0x80000 {
                    Region::Rom((a & self.rom_mask) as usize)
                } else {
                    Region::Chip((a & self.chip_mask) as usize)
                }
            }
            0x20..=0x9f => match self.fast_offset(a) {
                Some(o) => Region::Fast(o),
                None => Region::None,
            },
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
            0xd8..=0xdb | 0xde if self.gayle.is_some() => Region::Gayle,
            0xe8 => Region::Autoconfig,
            0xe9..=0xef if self.scsi.as_ref().and_then(|s| s.contains(a)).is_some() => {
                Region::Scsi(self.scsi.as_ref().unwrap().contains(a).unwrap())
            }
            0xf8..=0xff => Region::Rom((a & self.rom_mask) as usize),
            0xe0..=0xe7 if !self.ext_rom.is_empty() => Region::ExtRom((a as usize - 0xe0_0000) % self.ext_rom.len()),
            0xe0..=0xe7 if self.rom.len() > 0x80000 => Region::Rom((a & self.rom_mask) as usize),
            _ => Region::None,
        }
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
            Region::Fast(o) => {
                self.cycles += 4;
                self.fast[o]
            }
            Region::Rom(o) => {
                self.cycles += 4;
                self.rom[o]
            }
            Region::ExtRom(o) => {
                self.cycles += 4;
                self.ext_rom[o]
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
                self.cycles += 4;
                self.autoconfig_read(addr)
            }
            Region::Gayle => {
                self.cycles += 4;
                let v = self.gayle.as_mut().unwrap().read8(addr);
                self.update_cia_irq();
                v
            }
            Region::Scsi(o) => {
                self.cycles += 4;
                let now = self.cycles;
                let v = self.scsi.as_mut().unwrap().read8(o, now);
                if o < 0x2000 {
                    log::trace!("a2091 pc={:06x} read8( {o:02x} -> {v:x}", self.cur_pc);
                }
                self.update_cia_irq();
                v
            }
            Region::None => {
                self.cycles += 4;
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
            Region::Fast(o) => {
                self.cycles += 4;
                (self.fast[o] as u16) << 8 | self.fast[o + 1] as u16
            }
            Region::Rom(o) => {
                self.cycles += 4;
                (self.rom[o] as u16) << 8 | self.rom[o + 1] as u16
            }
            Region::ExtRom(o) => {
                self.cycles += 4;
                (self.ext_rom[o] as u16) << 8 | self.ext_rom[o + 1] as u16
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
                self.cycles += 4;
                (self.autoconfig_read(addr) as u16) << 8 | 0xff
            }
            Region::Gayle => {
                self.cycles += 4;
                let v = self.gayle.as_mut().unwrap().read16(addr);
                self.update_cia_irq();
                v
            }
            Region::Scsi(o) => {
                self.cycles += 4;
                let now = self.cycles;
                let v = self.scsi.as_mut().unwrap().read16(o, now);
                if o < 0x2000 {
                    log::trace!("a2091 pc={:06x} read16 {o:02x} -> {v:x}", self.cur_pc);
                }
                self.update_cia_irq();
                v
            }
            Region::None => {
                self.cycles += 4;
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
            Region::Fast(o) => {
                self.cycles += 4;
                self.fast[o] = v;
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
                self.cycles += 4;
                self.autoconfig_write(addr, v);
            }
            Region::Gayle => {
                self.cycles += 4;
                self.gayle.as_mut().unwrap().write8(addr, v);
                self.update_cia_irq();
            }
            Region::Scsi(o) => {
                self.cycles += 4;
                let now = self.cycles;
                log::trace!("a2091 pc={:06x} write8( {o:02x} = {v:x}", self.cur_pc);
                self.scsi.as_mut().unwrap().write8(o, v, now);
                self.update_cia_irq();
            }
            Region::Rom(_) | Region::ExtRom(_) | Region::None => self.cycles += 4,
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
            Region::Fast(o) => {
                self.cycles += 4;
                self.fast[o] = (v >> 8) as u8;
                self.fast[o + 1] = v as u8;
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
                self.cycles += 4;
                self.autoconfig_write(addr, (v >> 8) as u8);
            }
            Region::Gayle => {
                self.cycles += 4;
                self.gayle.as_mut().unwrap().write16(addr, v);
                self.update_cia_irq();
            }
            Region::Scsi(o) => {
                self.cycles += 4;
                let now = self.cycles;
                log::trace!("a2091 pc={:06x} write16 {o:02x} = {v:x}", self.cur_pc);
                self.scsi.as_mut().unwrap().write16(o, v, now);
                self.update_cia_irq();
            }
            Region::Rom(_) | Region::ExtRom(_) | Region::None => self.cycles += 4,
        }
    }

    #[inline]
    fn idle(&mut self, cycles: u32) {
        self.cycles += cycles as u64;
    }

    fn ipl(&mut self) -> u8 {
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
    pub break_hit: bool,
    /// Print registers every time the PC reaches this address.
    pub tracepoint: Option<u32>,
    /// Print every instruction executed within this address range.
    pub trace_range: Option<(u32, u32)>,
}

impl Amiga {
    pub fn new(cfg: Config, rom: Vec<u8>) -> Amiga {
        let cpu = crate::cpu::Cpu::with_model(cfg.model.cpu());
        let mut a = Amiga { cpu, board: Board::new(cfg, rom), pc_history: None, breakpoint: None, break_hit: false, tracepoint: None, trace_range: None };
        a.hard_reset();
        a
    }

    pub fn hard_reset(&mut self) {
        self.board.reset_chips();
        self.board.keyboard = Keyboard::new();
        self.cpu.reset(&mut self.board);
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
                if (lo..hi).contains(&pc) {
                    let c = &self.cpu;
                    let (t, _) = crate::disasm::disasm(pc, &|x| self.board.peek16(x));
                    eprintln!("{pc:06x} {t:<32} D={:08x?} A={:08x?}", c.d, c.a);
                }
            }
            if self.breakpoint == Some(self.cpu.pc) && !self.break_hit {
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
            self.board.cur_pc = self.cpu.pc;
            self.cpu.step(&mut self.board);
            self.board.sync();
        }
    }
}
