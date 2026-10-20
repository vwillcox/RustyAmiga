//! Gayle (A600/A1200 gate array): the ID register, the IDE interface and its
//! interrupt logic. Hard disks are HDF images attached as ATA master and slave.
//!
//! Address map (A1200/A600):
//! - $DA2000 + 4n: ATA task file register n (data, error/features, sector
//!   count, sector, cylinder low/high, drive/head, status/command)
//! - $DA3018: alternate status / device control
//! - $DA8000: card status (bit 7 = IDE interrupt line)
//! - $DA9000: interrupt change latch (write 0 bits to clear)
//! - $DAA000: interrupt enable
//! - $DE1000: Gayle ID, read one bit at a time in bit 7
//!
//! The A4000 has the same IDE interface without the rest of Gayle:
//! - $DD2020: data; $DD2022 + 4n: task file register n
//! - $DD303A: alternate status / device control
//! - $DD3020: interrupt status (bit 7)

const GAYLE_ID: u8 = 0xd0;
const IRQ_IDE: u8 = 0x80;

// ATA status bits.
const BSY: u8 = 0x80;
const DRDY: u8 = 0x40;
const DSC: u8 = 0x10;
const DRQ: u8 = 0x08;
const ERR: u8 = 0x01;

// Error register bits.
const ABRT: u8 = 0x04;
const IDNF: u8 = 0x10;

/// An HDF image attached as an ATA drive.
pub struct AtaDrive {
    pub name: String,
    pub data: Vec<u8>,
    cyls: u32,
    heads: u32,
    secs: u32,
    /// Sectors per interrupt for READ/WRITE MULTIPLE.
    multiple: u32,
}

impl AtaDrive {
    pub fn new(name: String, data: Vec<u8>) -> AtaDrive {
        let blocks = (data.len() / 512) as u32;
        let (heads, secs) = (16, 63);
        let cyls = blocks.div_ceil(heads * secs).clamp(1, 65535);
        AtaDrive { name, data, cyls, heads, secs, multiple: 1 }
    }

    fn blocks(&self) -> u32 {
        (self.data.len() / 512) as u32
    }

    fn identify(&self) -> Vec<u8> {
        let mut w = [0u16; 256];
        w[0] = 0x0040; // fixed disk
        w[1] = self.cyls as u16;
        w[3] = self.heads as u16;
        w[6] = self.secs as u16;
        let text = |w: &mut [u16; 256], at: usize, len: usize, s: &str| {
            let b: Vec<u8> = s.bytes().chain(std::iter::repeat(b' ')).take(len * 2).collect();
            for i in 0..len {
                w[at + i] = (b[i * 2] as u16) << 8 | b[i * 2 + 1] as u16;
            }
        };
        text(&mut w, 10, 10, "00000001");
        text(&mut w, 23, 4, "1.0");
        text(&mut w, 27, 20, "AMIGA HARDFILE");
        w[47] = 0x8010; // up to 16 sectors per READ/WRITE MULTIPLE
        w[49] = 0x0200; // LBA supported
        w[53] = 1;
        w[54] = self.cyls as u16;
        w[55] = self.heads as u16;
        w[56] = self.secs as u16;
        let chs = self.cyls * self.heads * self.secs;
        w[57] = chs as u16;
        w[58] = (chs >> 16) as u16;
        w[59] = 0x100 | self.multiple as u16;
        let lba = self.blocks();
        w[60] = lba as u16;
        w[61] = (lba >> 16) as u16;
        // Gayle wires the IDE data bus byte-swapped, so ATA words reach the CPU
        // swapped (sector data, written by the Amiga, round-trips unchanged);
        // scsi.device swaps IDENTIFY data back.
        w.iter().flat_map(|v| v.to_le_bytes()).collect()
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Xfer {
    None,
    /// Host reads `count` more sectors; `per_irq` sectors between interrupts.
    Read { count: u32, per_irq: u32 },
    Write { count: u32, per_irq: u32 },
}

pub struct Gayle {
    /// A4000 IDE (no Gayle registers, different address decode).
    pub a4000: bool,
    pub drives: [Option<AtaDrive>; 2],
    id_pos: u8,
    irq_latch: u8,
    int_enable: u8,
    cfg: u8,

    // ATA task file.
    error: u8,
    features: u8,
    nsect: u8,
    sector: u8,
    cyl: u16,
    select: u8,
    status: u8,
    devctl: u8,
    /// Device interrupt line (INTRQ).
    intrq: bool,

    buf: Vec<u8>,
    pos: usize,
    xfer: Xfer,
    /// LBA of the next sector to transfer.
    lba: u32,
}

impl Gayle {
    pub fn new() -> Gayle {
        Gayle {
            a4000: false,
            drives: [None, None],
            id_pos: 0,
            irq_latch: 0,
            int_enable: 0,
            cfg: 0,
            error: 1,
            features: 0,
            nsect: 1,
            sector: 1,
            cyl: 0,
            select: 0xa0,
            status: DRDY | DSC,
            devctl: 0,
            intrq: false,
            buf: vec![],
            pos: 0,
            xfer: Xfer::None,
            lba: 0,
        }
    }

    pub fn new_a4000() -> Gayle {
        Gayle { a4000: true, ..Gayle::new() }
    }

    pub fn reset(&mut self) {
        let drives = std::mem::take(&mut self.drives);
        let a4000 = self.a4000;
        *self = Gayle::new();
        self.drives = drives;
        self.a4000 = a4000;
    }

    /// Interrupt line to Paula's INT2 (PORTS).
    pub fn irq(&self) -> bool {
        if self.a4000 {
            return self.intrq;
        }
        self.irq_latch & self.int_enable & IRQ_IDE != 0
    }

    /// A4000 decode: Some(Ok(reg)) for a task file register, Some(Err(())) for
    /// the alternate status/device control register.
    fn a4000_reg(a: u32) -> Option<Result<u8, ()>> {
        match a & 0xff_ffff {
            0xdd_2020..=0xdd_203f => Some(Ok(((a - 0xdd_2020) >> 2) as u8 & 7)),
            0xdd_3038..=0xdd_303b => Some(Err(())),
            _ => None,
        }
    }

    fn unit(&self) -> usize {
        ((self.select >> 4) & 1) as usize
    }

    fn drive(&self) -> Option<&AtaDrive> {
        self.drives[self.unit()].as_ref()
    }

    fn raise(&mut self) {
        if self.devctl & 2 == 0 {
            self.intrq = true;
            self.irq_latch |= IRQ_IDE;
        }
    }

    // ---- bus interface ----------------------------------------------------------

    pub fn read8(&mut self, a: u32) -> u8 {
        if self.a4000 {
            return match Self::a4000_reg(a) {
                Some(Ok(r)) => self.ide_read(r),
                Some(Err(())) => self.alt_status(),
                None if a & 0xff_fffc == 0xdd_3020 => {
                    if self.intrq {
                        IRQ_IDE
                    } else {
                        0
                    }
                }
                None => 0xff,
            };
        }
        match a >> 16 & 0xff {
            0xda => match a & 0xf000 {
                0x8000 => {
                    if self.intrq {
                        IRQ_IDE
                    } else {
                        0
                    }
                }
                0x9000 => self.irq_latch,
                0xa000 => self.int_enable,
                0xb000 => self.cfg,
                0x2000 => self.ide_read((a >> 2) as u8 & 7),
                0x3000 if (a >> 2) & 7 == 6 => self.alt_status(),
                _ => 0xff,
            },
            0xde if a & 0xf000 == 0x1000 => {
                let bit = GAYLE_ID << (self.id_pos & 7) & 0x80;
                self.id_pos = self.id_pos.wrapping_add(1);
                bit
            }
            _ => 0,
        }
    }

    pub fn read16(&mut self, a: u32) -> u16 {
        if self.a4000 && a & 0xff_fffc == 0xdd_2020 {
            return self.data_read();
        }
        if !self.a4000 && a & 0xff_f000 == 0xda_2000 && (a >> 2) & 7 == 0 {
            return self.data_read();
        }
        let v = self.read8(a);
        (v as u16) << 8 | v as u16
    }

    pub fn write8(&mut self, a: u32, v: u8) {
        if self.a4000 {
            match Self::a4000_reg(a) {
                Some(Ok(r)) => self.ide_write(r, v),
                Some(Err(())) => self.devctl_write(v),
                None => {}
            }
            return;
        }
        match a >> 16 & 0xff {
            0xda => match a & 0xf000 {
                0x9000 => self.irq_latch &= v,
                0xa000 => self.int_enable = v,
                0xb000 => self.cfg = v,
                0x8000 => {}
                0x2000 => self.ide_write((a >> 2) as u8 & 7, v),
                0x3000 if (a >> 2) & 7 == 6 => self.devctl_write(v),
                _ => {}
            },
            0xde if a & 0xf000 == 0x1000 => self.id_pos = 0,
            _ => {}
        }
    }

    pub fn write16(&mut self, a: u32, v: u16) {
        if self.a4000 && a & 0xff_fffc == 0xdd_2020 {
            return self.data_write(v);
        }
        if !self.a4000 && a & 0xff_f000 == 0xda_2000 && (a >> 2) & 7 == 0 {
            return self.data_write(v);
        }
        // Byte registers sit on the low byte lane on the A4000, high on Gayle.
        self.write8(a, if self.a4000 { v as u8 } else { (v >> 8) as u8 });
    }

    // ---- ATA registers ----------------------------------------------------------

    fn alt_status(&self) -> u8 {
        if self.drive().is_none() {
            return 0;
        }
        self.status
    }

    fn ide_read(&mut self, reg: u8) -> u8 {
        if self.drive().is_none() && reg != 6 {
            // No device answers for an absent drive.
            return if reg == 7 { 0 } else { 0xff };
        }
        let v = match reg {
            0 => (self.data_read() >> 8) as u8,
            1 => self.error,
            2 => self.nsect,
            3 => self.sector,
            4 => self.cyl as u8,
            5 => (self.cyl >> 8) as u8,
            6 => self.select,
            _ => {
                self.intrq = false;
                self.status
            }
        };
        log::trace!("ata: read {reg} = {v:02x}");
        v
    }

    fn ide_write(&mut self, reg: u8, v: u8) {
        log::trace!("ata: write {reg} = {v:02x}");
        match reg {
            0 => self.data_write((v as u16) << 8 | v as u16),
            1 => self.features = v,
            2 => self.nsect = v,
            3 => self.sector = v,
            4 => self.cyl = (self.cyl & 0xff00) | v as u16,
            5 => self.cyl = (self.cyl & 0x00ff) | (v as u16) << 8,
            6 => self.select = v | 0xa0,
            _ => self.command(v),
        }
    }

    fn devctl_write(&mut self, v: u8) {
        if v & 4 != 0 && self.devctl & 4 == 0 {
            // Software reset.
            self.xfer = Xfer::None;
            self.status = BSY;
        } else if v & 4 == 0 && self.devctl & 4 != 0 {
            self.status = DRDY | DSC;
            self.error = 1;
            self.nsect = 1;
            self.sector = 1;
            self.cyl = 0;
            self.select = 0xa0;
        }
        self.devctl = v;
    }

    /// Current CHS or LBA address from the task file.
    fn address(&self) -> Option<u32> {
        let d = self.drive()?;
        if self.select & 0x40 != 0 {
            Some((self.select as u32 & 15) << 24 | (self.cyl as u32) << 8 | self.sector as u32)
        } else {
            let head = (self.select & 15) as u32;
            if self.sector == 0 {
                return None;
            }
            Some((self.cyl as u32 * d.heads + head) * d.secs + self.sector as u32 - 1)
        }
    }

    /// Write the LBA back into the task file (it reflects the last sector moved).
    fn set_address(&mut self, lba: u32) {
        let Some(d) = self.drive() else { return };
        if self.select & 0x40 != 0 {
            self.sector = lba as u8;
            self.cyl = (lba >> 8) as u16;
            self.select = (self.select & 0xf0) | ((lba >> 24) & 15) as u8;
        } else {
            let (heads, secs) = (d.heads, d.secs);
            self.sector = (lba % secs + 1) as u8;
            let t = lba / secs;
            self.select = (self.select & 0xf0) | (t % heads) as u8;
            self.cyl = (t / heads) as u16;
        }
    }

    fn abort(&mut self) {
        self.error = ABRT;
        self.status = DRDY | DSC | ERR;
        self.raise();
    }

    fn command(&mut self, cmd: u8) {
        log::debug!("ata{}: command {cmd:02x} nsect={} lba={:?}", self.unit(), self.nsect, self.address());
        if self.drive().is_none() {
            return;
        }
        self.error = 0;
        let count = if self.nsect == 0 { 256 } else { self.nsect as u32 };
        match cmd {
            0xec => {
                self.buf = self.drive().unwrap().identify();
                self.pos = 0;
                self.xfer = Xfer::Read { count: 1, per_irq: 1 };
                self.lba = 0;
                self.status = DRDY | DSC | DRQ;
                self.raise();
            }
            0x20 | 0x21 | 0xc4 => {
                let Some(lba) = self.address() else { return self.abort() };
                let per_irq = if cmd == 0xc4 { self.drive().unwrap().multiple } else { 1 };
                self.lba = lba;
                self.xfer = Xfer::Read { count, per_irq };
                self.load_sectors();
            }
            0x30 | 0x31 | 0xc5 => {
                let Some(lba) = self.address() else { return self.abort() };
                let per_irq = if cmd == 0xc5 { self.drive().unwrap().multiple } else { 1 };
                self.lba = lba;
                self.xfer = Xfer::Write { count, per_irq };
                self.buf = vec![0; (per_irq.min(count) * 512) as usize];
                self.pos = 0;
                // The first block is requested without an interrupt.
                self.status = DRDY | DSC | DRQ;
            }
            0xc6 => {
                let n = self.nsect as u32;
                if n == 0 || n > 16 || !n.is_power_of_two() {
                    return self.abort();
                }
                self.drives[self.unit()].as_mut().unwrap().multiple = n;
                self.status = DRDY | DSC;
                self.raise();
            }
            0x91 => {
                // INITIALIZE DEVICE PARAMETERS: adopt the host's translation.
                let heads = (self.select & 15) as u32 + 1;
                let secs = self.nsect as u32;
                if secs == 0 {
                    return self.abort();
                }
                let d = self.drives[self.unit()].as_mut().unwrap();
                d.heads = heads;
                d.secs = secs;
                d.cyls = d.blocks().div_ceil(heads * secs).clamp(1, 65535);
                self.status = DRDY | DSC;
                self.raise();
            }
            0x10..=0x1f | 0x40 | 0x41 | 0x70..=0x7f | 0xe0..=0xe7 | 0xef | 0x94..=0x99 => {
                // Recalibrate, verify, seek, power management, set features.
                self.status = DRDY | DSC;
                self.raise();
            }
            0x90 => {
                // EXECUTE DEVICE DIAGNOSTIC
                self.error = 1;
                self.status = DRDY | DSC;
                self.raise();
            }
            _ => {
                log::debug!("ata: unsupported command {cmd:02x}");
                self.abort();
            }
        }
    }

    /// Fill the buffer with the next block of sectors for a read.
    fn load_sectors(&mut self) {
        let Xfer::Read { count, per_irq } = self.xfer else { return };
        let n = per_irq.min(count);
        let d = self.drives[self.unit()].as_ref().unwrap();
        if self.lba + n > d.cyls * d.heads * d.secs && self.lba + n > d.blocks() {
            self.xfer = Xfer::None;
            self.error = IDNF;
            self.status = DRDY | DSC | ERR;
            self.raise();
            return;
        }
        let a = self.lba as usize * 512;
        let mut buf = vec![0u8; n as usize * 512];
        if a < d.data.len() {
            let end = (a + buf.len()).min(d.data.len());
            buf[..end - a].copy_from_slice(&d.data[a..end]);
        }
        self.buf = buf;
        self.pos = 0;
        self.status = DRDY | DSC | DRQ;
        self.raise();
    }

    fn data_read(&mut self) -> u16 {
        let Xfer::Read { count, per_irq } = self.xfer else { return 0xffff };
        let v = (self.buf[self.pos] as u16) << 8 | self.buf[self.pos + 1] as u16;
        self.pos += 2;
        if self.pos >= self.buf.len() {
            let n = per_irq.min(count);
            self.lba += n;
            if self.lba > 0 {
                self.set_address(self.lba - 1);
            }
            let left = count - n;
            self.nsect = left as u8;
            if left == 0 {
                self.xfer = Xfer::None;
                self.status = DRDY | DSC;
            } else {
                self.xfer = Xfer::Read { count: left, per_irq };
                self.load_sectors();
            }
        }
        v
    }

    fn data_write(&mut self, v: u16) {
        let Xfer::Write { count, per_irq } = self.xfer else { return };
        self.buf[self.pos] = (v >> 8) as u8;
        self.buf[self.pos + 1] = v as u8;
        self.pos += 2;
        if self.pos < self.buf.len() {
            return;
        }
        let n = per_irq.min(count);
        let unit = self.unit();
        let a = self.lba as usize * 512;
        let d = self.drives[unit].as_mut().unwrap();
        if a < d.data.len() {
            let end = (a + self.buf.len()).min(d.data.len());
            d.data[a..end].copy_from_slice(&self.buf[..end - a]);
        }
        self.lba += n;
        self.set_address(self.lba - 1);
        let left = count - n;
        self.nsect = left as u8;
        if left == 0 {
            self.xfer = Xfer::None;
            self.status = DRDY | DSC;
        } else {
            self.xfer = Xfer::Write { count: left, per_irq };
            self.buf = vec![0; (per_irq.min(left) * 512) as usize];
            self.pos = 0;
            self.status = DRDY | DSC | DRQ;
        }
        self.raise();
    }
}

impl Default for Gayle {
    fn default() -> Self {
        Self::new()
    }
}
