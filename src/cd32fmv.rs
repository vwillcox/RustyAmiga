//! The CD32 FMV cartridge (Commodore, 1994), which plays Video CDs: a C-Cube
//! CL450 MPEG-1 video decoder with 512 KB of DRAM, an LSI Logic L64111 MPEG
//! audio decoder, and a genlock that shows the video wherever the Amiga's
//! picture is transparent.
//!
//! The 1 MB board (autoconfig, normally at $200000):
//! - $000000-$03FFFF ROM (`amiga-crt-310-cd32-fmv.rom`): autoconfig nibbles,
//!   a DiagArea at $80 and the player software (cd32mpeg.device,
//!   videocd.library, mpegplayer.library, its own cd.device)
//! - $040000 control and status word. Read: bit 15 CL450 interrupt, bit 14
//!   L64111 interrupt (both active low), bit 11 CL450 FIFO level. Write:
//!   bit 14 shows the video, bit 9 mutes the audio.
//! - $050000 L64111 registers (32 words)
//! - $060000 CL450 bitstream port (words)
//! - $070000 CL450 registers
//! - $080000-$0FFFFF the CL450's DRAM
//!
//! Both chips interrupt on INT2. Register behaviour follows WinUAE's
//! cd32_fmv.cpp; timing and synchronisation follow what cd32mpeg.device
//! expects of the chips. The chips' microcode is not run: commands are
//! carried out directly and `crate::mpeg` decodes the streams.
//!
//! The picture follows the sound. The L64111 shows each audio packet's PTS
//! as the frame it belongs to arrives; the driver files it under CB_WRITE,
//! and when NEW_FRAME_S says that frame has started playing it loads the
//! CL450's clock (SCR) with it. The CL450 shows each picture when the clock
//! reaches the PTS that NewPacket gave the packet the picture started in.
//! The driver sends video until the CL450's buffer is nearly full, and
//! audio while the L64111 holds fewer than 48 frames.

use crate::expansion::{Controller, DmaMem, Overlay};
use crate::mpeg::{audio, video};
use std::collections::VecDeque;

const ROM_SIZE: usize = 256 * 1024;
const RAM_SIZE: usize = 512 * 1024;
const IO: u32 = 0x4_0000;
const L64111: u32 = 0x5_0000;
const CL450_DATA: u32 = 0x6_0000;
const CL450: u32 = 0x7_0000;
const RAM: u32 = 0x8_0000;

// Control and status word.
const IO_CL450_IRQ: u16 = 0x8000;
const IO_L64111_IRQ: u16 = 0x4000;
const IO_CL450_FIFO: u16 = 0x0800;
const IO_VIDEO: u16 = 0x4000;
const IO_MUTE: u16 = 0x0200;

/// `now` counts colour clocks twice over (PAL, like the CD drives' timing).
const TICKS_PER_SEC: f64 = 2.0 * crate::board::PAL_CCK_RATE as f64;
/// Scan lines (of `now`) between the CL450's buffer checks.
const LINE: u64 = 455;

pub struct Fmv {
    rom: Vec<u8>,
    /// The CL450's DRAM, which the 68020 can read and write too.
    ram: Vec<u8>,
    io: u16,
    video: Cl450,
    audio: L64111Chip,
    last: u64,
}

impl Default for Fmv {
    fn default() -> Self {
        Fmv { rom: vec![], ram: vec![0; RAM_SIZE], io: 0, video: Cl450::new(), audio: L64111Chip::new(), last: 0 }
    }
}

impl Fmv {
    fn status(&self) -> u16 {
        let mut v = IO_CL450_IRQ | IO_L64111_IRQ;
        // Room for the next burst of 11 words the driver writes.
        if self.video.fullness() + 32 <= FIFO_SIZE {
            v |= IO_CL450_FIFO;
        }
        if self.video.irq() {
            v &= !IO_CL450_IRQ;
        }
        if self.audio.irq() {
            v &= !IO_L64111_IRQ;
        }
        v
    }

    fn muted(&self) -> bool {
        self.io & IO_MUTE != 0
    }
}

impl Controller for Fmv {
    fn read8(&mut self, off: u32, now: u64) -> u8 {
        match off & 0xf_0000 {
            0..IO => self.rom.get(off as usize).copied().unwrap_or(0xff),
            RAM.. => self.ram[(off - RAM) as usize],
            // The chips sit on the low byte of the bus.
            L64111 | CL450 => self.read16(off & !1, now) as u8,
            _ => 0,
        }
    }

    fn write8(&mut self, off: u32, v: u8, now: u64) {
        match off & 0xf_0000 {
            RAM.. => self.ram[(off - RAM) as usize] = v,
            L64111 | CL450 => self.write16(off & !1, v as u16, now),
            _ => {}
        }
    }

    fn read16(&mut self, off: u32, _now: u64) -> u16 {
        match off & 0xf_0000 {
            0..IO | RAM.. => {
                let (a, b) = (self.read8(off, 0), self.read8(off + 1, 0));
                (a as u16) << 8 | b as u16
            }
            IO if off & 0xffff == 0 => self.status(),
            L64111 => self.audio.read((off >> 1) as usize & 31),
            CL450 => self.video.read(off & 0xfe),
            _ => 0,
        }
    }

    fn write16(&mut self, off: u32, v: u16, _now: u64) {
        match off & 0xf_0000 {
            RAM.. => self.ram[(off - RAM) as usize..][..2].copy_from_slice(&v.to_be_bytes()),
            IO if off & 0xffff == 0 => {
                if (v ^ self.io) & (IO_VIDEO | IO_MUTE) != 0 {
                    log::debug!("FMV: video {}, audio {}", v & IO_VIDEO != 0, if v & IO_MUTE != 0 { "muted" } else { "on" });
                }
                self.io = v;
            }
            L64111 => self.audio.write((off >> 1) as usize & 31, v),
            CL450_DATA => self.video.data(v),
            CL450 => self.video.write(off & 0xfe, v, &mut self.ram),
            _ => {}
        }
    }

    fn peek16(&self, off: u32) -> u16 {
        let b = |o: u32| match o {
            0..IO => self.rom.get(o as usize).copied().unwrap_or(0xff),
            RAM.. if o < RAM + RAM_SIZE as u32 => self.ram[(o - RAM) as usize],
            _ => 0,
        };
        (b(off) as u16) << 8 | b(off + 1) as u16
    }

    fn tick(&mut self, now: u64, _mem: &mut dyn DmaMem) {
        if self.last == 0 || now < self.last {
            self.last = now;
        }
        let dt = now - self.last;
        self.last = now;
        self.video.tick(now, dt, &mut self.ram);
        let muted = self.muted();
        self.audio.tick(now, muted);
    }

    fn irq(&self) -> bool {
        self.video.irq() || self.audio.irq()
    }

    fn reset(&mut self) {
        self.io = 0;
        self.ram.fill(0);
        self.video = Cl450::new();
        self.video.reset(&mut self.ram);
        self.audio = L64111Chip::new();
    }

    fn rom_files(&self) -> &'static [&'static str] {
        &["amiga-crt-310-cd32-fmv.rom", "cd32fmv.rom"]
    }

    fn set_rom(&mut self, rom: Vec<u8>) -> Result<(), String> {
        if rom.len() != ROM_SIZE {
            return Err(format!("expected a 256 KB CD32 FMV ROM, got {} bytes", rom.len()));
        }
        self.rom = rom;
        Ok(())
    }

    fn has_rom(&self) -> bool {
        !self.rom.is_empty()
    }

    fn take_cd_audio(&mut self, out: &mut VecDeque<(i16, i16)>) {
        out.extend(self.audio.samples.drain(..));
    }

    fn overlay(&self) -> Option<Overlay<'_>> {
        if self.io & IO_VIDEO == 0 {
            return None;
        }
        let border = fb_pixel(self.video.border);
        Some(match (&self.video.shown, self.video.blank) {
            (Some(p), false) => Overlay { pixels: &p.pixels, width: p.width, height: p.height, border },
            _ => Overlay { pixels: &[], width: 0, height: 0, border },
        })
    }
}

/// 0xRRGGBB to the frame buffer's 0xAABBGGRR.
fn fb_pixel(rgb: u32) -> u32 {
    0xff00_0000 | (rgb & 0xff) << 16 | (rgb & 0xff00) | (rgb >> 16 & 0xff)
}

// ---- CL450 ---------------------------------------------------------------

// Registers (byte offsets).
const CMEM_DATA: u32 = 0x02;
const CPU_CONTROL: u32 = 0x20;
const CPU_PC: u32 = 0x22;
const CPU_TADDR: u32 = 0x38;
const CPU_IADDR: u32 = 0x3e;
const CPU_IMEM: u32 = 0x42;
const CPU_TMEM: u32 = 0x46;
const HOST_NEWCMD: u32 = 0x56;
const CMEM_CONTROL: u32 = 0x80;
const CMEM_DMACTRL: u32 = 0x84;
const HOST_RADDR: u32 = 0x88;
const HOST_RDATA: u32 = 0x8c;
const HOST_CONTROL: u32 = 0x90;
const HOST_SCR0: u32 = 0x92;
const HOST_SCR1: u32 = 0x94;
const HOST_SCR2: u32 = 0x96;
const HOST_INTVECW: u32 = 0x98;
const HOST_INTVECR: u32 = 0x9c;
const DRAM_REFCNT: u32 = 0xac;
const VID_CONTROL: u32 = 0xec;
const VID_REGDATA: u32 = 0xee;

// Commands (the first word of host memory).
const CMD_SET_BLANK: u16 = 0x030f;
const CMD_SET_BORDER: u16 = 0x0407;
const CMD_SET_COLOR_MODE: u16 = 0x0111;
const CMD_SET_INTERRUPT_MASK: u16 = 0x0104;
const CMD_SET_THRESHOLD: u16 = 0x0103;
const CMD_SET_VIDEO_FORMAT: u16 = 0x0105;
const CMD_SET_WINDOW: u16 = 0x0406;
const CMD_DISPLAY_STILL: u16 = 0x000c;
const CMD_PAUSE: u16 = 0x000e;
const CMD_PLAY: u16 = 0x000d;
const CMD_SCAN: u16 = 0x000a;
const CMD_SINGLE_STEP: u16 = 0x000b;
const CMD_SLOW_MOTION: u16 = 0x0109;
const CMD_ACCESS_SCR: u16 = 0x8312;
const CMD_FLUSH_BITSTREAM: u16 = 0x8102;
const CMD_INQUIRE_BUFFER_FULLNESS: u16 = 0x8001;
const CMD_NEW_PACKET: u16 = 0x0408;
const CMD_RESET: u16 = 0x8000;

// Interrupts.
const INT_RDY: u16 = 1 << 10;
const INT_PIC_D: u16 = 1 << 6;
const INT_SEQ_V: u16 = 1 << 3;
const INT_UND: u16 = 1 << 8;

/// Host memory word holding the interrupt status, and the buffer fullness answer.
const HMEM_INT_STATUS: usize = 0x0a;
const HMEM_FULLNESS: usize = 0x0b;

// Variables the CL450 keeps in DRAM (word addresses).
const DRAM_H_SIZE: usize = 0x12;
const DRAM_V_SIZE: usize = 0x13;
const DRAM_PICTURE_RATE: usize = 0x14;
const DRAM_TIME_CODE_0: usize = 0x17;
const DRAM_TIME_CODE_1: usize = 0x18;
const DRAM_VER: usize = 0xa0;
const DRAM_PID: usize = 0xa1;

/// Bytes the bitstream buffer holds. cd32mpeg.device sends video until the
/// fullness reaches $a800, then waits for RDY.
const FIFO_SIZE: usize = 65536;
/// Pictures decoded ahead of the one on screen. The stream waits in the
/// bitstream buffer until then, as in the chip, whose fullness the driver's
/// flow control goes by.
const QUEUE: usize = 2;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Play {
    Stopped,
    Playing,
    Paused,
}

pub struct Picture {
    pixels: Vec<u32>,
    width: usize,
    height: usize,
    /// When to show it (90 kHz), from the packet it started in.
    pts: Option<u64>,
}

struct Cl450 {
    regs: [u16; 128],
    hmem: [u16; 16],
    vid: [u16; 16],
    /// The CL450's processor runs (CPU_control bit 0).
    running: bool,
    play: Play,
    blank: bool,
    /// Border colour (0xRRGGBB) around the picture.
    border: u32,
    int_mask: u16,
    pending: u16,
    threshold: usize,
    /// Bitstream received and not yet taken by the decoder.
    fifo: Vec<u8>,
    /// Bytes of the packets announced with NewPacket still to arrive.
    to_receive: usize,
    newpacket_mode: bool,
    decoder: video::Decoder,
    frames: VecDeque<Picture>,
    shown: Option<Picture>,
    /// The time stamp of the picture on screen, given or worked out.
    shown_pts: Option<f64>,
    picture_rate: f64,
    /// System clock reference, 90 kHz.
    scr: f64,
    next_display: u64,
    next_check: u64,
    next_underflow_check: u64,
    empty_checks: u32,
}

impl Cl450 {
    fn new() -> Self {
        Cl450 {
            regs: [0; 128],
            hmem: [0; 16],
            vid: [0; 16],
            running: false,
            play: Play::Stopped,
            blank: false,
            border: 0,
            int_mask: 0,
            pending: 0,
            threshold: 4096,
            fifo: vec![],
            to_receive: 0,
            newpacket_mode: false,
            decoder: video::Decoder::new(),
            frames: VecDeque::new(),
            shown: None,
            shown_pts: None,
            picture_rate: 25.0,
            scr: 0.0,
            next_display: 0,
            next_check: 0,
            next_underflow_check: 0,
            empty_checks: 0,
        }
    }

    fn reg(&self, r: u32) -> u16 {
        self.regs[(r >> 1) as usize]
    }

    fn set_reg(&mut self, r: u32, v: u16) {
        self.regs[(r >> 1) as usize] = v;
    }

    fn irq(&self) -> bool {
        self.running && self.reg(HOST_CONTROL) & 0x80 == 0
    }

    fn dram(ram: &mut [u8], word: usize, v: u16) {
        ram[word * 2..word * 2 + 2].copy_from_slice(&v.to_be_bytes());
    }

    /// The chip's reset (CMEM_control bit 6).
    fn reset(&mut self, ram: &mut [u8]) {
        let shown = self.shown.take();
        *self = Cl450::new();
        self.shown = shown;
        ram[..0x100].fill(0);
        Self::dram(ram, DRAM_VER, 0x0200);
        Self::dram(ram, DRAM_PID, 0x0002);
    }

    /// The processor starts (its microcode has been loaded).
    fn start(&mut self, ram: &mut [u8]) {
        log::debug!("CL450: processor started");
        self.hmem[15] = 0;
        self.set_reg(HOST_NEWCMD, 0);
        self.set_reg(CMEM_CONTROL, 2);
        self.set_reg(HOST_CONTROL, 0x81);
        self.vid[0] = 0xa8c6;
        self.vid[1] = 0x4967;
        self.set_reg(HOST_SCR2, 0x1de0);
        self.set_reg(HOST_SCR1, 0);
        self.set_reg(HOST_SCR0, 0);
        self.scr = 0.0;
        ram[0x10..0x100].fill(0);
        Self::dram(ram, DRAM_VER, 0x0200);
        Self::dram(ram, DRAM_PID, 0x0002);
    }

    /// Raise interrupts. The host reads them from host memory word 10 and
    /// clears it; more arriving meanwhile wait until it has.
    fn interrupt(&mut self, mask: u16) {
        self.pending |= mask & self.int_mask;
        self.deliver();
    }

    fn deliver(&mut self) {
        if self.pending != 0 && self.hmem[HMEM_INT_STATUS] == 0 {
            self.hmem[HMEM_INT_STATUS] = self.pending;
            self.pending = 0;
            let v = self.reg(HOST_CONTROL) & !0x80;
            self.set_reg(HOST_CONTROL, v);
        }
    }

    fn read(&self, r: u32) -> u16 {
        match r {
            HOST_RDATA => self.hmem[self.reg(HOST_RADDR) as usize & 15],
            VID_REGDATA => self.vid[(self.reg(VID_CONTROL) >> 1) as usize & 15],
            HOST_INTVECR | HOST_CONTROL | HOST_RADDR | HOST_NEWCMD | CPU_IADDR | CPU_TADDR | CPU_PC | CPU_CONTROL | VID_CONTROL => self.reg(r),
            _ => {
                log::trace!("CL450: read of register {r:02x}");
                0
            }
        }
    }

    fn write(&mut self, r: u32, v: u16, ram: &mut [u8]) {
        match r {
            CMEM_DATA => self.data(v),
            CMEM_CONTROL => {
                self.set_reg(r, v);
                if v & 0x40 != 0 {
                    self.reset(ram);
                }
            }
            HOST_INTVECW => self.set_reg(HOST_INTVECR, v),
            HOST_RADDR => self.set_reg(r, v & 15),
            HOST_RDATA => {
                let a = self.reg(HOST_RADDR) as usize & 15;
                self.hmem[a] = v;
                self.set_reg(HOST_RADDR, (a as u16 + 1) & 15);
                self.deliver();
            }
            HOST_CONTROL => {
                self.set_reg(r, v);
                self.deliver();
            }
            HOST_NEWCMD => {
                self.set_reg(r, v);
                self.command(ram);
                self.set_reg(r, 0);
            }
            CPU_CONTROL => {
                if !self.running && v & 1 != 0 {
                    self.start(ram);
                }
                self.running = v & 1 != 0;
                self.set_reg(r, v & 1);
            }
            // Microcode and its tables: kept count of, not run.
            CPU_IMEM => self.set_reg(CPU_IADDR, (self.reg(CPU_IADDR) + 1) & 0x3ff),
            CPU_TMEM => self.set_reg(CPU_TADDR, (self.reg(CPU_TADDR) + 1) & 0x7f),
            CPU_IADDR => self.set_reg(r, v & 0x3ff),
            CPU_TADDR => self.set_reg(r, v & 0x7f),
            VID_CONTROL => self.set_reg(r, v & 0x1e),
            VID_REGDATA => self.vid[(self.reg(VID_CONTROL) >> 1) as usize & 15] = v,
            CPU_PC | CMEM_DMACTRL | DRAM_REFCNT | HOST_SCR0 | HOST_SCR1 | HOST_SCR2 => self.set_reg(r, v),
            _ => log::trace!("CL450: write {v:04x} to register {r:02x}"),
        }
    }

    /// Bytes in the bitstream buffer: received and not yet decoded.
    fn fullness(&self) -> usize {
        self.fifo.len() + self.decoder.buffered()
    }

    /// A word of bitstream.
    fn data(&mut self, v: u16) {
        if self.fullness() < FIFO_SIZE {
            self.fifo.extend_from_slice(&v.to_be_bytes());
        }
        self.to_receive = self.to_receive.saturating_sub(2);
    }

    fn scr_regs(&self) -> u64 {
        (self.reg(HOST_SCR0) as u64 & 7) << 30 | (self.reg(HOST_SCR1) as u64 & 0x7fff) << 15 | self.reg(HOST_SCR2) as u64 & 0x7fff
    }

    fn command(&mut self, ram: &mut [u8]) {
        let h = self.hmem;
        log::trace!("CL450: command {:04x?}", &h[..5]);
        match h[0] {
            CMD_PLAY => {
                log::debug!("CL450: play");
                self.play = Play::Playing;
            }
            CMD_PAUSE => {
                // Pausing while playing clears the clock.
                if self.play == Play::Playing {
                    self.scr = 0.0;
                }
                self.play = Play::Paused;
            }
            CMD_NEW_PACKET => {
                // The time stamp, when bit 15 says there is one, is for the
                // first picture that starts in the packet.
                if h[2] & 0x8000 != 0 {
                    let pts = (h[2] as u64 & 7) << 30 | (h[3] as u64 & 0x7fff) << 15 | h[4] as u64 & 0x7fff;
                    self.decoder.stamp(self.fifo.len() + self.to_receive, pts);
                }
                self.newpacket_mode = true;
                self.to_receive += h[1] as usize;
            }
            CMD_INQUIRE_BUFFER_FULLNESS => self.hmem[HMEM_FULLNESS] = self.fullness().min(0xffff) as u16,
            CMD_SET_BLANK => self.blank = h[1] & 1 != 0,
            CMD_SET_BORDER => self.border = ((h[3] & 0xff) as u32) << 16 | h[4] as u32,
            CMD_SET_INTERRUPT_MASK => {
                self.int_mask = h[1];
                log::debug!("CL450: interrupt mask {:04x}", h[1]);
            }
            CMD_SET_THRESHOLD => self.threshold = h[1] as usize,
            CMD_ACCESS_SCR => {
                if h[1] & 0x8000 != 0 {
                    let v = self.scr as u64;
                    self.hmem[1] = 0x8000 | (v >> 30 & 7) as u16;
                    self.hmem[2] = (v >> 15 & 0x7fff) as u16;
                    self.hmem[3] = (v & 0x7fff) as u16;
                } else {
                    self.set_reg(HOST_SCR0, h[1] & 7);
                    self.set_reg(HOST_SCR1, h[2] & 0x7fff);
                    self.set_reg(HOST_SCR2, h[3] & 0x7fff);
                    log::trace!("CL450: SCR {} set to {}", self.scr as u64, self.scr_regs());
                    self.scr = self.scr_regs() as f64;
                }
            }
            CMD_RESET => {
                log::debug!("CL450: reset command");
                self.blank = true;
                self.play = Play::Stopped;
                self.newpacket_mode = false;
                self.to_receive = 0;
                self.int_mask = 0;
                self.fifo.clear();
                self.empty_checks = 0;
            }
            CMD_FLUSH_BITSTREAM => {
                self.fifo.clear();
                self.to_receive = 0;
                self.decoder.reset();
                self.frames.clear();
                self.shown_pts = None;
            }
            CMD_SET_COLOR_MODE | CMD_SET_VIDEO_FORMAT | CMD_SET_WINDOW => {}
            CMD_DISPLAY_STILL | CMD_SCAN | CMD_SINGLE_STEP | CMD_SLOW_MOTION => log::debug!("CL450: command {:04x} not emulated", h[0]),
            c => log::debug!("CL450: unknown command {c:04x}"),
        }
        let _ = ram;
    }

    fn tick(&mut self, now: u64, dt: u64, ram: &mut [u8]) {
        if self.play == Play::Playing {
            self.scr += dt as f64 * 90_000.0 / TICKS_PER_SEC;
        }
        self.deliver();
        // The stream's picture period: PIC_D, and the pace of pictures
        // without time stamps.
        if self.next_display == 0 || now < self.next_display.saturating_sub(TICKS_PER_SEC as u64) {
            self.next_display = now;
        }
        let period = now >= self.next_display;
        if period {
            self.next_display += (TICKS_PER_SEC / self.picture_rate) as u64;
            if self.running {
                self.interrupt(INT_PIC_D);
            }
        }
        if self.play == Play::Playing {
            self.show(period);
        }
        if now >= self.next_check {
            self.next_check = now + 8 * LINE;
            // Ready for the next packet once the announced ones are in and
            // the buffer is below the threshold.
            if self.play == Play::Playing {
                if self.newpacket_mode && self.fullness() < self.threshold && self.to_receive <= 6 {
                    self.interrupt(INT_RDY);
                }
                self.decode(ram);
            }
        }
        // Every 64 lines, a decoder with nothing left (no stream to decode
        // and no pictures waiting) counts towards an underflow.
        if now >= self.next_underflow_check {
            self.next_underflow_check = now + 64 * LINE;
            if self.fifo.is_empty() && self.frames.is_empty() {
                if self.empty_checks >= 2 {
                    self.interrupt(INT_UND);
                } else {
                    self.empty_checks += 1;
                }
            } else {
                self.empty_checks = 0;
            }
        }
    }

    /// Put the next picture on screen when the clock reaches its time stamp
    /// (or the last one's plus a picture period), skipping any that are
    /// late. Without time stamps, pictures go at the stream's rate.
    fn show(&mut self, period: bool) {
        while let Some(p) = self.frames.front() {
            let due = p.pts.map(|t| t as f64).or(self.shown_pts.map(|t| t + 90_000.0 / self.picture_rate));
            match due {
                Some(t) if self.scr < t => return,
                None if !period => return,
                _ => {}
            }
            self.shown = self.frames.pop_front();
            self.shown_pts = due;
            if due.is_none() {
                return;
            }
        }
    }

    /// Decode pictures ahead, while there is room for them.
    fn decode(&mut self, ram: &mut [u8]) {
        if self.frames.len() >= QUEUE || (self.fifo.len() < 512 && self.decoder.buffered() == 0) {
            return;
        }
        self.decoder.push(&self.fifo);
        self.fifo.clear();
        while self.frames.len() < QUEUE {
            match self.decoder.next_event() {
                Some(video::Event::Sequence(s)) => {
                    self.picture_rate = video::PICTURE_RATES.get(s.picture_rate as usize).copied().filter(|&r| r > 0.0).unwrap_or(25.0);
                    log::debug!("CL450: sequence {}x{} at {} pictures/s", s.width, s.height, self.picture_rate);
                    Self::dram(ram, DRAM_PICTURE_RATE, self.picture_rate.round() as u16);
                    Self::dram(ram, DRAM_H_SIZE, s.width);
                    Self::dram(ram, DRAM_V_SIZE, s.height);
                    self.interrupt(INT_SEQ_V);
                }
                Some(video::Event::Group(t)) => {
                    Self::dram(ram, DRAM_TIME_CODE_0, (t.hours as u16) << 6 | t.minutes as u16);
                    Self::dram(ram, DRAM_TIME_CODE_1, (t.seconds as u16) << 6 | t.pictures as u16);
                }
                Some(video::Event::Picture(f)) => {
                    let pixels = f.to_rgb().into_iter().map(fb_pixel).collect();
                    self.frames.push_back(Picture { pixels, width: f.width, height: f.height, pts: f.pts });
                }
                None => break,
            }
        }
    }
}

// ---- L64111 --------------------------------------------------------------

// Registers (word numbers).
const A_DATA: usize = 0;
const A_CONTROL1: usize = 1;
const A_CONTROL2: usize = 2;
const A_CONTROL3: usize = 3;
const A_INT1: usize = 4;
const A_INT2: usize = 5;
const A_PARAM1: usize = 9;
const A_PRESENT1: usize = 12;
const A_CB_STATUS: usize = 18;
const A_CB_WRITE: usize = 19;
const A_CB_READ: usize = 20;

// Status register 2.
const PTS_AVAILABLE: u16 = 0x20;
const SYNC_AUD: u16 = 0x10;
const SYNC_SYS: u16 = 0x08;
const FRAME_DETECT_IN: u16 = 0x04;
const NEW_FRAME_S: u16 = 0x01;

/// Frames the channel buffer holds (Layer II).
const CHANNEL_FRAMES: usize = 64;
/// Frames buffered before playback starts. Audio trickles in while the
/// driver fills the CL450's buffer, and comes in a burst after that.
const PREROLL: usize = 8;

struct L64111Chip {
    regs: [u16; 32],
    int_mask: [u16; 2],
    int_status: [u16; 2],
    /// Bytes written to the data register, not yet parsed.
    input: Vec<u8>,
    /// Bytes of the current packet to skip, and of audio to take.
    skip: usize,
    audio_left: usize,
    /// The audio elementary stream, up to the next whole frame.
    es: Vec<u8>,
    /// The stream position of `es[0]`.
    es_start: u64,
    /// Time stamps by the stream position their packet's audio starts at.
    stamps: VecDeque<(u64, u64)>,
    headers_seen: u32,
    frames_seen: u32,
    decoder: audio::Decoder,
    channel: VecDeque<Vec<(i16, i16)>>,
    playing: bool,
    /// When the next frame starts (in `now` ticks, kept exact).
    next_frame: f64,
    samples: Vec<(i16, i16)>,
}

impl L64111Chip {
    fn new() -> Self {
        let mut c = L64111Chip {
            regs: [0; 32],
            int_mask: [0; 2],
            int_status: [0; 2],
            input: vec![],
            skip: 0,
            audio_left: 0,
            es: vec![],
            es_start: 0,
            stamps: VecDeque::new(),
            headers_seen: 0,
            frames_seen: 0,
            decoder: audio::Decoder::new(),
            channel: VecDeque::new(),
            playing: false,
            next_frame: 0.0,
            samples: vec![],
        };
        // Ignore the stream number: take any audio stream.
        c.regs[A_CONTROL3] = 0x80;
        c
    }

    fn irq(&self) -> bool {
        let s2 = self.int_status[1];
        // Status 2's bits sit one place up in its mask (bit 7 wraps to bit 0).
        self.int_status[0] & self.int_mask[0] != 0 || (s2 << 1 | s2 >> 7) & 0xff & self.int_mask[1] != 0
    }

    fn status(&mut self, reg: usize, bits: u16) {
        self.int_status[reg] |= bits;
    }

    fn read(&mut self, r: usize) -> u16 {
        let v = match r {
            A_INT1 => std::mem::take(&mut self.int_status[0]),
            A_INT2 => std::mem::take(&mut self.int_status[1]) & 0x7f,
            A_CB_STATUS => self.channel.len() as u16,
            _ => self.regs[r],
        };
        log::trace!("L64111: read {r} = {v:04x}");
        v
    }

    fn write(&mut self, r: usize, v: u16) {
        if r != A_DATA {
            log::trace!("L64111: write {r} = {v:04x}");
        }
        match r {
            A_DATA => {
                self.input.extend_from_slice(&v.to_be_bytes());
                if self.input.len() >= 128 {
                    self.parse();
                }
                return;
            }
            A_CONTROL1 => {
                if v & 2 != 0 {
                    *self = L64111Chip { samples: std::mem::take(&mut self.samples), ..L64111Chip::new() };
                }
                if v & 4 != 0 {
                    self.channel.clear();
                    self.playing = false;
                    self.regs[A_CB_WRITE] = 0;
                    self.regs[A_CB_READ] = 0;
                }
            }
            A_INT1 | A_INT2 => {
                self.int_mask[r - A_INT1] = v;
                return;
            }
            _ => {}
        }
        self.regs[r] = v;
    }

    /// START: the channel buffer plays. The stream is taken in either
    /// way; cd32mpeg.device pauses by clearing START and goes on writing the
    /// data it has in hand.
    fn started(&self) -> bool {
        self.regs[A_CONTROL1] & 1 != 0
    }

    fn muted(&self) -> bool {
        self.regs[A_CONTROL2] & 0x20 != 0
    }

    /// Take the audio out of the system stream (or all of it in audio-only
    /// mode) and decode its frames.
    fn parse(&mut self) {
        let audio_only = self.regs[A_CONTROL2] & 0x08 != 0;
        let mut at = 0;
        let data = std::mem::take(&mut self.input);
        loop {
            let left = &data[at..];
            if left.is_empty() {
                break;
            }
            if audio_only {
                self.es.extend_from_slice(left);
                at = data.len();
                break;
            }
            if self.skip > 0 {
                let n = self.skip.min(left.len());
                self.skip -= n;
                at += n;
                continue;
            }
            if self.audio_left > 0 {
                let n = self.audio_left.min(left.len());
                self.es.extend_from_slice(&left[..n]);
                self.audio_left -= n;
                at += n;
                continue;
            }
            // The next packet: wait for enough of its header.
            let Some(p) = left.windows(3).position(|w| w == [0, 0, 1]) else {
                at = data.len().saturating_sub(2).max(at);
                break;
            };
            at += p;
            let left = &data[at..];
            if left.len() < 6 {
                break;
            }
            let id = left[3];
            let len = (left[4] as usize) << 8 | left[5] as usize;
            match id {
                // Pack header (MPEG-1): 12 bytes.
                0xba => {
                    if left.len() < 12 {
                        break;
                    }
                    at += 12;
                }
                0xc0..=0xdf if self.regs[A_CONTROL3] & 0x80 != 0 || (id - 0xc0) as u16 == self.regs[A_CONTROL3] & 31 => {
                    // Stuffing, buffer size and time stamps come first.
                    const MAX_HEADER: usize = 6 + 16 + 2 + 10;
                    if left.len() < (6 + len).min(MAX_HEADER) {
                        break;
                    }
                    let mut head = [0u8; MAX_HEADER];
                    let n = left.len().min(MAX_HEADER);
                    head[..n].copy_from_slice(&left[..n]);
                    let mut h = 6;
                    while h < 6 + 16 && head[h] == 0xff {
                        h += 1;
                    }
                    if head[h] & 0xc0 == 0x40 {
                        h += 2;
                    }
                    let mut pts = None;
                    match head[h] & 0xf0 {
                        0x20 => {
                            pts = Some(timestamp(&head[h..]));
                            h += 5;
                        }
                        0x30 => {
                            pts = Some(timestamp(&head[h..]));
                            h += 10;
                        }
                        _ => h += 1, // 0x0f: none
                    }
                    if self.headers_seen < 3 {
                        self.headers_seen += 1;
                        if self.headers_seen == 3 {
                            self.status(1, SYNC_SYS);
                        }
                    }
                    if let (Some(t), 3) = (pts, self.headers_seen) {
                        self.stamps.push_back((self.es_start + self.es.len() as u64, t));
                    }
                    self.audio_left = (6 + len).saturating_sub(h);
                    at += h;
                }
                _ => {
                    at += 6;
                    self.skip = len;
                }
            }
        }
        self.input = data[at..].to_vec();
        self.frames();
    }

    fn consume(&mut self, n: usize) {
        self.es.drain(..n);
        self.es_start += n as u64;
    }

    /// Decode the whole frames in the elementary stream buffer.
    ///
    /// A time stamp belongs to the first frame that starts in its packet. It
    /// shows in the PRESENT registers as that frame's header arrives, while
    /// CB_WRITE still points at the frame's place in the channel buffer:
    /// cd32mpeg.device files it by that index, and when NEW_FRAME_S says the
    /// frame has started playing it loads the CL450's clock with it.
    fn frames(&mut self) {
        loop {
            let Some(p) = self.es.windows(2).position(|w| w[0] == 0xff && w[1] & 0xe0 == 0xe0) else {
                let keep = self.es.len().min(1);
                self.consume(self.es.len() - keep);
                return;
            };
            self.consume(p);
            let Some(h) = audio::Header::parse(&self.es) else {
                if self.es.len() < 4 {
                    return;
                }
                self.consume(1);
                continue;
            };
            let mut pts = None;
            while let Some(&(at, t)) = self.stamps.front() {
                if at > self.es_start {
                    break;
                }
                pts = Some(t);
                self.stamps.pop_front();
            }
            if let Some(t) = pts {
                for i in 0..5 {
                    self.regs[A_PRESENT1 + i] = (t >> (8 * i) & 0xff) as u16;
                }
                self.status(1, PTS_AVAILABLE);
            }
            if self.es.len() < h.size {
                return;
            }
            self.frames_seen += 1;
            self.regs[A_PARAM1] = ((self.es[1] as u16) << 4 | (self.es[2] as u16) >> 4) & 0xff;
            self.regs[A_PARAM1 + 1] = ((self.es[2] as u16) << 4 | (self.es[3] as u16) >> 4) & 0xff;
            self.regs[A_PARAM1 + 2] = (self.es[3] as u16) << 4 & 0xff;
            self.status(1, FRAME_DETECT_IN | if self.frames_seen == 3 { SYNC_AUD } else { 0 });
            let mut pcm = Vec::with_capacity(audio::FRAME_SAMPLES);
            if self.decoder.decode(&self.es[..h.size], &mut pcm).is_none() || h.sample_rate != 44100 {
                pcm = vec![(0, 0); audio::FRAME_SAMPLES];
            }
            self.consume(h.size);
            if self.channel.len() >= CHANNEL_FRAMES {
                log::debug!("L64111: channel buffer overflow");
                continue;
            }
            self.channel.push_back(pcm);
            self.regs[A_CB_WRITE] = (self.regs[A_CB_WRITE] + 1) % CHANNEL_FRAMES as u16;
        }
    }

    /// Play decoded frames at 44.1 kHz.
    fn tick(&mut self, now: u64, muted: bool) {
        let period = TICKS_PER_SEC * audio::FRAME_SAMPLES as f64 / 44100.0;
        if !self.started() {
            self.playing = false;
            return;
        }
        if !self.playing {
            if self.channel.len() < PREROLL {
                return;
            }
            self.playing = true;
            self.next_frame = now as f64;
        }
        while now as f64 >= self.next_frame {
            let Some(f) = self.channel.pop_front() else {
                log::debug!("L64111: channel buffer underflow");
                self.playing = false;
                return;
            };
            self.regs[A_CB_READ] = (self.regs[A_CB_READ] + 1) % CHANNEL_FRAMES as u16;
            self.status(1, NEW_FRAME_S);
            if muted || self.muted() {
                self.samples.extend(std::iter::repeat_n((0, 0), f.len()));
            } else {
                self.samples.extend(f);
            }
            self.next_frame += period;
        }
    }
}

/// A 33-bit PTS or DTS from its five bytes.
fn timestamp(b: &[u8]) -> u64 {
    (b[0] as u64 >> 1 & 7) << 30 | (b[1] as u64) << 22 | (b[2] as u64 >> 1) << 15 | (b[3] as u64) << 7 | b[4] as u64 >> 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rom_autoconfig_matches_the_card() {
        // The card's autoconfig (registry "cd32fmv") as the ROM's nibbles give it.
        let ac = crate::expansion::registry::find("cd32fmv").unwrap().build(&Default::default()).unwrap().cards[0].ac;
        let r = ac.regs();
        assert_eq!((r[0], r[1], r[4], r[5], r[10], r[11]), (0xd5, 0x6a, 2, 2, 0, 0x80));
    }

    #[test]
    fn host_commands_and_interrupts() {
        let mut f = Fmv::default();
        f.reset();
        let base = CL450;
        f.write16(base + CPU_CONTROL, 1, 0);
        assert!(!f.irq());
        assert_eq!(f.read16(IO, 0) & IO_CL450_IRQ, IO_CL450_IRQ, "no interrupt: the bit reads 1");
        // SetInterruptMask(PIC_D) through host memory, then the command.
        f.write16(base + HOST_RADDR, 0, 0);
        f.write16(base + HOST_RDATA, CMD_SET_INTERRUPT_MASK, 0);
        f.write16(base + HOST_RDATA, INT_PIC_D, 0);
        f.write16(base + HOST_NEWCMD, 1, 0);
        assert_eq!(f.read16(base + HOST_NEWCMD, 0), 0);
        let mut mem = NoMem;
        f.tick(1000, &mut mem);
        f.tick(1000 + TICKS_PER_SEC as u64 / 10, &mut mem);
        assert!(f.irq());
        assert_eq!(f.read16(IO, 0) & IO_CL450_IRQ, 0);
        f.write16(base + HOST_RADDR, HMEM_INT_STATUS as u16, 0);
        assert_eq!(f.read16(base + HOST_RDATA, 0), INT_PIC_D);
        // The host clears the status word and acknowledges.
        f.write16(base + HOST_RDATA, 0, 0);
        f.write16(base + HOST_CONTROL, 0x81, 0);
        assert!(!f.irq());
        // The DRAM holds the version for the 68020 to read.
        assert_eq!(f.read16(RAM + DRAM_VER as u32 * 2, 0), 0x0200);
    }

    struct NoMem;
    impl DmaMem for NoMem {
        fn read8(&mut self, _a: u32) -> u8 {
            0
        }
        fn write8(&mut self, _a: u32, _v: u8) {}
    }

    #[test]
    fn pictures_wait_for_the_clock() {
        let mut f = Fmv::default();
        f.reset();
        f.write16(CL450 + CPU_CONTROL, 1, 0);
        let command = |f: &mut Fmv, words: &[u16]| {
            f.write16(CL450 + HOST_RADDR, 0, 0);
            for &w in words {
                f.write16(CL450 + HOST_RDATA, w, 0);
            }
            f.write16(CL450 + HOST_NEWCMD, 1, 0);
        };
        let send = |f: &mut Fmv, pts: Option<u64>, data: &[u8]| {
            let (flag, t) = pts.map_or((0, 0), |t| (0x8000, t));
            command(f, &[CMD_NEW_PACKET, data.len() as u16, flag | (t >> 30) as u16, (t >> 15 & 0x7fff) as u16, (t & 0x7fff) as u16]);
            for w in data.chunks(2) {
                f.write16(CL450_DATA, (w[0] as u16) << 8 | *w.get(1).unwrap_or(&0) as u16, 0);
            }
        };
        command(&mut f, &[CMD_PLAY]);
        // A 16x16 sequence and two I pictures (headers only) for 0.5 s and
        // 0.54 s, then the sequence end and padding to make it worth decoding.
        send(&mut f, None, &[0, 0, 1, 0xb3, 0x01, 0x00, 0x10, 0x13, 0xff, 0xff, 0xe0, 0x18]);
        send(&mut f, Some(45_000), &[0, 0, 1, 0, 0x00, 0x0f, 0xff, 0xf8]);
        send(&mut f, Some(48_600), &[&[0, 0, 1, 0, 0x00, 0x4f, 0xff, 0xf8, 0, 0, 1, 0xb7][..], &[0; 512]].concat());
        let mut mem = NoMem;
        let mut at = |f: &mut Fmv, s: f64| {
            f.tick(1000 + (s * TICKS_PER_SEC) as u64, &mut mem);
            f.video.shown_pts
        };
        assert_eq!(at(&mut f, 0.0), None);
        assert_eq!(f.video.frames.len(), 2);
        assert_eq!(at(&mut f, 0.4), None, "the clock is at 0.4 s");
        assert_eq!(at(&mut f, 0.52), Some(45_000.0));
        // The driver sets the clock back: the next picture waits for it.
        command(&mut f, &[CMD_ACCESS_SCR, 0, 40_000 >> 15, 40_000 & 0x7fff]);
        assert_eq!(at(&mut f, 0.56), Some(45_000.0));
        assert_eq!(at(&mut f, 0.66), Some(48_600.0));
    }

    #[test]
    fn audio_packets_are_found_and_decoded() {
        let mut a = L64111Chip::new();
        a.write(A_CONTROL1, 1);
        a.write(A_INT2, FRAME_DETECT_IN << 1);
        // A pack header, then an audio packet holding one silent frame.
        let mut frame = vec![0u8; 731];
        frame[..4].copy_from_slice(&[0xff, 0xfd, 0xb0, 0x04]);
        let mut s = vec![0, 0, 1, 0xba, 0x21, 0, 1, 0, 1, 0x80, 0, 1];
        let len = 1 + 5 + frame.len();
        s.extend_from_slice(&[0, 0, 1, 0xc0, (len >> 8) as u8, len as u8, 0xff, 0x21, 0, 1, 0, 1]);
        s.extend_from_slice(&frame);
        s.resize(s.len() + 200, 0);
        for w in s.chunks(2) {
            a.write(A_DATA, (w[0] as u16) << 8 | *w.get(1).unwrap_or(&0) as u16);
        }
        assert_eq!(a.channel.len(), 1);
        assert!(a.irq());
        assert_eq!(a.read(A_INT2) & FRAME_DETECT_IN, FRAME_DETECT_IN);
        assert!(!a.irq());
    }

    #[test]
    fn audio_time_stamps_wait_for_their_frame() {
        let packet = |pts: Option<u64>, payload: &[u8]| {
            let head = match pts {
                Some(t) => vec![0x21 | (t >> 29 & 0x0e) as u8, (t >> 22) as u8, (t >> 14) as u8 | 1, (t >> 7) as u8, (t << 1) as u8 | 1],
                None => vec![0x0f],
            };
            let len = head.len() + payload.len();
            [&[0, 0, 1, 0xc0, (len >> 8) as u8, len as u8][..], &head, payload].concat()
        };
        let send = |a: &mut L64111Chip, p: Vec<u8>| {
            // A padding packet after it gets all of it parsed.
            let s = [p, vec![0, 0, 1, 0xbe, 0, 200], vec![0xff; 200]].concat();
            for w in s.chunks(2) {
                a.write(A_DATA, (w[0] as u16) << 8 | *w.get(1).unwrap_or(&0) as u16);
            }
        };
        let present = |a: &L64111Chip| ((0..5).fold(0, |t, i| t | (a.regs[A_PRESENT1 + i] as u64) << (8 * i)), a.regs[A_CB_WRITE]);
        let mut frame = vec![0u8; 731];
        frame[..4].copy_from_slice(&[0xff, 0xfd, 0xb0, 0x04]);
        let mut a = L64111Chip::new();
        a.write(A_CONTROL1, 1);
        // Time stamps count from the third packet header.
        send(&mut a, packet(Some(1000), &frame));
        send(&mut a, packet(Some(2000), &frame));
        send(&mut a, packet(Some(3000), &[&frame[..], &frame[..100]].concat()));
        assert_eq!(present(&a), (3000, 3));
        // A packet's stamp is for the first frame that starts in it, and
        // shows while CB_WRITE points at that frame's place.
        send(&mut a, packet(Some(4000), &[&frame[100..], &frame[..300]].concat()));
        assert_eq!(present(&a), (4000, 4));
        send(&mut a, packet(None, &frame[300..]));
        assert_eq!(present(&a), (4000, 5));
        // NEW_FRAME_S comes as a frame starts playing.
        for _ in 5..PREROLL {
            send(&mut a, packet(None, &frame));
        }
        a.write(A_INT2, NEW_FRAME_S << 1);
        a.read(A_INT2);
        a.tick(0, false);
        assert_eq!((a.irq(), a.read(A_CB_READ)), (true, 1));
    }
}
