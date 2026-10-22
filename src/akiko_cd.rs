//! The CD32's CD-ROM drive (a Chinon O-658) as Akiko presents it.
//!
//! Akiko talks to the drive's microcontroller over a serial link and moves
//! everything by DMA. Commands go from a 256-byte ring at misc+$200 (TX), the
//! drive's answers come back into a ring at misc+$000 (RX), subchannel data
//! goes to misc+$100, and sectors land in up to 16 4 KB slots at the data
//! address, one slot per bit set in PBX. Each packet ends with a checksum byte
//! that makes the packet's bytes sum to $FF.
//!
//! Drive packets (low nibble of the first byte; the high nibble is the
//! caller's sequence number, echoed in the answer):
//! - 0 nop, 1 stop, 2 pause, 3 unpause
//! - 4 play/read: start and end MSF; byte 7 bit 7 selects data reads, else CD
//!   audio is played. A start before the disc (MSF 00:00:00) reads the TOC,
//!   which the drive sends as a stream of 6 packets as it would from the lead-in.
//! - 5 LED, 6 Q subchannel position, 7 status (firmware id)
//!
//! Unsolicited packets: $0A media status, 4 play started/ended.

use crate::cdimage::{bcd, from_bcd, msf_to_lsn, Disc};
use crate::expansion::DmaMem;

const INT_SUBCODE: u32 = 0x8000_0000;
const INT_DRIVEXMIT: u32 = 0x4000_0000;
const INT_DRIVERECV: u32 = 0x2000_0000;
const INT_RXDMADONE: u32 = 0x1000_0000;
const INT_TXDMADONE: u32 = 0x0800_0000;
const INT_PBX: u32 = 0x0400_0000;
const INT_OVERFLOW: u32 = 0x0200_0000;

const FLAG_SUBCODE: u32 = 0x8000_0000;
const FLAG_TXD: u32 = 0x4000_0000;
const FLAG_RXD: u32 = 0x2000_0000;
const FLAG_PBX: u32 = 0x0800_0000;
const FLAG_ENABLE: u32 = 0x0400_0000;

const STATUS_ERROR: u8 = 0x80;
const STATUS_PLAYING: u8 = 0x08;
const ERR_CHECKSUM: u8 = 0x88;
const ERR_BADCOMMAND: u8 = 0x80;
const ERR_NODISK: u8 = 0xf8;

const FIRMWARE: &[u8; 18] = b"CHINON  O-658-2 24";
/// Bytes per command packet, without the checksum.
const COMMAND_LEN: [i8; 16] = [1, 2, 1, 1, 12, 2, 1, 1, 4, 1, 2, -1, -1, -1, -1, -1];
/// Each TOC entry is sent this many times, as the lead-in repeats it.
const TOC_REPEAT: usize = 3;
/// Scan lines per second (PAL; NTSC differs by under 1%).
const LINES_PER_SEC: f32 = 15625.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Audio {
    Stopped,
    /// Started; the "play started" packet is still owed.
    Starting,
    Playing,
    /// Reached the end; the "play ended" packet is still owed.
    Ended,
    /// Could not start (not an audio address); an error packet is owed.
    Failed,
}

pub struct CdDrive {
    pub disc: Option<Disc>,
    pub intreq: u32,
    pub intena: u32,
    data_addr: u32,
    misc_addr: u32,
    pub subcode_offset: u8,
    pub flags: u32,
    pub pbx: u16,
    pub tx_inx: u8,
    pub rx_inx: u8,
    tx_cmp: u8,
    rx_cmp: u8,
    tx_delay: u32,
    rx_delay: u32,

    command: Vec<u8>,
    /// A complete command waiting to run.
    command_ready: bool,
    checksum_error: bool,
    unknown_command: bool,
    /// The answer being sent (with its checksum), and how much has gone.
    result: Vec<u8>,
    result_pos: usize,
    last_rx: u8,
    /// 0 before the first media status, 1 until the status command, 2 after.
    initialized: u8,
    door: u8,
    led: bool,

    /// TOC entries (13 bytes each) and the position in the TOC stream.
    toc: Vec<[u8; 13]>,
    toc_counter: Option<usize>,

    /// Data read: first sector and sectors delivered so far.
    data_start: Option<u32>,
    sector_counter: u32,
    seek_delay: u32,
    speed: u32,
    sector_timer: f32,
    frame_timer: f32,

    audio: Audio,
    paused: bool,
    play_pos: u32,
    play_end: u32,
    /// The position last reported (for Q subchannel requests while stopped).
    qpos: Option<u32>,
    /// CD audio samples (44.1 kHz stereo) for the mixer.
    pub samples: Vec<(i16, i16)>,
    /// The audio output is muted (CIA-A PA0 low).
    pub muted: bool,
    /// Sectors delivered, for the activity light.
    pub activity: u64,
}

impl Default for CdDrive {
    fn default() -> Self {
        Self::new(None)
    }
}

impl CdDrive {
    pub fn new(disc: Option<Disc>) -> Self {
        let mut d = CdDrive {
            disc,
            intreq: 0,
            intena: 0,
            data_addr: 0,
            misc_addr: 0,
            subcode_offset: 0,
            flags: 0,
            pbx: 0,
            tx_inx: 0,
            rx_inx: 0,
            tx_cmp: 0,
            rx_cmp: 0,
            tx_delay: 0,
            rx_delay: 0,
            command: vec![],
            command_ready: false,
            checksum_error: false,
            unknown_command: false,
            result: vec![],
            result_pos: 0,
            last_rx: 0,
            initialized: 0,
            door: 1,
            led: false,
            toc: vec![],
            toc_counter: None,
            data_start: None,
            sector_counter: 0,
            seek_delay: 0,
            speed: 1,
            sector_timer: 0.0,
            frame_timer: 0.0,
            audio: Audio::Stopped,
            paused: false,
            play_pos: 0,
            play_end: 0,
            qpos: None,
            samples: vec![],
            muted: false,
            activity: 0,
        };
        d.reset();
        d
    }

    pub fn reset(&mut self) {
        self.intreq = INT_SUBCODE;
        self.intena = 0;
        self.flags = 0;
        self.pbx = 0;
        self.tx_inx = 0;
        self.rx_inx = 0;
        self.tx_cmp = 0;
        self.rx_cmp = 0;
        self.subcode_offset = 0xc2;
        self.command.clear();
        self.command_ready = false;
        self.result.clear();
        self.result_pos = 0;
        self.last_rx = 0;
        self.initialized = 0;
        self.led = false;
        self.toc_counter = None;
        self.data_start = None;
        self.speed = 1;
        self.audio = Audio::Stopped;
        self.paused = false;
        self.qpos = None;
        self.samples.clear();
        self.build_toc();
    }

    pub fn irq(&self) -> bool {
        self.intreq & self.intena != 0
    }

    fn status(&mut self, bits: u32) {
        self.intreq |= bits;
    }

    // ---- registers (byte offsets from $B80000) -------------------------------

    pub fn read8(&mut self, off: u32) -> u8 {
        let byte = |v: u32, i: u32| (v >> ((3 - i) * 8)) as u8;
        match off {
            0x04..=0x07 => byte(self.intreq, off - 4),
            0x08..=0x0f => byte(self.intena, off & 3),
            // $18-$1B are mirrored at $10, $14 and $1C.
            0x10 | 0x14 | 0x18 | 0x1c => self.subcode_offset,
            0x11 | 0x15 | 0x19 | 0x1d => self.tx_inx,
            0x12 | 0x16 | 0x1a | 0x1e => self.rx_inx,
            0x20 => (self.pbx >> 8) as u8,
            0x21 => self.pbx as u8,
            0x24..=0x27 => byte(self.flags, off - 0x24),
            0x28 => {
                // The drive's serial port read by the CPU (when RX DMA is off).
                if self.flags & FLAG_RXD == 0 && self.result_pos < self.result.len() {
                    self.last_rx = self.result[self.result_pos];
                    self.result_pos += 1;
                    if self.result_pos == self.result.len() {
                        self.result_done();
                    }
                } else {
                    self.intreq &= !INT_DRIVERECV;
                }
                self.last_rx
            }
            _ => 0,
        }
    }

    pub fn write8(&mut self, off: u32, v: u8) {
        let set = |r: &mut u32, i: u32, v: u8| {
            let sh = (3 - i) * 8;
            *r = (*r & !(0xff << sh)) | (v as u32) << sh;
        };
        match off {
            0x08..=0x0b => {
                set(&mut self.intena, off - 8, v);
                self.intena &= 0xff00_0000;
            }
            0x10..=0x13 => {
                set(&mut self.data_addr, off - 0x10, v);
                self.data_addr &= 0x00ff_f000;
            }
            0x14..=0x17 => {
                set(&mut self.misc_addr, off - 0x14, v);
                self.misc_addr &= 0x00ff_fc00;
            }
            0x18 => self.intreq &= !INT_SUBCODE,
            0x1d => {
                self.intreq &= !INT_TXDMADONE;
                self.tx_cmp = v;
                self.tx_delay = 3;
            }
            0x1f => {
                self.intreq &= !INT_RXDMADONE;
                self.rx_cmp = v;
                self.rx_delay = 3;
            }
            0x20 | 0x21 => {
                // Bits written set slots free; the drive clears them as it fills them.
                let v = if off == 0x20 { (v as u16) << 8 } else { v as u16 };
                self.pbx |= v;
                if self.flags & FLAG_PBX == 0 {
                    self.pbx = 0;
                }
                self.intreq &= !INT_PBX;
            }
            0x24..=0x27 => {
                let old = self.flags;
                set(&mut self.flags, off - 0x24, v);
                if self.flags & FLAG_ENABLE != 0 && old & FLAG_ENABLE == 0 {
                    self.sector_counter = 0;
                    self.intreq &= !INT_OVERFLOW;
                }
                if self.flags & FLAG_PBX == 0 {
                    self.pbx = 0;
                }
                self.flags &= 0xff80_0000;
            }
            0x28 => {
                // The CPU sends a command byte itself (TX DMA off).
                if self.flags & FLAG_TXD == 0 {
                    self.intreq &= !INT_DRIVEXMIT;
                    if self.can_send() {
                        self.add_command_byte(v);
                        if self.can_send() {
                            self.status(INT_DRIVEXMIT);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // ---- time --------------------------------------------------------------

    /// Advance one scan line, with access to memory for DMA.
    pub fn tick_line(&mut self, mem: &mut dyn DmaMem) {
        // Sectors pass the head at 75 per second (150 at double speed).
        let per_sector = LINES_PER_SEC / (75.0 * self.speed as f32);
        self.sector_timer -= 1.0;
        if self.sector_timer <= 0.0 {
            self.sector_timer += per_sector;
            if self.seek_delay > 0 {
                self.seek_delay -= 1;
            } else {
                self.read_sector(mem);
            }
            self.play_sector(mem);
        }
        let mut frame = false;
        self.frame_timer -= 1.0;
        if self.frame_timer <= 0.0 {
            self.frame_timer += per_sector;
            frame = true;
        }
        self.tx_delay = self.tx_delay.saturating_sub(1);
        self.rx_delay = self.rx_delay.saturating_sub(1);
        self.return_data(mem);
        self.run_command(mem);
        if self.command_ready {
            self.command_ready = false;
            self.execute();
        }
        self.drive_events(frame);
    }

    fn can_send(&self) -> bool {
        self.initialized > 0 && !self.command_ready && self.result.is_empty()
    }

    // ---- command transmission ---------------------------------------------

    /// TX DMA: one byte per call from the command ring to the drive.
    fn run_command(&mut self, mem: &mut dyn DmaMem) {
        if self.flags & FLAG_TXD == 0 || self.flags & FLAG_ENABLE != 0 || self.tx_inx == self.tx_cmp || self.tx_delay > 0 || !self.can_send() {
            return;
        }
        let b = mem.read8((self.misc_addr | 0x200) + self.tx_inx as u32);
        self.add_command_byte(b);
        self.tx_inx = self.tx_inx.wrapping_add(1);
        if self.tx_inx == self.tx_cmp {
            self.status(INT_TXDMADONE);
        }
    }

    fn add_command_byte(&mut self, b: u8) {
        self.command.push(b);
        self.checksum_error = false;
        self.unknown_command = false;
        let len = COMMAND_LEN[(self.command[0] & 15) as usize];
        if len < 0 {
            self.unknown_command = true;
            self.command_ready = true;
            return;
        }
        let len = len as usize;
        if self.command.len() < len + 1 {
            return;
        }
        let sum = self.command.iter().fold(0u8, |s, &b| s.wrapping_add(b));
        self.checksum_error = sum != 0xff;
        self.command_ready = true;
    }

    /// Start sending an answer (`len` bytes of `r`), with its checksum.
    fn answer(&mut self, mut r: Vec<u8>) {
        if r.is_empty() {
            self.status(INT_DRIVEXMIT);
            return;
        }
        let sum = r.iter().fold(0xffu8, |s, &b| s.wrapping_sub(b));
        r.push(sum);
        log::debug!("cd32: answer {r:02x?}");
        self.result = r;
        self.result_pos = 0;
        self.status(INT_DRIVERECV);
    }

    fn result_done(&mut self) {
        self.result.clear();
        self.result_pos = 0;
        self.intreq &= !INT_DRIVERECV;
        self.status(INT_DRIVEXMIT);
    }

    /// RX DMA: the answer into the result ring, stopping where the CPU said.
    fn return_data(&mut self, mem: &mut dyn DmaMem) {
        if self.result.is_empty() || self.flags & FLAG_RXD == 0 || self.rx_inx == self.rx_cmp || self.rx_delay > 0 {
            return;
        }
        while self.result_pos < self.result.len() {
            self.last_rx = self.result[self.result_pos];
            mem.write8(self.misc_addr + self.rx_inx as u32, self.last_rx);
            self.rx_inx = self.rx_inx.wrapping_add(1);
            self.result_pos += 1;
            if self.rx_inx == self.rx_cmp {
                self.status(INT_RXDMADONE);
                break;
            }
        }
        if self.result_pos == self.result.len() {
            self.result_done();
        }
    }

    // ---- commands ----------------------------------------------------------

    fn execute(&mut self) {
        let cmd = std::mem::take(&mut self.command);
        log::debug!("cd32: command {cmd:02x?}");
        let c = cmd[0];
        if self.checksum_error || self.unknown_command {
            let err = if self.checksum_error { ERR_CHECKSUM } else { ERR_BADCOMMAND };
            return self.answer(vec![(c & 0xf0) | 5, err | self.door]);
        }
        let no_disc = self.disc.is_none();
        let r = match c & 15 {
            0 => vec![c],
            1 => {
                if no_disc {
                    vec![c, ERR_NODISK | self.door]
                } else {
                    self.stop_audio();
                    vec![c, 0]
                }
            }
            2 | 3 => {
                self.toc_counter = self.toc_counter.filter(|_| c & 15 == 3);
                if no_disc {
                    vec![c, ERR_NODISK | self.door]
                } else {
                    let playing = matches!(self.audio, Audio::Playing | Audio::Starting);
                    self.paused = c & 15 == 2;
                    vec![c, if playing { STATUS_PLAYING } else { 0 } | self.door]
                }
            }
            4 => self.multi(&cmd),
            5 => {
                self.led = cmd[1] & 1 != 0;
                if cmd[1] & 0x80 != 0 {
                    vec![c, self.led as u8]
                } else {
                    vec![]
                }
            }
            6 => {
                let mut r = vec![c, 0];
                r.extend(self.subq());
                r
            }
            7 => {
                self.initialized = 2;
                self.build_toc();
                let mut r = vec![c, self.door];
                r.extend(FIRMWARE);
                r
            }
            _ => vec![],
        };
        self.answer(r);
    }

    /// Seek, play audio, read data, or read the TOC.
    fn multi(&mut self, cmd: &[u8]) -> Vec<u8> {
        let lsn = |p: &[u8]| msf_to_lsn(from_bcd(p[0]), from_bcd(p[1]), from_bcd(p[2]));
        let (start, end) = (lsn(&cmd[1..4]), lsn(&cmd[4..7]));
        if matches!(self.audio, Audio::Playing | Audio::Starting) {
            self.stop_audio();
        }
        self.paused = false;
        self.speed = if cmd[8] & 0x40 != 0 { 2 } else { 1 };
        let c = cmd[0];
        if self.disc.is_none() {
            return vec![c, 1];
        }
        if cmd[7] & 0x80 != 0 {
            log::debug!("cd32: read data {start}-{end} at {}x", self.speed);
            let from = self.data_start.map_or(0, |s| s + self.sector_counter) as i64;
            let dist = (from - start as i64).unsigned_abs();
            self.seek_delay = if dist < 100 { 1 } else { (dist / 1000 + 10).min(100) as u32 };
            self.data_start = Some(start.max(0) as u32);
            self.sector_counter = 0;
            return vec![c, 2];
        }
        if start < 0 {
            log::debug!("cd32: read TOC");
            self.toc_counter = Some(0);
            return vec![c, 0];
        }
        log::debug!("cd32: play audio {start}-{end}");
        self.toc_counter = None;
        let disc = self.disc.as_ref().unwrap();
        // A play of the data track starts at the first audio track.
        let mut start = start as u32;
        if disc.is_data(start) {
            match disc.tracks.iter().find(|t| t.audio && t.start >= start) {
                Some(t) => start = t.start,
                None => {
                    self.audio = Audio::Failed;
                    return vec![c, 0x42];
                }
            }
        }
        self.play_pos = start;
        self.play_end = (end.max(0) as u32).min(disc.leadout);
        self.audio = Audio::Starting;
        vec![c, 0x42]
    }

    fn stop_audio(&mut self) {
        self.audio = Audio::Stopped;
        self.paused = false;
        self.samples.clear();
    }

    /// Current position as the Q subchannel reports it (13 bytes after the status).
    fn subq(&self) -> Vec<u8> {
        let mut d = vec![0u8; 13];
        let playing = matches!(self.audio, Audio::Playing | Audio::Starting);
        let (Some(disc), true) = (self.disc.as_ref(), playing) else { return d };
        let q = disc.q_subchannel(self.play_pos);
        // CtlAdr, track, index, track position, disc position.
        d[1..7].copy_from_slice(&q[0..6]);
        d[8..11].copy_from_slice(&q[7..10]);
        d
    }

    // ---- TOC -----------------------------------------------------------------

    fn build_toc(&mut self) {
        self.toc.clear();
        let Some(disc) = self.disc.as_ref() else { return };
        let entry = |control: u8, point: u8, pmsf: (u8, u8, u8)| {
            let mut e = [0u8; 13];
            e[1] = 1 | control << 4;
            e[3] = point;
            e[8] = bcd(pmsf.0);
            e[9] = bcd(pmsf.1);
            e[10] = bcd(pmsf.2);
            e
        };
        let ctl = |audio: bool| if audio { 0 } else { 4 };
        let first = &disc.tracks[0];
        let last = disc.tracks.last().unwrap();
        self.toc.push(entry(ctl(first.audio), 0xa0, (first.number, 0, 0)));
        self.toc.push(entry(ctl(last.audio), 0xa1, (last.number, 0, 0)));
        self.toc.push(entry(ctl(last.audio), 0xa2, crate::cdimage::lsn_to_msf(disc.leadout as i32)));
        for t in &disc.tracks {
            self.toc.push(entry(ctl(t.audio), bcd(t.number), crate::cdimage::lsn_to_msf(t.start as i32)));
        }
    }

    /// One TOC packet, as read from the lead-in's Q subchannel.
    fn toc_packet(&mut self) -> Vec<u8> {
        let n = self.toc_counter.unwrap();
        if self.toc.is_empty() {
            self.toc_counter = None;
            let mut r = vec![6, STATUS_ERROR | self.door];
            r.resize(15, 0);
            return r;
        }
        let mut r = vec![6, 0x0a];
        r.extend(self.toc[n / TOC_REPEAT]);
        // The lead-in's running time.
        r[6] = bcd(99);
        r[7] = bcd((24 + n / 75) as u8);
        r[8] = bcd((n % 75) as u8);
        let next = n + 1;
        self.toc_counter = (next / TOC_REPEAT < self.toc.len()).then_some(next);
        r
    }

    /// Unsolicited packets: media status, play started/ended, TOC entries.
    fn drive_events(&mut self, frame: bool) {
        if !self.result.is_empty() {
            return;
        }
        if self.initialized == 0 {
            // The first packet after power-on reports a disc in the drive.
            if self.disc.is_some() {
                self.answer(vec![0x0a, 1]);
            }
            self.initialized = 1;
            return;
        }
        if self.initialized < 2 {
            return;
        }
        match self.audio {
            Audio::Starting => {
                self.audio = Audio::Playing;
                return self.answer(vec![4, STATUS_PLAYING | 2 | self.door]);
            }
            Audio::Ended => {
                self.audio = Audio::Stopped;
                return self.answer(vec![4, self.door]);
            }
            Audio::Failed => {
                self.audio = Audio::Stopped;
                return self.answer(vec![4, STATUS_ERROR | self.door]);
            }
            _ => {}
        }
        if self.toc_counter.is_some() && frame && !self.command_ready {
            let r = self.toc_packet();
            self.answer(r);
        }
    }

    // ---- data and audio ----------------------------------------------------

    /// Deliver the next data sector into the highest free PBX slot.
    fn read_sector(&mut self, mem: &mut dyn DmaMem) {
        if self.flags & FLAG_ENABLE == 0 || self.flags & FLAG_PBX == 0 || self.pbx == 0 {
            return;
        }
        let (Some(start), Some(disc)) = (self.data_start, self.disc.as_mut()) else { return };
        let lsn = start + self.sector_counter;
        if !disc.is_data(lsn) {
            return;
        }
        let Some(mut buf) = disc.read_raw(lsn) else { return };
        let q = disc.q_subchannel(lsn);
        // Akiko replaces the sync pattern's first long with a sector count.
        buf[..4].copy_from_slice(&[0, 0, 0, (self.sector_counter & 31) as u8]);
        let slot = 15 - self.pbx.leading_zeros();
        let base = self.data_addr + slot * 4096;
        for (i, &b) in buf.iter().enumerate() {
            mem.write8(base + i as u32, b);
        }
        // Error flags: none.
        for i in 0..146 {
            mem.write8(base + 0xc00 + i, 0);
        }
        self.pbx &= !(1 << slot);
        self.status(INT_PBX);
        self.write_subcode(mem, &q);
        self.sector_counter += 1;
        self.activity += 1;
    }

    /// Play one sector of CD audio.
    fn play_sector(&mut self, mem: &mut dyn DmaMem) {
        if self.audio != Audio::Playing && self.audio != Audio::Starting || self.paused {
            return;
        }
        let Some(disc) = self.disc.as_mut() else { return };
        if self.play_pos >= self.play_end {
            self.audio = Audio::Ended;
            return;
        }
        let q = disc.q_subchannel(self.play_pos);
        if let Some(buf) = disc.read_raw(self.play_pos) {
            if self.samples.len() < 44100 {
                let gain = if self.muted { 0 } else { 1 };
                self.samples.extend(buf.chunks_exact(4).map(|s| (i16::from_le_bytes([s[0], s[1]]) * gain, i16::from_le_bytes([s[2], s[3]]) * gain)));
            }
        }
        self.qpos = Some(self.play_pos);
        self.play_pos += 1;
        self.write_subcode(mem, &q);
    }

    /// 96 bytes of interleaved subchannel data (Q in bit 6) and an end marker,
    /// alternating between two buffers.
    fn write_subcode(&mut self, mem: &mut dyn DmaMem, q: &[u8; 12]) {
        if self.flags & FLAG_SUBCODE == 0 {
            return;
        }
        self.subcode_offset = if self.subcode_offset >= 128 { 0 } else { 128 };
        let base = (self.misc_addr | 0x100) + self.subcode_offset as u32;
        for i in 0..96u32 {
            let bit = q[(i / 8) as usize] >> (7 - i % 8) & 1;
            mem.write8(base + i, bit << 6);
        }
        for (i, b) in [0xff, 0xff, 0, 0].into_iter().enumerate() {
            mem.write8(base + 96 + i as u32, b);
        }
        self.subcode_offset = self.subcode_offset.wrapping_add(100);
        self.status(INT_SUBCODE);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Mem(Vec<u8>);
    impl DmaMem for Mem {
        fn read8(&mut self, a: u32) -> u8 {
            self.0[a as usize]
        }
        fn write8(&mut self, a: u32, v: u8) {
            self.0[a as usize] = v;
        }
    }

    fn packet(bytes: &[u8]) -> Vec<u8> {
        let mut p = bytes.to_vec();
        p.push(bytes.iter().fold(0xffu8, |s, &b| s.wrapping_sub(b)));
        p
    }

    #[test]
    fn status_command_answers_with_firmware_id() {
        let mut d = CdDrive::new(None);
        let mut mem = Mem(vec![0; 0x10000]);
        d.write8(0x14, 0);
        d.write8(0x15, 0);
        d.write8(0x16, 0x10);
        d.write8(0x17, 0);
        for (i, b) in packet(&[0x17]).into_iter().enumerate() {
            mem.0[0x1200 + i] = b;
        }
        d.write8(0x24, 0x60); // TXD | RXD
        d.write8(0x1f, 21);
        d.write8(0x1d, 2);
        for _ in 0..20 {
            d.tick_line(&mut mem);
        }
        assert_eq!(d.tx_inx, 2);
        assert_eq!(d.rx_inx, 21);
        assert_eq!(&mem.0[0x1000..0x1002], &[0x17, 1]);
        assert_eq!(&mem.0[0x1002..0x1014], FIRMWARE);
        let sum = mem.0[0x1000..0x1015].iter().fold(0u8, |s, &b| s.wrapping_add(b));
        assert_eq!(sum, 0xff);
        assert!(d.intreq & INT_RXDMADONE != 0);
    }
}
