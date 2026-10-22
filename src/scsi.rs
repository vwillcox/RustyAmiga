//! SCSI targets and the bus between them and a controller chip.
//!
//! Hard disks are HDF images (raw block images, usually carrying a Rigid Disk
//! Block); CD-ROM drives hold ISO 9660 images. The WD33C93 (A590/A2091, A3000) drives them directly; the newer chip
//! cores talk to them through `Bus`, which plays the target's side of the bus
//! phases and messages.

// SCSI status bytes.
pub const GOOD: u8 = 0x00;
pub const CHECK_CONDITION: u8 = 0x02;

/// What a target is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Disk,
    /// A read-only CD-ROM drive (2048-byte blocks) with a disc inserted.
    CdRom,
}

/// A hard disk image, or a CD-ROM drive holding a disc image, attached as a
/// SCSI target.
pub struct HardDisk {
    pub name: String,
    pub kind: Kind,
    /// Disk contents. Writes stay here; a disc image is read from `file` instead.
    pub data: Vec<u8>,
    file: Option<(std::fs::File, u64)>,
    pub read_only: bool,
    /// Sense key / additional sense code for REQUEST SENSE.
    pub(crate) sense: (u8, u8),
    /// Block of the last read or write, until the drive sounds collect it.
    pub pending_seek: Option<u32>,
}

impl HardDisk {
    pub fn new(name: String, data: Vec<u8>, read_only: bool) -> HardDisk {
        HardDisk { name, kind: Kind::Disk, data, file: None, read_only, sense: (0, 0), pending_seek: None }
    }

    /// A CD-ROM drive with an ISO 9660 image (raw 2048-byte sectors) inserted.
    pub fn cdrom(path: &std::path::Path) -> std::io::Result<HardDisk> {
        let file = std::fs::File::open(path)?;
        let len = file.metadata()?.len();
        let name = path.display().to_string();
        Ok(HardDisk { name, kind: Kind::CdRom, data: vec![], file: Some((file, len)), read_only: true, sense: (0, 0), pending_seek: None })
    }

    fn block_size(&self) -> usize {
        match self.kind {
            Kind::Disk => 512,
            Kind::CdRom => 2048,
        }
    }

    pub fn blocks(&self) -> u32 {
        let len = self.file.as_ref().map_or(self.data.len() as u64, |f| f.1);
        (len / self.block_size() as u64) as u32
    }

    /// Blocks `lba..lba + n`, or None past the end.
    fn read_blocks(&mut self, lba: u32, n: u32) -> Option<Vec<u8>> {
        if lba as u64 + n as u64 > self.blocks() as u64 {
            return None;
        }
        let bs = self.block_size();
        let (a, len) = (lba as usize * bs, n as usize * bs);
        match self.file.as_mut() {
            Some((f, _)) => {
                use std::io::{Read, Seek, SeekFrom};
                let mut buf = vec![0u8; len];
                f.seek(SeekFrom::Start(a as u64)).ok()?;
                f.read_exact(&mut buf).ok()?;
                Some(buf)
            }
            None => Some(self.data[a..a + len].to_vec()),
        }
    }

    /// Geometry for MODE SENSE: from the RDB if there is one, else a plausible fake.
    fn geometry(&self) -> (u32, u32, u32) {
        let l = |o: usize| u32::from_be_bytes(self.data[o..o + 4].try_into().unwrap());
        for blk in 0..16 {
            let o = blk * 512;
            if self.data.len() >= o + 512 && &self.data[o..o + 4] == b"RDSK" {
                return (l(o + 64), l(o + 72), l(o + 68));
            }
        }
        let (heads, secs) = (1, 32);
        (self.blocks() / (heads * secs), heads, secs)
    }

    /// Run one SCSI command. Returns the status byte; data in is placed in
    /// `out`, data out is taken from `data_out`.
    pub(crate) fn execute(&mut self, cdb: &[u8], data_out: &[u8], out: &mut Vec<u8>) -> u8 {
        let lba6 = || (cdb[1] as u32 & 0x1f) << 16 | (cdb[2] as u32) << 8 | cdb[3] as u32;
        let len6 = || if cdb[4] == 0 { 256 } else { cdb[4] as u32 };
        let lba10 = || u32::from_be_bytes([cdb[2], cdb[3], cdb[4], cdb[5]]);
        let len10 = || (cdb[7] as u32) << 8 | cdb[8] as u32;
        let alloc10 = || (cdb[7] as usize) << 8 | cdb[8] as usize;
        let ok = |s: &mut HardDisk| {
            s.sense = (0, 0);
            GOOD
        };
        let cd = self.kind == Kind::CdRom;
        let bs = self.block_size() as u32;
        match cdb[0] {
            0x00 | 0x1b | 0x1e | 0x15 | 0x04 | 0x2f | 0x35 => ok(self),
            0x03 => {
                let mut s = vec![0u8; 18];
                s[0] = 0x70;
                s[2] = self.sense.0;
                s[7] = 10;
                s[12] = self.sense.1;
                s.truncate((cdb[4] as usize).max(4));
                out.extend(s);
                ok(self)
            }
            0x12 => {
                let mut d = vec![0u8; 36];
                if cd {
                    d[0] = 5;
                    d[1] = 0x80;
                }
                d[2] = 2;
                d[3] = 2;
                d[4] = 31;
                d[8..16].copy_from_slice(b"AMIGA   ");
                d[16..32].copy_from_slice(if cd { b"CD-ROM          " } else { b"HARDFILE        " });
                d[32..36].copy_from_slice(b"1.0 ");
                d.truncate(cdb[4] as usize);
                out.extend(d);
                ok(self)
            }
            0x25 => {
                out.extend((self.blocks() - 1).to_be_bytes());
                out.extend(bs.to_be_bytes());
                ok(self)
            }
            0x1a => {
                let page = cdb[2] & 0x3f;
                let mut d = vec![0u8, 0, 0, 8];
                d.extend((self.blocks() & 0xff_ffff).to_be_bytes());
                d.extend(bs.to_be_bytes());
                if cd {
                    d[2] = 0x80; // write protected
                } else {
                    let (cyls, heads, secs) = self.geometry();
                    if page == 3 || page == 0x3f {
                        let mut p = vec![0u8; 24];
                        p[0] = 3;
                        p[1] = 22;
                        p[10..12].copy_from_slice(&(secs as u16).to_be_bytes());
                        p[12..14].copy_from_slice(&512u16.to_be_bytes());
                        d.extend(p);
                    }
                    if page == 4 || page == 0x3f {
                        let mut p = vec![0u8; 24];
                        p[0] = 4;
                        p[1] = 22;
                        p[2..5].copy_from_slice(&cyls.to_be_bytes()[1..]);
                        p[5] = heads as u8;
                        d.extend(p);
                    }
                }
                d[0] = (d.len() - 1) as u8;
                d.truncate(cdb[4] as usize);
                out.extend(d);
                ok(self)
            }
            0x08 | 0x28 | 0xa8 => {
                let (lba, n) = match cdb[0] {
                    0x08 => (lba6(), len6()),
                    0x28 => (lba10(), len10()),
                    _ => (lba10(), u32::from_be_bytes([cdb[6], cdb[7], cdb[8], cdb[9]])),
                };
                if !cd {
                    self.pending_seek = Some(lba);
                }
                match self.read_blocks(lba, n) {
                    Some(d) => {
                        out.extend(d);
                        ok(self)
                    }
                    None => {
                        self.sense = (5, 0x21);
                        CHECK_CONDITION
                    }
                }
            }
            0x0a | 0x2a => {
                let (lba, n) = if cdb[0] == 0x0a { (lba6(), len6()) } else { (lba10(), len10()) };
                self.pending_seek = Some(lba);
                let (a, b) = (lba as usize * 512, (lba + n) as usize * 512);
                if self.read_only {
                    self.sense = (7, 0x27);
                    return CHECK_CONDITION;
                }
                if b > self.data.len() || data_out.len() < b - a {
                    self.sense = (5, 0x21);
                    return CHECK_CONDITION;
                }
                self.data[a..b].copy_from_slice(&data_out[..b - a]);
                ok(self)
            }
            // READ TOC: one data track and the lead-out, as LBAs or MSF.
            0x43 if cd => {
                let msf = cdb[1] & 2 != 0;
                let addr = |lba: u32| -> [u8; 4] {
                    if msf {
                        let f = lba + 150;
                        [0, (f / 4500) as u8, (f / 75 % 60) as u8, (f % 75) as u8]
                    } else {
                        lba.to_be_bytes()
                    }
                };
                let mut d = vec![0u8, 0, 1, 1];
                for (track, lba) in [(1u8, 0u32), (0xaa, self.blocks())] {
                    if cdb[6] <= track || track == 0xaa {
                        d.extend([0, 0x14, track, 0]);
                        d.extend(addr(lba));
                    }
                }
                let n = (d.len() - 2) as u16;
                d[..2].copy_from_slice(&n.to_be_bytes());
                d.truncate(alloc10());
                out.extend(d);
                ok(self)
            }
            // READ SUB-CHANNEL: no audio playing.
            0x42 if cd => {
                let mut d = vec![0u8, 0x15, 0, 0];
                d.truncate(alloc10());
                out.extend(d);
                ok(self)
            }
            op => {
                log::debug!("scsi: unsupported command {op:02x}");
                self.sense = (5, 0x20);
                CHECK_CONDITION
            }
        }
    }

    /// Bytes the host must send for a data-out command.
    pub(crate) fn data_out_len(cdb: &[u8]) -> usize {
        match cdb[0] {
            0x0a => (if cdb[4] == 0 { 256 } else { cdb[4] as usize }) * 512,
            0x2a => ((cdb[7] as usize) << 8 | cdb[8] as usize) * 512,
            0x15 => cdb[4] as usize,
            _ => 0,
        }
    }
}


/// Length of a SCSI command descriptor block, from its group code.
pub fn cdb_len(op: u8) -> usize {
    match op >> 5 {
        0 => 6,
        1 | 2 => 10,
        5 => 12,
        _ => 6,
    }
}

/// Bus phases, numbered by their MSG, C/D and I/O lines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    DataOut = 0,
    DataIn = 1,
    Command = 2,
    Status = 3,
    MsgOut = 6,
    MsgIn = 7,
}

impl Phase {
    pub fn from_lines(v: u8) -> Phase {
        match v & 7 {
            0 => Phase::DataOut,
            1 => Phase::DataIn,
            2 => Phase::Command,
            3 => Phase::Status,
            6 => Phase::MsgOut,
            _ => Phase::MsgIn,
        }
    }

    /// Target-to-initiator phases.
    pub fn is_in(self) -> bool {
        matches!(self, Phase::DataIn | Phase::Status | Phase::MsgIn)
    }
}

/// Messages.
const COMMAND_COMPLETE: u8 = 0x00;
const EXTENDED: u8 = 0x01;
const ABORT: u8 = 0x06;
const MESSAGE_REJECT: u8 = 0x07;
const NOP: u8 = 0x08;
const BUS_DEVICE_RESET: u8 = 0x0c;

/// The target connected to the bus, and where it is in its command.
struct Nexus {
    target: usize,
    lun: u8,
    phase: Phase,
    /// The phase to go on with once a message exchange ends.
    resume: Phase,
    cdb: Vec<u8>,
    data_in: Vec<u8>,
    in_pos: usize,
    data_out: Vec<u8>,
    out_len: usize,
    status: u8,
    /// A message being received (extended messages span several bytes).
    msg_out: Vec<u8>,
    msg_in: std::collections::VecDeque<u8>,
    /// COMMAND COMPLETE has been sent: the target leaves once ACK is released.
    done: bool,
    /// The last MESSAGE IN byte is still acknowledged: the target waits for
    /// the initiator to release ACK (it may first raise ATN to reject it).
    held: bool,
}

/// Up to eight SCSI IDs with their targets, as seen from the initiator. The
/// targets never disconnect in the middle of a command: they are fast enough.
pub struct Bus {
    pub disks: Vec<Option<HardDisk>>,
    nexus: Option<Nexus>,
    atn: bool,
    /// Commands run, for the activity light.
    pub activity: u64,
}

impl Default for Bus {
    fn default() -> Self {
        Bus { disks: (0..8).map(|_| None).collect(), nexus: None, atn: false, activity: 0 }
    }
}

impl Bus {
    /// SCSI bus reset: every target lets go.
    pub fn reset(&mut self) {
        self.nexus = None;
        self.atn = false;
    }

    pub fn connected(&self) -> bool {
        self.nexus.is_some()
    }

    pub fn atn(&self) -> bool {
        self.atn
    }

    /// Select target `id`. Returns false (selection timeout) if nothing answers.
    pub fn select(&mut self, id: usize, atn: bool) -> bool {
        if self.disks.get(id).is_none_or(|d| d.is_none()) {
            return false;
        }
        self.atn = atn;
        self.nexus = Some(Nexus {
            target: id,
            lun: 0,
            phase: if atn { Phase::MsgOut } else { Phase::Command },
            resume: Phase::Command,
            cdb: vec![],
            data_in: vec![],
            in_pos: 0,
            data_out: vec![],
            out_len: 0,
            status: GOOD,
            msg_out: vec![],
            msg_in: Default::default(),
            done: false,
            held: false,
        });
        true
    }

    /// The phase the target is requesting, or None when the bus is free.
    /// `wait` is a wait for REQ: an initiator that waits releases a held ACK first.
    pub fn phase(&mut self, wait: bool) -> Option<Phase> {
        if wait {
            self.clear_ack();
        }
        self.nexus.as_ref().map(|n| n.phase)
    }

    pub fn set_atn(&mut self, on: bool) {
        self.atn = on;
        if on {
            if let Some(n) = self.nexus.as_mut().filter(|n| !n.held && n.phase != Phase::MsgOut) {
                n.resume = n.phase;
                n.phase = Phase::MsgOut;
            }
        }
    }

    /// Release ACK after a MESSAGE IN byte; the target moves on.
    pub fn clear_ack(&mut self) {
        let atn = self.atn;
        let Some(n) = self.nexus.as_mut().filter(|n| n.held) else { return };
        n.held = false;
        if atn {
            n.resume = if n.msg_in.is_empty() { n.resume } else { Phase::MsgIn };
            n.phase = Phase::MsgOut;
        } else if !n.msg_in.is_empty() {
            n.phase = Phase::MsgIn;
        } else if n.done {
            self.nexus = None;
        } else {
            n.phase = n.resume;
        }
    }

    /// The initiator sends a byte in DATA OUT, COMMAND or MESSAGE OUT.
    pub fn write(&mut self, b: u8) {
        let Some(n) = self.nexus.as_mut() else { return };
        match n.phase {
            Phase::MsgOut => {
                n.msg_out.push(b);
                self.message_out();
            }
            Phase::Command => {
                n.cdb.push(b);
                if n.cdb.len() >= cdb_len(n.cdb[0]) {
                    n.out_len = HardDisk::data_out_len(&n.cdb);
                    if n.out_len > 0 {
                        n.phase = Phase::DataOut;
                    } else {
                        self.run_command();
                    }
                }
            }
            Phase::DataOut => {
                n.data_out.push(b);
                if n.data_out.len() >= n.out_len {
                    self.run_command();
                }
            }
            _ => log::debug!("scsi: byte {b:02x} written in phase {:?}", n.phase),
        }
    }

    /// The initiator takes a byte in DATA IN, STATUS or MESSAGE IN.
    pub fn read(&mut self) -> u8 {
        let atn = self.atn;
        let Some(n) = self.nexus.as_mut() else { return 0 };
        let b = match n.phase {
            Phase::DataIn => {
                let b = n.data_in.get(n.in_pos).copied().unwrap_or(0);
                n.in_pos += 1;
                if n.in_pos >= n.data_in.len() {
                    n.phase = Phase::Status;
                }
                b
            }
            Phase::Status => {
                n.msg_in.push_back(COMMAND_COMPLETE);
                n.done = true;
                n.phase = Phase::MsgIn;
                n.status
            }
            Phase::MsgIn => {
                n.held = true;
                return n.msg_in.pop_front().unwrap_or(COMMAND_COMPLETE);
            }
            p => {
                log::debug!("scsi: byte read in phase {p:?}");
                return 0;
            }
        };
        if atn && n.phase != Phase::MsgIn {
            n.resume = n.phase;
            n.phase = Phase::MsgOut;
        }
        b
    }

    /// Act on a complete message from the initiator.
    fn message_out(&mut self) {
        let n = self.nexus.as_mut().unwrap();
        let m = &n.msg_out;
        if m[0] == EXTENDED && (m.len() < 2 || m.len() < 2 + m[1] as usize) {
            return;
        }
        let m = std::mem::take(&mut n.msg_out);
        match m[0] {
            0x80..=0xff => n.lun = m[0] & 7,
            EXTENDED => {
                // Answer a synchronous transfer request with offset 0 (asynchronous),
                // a wide request with 8 bits; reject the rest.
                let reply: &[u8] = match m.get(2) {
                    Some(1) => &[EXTENDED, 3, 1, m.get(3).copied().unwrap_or(0), 0],
                    Some(3) => &[EXTENDED, 2, 3, 0],
                    _ => &[MESSAGE_REJECT],
                };
                n.msg_in.extend(reply);
            }
            ABORT | BUS_DEVICE_RESET => {
                self.nexus = None;
                self.atn = false;
                return;
            }
            MESSAGE_REJECT => n.msg_in.clear(),
            NOP => {}
            b => {
                log::debug!("scsi{}: message {b:02x} rejected", n.target);
                n.msg_in.push_back(MESSAGE_REJECT);
            }
        }
        if !self.atn {
            n.phase = if n.msg_in.is_empty() { n.resume } else { Phase::MsgIn };
        }
    }

    fn run_command(&mut self) {
        let n = self.nexus.as_mut().unwrap();
        let disk = self.disks[n.target].as_mut().unwrap();
        self.activity += 1;
        n.status = if n.lun == 0 {
            disk.execute(&n.cdb, &n.data_out, &mut n.data_in)
        } else if n.cdb[0] == 0x12 {
            // INQUIRY of a missing logical unit: "not present".
            let mut d = vec![0u8; 36];
            d[0] = 0x7f;
            d.truncate(n.cdb[4] as usize);
            n.data_in = d;
            GOOD
        } else {
            disk.sense = (5, 0x25);
            CHECK_CONDITION
        };
        log::debug!("scsi{}: {:02x?} -> status {:02x}, {} bytes in", n.target, n.cdb, n.status, n.data_in.len());
        n.phase = if n.data_in.is_empty() { Phase::Status } else { Phase::DataIn };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bus() -> Bus {
        let mut b = Bus::default();
        let mut data = vec![0u8; 64 * 512];
        data[512..516].copy_from_slice(b"TEST");
        b.disks[2] = Some(HardDisk::new("t".into(), data, false));
        b
    }

    #[test]
    fn read_block_with_sync_negotiation() {
        let mut b = bus();
        assert!(!b.select(3, true));
        assert!(b.select(2, true));
        assert_eq!(b.phase(true), Some(Phase::MsgOut));
        // IDENTIFY, then SDTR: ATN stays up until the last byte.
        for &m in &[0xc0u8, 1, 3, 1, 25] {
            b.write(m);
        }
        b.set_atn(false);
        b.write(8);
        assert_eq!(b.phase(true), Some(Phase::MsgIn));
        let reply: Vec<u8> = (0..5).map(|_| { b.phase(true); b.read() }).collect();
        assert_eq!(reply, [1, 3, 1, 25, 0]);
        assert_eq!(b.phase(true), Some(Phase::Command));
        for &c in &[0x08u8, 0, 0, 1, 1, 0] {
            b.write(c);
        }
        assert_eq!(b.phase(true), Some(Phase::DataIn));
        let d: Vec<u8> = (0..512).map(|_| b.read()).collect();
        assert_eq!(&d[..4], b"TEST");
        assert_eq!(b.phase(true), Some(Phase::Status));
        assert_eq!(b.read(), GOOD);
        assert_eq!(b.phase(true), Some(Phase::MsgIn));
        assert_eq!(b.read(), COMMAND_COMPLETE);
        assert!(b.connected());
        b.clear_ack();
        assert!(!b.connected());
    }

    #[test]
    fn cdrom_reads_toc_and_sectors() {
        let path = std::env::temp_dir().join(format!("amiga-cd-test-{}.iso", std::process::id()));
        let mut img = vec![0u8; 20 * 2048];
        img[16 * 2048..16 * 2048 + 6].copy_from_slice(b"\x01CD001");
        std::fs::write(&path, &img).unwrap();
        let mut cd = HardDisk::cdrom(&path).unwrap();
        std::fs::remove_file(&path).ok();
        let mut out = vec![];
        assert_eq!(cd.execute(&[0x12, 0, 0, 0, 36, 0], &[], &mut out), GOOD);
        assert_eq!((out[0], out[1]), (5, 0x80));
        out.clear();
        cd.execute(&[0x25, 0, 0, 0, 0, 0, 0, 0, 0, 0], &[], &mut out);
        assert_eq!(out, [0, 0, 0, 19, 0, 0, 8, 0]);
        out.clear();
        cd.execute(&[0x28, 0, 0, 0, 0, 16, 0, 0, 1, 0], &[], &mut out);
        assert_eq!(&out[1..6], b"CD001");
        // TOC in MSF: track 1 at 00:02:00, lead-out after 20 sectors.
        out.clear();
        cd.execute(&[0x43, 2, 0, 0, 0, 0, 0, 0, 20, 0], &[], &mut out);
        assert_eq!(out, [0, 18, 1, 1, 0, 0x14, 1, 0, 0, 0, 2, 0, 0, 0x14, 0xaa, 0, 0, 0, 2, 20]);
        assert_eq!(cd.execute(&[0x2a, 0, 0, 0, 0, 0, 0, 0, 1, 0], &[0; 2048], &mut out), CHECK_CONDITION);
    }

    #[test]
    fn write_block() {
        let mut b = bus();
        assert!(b.select(2, false));
        for &c in &[0x0au8, 0, 0, 3, 1, 0] {
            b.write(c);
        }
        assert_eq!(b.phase(true), Some(Phase::DataOut));
        for i in 0..512 {
            b.write(i as u8);
        }
        assert_eq!(b.phase(true), Some(Phase::Status));
        assert_eq!(b.disks[2].as_ref().unwrap().data[3 * 512 + 5], 5);
    }
}
