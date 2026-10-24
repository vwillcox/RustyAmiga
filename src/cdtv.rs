//! The CDTV's CD-ROM controller: a Commodore DMAC on the autoconfig chain
//! (manufacturer 514, product 3), a 6525 TriPort at DMAC+$B0 for the drive's
//! handshake lines, and the Matshita CR-512 drive behind them.
//!
//! The CPU sends a command to the drive a byte at a time through DMAC+$A1 and
//! reads the answer back from the same register. The drive signals through
//! TriPort port C, which in interrupt mode latches its lines and raises INT2:
//! - bit 0 SBCP: a subchannel byte is ready (CD+G; not emulated)
//! - bit 1 SCOR: start of a subchannel frame, 75 times a second
//! - bit 2 STCH: the drive's status changed
//! - bit 3 STEN: a command was taken and its answer is ready
//!
//! Sectors are moved by the DMAC: a read command positions the drive, then the
//! CPU sets a word count and an address and starts DMA. The DMAC's "terminal
//! count" interrupt says when the transfer is done.
//!
//! Commands (after WinUAE's cdtv.cpp): 01 seek, 02 read, 04/05 motor on/off,
//! 09/0A play (LSN/MSF), 0B play tracks, 81 status, 82 error, 83 model,
//! 84/85 mode set/sense (sector size), 86 capacity, 87 Q subchannel, 89 disc
//! info, 8A TOC entry, 8B pause/resume, A3 front panel.

use crate::cdimage::{from_bcd, Disc};
use crate::expansion::{Controller, DmaMem};
use std::collections::VecDeque;

const MODEL_NAME: &[u8] = b"MATSHITA0.96";

// DMAC CNTR bits.
const CNTR_TCEN: u8 = 1 << 7;
const CNTR_PREST: u8 = 1 << 6;
const CNTR_INTEN: u8 = 1 << 4;
// DMAC ISTR bits.
const ISTR_INT_P: u8 = 1 << 4;
const ISTR_E_INT: u8 = 1 << 5;
const ISTR_FE_FLG: u8 = 1 << 0;

// Audio status bytes (as SCSI-2 reports them).
const AUDIO_NOT_SUPPORTED: u8 = 0x00;
const AUDIO_IN_PROGRESS: u8 = 0x11;
const AUDIO_PAUSED: u8 = 0x12;
const AUDIO_PLAY_COMPLETE: u8 = 0x13;
const AUDIO_NO_STATUS: u8 = 0x15;

/// Scan lines per CD frame (1/75 s), PAL.
const LINES_PER_FRAME: u32 = 312 * 50 / 75;
/// Colour clocks (x2) per scan line, the unit of `now`.
const LINE: u64 = 455;

/// A handshake line's state: idle, newly asserted, or asserted and seen.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Line {
    Idle,
    Pulse,
    Held,
}

pub struct CdtvDmac {
    disc: Option<Disc>,
    base: Option<u32>,
    next_line: u64,

    // DMAC
    istr: u8,
    cntr: u8,
    wtc: u32,
    acr: u32,
    dma: bool,
    /// Lines until a finished DMA raises its interrupt.
    dma_wait: Option<u32>,

    // TriPort (6525)
    tp_a: u8,
    tp_b: u8,
    tp_ad: u8,
    tp_bd: u8,
    tp_cd: u8,
    tp_cr: u8,
    tp_air: u8,
    tp_imask: u8,
    tp_ilatch: u8,
    tp_ilatch2: u8,
    sbcp: Line,
    scor: Line,
    stch: Line,
    sten: Line,
    /// Lines left until STEN, once taken, goes idle.
    sten_hold: u8,
    volstrobe1: bool,
    volstrobe2: bool,
    dac_data: u16,
    volume: u16,

    // Drive
    input: Vec<u8>,
    output: Vec<u8>,
    out_pos: Option<usize>,
    last_out: u8,
    command_done: bool,
    stch_pending: bool,
    sector_size: u32,
    /// Byte position of the next DMA transfer on the disc.
    offset: u64,
    /// The sector held for DMA (number, bytes).
    sector: Option<(u32, Vec<u8>)>,
    motor: bool,
    media: bool,
    error: bool,
    finished: bool,
    playing: bool,
    paused: bool,
    audio_status: u8,
    audio_finished: bool,
    play_pos: u32,
    play_end: u32,
    /// Where the head last was, for Q subchannel requests.
    qpos: u32,
    frame_lines: u32,
    poll_frames: u32,
    front_panel: bool,
    samples: Vec<(i16, i16)>,
    activity: u64,
}

impl CdtvDmac {
    pub fn new() -> Self {
        let mut d = CdtvDmac {
            disc: None,
            base: None,
            next_line: 0,
            istr: 0,
            cntr: 0,
            wtc: 0,
            acr: 0,
            dma: false,
            dma_wait: None,
            tp_a: 0,
            tp_b: 0,
            tp_ad: 0,
            tp_bd: 0,
            tp_cd: 0,
            tp_cr: 0,
            tp_air: 0,
            tp_imask: 0,
            tp_ilatch: 0,
            tp_ilatch2: 0,
            sbcp: Line::Idle,
            scor: Line::Idle,
            stch: Line::Idle,
            sten: Line::Idle,
            sten_hold: 0,
            volstrobe1: false,
            volstrobe2: false,
            dac_data: 0,
            volume: 0,
            input: vec![],
            output: vec![],
            out_pos: None,
            last_out: 0,
            command_done: false,
            stch_pending: false,
            sector_size: 0,
            offset: 0,
            sector: None,
            motor: false,
            media: false,
            error: false,
            finished: false,
            playing: false,
            paused: false,
            audio_status: AUDIO_NO_STATUS,
            audio_finished: false,
            play_pos: 0,
            play_end: 0,
            qpos: 0,
            frame_lines: 0,
            poll_frames: 0,
            front_panel: true,
            samples: vec![],
            activity: 0,
        };
        d.reset();
        d
    }

    /// The drive's own reset (DMAC CNTR PREST, and power-on).
    fn drive_reset(&mut self) {
        self.stop_audio();
        self.playing = false;
        self.paused = false;
        self.motor = false;
        self.media = false;
        self.error = false;
        self.finished = false;
        self.stch = Line::Pulse;
        self.front_panel = true;
    }

    fn stop_audio(&mut self) {
        self.finished = self.playing;
        self.audio_status = if self.playing { AUDIO_PLAY_COMPLETE } else { AUDIO_NO_STATUS };
        self.playing = false;
        self.paused = false;
        self.motor = false;
        self.audio_finished = false;
        self.samples.clear();
    }

    // ---- TriPort -------------------------------------------------------------

    /// Port C as read in I/O mode: the drive lines, low when asserted.
    fn tp_c(&self) -> u8 {
        let off = |l: Line, bit: u8| if l == Line::Idle { 1 << bit } else { 0 };
        off(self.sbcp, 0) | off(self.scor, 1) | off(self.stch, 2) | if self.sten == Line::Idle { 0x18 } else { 0 }
    }

    /// The lines as the interrupt latch sees them: only new pulses count.
    fn tp_c_level(&mut self) -> u8 {
        let mut v = 0x10;
        for (l, bit) in [(&mut self.sbcp, 0), (&mut self.scor, 1), (&mut self.stch, 2), (&mut self.sten, 3)] {
            if *l == Line::Pulse {
                if bit != 2 {
                    *l = Line::Held;
                }
            } else {
                v |= 1 << bit;
            }
        }
        v
    }

    fn tp_check_interrupts(&mut self) {
        let level = self.tp_c_level();
        if self.tp_cr & 1 == 0 {
            return;
        }
        self.tp_ilatch |= level ^ 0x1f;
        self.stch = Line::Idle;
        if self.tp_ilatch & 0x20 == 0 && self.tp_ilatch & self.tp_imask != 0 {
            let pending = self.tp_ilatch & self.tp_imask;
            let mask = 1u8 << (7 - pending.leading_zeros());
            self.tp_air = pending & mask;
            self.tp_ilatch |= 0x20;
            self.tp_ilatch2 = mask;
            self.tp_ilatch &= !mask;
        }
    }

    fn tp_read(&mut self, r: u32) -> u8 {
        let v = match r {
            // Subchannel bytes for CD+G (none are produced).
            0 => 0,
            1 => self.tp_b,
            2 => {
                if self.tp_cr & 1 != 0 {
                    self.tp_ilatch | self.tp_ilatch2
                } else {
                    self.tp_c()
                }
            }
            3 => self.tp_ad,
            4 => self.tp_bd,
            5 => {
                if self.tp_cr & 1 != 0 {
                    self.tp_imask
                } else {
                    self.tp_cd
                }
            }
            6 => self.tp_cr,
            _ => {
                let v = self.tp_air;
                if self.tp_cr & 1 != 0 {
                    self.tp_ilatch &= !0x20;
                    self.tp_ilatch2 = 0;
                }
                self.tp_air = 0;
                v
            }
        };
        self.tp_check_interrupts();
        v
    }

    fn tp_write(&mut self, r: u32, v: u8) {
        match r {
            0 => self.tp_a = v,
            1 => self.tp_b = v,
            2 => {
                if self.tp_cr & 1 != 0 {
                    // Writing 0 clears a latched interrupt.
                    self.tp_ilatch &= 0xe0 | v;
                }
            }
            3 => self.tp_ad = v,
            4 => self.tp_bd = v,
            5 => {
                if self.tp_cr & 1 != 0 {
                    self.tp_imask = v & 0x1f;
                } else {
                    self.tp_cd = v;
                }
            }
            6 => self.tp_cr = v,
            _ => self.tp_air = v,
        }
        // Port B bits 5-7 shift the CD audio volume into the DAC.
        let strobe1 = self.tp_b & 0x40 != 0;
        if strobe1 && !self.volstrobe1 {
            self.dac_data = self.dac_data >> 1 | ((self.tp_b as u16 >> 5) & 1) << 11;
        }
        self.volstrobe1 = strobe1;
        let strobe2 = self.tp_b & 0x80 != 0;
        if strobe2 && !self.volstrobe2 {
            self.volume = self.dac_data & 1023;
            log::debug!("cdtv: CD volume {}", self.volume);
        }
        self.volstrobe2 = strobe2;
        self.tp_check_interrupts();
    }

    // ---- drive commands ------------------------------------------------------

    fn command_byte(&mut self, b: u8) {
        self.input.push(b);
        let n = self.input.len();
        let s = self.input.clone();
        let need = match s[0] {
            0x00 | 0x80 => 2,
            0x81 | 0x85 | 0x86 | 0x88 | 0xa2 => 1,
            _ => 7,
        };
        if n < need {
            return;
        }
        log::debug!("cdtv: command {s:02x?}");
        let mut out: Vec<u8> = vec![];
        match s[0] {
            0x00 | 0x80 => out = vec![0xaa, 0x55],
            0x01 => {
                self.finished = true;
                self.stch_pending = true;
            }
            0x02 => {
                let start = (s[1] as u32) << 16 | (s[2] as u32) << 8 | s[3] as u32;
                let count = (s[4] as u32) << 8 | s[5] as u32;
                log::debug!("cdtv: read {count} sectors from {start} ({} bytes each)", self.sector_size);
                if self.playing {
                    self.stop_audio();
                }
                self.offset = start as u64 * self.sector_size as u64;
                self.motor = true;
                self.audio_status = AUDIO_NOT_SUPPORTED;
            }
            0x04 | 0x05 => {
                self.motor = s[0] == 0x04;
                self.finished = true;
            }
            0x09 | 0x0a => self.play_cd(&s),
            0x0b => self.play_tracks(&s),
            0x81 => {
                let flag = 1 // never "ready" (as WinUAE)
                    | (self.playing as u8) << 2
                    | (self.finished as u8) << 3
                    | (self.error as u8) << 4
                    | (self.motor as u8) << 5
                    | (self.media as u8) << 6;
                out = vec![flag];
                self.finished = false;
            }
            0x82 => {
                out = vec![0, 0, (self.error as u8) << 4, 0, 0, 0];
                self.error = false;
                self.finished = true;
            }
            0x83 => {
                out = MODEL_NAME.to_vec();
                self.finished = true;
            }
            0x84 => {
                let size = (s[2] as u32) << 8 | s[3] as u32;
                if [512, 1024, 2048, 2052, 2336, 2340].contains(&size) {
                    self.sector_size = size;
                } else {
                    log::warn!("cdtv: unknown sector size {size}");
                    self.error = true;
                }
                self.finished = true;
            }
            0x85 => out = vec![(self.sector_size >> 8) as u8, self.sector_size as u8],
            0x86 => match &self.disc {
                Some(d) => {
                    let last = d.leadout.saturating_sub(1);
                    out = vec![(last >> 16) as u8, (last >> 8) as u8, last as u8, (self.sector_size >> 8) as u8, self.sector_size as u8];
                }
                None => self.error = true,
            },
            0x87 => out = self.subq(s[1] & 2 != 0),
            0x88 => out = vec![0; 14],
            0x89 => {
                if let Some(d) = &self.disc {
                    self.motor = true;
                    let msf = msf_packed(d.leadout as i32);
                    let (first, last) = (d.tracks[0].number, d.tracks.last().unwrap().number);
                    out = vec![first, last, (msf >> 16) as u8, (msf >> 8) as u8, msf as u8];
                    self.finished = true;
                }
            }
            0x8a => out = self.toc_entry(s[2], s[1] & 2 != 0),
            0x8b => {
                self.pause(s[1] == 0);
                self.finished = true;
            }
            0xa2 => out = vec![0; 4],
            0xa3 => {
                self.front_panel = s[1] != 0;
                self.finished = true;
            }
            c => {
                log::warn!("cdtv: unknown drive command {c:02x}");
                self.error = true;
            }
        }
        self.input.clear();
        log::debug!("cdtv: answer {out:02x?}");
        self.output = out;
        self.out_pos = Some(0);
        self.command_done = true;
    }

    fn read_output(&mut self) -> u8 {
        self.sten = Line::Idle;
        let Some(pos) = self.out_pos else { return self.last_out };
        let v = self.output.get(pos).copied().unwrap_or(0);
        self.last_out = v;
        if pos + 1 >= self.output.len() {
            self.out_pos = None;
        } else {
            self.out_pos = Some(pos + 1);
            self.sten = Line::Pulse;
        }
        self.tp_check_interrupts();
        v
    }

    fn start_play(&mut self, start: u32, end: u32) {
        log::debug!("cdtv: play {start}-{end}");
        self.play_pos = start;
        self.play_end = end;
        self.paused = false;
        self.playing = true;
        self.audio_status = AUDIO_IN_PROGRESS;
        if !self.motor {
            self.motor = true;
        }
        self.stch_pending = true;
    }

    fn play_cd(&mut self, p: &[u8]) {
        let mut start = (p[1] as u32) << 16 | (p[2] as u32) << 8 | p[3] as u32;
        let mut end = (p[4] as u32) << 16 | (p[5] as u32) << 8 | p[6] as u32;
        if p[0] == 0x09 {
            // LSN mode: the end is a length.
            end += start;
        }
        let last = self.disc.as_ref().map_or(0, |d| d.leadout);
        if start == 0 && end == 0 {
            self.stop_audio();
            self.audio_status = AUDIO_NO_STATUS;
            self.error = true;
            self.stch_pending = true;
            return;
        }
        if p[0] != 0x09 {
            start = msf_to_lsn(start).max(0) as u32;
            if end < 0xff_ffff {
                end = msf_to_lsn(end).max(0) as u32;
            }
        }
        if end >= 0xff_ffff || end > last {
            end = last;
        }
        self.start_play(start, end);
    }

    fn play_tracks(&mut self, p: &[u8]) {
        let (first, last) = (p[1], p[3]);
        if first == 0 && last == 0 {
            return;
        }
        let Some(d) = &self.disc else { return };
        let Some(start) = d.tracks.iter().find(|t| t.number == first).map(|t| t.start) else {
            log::warn!("cdtv: play from missing track {first}");
            self.stop_audio();
            self.error = true;
            self.stch_pending = true;
            return;
        };
        let end = d.tracks.iter().find(|t| t.number == last).map_or(d.leadout, |t| t.start);
        self.start_play(start, end);
    }

    fn pause(&mut self, pause: bool) {
        if !self.playing {
            self.paused = false;
            self.audio_status = AUDIO_NO_STATUS;
            return;
        }
        self.paused = pause;
        self.audio_status = if pause { AUDIO_PAUSED } else { AUDIO_IN_PROGRESS };
    }

    /// Q subchannel: audio status, ADR/control, track, index, then the disc and
    /// track positions (as LSN, or binary MSF).
    fn subq(&mut self, msf: bool) -> Vec<u8> {
        let mut out = vec![0u8; 13];
        out[0] = self.audio_status;
        let Some(d) = &self.disc else { return out };
        let q = d.q_subchannel(self.qpos);
        out[1] = q[0].rotate_left(4);
        out[2] = from_bcd(q[1]);
        out[3] = from_bcd(q[2]);
        let packed = |b: &[u8]| (from_bcd(b[0]) as u32) << 16 | (from_bcd(b[1]) as u32) << 8 | from_bcd(b[2]) as u32;
        let (track, disc) = (packed(&q[3..6]), packed(&q[7..10]));
        let pick = |m: u32| if msf { m } else { msf_to_lsn(m) as u32 & 0xff_ffff };
        let (disc, track) = (pick(disc), pick(track));
        out[5..8].copy_from_slice(&disc.to_be_bytes()[1..]);
        out[9..12].copy_from_slice(&track.to_be_bytes()[1..]);
        out
    }

    /// One TOC entry: point 1-99 (a track), $A0/$A1 (first/last track) or $A2
    /// (the lead-out).
    fn toc_entry(&mut self, point: u8, msf: bool) -> Vec<u8> {
        let Some(d) = &self.disc else { return vec![] };
        let ctl = |audio: bool| if audio { 0 } else { 4 };
        let (first, last) = (&d.tracks[0], d.tracks.last().unwrap());
        let (control, lsn) = match point {
            0xa0 => (ctl(first.audio), crate::cdimage::msf_to_lsn(first.number, 0, 0)),
            0xa1 => (ctl(last.audio), crate::cdimage::msf_to_lsn(last.number, 0, 0)),
            0xa2 => (ctl(last.audio), d.leadout as i32),
            n => match d.tracks.iter().find(|t| t.number == n) {
                Some(t) => (ctl(t.audio), t.start as i32),
                None => return vec![],
            },
        };
        self.motor = true;
        self.finished = true;
        let a = if msf { msf_packed(lsn) } else { lsn as u32 & 0xff_ffff };
        vec![0, 0x10 | control, point, d.tracks.len() as u8, 0, (a >> 16) as u8, (a >> 8) as u8, a as u8]
    }

    // ---- data and audio ----------------------------------------------------

    /// The DMAC copies `wtc` words from the drive to memory.
    fn run_dma(&mut self, mem: &mut dyn DmaMem) {
        if self.sector_size == 0 {
            return;
        }
        let size = self.sector_size as u64;
        let words = self.wtc;
        for _ in 0..words {
            let lsn = (self.offset / size) as u32;
            if self.sector.as_ref().map(|s| s.0) != Some(lsn) {
                match self.read_sector(lsn) {
                    Some(b) => self.sector = Some((lsn, b)),
                    None => {
                        log::warn!("cdtv: read error at sector {lsn}");
                        self.error = true;
                        self.stch_pending = true;
                        break;
                    }
                }
                self.activity += 1;
            }
            let buf = &self.sector.as_ref().unwrap().1;
            let o = (self.offset % size) as usize;
            mem.write8(self.acr & 0xff_ffff, buf[o]);
            mem.write8((self.acr + 1) & 0xff_ffff, buf[o + 1]);
            self.acr += 2;
            self.offset += 2;
        }
        self.wtc = 0;
        self.dma = false;
        self.finished = true;
        // The transfer takes as long as the disc needs to deliver the data.
        self.dma_wait = Some(words * LINES_PER_FRAME / (self.sector_size / 2).max(1) + 1);
    }

    fn read_sector(&mut self, lsn: u32) -> Option<Vec<u8>> {
        let d = self.disc.as_mut()?;
        match self.sector_size {
            2048 => d.read_data(lsn),
            512 | 1024 => d.read_data(lsn),
            n => {
                let raw = d.read_raw(lsn)?;
                Some(match n {
                    2052 => [&raw[12..16], &raw[16..2064]].concat(),
                    2336 => raw[16..].to_vec(),
                    _ => raw[12..].to_vec(),
                })
            }
        }
    }

    /// One frame (1/75 s) of CD audio.
    fn play_frame(&mut self) {
        if !self.playing || self.paused {
            return;
        }
        let Some(d) = self.disc.as_mut() else { return };
        if self.play_pos >= self.play_end {
            self.audio_finished = true;
            self.audio_status = AUDIO_PLAY_COMPLETE;
            return;
        }
        if let Some(buf) = d.read_raw(self.play_pos).filter(|_| !d.is_data(self.play_pos)) {
            if self.samples.len() < 44100 {
                let gain = self.volume as i32;
                let s = |lo: u8, hi: u8| (i16::from_le_bytes([lo, hi]) as i32 * gain / 1023) as i16;
                self.samples.extend(buf.chunks_exact(4).map(|c| (s(c[0], c[1]), s(c[2], c[3]))));
            }
        }
        self.qpos = self.play_pos;
        self.play_pos += 1;
    }

    /// One scan line: DMA completion, drive status lines, CD frames.
    fn line(&mut self) {
        if let Some(w) = self.dma_wait.as_mut() {
            if *w > 0 {
                *w -= 1;
            } else {
                self.dma_wait = None;
                if self.cntr & (CNTR_INTEN | CNTR_TCEN) == CNTR_INTEN | CNTR_TCEN {
                    self.istr |= ISTR_INT_P | ISTR_E_INT;
                }
            }
        }
        if self.command_done {
            self.command_done = false;
            self.sten = Line::Pulse;
            self.sten_hold = 3;
            self.tp_check_interrupts();
        } else if self.sten == Line::Held {
            self.sten_hold = self.sten_hold.saturating_sub(1);
            if self.sten_hold == 0 {
                self.sten = Line::Idle;
            }
        }
        if self.audio_finished {
            self.audio_finished = false;
            self.playing = false;
            self.paused = false;
            self.finished = true;
            self.stch_pending = true;
        }
        self.frame_lines += 1;
        if self.frame_lines < LINES_PER_FRAME {
            return;
        }
        self.frame_lines = 0;
        self.play_frame();
        // Subchannel frames arrive all the time the disc turns.
        if self.scor == Line::Idle {
            self.scor = Line::Pulse;
            self.tp_check_interrupts();
            self.scor = Line::Idle;
        }
        // The drive notices a disc (and keeps its position current).
        if self.poll_frames == 0 {
            let media = self.disc.is_some();
            if media != self.media {
                self.media = media;
                self.stch_pending = true;
                if self.playing {
                    self.error = true;
                }
            }
            self.poll_frames = if self.playing { 10 } else { 75 };
        }
        self.poll_frames -= 1;
        if self.stch_pending && self.tp_cr & 1 != 0 && self.tp_air & 4 == 0 {
            self.stch_pending = false;
            self.stch = Line::Pulse;
            self.tp_check_interrupts();
        }
    }
}

impl Default for CdtvDmac {
    fn default() -> Self {
        Self::new()
    }
}

/// Binary MSF packed as $00MMSSFF.
fn msf_packed(lsn: i32) -> u32 {
    let (m, s, f) = crate::cdimage::lsn_to_msf(lsn);
    (m as u32) << 16 | (s as u32) << 8 | f as u32
}

fn msf_to_lsn(msf: u32) -> i32 {
    crate::cdimage::msf_to_lsn((msf >> 16) as u8, (msf >> 8) as u8, msf as u8)
}

impl Controller for CdtvDmac {
    fn read8(&mut self, off: u32, _now: u64) -> u8 {
        let off = off & 0xffff;
        match off {
            0xb0..=0xbf => self.tp_read((off - 0xb0) >> 1),
            0x41 => {
                let mut v = self.istr;
                if v != 0 {
                    v |= ISTR_INT_P;
                }
                self.istr &= !0x0f;
                v
            }
            0x43 => self.cntr,
            0xa1 => self.read_output(),
            0xe8 | 0xe9 => {
                self.istr |= ISTR_FE_FLG;
                0
            }
            // The unused XT port.
            0xa3 | 0xa5 | 0xa7 => 0xff,
            _ => 0,
        }
    }

    fn write8(&mut self, off: u32, v: u8, _now: u64) {
        let off = off & 0xffff;
        let set = |r: &mut u32, byte: u32, v: u8| {
            let sh = (3 - byte) * 8;
            *r = (*r & !(0xff << sh)) | (v as u32) << sh;
        };
        match off {
            0xb0..=0xbf => return self.tp_write((off - 0xb0) >> 1, v),
            0x43 => {
                self.cntr = v;
                if v & CNTR_PREST != 0 {
                    self.drive_reset();
                }
            }
            0x80..=0x83 => set(&mut self.wtc, off - 0x80, v),
            0x84..=0x87 => {
                set(&mut self.acr, off - 0x84, v);
                self.acr &= !1;
            }
            0xa1 => self.command_byte(v),
            // Start DMA (it runs at the next tick).
            0xe0 | 0xe1 => self.dma = true,
            0xe2 | 0xe3 => {
                self.dma = false;
                self.dma_wait = None;
            }
            0xe4 | 0xe5 => self.istr = 0,
            0xe8 | 0xe9 => self.istr |= ISTR_FE_FLG,
            _ => {}
        }
        self.tp_check_interrupts();
    }

    fn configured(&mut self, base: u32) {
        self.base = Some(base);
    }

    fn tick(&mut self, now: u64, mem: &mut dyn DmaMem) {
        if self.dma {
            self.run_dma(mem);
        }
        if self.next_line == 0 || now < self.next_line.saturating_sub(LINE * 4) {
            self.next_line = now;
        }
        while self.next_line <= now {
            self.line();
            self.next_line += LINE;
        }
    }

    fn irq(&self) -> bool {
        self.tp_ilatch & 0x20 != 0 || (self.cntr & CNTR_INTEN != 0 && self.istr & ISTR_E_INT != 0)
    }

    fn reset(&mut self) {
        self.istr = 0;
        self.cntr = 0;
        self.wtc = 0;
        self.acr = 0;
        self.dma = false;
        self.dma_wait = None;
        self.tp_a = 0;
        self.tp_b = 0;
        self.tp_ad = 0;
        self.tp_bd = 0;
        self.tp_cd = 0;
        self.tp_cr = 0;
        self.tp_air = 0;
        self.tp_imask = 0;
        self.tp_ilatch = 0;
        self.tp_ilatch2 = 0;
        self.sbcp = Line::Idle;
        self.scor = Line::Idle;
        self.sten = Line::Idle;
        self.input.clear();
        self.out_pos = None;
        self.command_done = false;
        self.stch_pending = false;
        self.sector = None;
        self.drive_reset();
        self.stch = Line::Idle;
    }

    fn activity(&self) -> u64 {
        self.activity
    }

    fn insert_disc(&mut self, disc: Disc) -> bool {
        self.disc = Some(disc);
        true
    }

    fn take_cd_audio(&mut self, out: &mut VecDeque<(i16, i16)>) {
        out.extend(self.samples.drain(..));
    }

    fn front_panel_active(&self) -> bool {
        self.front_panel
    }

    fn front_panel(&mut self, button: u8) {
        // As WinUAE's cdtv_front_panel: rewind and fast forward do nothing.
        let Some(d) = &self.disc else { return };
        let leadout = d.leadout;
        match button {
            0 => {
                self.stop_audio();
                self.stch_pending = true;
            }
            1 if self.playing => self.pause(!self.paused),
            1 => {
                if let Some(t) = d.tracks.iter().find(|t| t.audio) {
                    self.start_play(t.start, leadout);
                }
            }
            2 | 3 if self.playing => {
                let track = from_bcd(d.q_subchannel(self.qpos)[1]);
                let want = if button == 2 { track.saturating_sub(1).max(1) } else { track + 1 };
                let start = d.tracks.iter().find(|t| t.number == want).or_else(|| d.tracks.iter().find(|t| t.number == track)).map(|t| t.start);
                if let Some(start) = start {
                    self.start_play(start, leadout);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_command_answers_with_its_name() {
        let mut d = CdtvDmac::new();
        for b in [0x83, 0, 0, 0, 0, 0, 0] {
            d.write8(0xa1, b, 0);
        }
        let name: Vec<u8> = (0..MODEL_NAME.len()).map(|_| d.read8(0xa1, 0)).collect();
        assert_eq!(name, MODEL_NAME);
    }
}
