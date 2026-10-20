//! Commodore A590 / A2091 SCSI controller: a Zorro II board with the DMAC
//! (DMA controller) and a WD33C93 SCSI bus interface, driven by Commodore's own
//! boot ROM (`amiga-boot-a590.rom`). Hard disks are HDF images (raw block
//! images, usually carrying a Rigid Disk Block) attached as SCSI targets.

/// Board offset where the boot ROM appears (the ROM repeats up to $FFFF).
const ROM_OFFSET: u32 = 0x2000;

// DMAC control register (CNTR) bits.
const CNTR_INTEN: u16 = 1 << 4;

// DMAC interrupt status register (ISTR) bits.
const ISTR_INT_F: u16 = 1 << 7;
const ISTR_INTS: u16 = 1 << 6;
const ISTR_INT_P: u16 = 1 << 4;
const ISTR_FE_FLG: u16 = 1 << 0;

// WD33C93 registers.
const WD_OWN_ID: usize = 0x00;
const WD_CONTROL: usize = 0x01;
const WD_CDB1: usize = 0x03;
const WD_TARGET_LUN: usize = 0x0f;
const WD_COMMAND_PHASE: usize = 0x10;
const WD_TC_HI: usize = 0x12;
const WD_DEST_ID: usize = 0x15;
const WD_SCSI_STATUS: usize = 0x17;
const WD_COMMAND: usize = 0x18;
const WD_DATA: usize = 0x19;
const WD_AUX_STATUS: usize = 0x1f;

// Auxiliary status bits.
const ASR_INT: u8 = 0x80;
const ASR_BSY: u8 = 0x20;
const ASR_CIP: u8 = 0x10;
const ASR_DBR: u8 = 0x01;

// SCSI status bytes.
const GOOD: u8 = 0x00;
const CHECK_CONDITION: u8 = 0x02;

/// A hard disk image attached as a SCSI target.
pub struct HardDisk {
    pub name: String,
    pub data: Vec<u8>,
    pub read_only: bool,
    /// Sense key / additional sense code for REQUEST SENSE.
    sense: (u8, u8),
}

impl HardDisk {
    pub fn new(name: String, data: Vec<u8>, read_only: bool) -> HardDisk {
        HardDisk { name, data, read_only, sense: (0, 0) }
    }

    fn blocks(&self) -> u32 {
        (self.data.len() / 512) as u32
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
    fn execute(&mut self, cdb: &[u8], data_out: &[u8], out: &mut Vec<u8>) -> u8 {
        let lba6 = || (cdb[1] as u32 & 0x1f) << 16 | (cdb[2] as u32) << 8 | cdb[3] as u32;
        let len6 = || if cdb[4] == 0 { 256 } else { cdb[4] as u32 };
        let lba10 = || u32::from_be_bytes([cdb[2], cdb[3], cdb[4], cdb[5]]);
        let len10 = || (cdb[7] as u32) << 8 | cdb[8] as u32;
        let ok = |s: &mut HardDisk| {
            s.sense = (0, 0);
            GOOD
        };
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
                d[2] = 2;
                d[3] = 2;
                d[4] = 31;
                d[8..16].copy_from_slice(b"AMIGA   ");
                d[16..32].copy_from_slice(b"HARDFILE        ");
                d[32..36].copy_from_slice(b"1.0 ");
                d.truncate(cdb[4] as usize);
                out.extend(d);
                ok(self)
            }
            0x25 => {
                out.extend((self.blocks() - 1).to_be_bytes());
                out.extend(512u32.to_be_bytes());
                ok(self)
            }
            0x1a => {
                let (cyls, heads, secs) = self.geometry();
                let page = cdb[2] & 0x3f;
                let mut d = vec![0u8, 0, 0, 8];
                d.extend((self.blocks() & 0xff_ffff).to_be_bytes());
                d.extend(512u32.to_be_bytes());
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
                d[0] = (d.len() - 1) as u8;
                d.truncate(cdb[4] as usize);
                out.extend(d);
                ok(self)
            }
            0x08 | 0x28 => {
                let (lba, n) = if cdb[0] == 0x08 { (lba6(), len6()) } else { (lba10(), len10()) };
                let (a, b) = (lba as usize * 512, (lba + n) as usize * 512);
                if b > self.data.len() {
                    self.sense = (5, 0x21);
                    return CHECK_CONDITION;
                }
                out.extend_from_slice(&self.data[a..b]);
                ok(self)
            }
            0x0a | 0x2a => {
                let (lba, n) = if cdb[0] == 0x0a { (lba6(), len6()) } else { (lba10(), len10()) };
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
            op => {
                log::debug!("scsi: unsupported command {op:02x}");
                self.sense = (5, 0x20);
                CHECK_CONDITION
            }
        }
    }

    /// Bytes the host must send for a data-out command.
    fn data_out_len(cdb: &[u8]) -> usize {
        match cdb[0] {
            0x0a => (if cdb[4] == 0 { 256 } else { cdb[4] as usize }) * 512,
            0x2a => ((cdb[7] as usize) << 8 | cdb[8] as usize) * 512,
            0x15 => cdb[4] as usize,
            _ => 0,
        }
    }
}


/// Length of a SCSI command descriptor block, from its group code.
fn cdb_len(op: u8) -> usize {
    match op >> 5 {
        0 => 6,
        1 | 2 => 10,
        5 => 12,
        _ => 6,
    }
}

/// A DMA transfer the board must carry out against system memory.
pub enum Dma {
    /// Write these bytes to memory starting at the address.
    ToMemory(u32, Vec<u8>),
    /// Read this many bytes from memory at the address, then call `dma_done`.
    FromMemory(u32, usize),
}

/// SCSI bus phases, numbered by their MSG/C/D/I/O lines as the WD33C93 reports them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    DataOut = 0,
    DataIn = 1,
    Command = 2,
    Status = 3,
    MsgOut = 6,
    MsgIn = 7,
}

impl Phase {
    /// Target-to-initiator phases.
    fn is_in(self) -> bool {
        matches!(self, Phase::DataIn | Phase::Status | Phase::MsgIn)
    }
}

/// The target currently connected to the bus.
struct Nexus {
    target: usize,
    lun: u8,
    phase: Phase,
    cdb: Vec<u8>,
    /// Bytes the target sends in the current in-phase.
    data_in: Vec<u8>,
    in_pos: usize,
    /// Bytes received in DATA OUT, and how many the command expects.
    data_out: Vec<u8>,
    out_len: usize,
    status: u8,
}

/// A Transfer Info command in progress.
enum Xfer {
    None,
    /// Polled: the host reads or writes this many more bytes through the data register.
    Polled(usize),
    /// DMA: runs when the board ticks past `at`.
    Dma { at: u64, len: usize },
    /// DMA from memory has been requested; waiting for `dma_done`.
    DmaOut,
}

/// Select-and-Transfer: the whole command runs as one operation.
struct SatJob {
    at: u64,
    target: usize,
    lun: u8,
    cdb: Vec<u8>,
    data_out: Option<Vec<u8>>,
}

pub struct A2091 {
    rom: Vec<u8>,
    pub base: Option<u32>,
    pub disks: [Option<HardDisk>; 7],

    // DMAC.
    cntr: u16,
    acr: u32,
    dma_on: bool,

    // WD33C93.
    regs: [u8; 32],
    sasr: usize,
    asr: u8,
    /// Interrupt statuses not yet presented (each one waits for the previous to be read).
    irq_queue: std::collections::VecDeque<u8>,
    irq_at: u64,
    nexus: Option<Nexus>,
    xfer: Xfer,
    /// Received MSG IN byte waiting for Negate ACK.
    msg_in_ack: Option<u8>,
    sat: Option<SatJob>,
    /// A resumed Select-and-Transfer is running its data phase.
    sat_resume: bool,
}

impl A2091 {
    pub fn new(rom: Vec<u8>) -> A2091 {
        A2091 {
            rom,
            base: None,
            disks: Default::default(),
            cntr: 0,
            acr: 0,
            dma_on: false,
            regs: [0; 32],
            sasr: 0,
            asr: 0,
            irq_queue: Default::default(),
            irq_at: 0,
            nexus: None,
            xfer: Xfer::None,
            msg_in_ack: None,
            sat: None,
            sat_resume: false,
        }
    }

    pub fn reset(&mut self) {
        let rom = std::mem::take(&mut self.rom);
        let disks = std::mem::take(&mut self.disks);
        *self = A2091::new(rom);
        self.disks = disks;
    }

    /// Autoconfig ROM contents (not inverted).
    pub fn autoconfig_regs() -> [u8; 16] {
        [
            0xc0 | 0x10 | 0x01, // Zorro II, DiagArea valid, 64 KB
            0x03,               // A590 / A2091
            0x00,
            0x00,
            0x02, // Commodore (514)
            0x02,
            0, 0, 0, 0, // serial number
            (ROM_OFFSET >> 8) as u8,
            ROM_OFFSET as u8,
            0, 0, 0, 0,
        ]
    }

    pub fn contains(&self, a: u32) -> Option<u32> {
        let off = a.wrapping_sub(self.base?);
        (off < 0x1_0000).then_some(off)
    }

    /// Interrupt line to Paula's INT2.
    pub fn irq(&self) -> bool {
        self.cntr & CNTR_INTEN != 0 && self.asr & ASR_INT != 0
    }

    fn istr(&self) -> u16 {
        let mut v = ISTR_FE_FLG;
        if self.asr & ASR_INT != 0 {
            v |= ISTR_INTS | ISTR_INT_F;
        }
        if self.irq() {
            v |= ISTR_INT_P;
        }
        v
    }

    pub fn peek_rom16(&self, off: u32) -> u16 {
        let o = (off as usize) & (self.rom.len() - 1) & !1;
        (self.rom[o] as u16) << 8 | self.rom[o + 1] as u16
    }

    // ---- DMAC registers -------------------------------------------------------

    pub fn read8(&mut self, off: u32, now: u64) -> u8 {
        if off >= ROM_OFFSET {
            return self.rom[(off as usize) & (self.rom.len() - 1)];
        }
        match off & 0xff {
            0x91 => self.asr,
            0x93 => self.read_wd(now),
            o if o & 1 == 0 => (self.read16(off, now) >> 8) as u8,
            _ => self.read16(off & !1, now) as u8,
        }
    }

    pub fn read16(&mut self, off: u32, now: u64) -> u16 {
        if off >= ROM_OFFSET {
            let o = (off as usize) & (self.rom.len() - 1);
            return (self.rom[o] as u16) << 8 | self.rom[o + 1] as u16;
        }
        match off & 0xfe {
            0x40 => self.istr(),
            0x42 => self.cntr,
            0x90 => self.asr as u16 | 0xff00,
            0x92 => self.read_wd(now) as u16 | 0xff00,
            // Strobe registers act on reads as well as writes.
            0xe0 => {
                self.dma_on = true;
                0
            }
            0xe2 => {
                self.dma_on = false;
                0
            }
            0xe4 | 0xe8 => 0,
            // A590 XT drive interface: an all-ones status means no XT drive.
            0xa0..=0xa6 => 0xffff,
            o => {
                log::trace!("a2091: read {o:02x}");
                0xffff
            }
        }
    }

    pub fn write8(&mut self, off: u32, v: u8, now: u64) {
        match off & 0xff {
            0x91 => self.sasr = v as usize & 0x1f,
            0x93 => self.write_wd(v, now),
            o if o & 1 == 0 => self.write16(off, (v as u16) << 8 | v as u16, now),
            _ => self.write16(off & !1, v as u16, now),
        }
    }

    pub fn write16(&mut self, off: u32, v: u16, now: u64) {
        match off & 0xfe {
            0x42 => self.cntr = v,
            0x84 => self.acr = (self.acr & 0xffff) | (v as u32) << 16,
            0x86 => self.acr = (self.acr & 0xffff_0000) | (v as u32 & 0xfffe),
            0x80 | 0x82 | 0x8e | 0xe4 | 0xe8 => {}
            0x90 => self.sasr = v as usize & 0x1f,
            0x92 => self.write_wd(v as u8, now),
            0xe0 => self.dma_on = true,
            0xe2 => self.dma_on = false,
            o => log::trace!("a2091: write {o:02x} = {v:04x}"),
        }
    }

    // ---- WD33C93 register file ------------------------------------------------

    fn advance_sasr(&mut self) {
        if !matches!(self.sasr, WD_AUX_STATUS | WD_DATA | WD_COMMAND) {
            self.sasr = (self.sasr + 1) & 0x1f;
        }
    }

    fn read_wd(&mut self, now: u64) -> u8 {
        let r = self.sasr;
        let v = match r {
            WD_AUX_STATUS => self.asr,
            WD_SCSI_STATUS => {
                // Reading the status acknowledges the interrupt.
                if self.asr & ASR_INT != 0 {
                    self.asr &= !ASR_INT;
                    self.irq_at = now + 40;
                }
                self.regs[r]
            }
            WD_DATA => self.data_read(),
            _ => self.regs[r],
        };
        log::trace!("wd33c93: read {r:02x} = {v:02x}");
        self.advance_sasr();
        v
    }

    fn write_wd(&mut self, v: u8, now: u64) {
        let r = self.sasr;
        log::trace!("wd33c93: write {r:02x} = {v:02x}");
        match r {
            WD_AUX_STATUS | WD_SCSI_STATUS => {}
            WD_COMMAND => {
                self.regs[r] = v;
                self.command(v, now);
            }
            WD_DATA => self.data_write(v),
            _ => self.regs[r] = v,
        }
        self.advance_sasr();
    }

    fn tc(&self) -> usize {
        (self.regs[WD_TC_HI] as usize) << 16 | (self.regs[WD_TC_HI + 1] as usize) << 8 | self.regs[WD_TC_HI + 2] as usize
    }

    fn set_tc(&mut self, n: usize) {
        self.regs[WD_TC_HI] = (n >> 16) as u8;
        self.regs[WD_TC_HI + 1] = (n >> 8) as u8;
        self.regs[WD_TC_HI + 2] = n as u8;
    }

    fn dma_mode(&self) -> bool {
        self.regs[WD_CONTROL] & 0xe0 != 0
    }

    /// Finish the current command with an interrupt status.
    fn finish(&mut self, status: u8) {
        self.asr &= !(ASR_BSY | ASR_CIP | ASR_DBR);
        self.irq_queue.push_back(status);
    }

    // ---- WD33C93 commands -----------------------------------------------------

    fn command(&mut self, cmd: u8, now: u64) {
        log::debug!("wd33c93: command {cmd:02x} phase={:?}", self.nexus.as_ref().map(|n| n.phase));
        self.asr |= ASR_CIP | ASR_BSY;
        self.irq_at = self.irq_at.max(now + 40);
        match cmd & 0x7f {
            0x00 => {
                let eaf = self.regs[WD_OWN_ID] & 0x08 != 0;
                let own = self.regs[WD_OWN_ID];
                self.regs = [0; 32];
                self.regs[WD_OWN_ID] = own;
                self.nexus = None;
                self.xfer = Xfer::None;
                self.sat = None;
                self.irq_queue.clear();
                self.finish(if eaf { 0x01 } else { 0x00 });
            }
            0x01 => {
                // Abort.
                self.xfer = Xfer::None;
                self.finish(0x50);
            }
            0x02 => self.asr &= !(ASR_BSY | ASR_CIP), // Assert ATN
            0x03 => {
                // Negate ACK: a COMMAND COMPLETE message ends the connection.
                self.asr &= !(ASR_BSY | ASR_CIP);
                if let Some(m) = self.msg_in_ack.take() {
                    if m == 0x00 || m == 0x04 {
                        self.nexus = None;
                        self.irq_queue.push_back(0x85);
                    }
                }
            }
            0x04 => {
                self.nexus = None;
                self.finish(0x85);
            }
            0x06 | 0x07 => self.select(cmd & 0x7f == 0x06),
            0x08 | 0x09 if self.nexus.is_some() => {
                // Resume a connected transaction (Command Phase register set):
                // the chip runs the remaining phases itself.
                log::debug!("wd33c93: SAT resume, command phase {:02x}", self.regs[WD_COMMAND_PHASE]);
                let nx = self.nexus.as_mut().unwrap();
                if nx.phase == Phase::Command {
                    let n = cdb_len(self.regs[WD_CDB1]);
                    nx.cdb = self.regs[WD_CDB1..WD_CDB1 + n].to_vec();
                    self.run_command();
                }
                let nx = self.nexus.as_ref().unwrap();
                if matches!(nx.phase, Phase::DataIn | Phase::DataOut) {
                    let len = self.tc();
                    self.xfer = Xfer::Dma { at: now + 300 + len as u64 * 5, len };
                    self.sat_resume = true;
                } else {
                    self.sat_finish();
                }
            }
            0x08 | 0x09 => {
                let target = (self.regs[WD_DEST_ID] & 7) as usize;
                let lun = self.regs[WD_TARGET_LUN] & 7;
                let n = cdb_len(self.regs[WD_CDB1]);
                let cdb = self.regs[WD_CDB1..WD_CDB1 + n].to_vec();
                let bytes = match cdb[0] {
                    0x08 | 0x0a => (if cdb[4] == 0 { 256 } else { cdb[4] as u64 }) * 512,
                    0x28 | 0x2a => ((cdb[7] as u64) << 8 | cdb[8] as u64) * 512,
                    _ => 0,
                };
                self.sat = Some(SatJob { at: now + 1500 + bytes * 5, target, lun, cdb, data_out: None });
            }
            0x20 => self.transfer_info(cmd & 0x80 != 0, now),
            c => {
                log::warn!("wd33c93: unsupported command {c:02x}");
                self.finish(0x22);
            }
        }
    }

    fn select(&mut self, atn: bool) {
        let target = (self.regs[WD_DEST_ID] & 7) as usize;
        if self.disks[target].is_none() {
            self.finish(0x42);
            return;
        }
        let phase = if atn { Phase::MsgOut } else { Phase::Command };
        self.nexus = Some(Nexus {
            target,
            lun: self.regs[WD_TARGET_LUN] & 7,
            phase,
            cdb: vec![],
            data_in: vec![],
            in_pos: 0,
            data_out: vec![],
            out_len: 0,
            status: GOOD,
        });
        self.finish(0x11);
        // The target then requests its first phase.
        self.irq_queue.push_back(0x88 | phase as u8);
    }

    fn transfer_info(&mut self, single: bool, now: u64) {
        let Some(nx) = self.nexus.as_ref() else {
            self.finish(0x22);
            return;
        };
        let len = if single { 1 } else { self.tc().max(1) };
        let phase = nx.phase;
        if self.dma_mode() && !single && matches!(phase, Phase::DataIn | Phase::DataOut) {
            self.xfer = Xfer::Dma { at: now + 300 + len as u64 * 5, len };
        } else {
            self.xfer = Xfer::Polled(len);
            if phase.is_in() || matches!(phase, Phase::MsgOut | Phase::Command | Phase::DataOut) {
                self.asr |= ASR_DBR;
            }
        }
    }

    // ---- target side ----------------------------------------------------------

    /// Bytes delivered from the initiator in an out-phase.
    fn target_receive(&mut self, bytes: &[u8]) {
        let nx = self.nexus.as_mut().unwrap();
        match nx.phase {
            Phase::MsgOut => {
                for &b in bytes {
                    if b & 0x80 != 0 {
                        nx.lun = b & 7;
                    }
                }
                nx.phase = Phase::Command;
            }
            Phase::Command => {
                nx.cdb.extend_from_slice(bytes);
                if nx.cdb.len() >= cdb_len(nx.cdb[0]) {
                    self.run_command();
                }
            }
            Phase::DataOut => {
                nx.data_out.extend_from_slice(bytes);
                if nx.data_out.len() >= nx.out_len {
                    let (t, cdb, data) = (nx.target, nx.cdb.clone(), std::mem::take(&mut nx.data_out));
                    let status = self.disks[t].as_mut().unwrap().execute(&cdb, &data, &mut vec![]);
                    let nx = self.nexus.as_mut().unwrap();
                    nx.status = status;
                    nx.data_in = vec![status];
                    nx.in_pos = 0;
                    nx.phase = Phase::Status;
                }
            }
            _ => {}
        }
    }

    fn run_command(&mut self) {
        let nx = self.nexus.as_mut().unwrap();
        let disk = self.disks[nx.target].as_mut().unwrap();
        let out_len = HardDisk::data_out_len(&nx.cdb);
        if nx.lun == 0 && out_len > 0 {
            nx.out_len = out_len;
            nx.phase = Phase::DataOut;
            return;
        }
        let mut data = vec![];
        let status = if nx.lun != 0 {
            if nx.cdb[0] == 0x12 {
                data.extend([0x7f, 0, 2, 2]);
                data.resize((nx.cdb[4] as usize).min(36), 0);
                GOOD
            } else {
                disk.sense = (5, 0x25);
                CHECK_CONDITION
            }
        } else {
            disk.execute(&nx.cdb, &[], &mut data)
        };
        log::debug!("scsi{}: {:02x?} -> status {status:02x}, {} bytes", nx.target, nx.cdb, data.len());
        nx.status = status;
        if data.is_empty() {
            nx.data_in = vec![status];
            nx.phase = Phase::Status;
        } else {
            nx.data_in = data;
            nx.phase = Phase::DataIn;
        }
        nx.in_pos = 0;
    }

    /// The initiator has taken `n` bytes of an in-phase.
    fn target_sent(&mut self, n: usize) {
        let nx = self.nexus.as_mut().unwrap();
        nx.in_pos += n;
        if nx.in_pos < nx.data_in.len() {
            return;
        }
        nx.in_pos = 0;
        match nx.phase {
            Phase::DataIn => {
                nx.data_in = vec![nx.status];
                nx.phase = Phase::Status;
            }
            Phase::Status => {
                nx.data_in = vec![0x00]; // COMMAND COMPLETE
                nx.phase = Phase::MsgIn;
            }
            _ => {}
        }
    }

    /// End of a Transfer Info command: report the phase the target asks for next.
    fn transfer_done(&mut self, was: Phase) {
        self.xfer = Xfer::None;
        if self.sat_resume {
            self.sat_finish();
            return;
        }
        if was == Phase::MsgIn {
            // The chip pauses with ACK asserted so the message can be examined.
            let nx = self.nexus.as_ref().unwrap();
            self.msg_in_ack = nx.data_in.first().copied();
            self.finish(0x20);
            return;
        }
        let phase = self.nexus.as_ref().unwrap().phase;
        self.finish(0x18 | phase as u8);
    }

    /// Complete a resumed Select-and-Transfer: status and message phases, then disconnect.
    fn sat_finish(&mut self) {
        self.sat_resume = false;
        let status = self.nexus.take().map_or(GOOD, |n| n.status);
        self.regs[WD_TARGET_LUN] = status;
        self.regs[WD_COMMAND_PHASE] = 0x60;
        self.finish(0x16);
        self.sat_disconnect();
    }

    /// The original WD33C93 (as fitted to the A590/A2091) always follows a
    /// completed Select-and-Transfer with a disconnect interrupt once the bus is
    /// free; the WD33C93A only does so with EDI set. Commodore's driver relies on it.
    fn sat_disconnect(&mut self) {
        self.irq_queue.push_back(0x85);
    }

    fn data_read(&mut self) -> u8 {
        let Xfer::Polled(left) = self.xfer else { return 0 };
        let Some(nx) = self.nexus.as_ref().filter(|n| n.phase.is_in()) else { return 0 };
        let phase = nx.phase;
        let v = nx.data_in.get(nx.in_pos).copied().unwrap_or(0);
        if phase == Phase::MsgIn {
            // The message byte stays on the bus until Negate ACK.
            self.transfer_done(phase);
            return v;
        }
        self.target_sent(1);
        let tc = self.tc().saturating_sub(1);
        self.set_tc(tc);
        let changed = self.nexus.as_ref().unwrap().phase != phase;
        if left <= 1 || changed {
            self.transfer_done(phase);
        } else {
            self.xfer = Xfer::Polled(left - 1);
        }
        v
    }

    fn data_write(&mut self, v: u8) {
        let Xfer::Polled(left) = self.xfer else { return };
        let Some(phase) = self.nexus.as_ref().map(|n| n.phase).filter(|p| !p.is_in()) else { return };
        self.target_receive(&[v]);
        let tc = self.tc().saturating_sub(1);
        self.set_tc(tc);
        let changed = self.nexus.as_ref().unwrap().phase != phase;
        if left <= 1 || changed {
            self.transfer_done(phase);
        } else {
            self.xfer = Xfer::Polled(left - 1);
        }
    }

    // ---- time -------------------------------------------------------------------

    /// Advance to `now`. Returns a DMA transfer the board must perform.
    pub fn tick(&mut self, now: u64) -> Option<Dma> {
        if self.asr & ASR_INT == 0 && now >= self.irq_at {
            if let Some(s) = self.irq_queue.pop_front() {
                log::trace!("wd33c93: interrupt status {s:02x}");
                self.regs[WD_SCSI_STATUS] = s;
                self.asr |= ASR_INT;
            }
        }
        if let Xfer::Dma { at, len } = self.xfer {
            if now >= at {
                return self.dma_phase(len);
            }
        }
        if self.sat.as_ref().is_some_and(|j| now >= j.at) {
            return self.sat_run();
        }
        None
    }

    fn dma_phase(&mut self, len: usize) -> Option<Dma> {
        if !self.dma_on {
            log::warn!("a2091: SCSI DMA while the DMAC is stopped (cntr={:04x})", self.cntr);
        }
        let nx = self.nexus.as_mut().unwrap();
        let phase = nx.phase;
        if phase == Phase::DataOut {
            let n = len.min(nx.out_len - nx.data_out.len());
            self.xfer = Xfer::DmaOut;
            return Some(Dma::FromMemory(self.acr, n));
        }
        let n = len.min(nx.data_in.len() - nx.in_pos);
        let bytes = nx.data_in[nx.in_pos..nx.in_pos + n].to_vec();
        self.target_sent(n);
        let a = self.acr;
        self.acr = self.acr.wrapping_add(n as u32);
        self.set_tc(self.tc().saturating_sub(n));
        self.transfer_done(phase);
        Some(Dma::ToMemory(a, bytes))
    }

    /// Hand over the data fetched from memory for a DMA out-transfer.
    pub fn dma_done(&mut self, data: Vec<u8>) {
        if let Some(j) = self.sat.as_mut().filter(|j| j.data_out.is_none() && matches!(self.xfer, Xfer::None)) {
            j.data_out = Some(data);
            return;
        }
        let n = data.len();
        self.acr = self.acr.wrapping_add(n as u32);
        self.set_tc(self.tc().saturating_sub(n));
        self.target_receive(&data);
        self.transfer_done(Phase::DataOut);
    }

    fn sat_run(&mut self) -> Option<Dma> {
        let j = self.sat.as_ref().unwrap();
        if self.disks[j.target].is_none() {
            self.sat = None;
            self.regs[WD_COMMAND_PHASE] = 0;
            self.finish(0x42);
            return None;
        };
        // A data-out command first needs its data fetched from memory.
        let out_len = HardDisk::data_out_len(&j.cdb).min(self.tc());
        if out_len > 0 && j.data_out.is_none() {
            return Some(Dma::FromMemory(self.acr, out_len));
        }
        let j = self.sat.take().unwrap();
        let disk = self.disks[j.target].as_mut().unwrap();
        let mut data = vec![];
        let status = if j.lun != 0 {
            disk.sense = (5, 0x25);
            CHECK_CONDITION
        } else {
            disk.execute(&j.cdb, j.data_out.as_deref().unwrap_or(&[]), &mut data)
        };
        log::debug!("scsi{} (SAT): {:02x?} -> status {status:02x}, {} bytes", j.target, j.cdb, data.len());
        let tc = self.tc();
        data.truncate(tc);
        let moved = data.len().max(j.data_out.as_ref().map_or(0, |d| d.len()));
        self.set_tc(tc - moved.min(tc));
        self.regs[WD_TARGET_LUN] = status;
        self.regs[WD_COMMAND_PHASE] = 0x60;
        self.finish(0x16);
        self.sat_disconnect();
        let a = self.acr;
        self.acr = self.acr.wrapping_add(moved as u32);
        (!data.is_empty()).then_some(Dma::ToMemory(a, data))
    }
}
