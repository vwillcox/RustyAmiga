//! NCR 53C710 SCSI I/O processor (A4091, A4000T, many accelerator boards).
//!
//! The chip runs its own programs ("SCRIPTS") fetched from memory by DMA: block
//! moves between memory and the SCSI bus, selection, jumps and calls on bus
//! phase or data, register arithmetic and memory-to-memory copies. The host
//! loads DSP with a script address, and the chip interrupts when the script
//! executes INT or something goes wrong (phase mismatch, selection timeout).
//!
//! Registers are kept at their little-endian offsets, as the data manual lists
//! them. Amiga boards wire the chip big-endian, so the host sees register `r`
//! at byte offset `r ^ 3`; longword registers read naturally either way.

use crate::expansion::DmaMem;
use crate::scsi::{Bus, Phase};

const SCNTL1: usize = 0x01;
const SIEN: usize = 0x03;
const SXFER: usize = 0x05;
const SFBR: usize = 0x08;
const SBCL: usize = 0x0b;
const DSTAT: usize = 0x0c;
const SSTAT0: usize = 0x0d;
const SSTAT1: usize = 0x0e;
const SSTAT2: usize = 0x0f;
const DSA: usize = 0x10;
const CTEST1: usize = 0x15;
const CTEST2: usize = 0x16;
const TEMP: usize = 0x1c;
const ISTAT: usize = 0x21;
const CTEST8: usize = 0x22;
const DBC: usize = 0x24;
const DCMD: usize = 0x27;
const DNAD: usize = 0x28;
const DSP: usize = 0x2c;
const DSPS: usize = 0x30;
const DMODE: usize = 0x38;
const DIEN: usize = 0x39;
const DCNTL: usize = 0x3b;

// DSTAT
const DFE: u8 = 0x80;
const ABRT: u8 = 0x10;
const SIR: u8 = 0x04;
const IID: u8 = 0x01;
// SSTAT0
const MA: u8 = 0x80;
const STO: u8 = 0x20;
const UDC: u8 = 0x04;
const RST: u8 = 0x02;
// ISTAT
const ISTAT_ABRT: u8 = 0x80;
const ISTAT_RST: u8 = 0x40;
const SIGP: u8 = 0x20;
const CON: u8 = 0x08;
const SIP: u8 = 0x02;
const DIP: u8 = 0x01;

/// Chip revision in CTEST8's high nibble.
const REVISION: u8 = 2;

/// Timings in machine ticks (7.09 MHz): an instruction fetch and decode, a
/// byte on the SCSI bus (asynchronous, about 3.5 MB/s), and the selection
/// timeout (250 ms on the real chip; shortened, as nothing measures it).
const STEP: u64 = 4;
const SCSI_BYTE: u64 = 2;
const SELECT_TIMEOUT: u64 = 7_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Run {
    Halted,
    Running,
    /// WAIT RESELECT: until a target reselects (never) or the host sets SIGP.
    WaitReselect { alt: u32 },
    /// WAIT DISCONNECT: until the target leaves the bus.
    WaitDisconnect,
    /// SELECT of a missing target: the timeout fires at `at`.
    Selecting { at: u64 },
}

pub struct Ncr710 {
    r: [u8; 64],
    pub bus: Bus,
    run: Run,
    busy_until: u64,
    carry: bool,
    /// Where the host sees the registers, so scripts can move data to and from
    /// the chip itself: (bus address, window size).
    pub window: (u32, u32),
}

impl Default for Ncr710 {
    fn default() -> Self {
        let mut c = Ncr710 { r: [0; 64], bus: Bus::default(), run: Run::Halted, busy_until: 0, carry: false, window: (0, 0) };
        c.reset();
        c
    }
}

impl Ncr710 {
    /// Chip reset (power on, the board's reset line, or ISTAT RST).
    pub fn reset(&mut self) {
        self.r = [0; 64];
        self.r[DSTAT] = DFE;
        self.r[CTEST1] = 0xf0;
        self.r[CTEST8] = REVISION << 4;
        self.run = Run::Halted;
        self.carry = false;
    }

    fn get32(&self, i: usize) -> u32 {
        u32::from_le_bytes(self.r[i..i + 4].try_into().unwrap())
    }

    fn set32(&mut self, i: usize, v: u32) {
        self.r[i..i + 4].copy_from_slice(&v.to_le_bytes());
    }

    fn set_dbc(&mut self, n: u32) {
        let dcmd = self.r[DCMD];
        self.set32(DBC, n & 0xff_ffff | (dcmd as u32) << 24);
    }

    pub fn irq(&self) -> bool {
        self.r[DSTAT] & self.r[DIEN] & 0x7f != 0 || self.r[SSTAT0] & self.r[SIEN] != 0
    }

    fn istat(&self) -> u8 {
        let mut v = self.r[ISTAT] & (ISTAT_ABRT | ISTAT_RST | SIGP);
        if self.bus.connected() {
            v |= CON;
        }
        if self.r[SSTAT0] != 0 {
            v |= SIP;
        }
        if self.r[DSTAT] & 0x7f != 0 {
            v |= DIP;
        }
        v
    }

    /// The bus control lines as SBCL shows them: REQ ACK BSY SEL ATN MSG C/D I/O.
    fn sbcl(&mut self) -> u8 {
        let atn = if self.bus.atn() { 0x08 } else { 0 };
        match self.bus.phase(false) {
            Some(p) => 0x80 | 0x20 | atn | p as u8,
            None => atn,
        }
    }

    // ---- host access --------------------------------------------------------------

    /// Read register `i` (little-endian numbering).
    pub fn read(&mut self, i: usize) -> u8 {
        let i = i & 0x3f;
        let v = match i {
            DSTAT => {
                let v = self.r[DSTAT] | DFE;
                self.r[DSTAT] = DFE;
                v
            }
            SSTAT0 => std::mem::take(&mut self.r[SSTAT0]),
            // CON: connected to the bus.
            SCNTL1 => self.r[SCNTL1] & !0x10 | if self.bus.connected() { 0x10 } else { 0 },
            SSTAT1 => 0,
            SSTAT2 => self.bus.phase(false).map_or(0, |p| p as u8),
            SBCL => self.sbcl(),
            ISTAT => self.istat(),
            CTEST2 => {
                // Reading CTEST2 clears SIGP; bit 6 shows it.
                let v = (self.r[ISTAT] & SIGP) << 1;
                self.r[ISTAT] &= !SIGP;
                v
            }
            CTEST8 => REVISION << 4 | self.r[CTEST8] & 0x0f,
            _ => self.r[i],
        };
        log::trace!("ncr710: read {i:02x} = {v:02x}");
        v
    }

    /// Write register `i` (little-endian numbering).
    pub fn write(&mut self, i: usize, v: u8, now: u64) {
        let i = i & 0x3f;
        log::trace!("ncr710: write {i:02x} = {v:02x}");
        match i {
            DSTAT | SSTAT0 | SSTAT1 | SSTAT2 | SBCL => {}
            ISTAT => {
                if v & ISTAT_RST != 0 {
                    self.reset();
                    self.r[ISTAT] = ISTAT_RST;
                    return;
                }
                if v & ISTAT_ABRT != 0 && self.run != Run::Halted {
                    self.dma_int(ABRT);
                }
                self.r[ISTAT] = v & (ISTAT_ABRT | SIGP);
            }
            SCNTL1 => {
                if v & 0x08 != 0 && self.r[SCNTL1] & 0x08 == 0 {
                    // Assert SCSI RST: every target lets go.
                    self.bus.reset();
                    self.scsi_int(RST);
                }
                self.r[SCNTL1] = v;
            }
            DCNTL => {
                self.r[DCNTL] = v & !0x04;
                if v & 0x04 != 0 {
                    self.start(now);
                }
            }
            _ => {
                self.r[i] = v;
                // Writing DSP's low byte (the last in big-endian order) starts
                // the script, unless DMODE asks for a manual start.
                if i == DSP && self.r[DMODE] & 1 == 0 {
                    self.start(now);
                }
            }
        }
    }

    fn start(&mut self, now: u64) {
        log::debug!("ncr710: script starts at {:08x}", self.get32(DSP));
        self.run = Run::Running;
        self.busy_until = self.busy_until.max(now);
    }

    fn dma_int(&mut self, bits: u8) {
        self.r[DSTAT] |= bits;
        self.run = Run::Halted;
        log::debug!("ncr710: DMA interrupt {bits:02x} at {:08x} (DSPS {:08x})", self.get32(DSP), self.get32(DSPS));
    }

    fn scsi_int(&mut self, bits: u8) {
        self.r[SSTAT0] |= bits;
        self.run = Run::Halted;
        log::debug!("ncr710: SCSI interrupt {bits:02x} at {:08x}", self.get32(DSP));
    }

    // ---- memory -----------------------------------------------------------------

    /// The register a bus address hits, if it is the chip's own.
    fn own_reg(&self, a: u32) -> Option<usize> {
        let o = a.wrapping_sub(self.window.0);
        (o < self.window.1).then_some((o as usize & 0x3f) ^ 3)
    }

    fn mem8(&mut self, mem: &mut dyn DmaMem, a: u32) -> u8 {
        match self.own_reg(a) {
            Some(i) => self.r[i],
            None => mem.read8(a),
        }
    }

    fn set_mem8(&mut self, mem: &mut dyn DmaMem, a: u32, v: u8) {
        match self.own_reg(a) {
            Some(i) => self.r[i] = v,
            None => mem.write8(a, v),
        }
    }

    fn mem32(&mut self, mem: &mut dyn DmaMem, a: u32) -> u32 {
        (0..4).fold(0, |v, k| v << 8 | self.mem8(mem, a.wrapping_add(k)) as u32)
    }

    // ---- SCRIPTS ----------------------------------------------------------------

    /// Run the script up to `now`.
    pub fn tick(&mut self, now: u64, mem: &mut dyn DmaMem) {
        while self.busy_until <= now {
            match self.run {
                Run::Halted => return,
                Run::Running => {
                    let cost = self.step(mem, now);
                    self.busy_until += cost;
                }
                Run::WaitReselect { alt } => {
                    if self.r[ISTAT] & SIGP == 0 {
                        return;
                    }
                    self.set32(DSP, alt);
                    self.run = Run::Running;
                }
                Run::WaitDisconnect => {
                    if self.bus.connected() {
                        return;
                    }
                    self.run = Run::Running;
                }
                Run::Selecting { at } => {
                    if now < at {
                        return;
                    }
                    self.busy_until = at;
                    self.scsi_int(STO);
                }
            }
        }
    }

    /// Execute one instruction. Returns the time it took.
    fn step(&mut self, mem: &mut dyn DmaMem, now: u64) -> u64 {
        let dsp = self.get32(DSP);
        let w0 = self.mem32(mem, dsp);
        let w1 = self.mem32(mem, dsp.wrapping_add(4));
        self.set32(DBC, w0);
        self.set32(DSP, dsp.wrapping_add(8));
        log::trace!("ncr710: {dsp:08x}: {w0:08x} {w1:08x}");
        match w0 >> 30 {
            0 => {
                self.set32(DNAD, w1);
                self.block_move(mem, w0, w1)
            }
            1 => {
                self.set32(DSPS, w1);
                if (w0 >> 27) & 7 >= 5 {
                    self.register_op(w0);
                    STEP
                } else {
                    self.io(mem, w0, w1, now)
                }
            }
            2 => {
                self.set32(DSPS, w1);
                self.transfer_control(w0, w1);
                STEP
            }
            _ => {
                // Memory move: source in the second word, destination in a third.
                let dst = self.mem32(mem, dsp.wrapping_add(8));
                self.set32(DSP, dsp.wrapping_add(12));
                self.set32(DSPS, w1);
                self.set32(TEMP, dst);
                let n = w0 & 0xff_ffff;
                for k in 0..n {
                    let b = self.mem8(mem, w1.wrapping_add(k));
                    self.set_mem8(mem, dst.wrapping_add(k), b);
                }
                self.set_dbc(0);
                STEP + n as u64 / 2
            }
        }
    }

    /// Block move between memory and the SCSI bus.
    fn block_move(&mut self, mem: &mut dyn DmaMem, w0: u32, w1: u32) -> u64 {
        let (mut count, mut addr) = (w0 & 0xff_ffff, w1);
        if w0 & 1 << 28 != 0 {
            // Table indirect: a (count, address) pair at DSA + signed offset.
            let e = self.get32(DSA).wrapping_add(((w1 << 8) as i32 >> 8) as u32);
            count = self.mem32(mem, e) & 0xff_ffff;
            addr = self.mem32(mem, e.wrapping_add(4));
        } else if w0 & 1 << 29 != 0 {
            addr = self.mem32(mem, w1);
        }
        self.set_dbc(count);
        self.set32(DNAD, addr);
        let want = Phase::from_lines((w0 >> 24) as u8);
        let mut moved = 0;
        let mut cost = STEP;
        while moved < count {
            let Some(p) = self.bus.phase(true) else {
                self.scsi_int(UDC);
                break;
            };
            if p != want {
                self.scsi_int(MA);
                break;
            }
            let a = addr.wrapping_add(moved);
            if p.is_in() {
                let b = self.bus.read();
                if moved == 0 {
                    self.r[SFBR] = b;
                }
                self.set_mem8(mem, a, b);
            } else {
                // The chip drops ATN before the last byte of a MESSAGE OUT.
                if p == Phase::MsgOut && moved + 1 == count {
                    self.bus.set_atn(false);
                }
                let b = self.mem8(mem, a);
                self.bus.write(b);
            }
            moved += 1;
            cost += SCSI_BYTE;
        }
        self.set_dbc(count - moved);
        self.set32(DNAD, addr.wrapping_add(moved));
        cost
    }

    fn relative(&self, w1: u32) -> u32 {
        self.get32(DSP).wrapping_add(((w1 << 8) as i32 >> 8) as u32)
    }

    /// SELECT, WAIT DISCONNECT, WAIT RESELECT, SET and CLEAR.
    fn io(&mut self, mem: &mut dyn DmaMem, w0: u32, w1: u32, now: u64) -> u64 {
        // The alternate address may be relative too.
        let w1 = if w0 & 1 << 26 != 0 { self.relative(w1) } else { w1 };
        match (w0 >> 27) & 7 {
            0 => {
                let ids = if w0 & 1 << 25 != 0 {
                    // Table indirect: the ID (and an SXFER value) at DSA + offset.
                    let e = self.get32(DSA).wrapping_add(((w0 << 8) as i32 >> 8) as u32);
                    let v = self.mem32(mem, e);
                    self.r[SXFER] = (v >> 8) as u8;
                    (v >> 16) as u8
                } else {
                    (w0 >> 16) as u8
                };
                let id = ids.trailing_zeros() as usize;
                if self.bus.connected() {
                    self.set32(DSP, w1);
                } else if self.bus.select(id, w0 & 1 << 24 != 0) {
                    log::debug!("ncr710: selected target {id}");
                } else {
                    self.run = Run::Selecting { at: now + SELECT_TIMEOUT };
                }
            }
            1 => self.run = Run::WaitDisconnect,
            2 => {
                if self.bus.connected() {
                    self.dma_int(IID);
                } else {
                    self.run = Run::WaitReselect { alt: w1 };
                }
            }
            op @ (3 | 4) => {
                let set = op == 3;
                if w0 & 1 << 9 != 0 {
                    self.bus.set_atn(set);
                }
                if w0 & 1 << 6 != 0 && !set {
                    self.bus.clear_ack();
                }
            }
            _ => self.dma_int(IID),
        }
        STEP
    }

    /// Register read-modify-write and moves to and from SFBR.
    fn register_op(&mut self, w0: u32) {
        let opcode = (w0 >> 27) & 7;
        let reg = ((w0 >> 16) & 0x3f) as usize;
        let data = (w0 >> 8) as u8;
        let (a, b) = match opcode {
            5 => (self.r[SFBR], data),
            _ => (self.read(reg), data),
        };
        let v = match (w0 >> 24) & 7 {
            0 => b,
            1 => {
                let c = a >> 7;
                let v = a << 1 | self.carry as u8;
                self.carry = c != 0;
                v
            }
            2 => a | b,
            3 => a ^ b,
            4 => a & b,
            5 => {
                let c = a & 1;
                let v = a >> 1 | (self.carry as u8) << 7;
                self.carry = c != 0;
                v
            }
            6 => {
                let (v, c) = a.overflowing_add(b);
                self.carry = c;
                v
            }
            _ => {
                let (v, c1) = a.overflowing_add(b);
                let (v, c2) = v.overflowing_add(self.carry as u8);
                self.carry = c1 || c2;
                v
            }
        };
        match opcode {
            6 => self.r[SFBR] = v,
            _ => self.r[reg] = v,
        }
    }

    /// JUMP, CALL, RETURN and INT, on a phase and/or data condition.
    fn transfer_control(&mut self, w0: u32, w1: u32) {
        let mut cond = true;
        if w0 & 1 << 17 != 0 {
            // Compare phase; with "wait for valid phase", wait for REQ first.
            let want = Phase::from_lines((w0 >> 24) as u8);
            match self.bus.phase(w0 & 1 << 16 != 0) {
                Some(p) => cond &= p == want,
                None if w0 & 1 << 16 != 0 => {
                    self.scsi_int(UDC);
                    return;
                }
                None => cond = false,
            }
        }
        if w0 & 1 << 18 != 0 {
            let mask = !(w0 >> 8) as u8;
            cond &= self.r[SFBR] & mask == w0 as u8 & mask;
        }
        if cond != (w0 & 1 << 19 != 0) {
            return;
        }
        // Relative addressing: a signed 24-bit offset from the next instruction.
        let w1 = if w0 & 1 << 23 != 0 { self.relative(w1) } else { w1 };
        match (w0 >> 27) & 7 {
            0 => self.set32(DSP, w1),
            1 => {
                let ret = self.get32(DSP);
                self.set32(TEMP, ret);
                self.set32(DSP, w1);
            }
            2 => {
                let ret = self.get32(TEMP);
                self.set32(DSP, ret);
            }
            3 => self.dma_int(SIR),
            _ => self.dma_int(IID),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scsi::HardDisk;

    struct Ram(Vec<u8>);
    impl DmaMem for Ram {
        fn read8(&mut self, a: u32) -> u8 {
            self.0[a as usize]
        }
        fn write8(&mut self, a: u32, v: u8) {
            self.0[a as usize] = v;
        }
    }

    fn put(m: &mut Ram, a: usize, words: &[u32]) {
        for (k, w) in words.iter().enumerate() {
            m.0[a + 4 * k..a + 4 * k + 4].copy_from_slice(&w.to_be_bytes());
        }
    }

    /// A script that reads block 1 of target 1 into $2000 and interrupts with
    /// the status byte in SFBR, as a driver's would.
    #[test]
    fn script_reads_a_block() {
        let mut m = Ram(vec![0; 0x4000]);
        // IDENTIFY, then READ(6) of block 1.
        m.0[0x100] = 0x80;
        m.0[0x104..0x10a].copy_from_slice(&[0x08, 0, 0, 1, 1, 0]);
        put(
            &mut m,
            0x1000,
            &[
                0x4102_0000, 0x0000_1080, // SELECT ATN 1, alt
                0x0e00_0001, 0x100,       // MOVE 1, msg, WHEN MSG_OUT
                0x0a00_0006, 0x104,       // MOVE 6, cdb, WHEN CMD
                0x0900_0200, 0x2000,      // MOVE 512, buf, WHEN DATA_IN
                0x0b00_0001, 0x300,       // MOVE 1, status, WHEN STATUS
                0x0f00_0001, 0x301,       // MOVE 1, msg, WHEN MSG_IN
                0x6000_0040, 0,           // CLEAR ACK
                0x4800_0000, 0,           // WAIT DISCONNECT
                0x9808_0000, 0x1234,      // INT 0x1234
            ],
        );
        let mut c = Ncr710::default();
        let mut data = vec![0u8; 16 * 512];
        data[512..516].copy_from_slice(b"BLK1");
        c.bus.disks[1] = Some(HardDisk::new("t".into(), data, false));
        c.r[DIEN] = SIR;
        for (k, b) in 0x1000u32.to_le_bytes().iter().enumerate().rev() {
            c.write(DSP + k, *b, 0);
        }
        c.tick(100_000, &mut m);
        assert!(c.irq());
        assert_eq!(c.read(DSTAT) & SIR, SIR);
        assert_eq!(c.get32(DSPS), 0x1234);
        assert_eq!(&m.0[0x2000..0x2004], b"BLK1");
        assert_eq!(m.0[0x300], 0);
        assert!(!c.bus.connected());
    }

    /// WRITE(6) without a message phase, and a table-indirect SELECT.
    #[test]
    fn script_writes_a_block() {
        let mut m = Ram(vec![0; 0x4000]);
        m.0[0x104..0x10a].copy_from_slice(&[0x0a, 0, 0, 2, 1, 0]);
        for i in 0..512 {
            m.0[0x2000 + i] = i as u8 ^ 0x5a;
        }
        put(&mut m, 0x800, &[0x0001_0000]); // select table at DSA: ID bit 0
        put(
            &mut m,
            0x1000,
            &[
                0x4200_0000, 0x0000_1080, // SELECT FROM 0 (table indirect)
                0x0a00_0006, 0x104,       // MOVE 6, cdb, WHEN CMD
                0x0800_0200, 0x2000,      // MOVE 512, buf, WHEN DATA_OUT
                0x0b00_0001, 0x300,       // MOVE 1, status, WHEN STATUS
                0x0f00_0001, 0x301,       // MOVE 1, msg, WHEN MSG_IN
                0x6000_0040, 0,           // CLEAR ACK
                0x4800_0000, 0,           // WAIT DISCONNECT
                0x9808_0000, 0x77,        // INT 0x77
            ],
        );
        let mut c = Ncr710::default();
        c.bus.disks[0] = Some(HardDisk::new("t".into(), vec![0u8; 16 * 512], false));
        c.r[DIEN] = SIR;
        c.set32(DSA, 0x800);
        c.set32(DSP, 0x1000);
        c.start(0);
        c.tick(100_000, &mut m);
        assert_eq!(c.get32(DSPS), 0x77);
        let d = &c.bus.disks[0].as_ref().unwrap().data;
        assert_eq!(d[2 * 512 + 7], 7 ^ 0x5a);
        assert_eq!(m.0[0x300], 0);
    }

    #[test]
    fn missing_target_times_out() {
        let mut m = Ram(vec![0; 0x2000]);
        put(&mut m, 0x1000, &[0x4120_0000, 0x1080]); // SELECT ATN 5
        let mut c = Ncr710::default();
        c.r[SIEN] = STO;
        c.set32(DSP, 0x1000);
        c.start(0);
        c.tick(10, &mut m);
        assert!(!c.irq());
        c.tick(SELECT_TIMEOUT + 10, &mut m);
        assert!(c.irq());
        assert_eq!(c.read(ISTAT) & SIP, SIP);
        assert_eq!(c.read(SSTAT0), STO);
    }
}
