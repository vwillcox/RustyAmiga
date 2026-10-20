//! Motorola 68000 / 68020 interpreter.

mod ops;
mod ops020;

use crate::bus::Bus;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Size {
    B,
    W,
    L,
}

impl Size {
    #[inline]
    pub fn bytes(self) -> u32 {
        match self {
            Size::B => 1,
            Size::W => 2,
            Size::L => 4,
        }
    }
    #[inline]
    pub fn mask(self) -> u32 {
        match self {
            Size::B => 0xff,
            Size::W => 0xffff,
            Size::L => 0xffff_ffff,
        }
    }
    #[inline]
    pub fn msb(self) -> u32 {
        match self {
            Size::B => 0x80,
            Size::W => 0x8000,
            Size::L => 0x8000_0000,
        }
    }
    /// Standard two-bit size field (00=B, 01=W, 10=L).
    #[inline]
    pub fn from_bits(b: u16) -> Option<Size> {
        match b & 3 {
            0 => Some(Size::B),
            1 => Some(Size::W),
            2 => Some(Size::L),
            _ => None,
        }
    }
}

/// A resolved effective address.
#[derive(Clone, Copy, Debug)]
pub enum Ea {
    D(usize),
    A(usize),
    M(u32),
    Imm(u32),
}

pub const VEC_BUS_ERROR: u32 = 2;
pub const VEC_ADDRESS_ERROR: u32 = 3;
pub const VEC_ILLEGAL: u32 = 4;
pub const VEC_ZERO_DIVIDE: u32 = 5;
pub const VEC_CHK: u32 = 6;
pub const VEC_TRAPV: u32 = 7;
pub const VEC_PRIVILEGE: u32 = 8;
pub const VEC_TRACE: u32 = 9;
pub const VEC_LINE_A: u32 = 10;
pub const VEC_LINE_F: u32 = 11;
pub const VEC_FORMAT_ERROR: u32 = 14;
pub const VEC_TRAP_BASE: u32 = 32;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum CpuModel {
    #[default]
    M68000,
    /// 68020 (the A1200's 68EC020 differs only in its 24-bit address bus,
    /// which the board decodes).
    M68020,
}

#[derive(Clone, Debug, Default)]
pub struct Cpu {
    pub d: [u32; 8],
    /// a[7] is the active stack pointer.
    pub a: [u32; 8],
    pub pc: u32,
    /// Holds the inactive stack pointer: USP while in supervisor mode,
    /// SSP while in user mode.
    pub other_sp: u32,
    pub s: bool,
    pub t: bool,
    pub imask: u8,
    pub x: bool,
    pub n: bool,
    pub z: bool,
    pub v: bool,
    pub c: bool,
    pub stopped: bool,
    pub halted: bool,
    /// Address of the instruction currently executing.
    pub instr_pc: u32,
    /// Set by an instruction that raised an exception, so trace is suppressed.
    exception_taken: bool,
    /// Trace exception to be taken before the next instruction.
    trace_pending: bool,
    /// Two-word prefetch queue: words at `pq_addr` and `pq_addr + 2`.
    /// Instruction words come from here, so code modified after it was
    /// prefetched still executes in its old form, as on a real 68000.
    pq_addr: u32,
    pq: [u16; 2],
    pq_valid: bool,

    pub model: CpuModel,
    // 68010+/68020 control registers.
    pub vbr: u32,
    pub sfc: u8,
    pub dfc: u8,
    pub cacr: u32,
    pub caar: u32,
    /// Master stack pointer (68020 M bit). Amiga software never sets M, so the
    /// interrupt stack pointer is simply the SSP.
    pub msp: u32,
    pub m: bool,
}

impl Cpu {
    pub fn new() -> Self {
        Cpu {
            s: true,
            imask: 7,
            ..Default::default()
        }
    }

    pub fn with_model(model: CpuModel) -> Self {
        Cpu { model, ..Cpu::new() }
    }

    #[inline]
    fn is_020(&self) -> bool {
        self.model >= CpuModel::M68020
    }

    /// Hardware reset: load SSP and PC from vectors 0 and 1.
    pub fn reset<B: Bus>(&mut self, bus: &mut B) {
        self.s = true;
        self.t = false;
        self.imask = 7;
        self.stopped = false;
        self.halted = false;
        self.pq_valid = false;
        self.vbr = 0;
        self.cacr = 0;
        self.m = false;
        self.a[7] = self.read32(bus, 0);
        self.pc = self.read32(bus, 4);
    }

    // ---- status register -------------------------------------------------

    pub fn sr(&self) -> u16 {
        (self.t as u16) << 15
            | (self.s as u16) << 13
            | (self.m as u16) << 12
            | (self.imask as u16) << 8
            | self.ccr() as u16
    }

    pub fn ccr(&self) -> u8 {
        (self.x as u8) << 4 | (self.n as u8) << 3 | (self.z as u8) << 2 | (self.v as u8) << 1 | self.c as u8
    }

    pub fn set_ccr(&mut self, v: u8) {
        self.x = v & 0x10 != 0;
        self.n = v & 0x08 != 0;
        self.z = v & 0x04 != 0;
        self.v = v & 0x02 != 0;
        self.c = v & 0x01 != 0;
    }

    pub fn set_sr(&mut self, v: u16) {
        self.set_ccr(v as u8);
        self.t = v & 0x8000 != 0;
        self.imask = ((v >> 8) & 7) as u8;
        if self.is_020() {
            self.m = v & 0x1000 != 0;
        }
        self.set_supervisor(v & 0x2000 != 0);
    }

    pub fn set_supervisor(&mut self, s: bool) {
        if s != self.s {
            std::mem::swap(&mut self.a[7], &mut self.other_sp);
            self.s = s;
        }
    }

    pub fn usp(&self) -> u32 {
        if self.s {
            self.other_sp
        } else {
            self.a[7]
        }
    }

    pub fn ssp(&self) -> u32 {
        if self.s {
            self.a[7]
        } else {
            self.other_sp
        }
    }

    pub fn set_usp(&mut self, v: u32) {
        if self.s {
            self.other_sp = v
        } else {
            self.a[7] = v
        }
    }

    pub fn set_ssp(&mut self, v: u32) {
        if self.s {
            self.a[7] = v
        } else {
            self.other_sp = v
        }
    }

    // ---- bus helpers -------------------------------------------------------

    #[inline]
    fn read<B: Bus>(&mut self, bus: &mut B, addr: u32, sz: Size) -> u32 {
        match sz {
            Size::B => bus.read8(addr) as u32,
            Size::W => self.read16(bus, addr) as u32,
            Size::L => self.read32(bus, addr),
        }
    }

    /// Word read; the 68020 allows odd addresses (done as byte cycles).
    #[inline]
    fn read16<B: Bus>(&mut self, bus: &mut B, addr: u32) -> u16 {
        if addr & 1 != 0 && self.is_020() {
            return (bus.read8(addr) as u16) << 8 | bus.read8(addr.wrapping_add(1)) as u16;
        }
        bus.read16(addr)
    }

    #[inline]
    fn read32<B: Bus>(&mut self, bus: &mut B, addr: u32) -> u32 {
        let hi = self.read16(bus, addr) as u32;
        let lo = self.read16(bus, addr.wrapping_add(2)) as u32;
        hi << 16 | lo
    }

    #[inline]
    fn write16<B: Bus>(&mut self, bus: &mut B, addr: u32, v: u16) {
        if addr & 1 != 0 && self.is_020() {
            bus.write8(addr, (v >> 8) as u8);
            bus.write8(addr.wrapping_add(1), v as u8);
            return;
        }
        bus.write16(addr, v);
    }

    #[inline]
    fn write<B: Bus>(&mut self, bus: &mut B, addr: u32, sz: Size, v: u32) {
        match sz {
            Size::B => bus.write8(addr, v as u8),
            Size::W => self.write16(bus, addr, v as u16),
            Size::L => {
                self.write16(bus, addr, (v >> 16) as u16);
                self.write16(bus, addr.wrapping_add(2), v as u16);
            }
        }
    }

    #[inline]
    fn fetch16<B: Bus>(&mut self, bus: &mut B) -> u16 {
        if self.is_020() {
            let v = bus.read16(self.pc);
            self.pc = self.pc.wrapping_add(2);
            return v;
        }
        if !self.pq_valid || self.pq_addr != self.pc {
            // Control flow changed: refill the queue (two bus reads).
            self.pq_addr = self.pc;
            self.pq[0] = bus.read16(self.pc);
            self.pq[1] = bus.read16(self.pc.wrapping_add(2));
            self.pq_valid = true;
        }
        let v = self.pq[0];
        self.pq[0] = self.pq[1];
        self.pq_addr = self.pq_addr.wrapping_add(2);
        self.pq[1] = bus.read16(self.pq_addr.wrapping_add(2));
        self.pc = self.pc.wrapping_add(2);
        v
    }

    /// Discard the prefetch queue (reset, or tests that load memory directly).
    pub fn flush_prefetch(&mut self) {
        self.pq_valid = false;
    }

    #[inline]
    fn fetch32<B: Bus>(&mut self, bus: &mut B) -> u32 {
        let hi = self.fetch16(bus) as u32;
        let lo = self.fetch16(bus) as u32;
        hi << 16 | lo
    }

    fn push16<B: Bus>(&mut self, bus: &mut B, v: u16) {
        self.a[7] = self.a[7].wrapping_sub(2);
        bus.write16(self.a[7], v);
    }

    fn push32<B: Bus>(&mut self, bus: &mut B, v: u32) {
        // The 68000 writes the low word first when pushing a long.
        self.a[7] = self.a[7].wrapping_sub(4);
        bus.write16(self.a[7].wrapping_add(2), v as u16);
        bus.write16(self.a[7], (v >> 16) as u16);
    }

    fn pop16<B: Bus>(&mut self, bus: &mut B) -> u16 {
        let v = bus.read16(self.a[7]);
        self.a[7] = self.a[7].wrapping_add(2);
        v
    }

    fn pop32<B: Bus>(&mut self, bus: &mut B) -> u32 {
        let v = self.read32(bus, self.a[7]);
        self.a[7] = self.a[7].wrapping_add(4);
        v
    }

    // ---- effective addresses ----------------------------------------------

    fn index<B: Bus>(&mut self, bus: &mut B, base: u32) -> u32 {
        let ext = self.fetch16(bus);
        bus.idle(2);
        let r = ((ext >> 12) & 7) as usize;
        let mut idx = if ext & 0x8000 != 0 { self.a[r] } else { self.d[r] };
        if ext & 0x0800 == 0 {
            idx = idx as i16 as i32 as u32;
        }
        if !self.is_020() {
            return base.wrapping_add(idx).wrapping_add(ext as i8 as i32 as u32);
        }
        idx <<= (ext >> 9) & 3;
        if ext & 0x0100 == 0 {
            return base.wrapping_add(idx).wrapping_add(ext as i8 as i32 as u32);
        }
        // 68020 full extension word: base/index suppress, base displacement,
        // optional memory indirection with outer displacement.
        let base = if ext & 0x80 != 0 { 0 } else { base };
        let idx = if ext & 0x40 != 0 { 0 } else { idx };
        let bd = match (ext >> 4) & 3 {
            2 => self.fetch16(bus) as i16 as i32 as u32,
            3 => self.fetch32(bus),
            _ => 0,
        };
        let iis = ext & 7;
        if iis == 0 {
            return base.wrapping_add(bd).wrapping_add(idx);
        }
        let od = match iis & 3 {
            2 => self.fetch16(bus) as i16 as i32 as u32,
            3 => self.fetch32(bus),
            _ => 0,
        };
        if iis & 4 == 0 {
            // Pre-indexed: ([bd,base,index],od)
            let p = self.read32(bus, base.wrapping_add(bd).wrapping_add(idx));
            p.wrapping_add(od)
        } else {
            // Post-indexed: ([bd,base],index,od)
            let p = self.read32(bus, base.wrapping_add(bd));
            p.wrapping_add(idx).wrapping_add(od)
        }
    }

    /// Decode an effective address. `mode`/`reg` are the 3-bit fields.
    fn ea<B: Bus>(&mut self, bus: &mut B, mode: u16, reg: u16, sz: Size) -> Ea {
        let r = reg as usize;
        match mode {
            0 => Ea::D(r),
            1 => Ea::A(r),
            2 => Ea::M(self.a[r]),
            3 => {
                let addr = self.a[r];
                let inc = if r == 7 && sz == Size::B { 2 } else { sz.bytes() };
                self.a[r] = addr.wrapping_add(inc);
                Ea::M(addr)
            }
            4 => {
                bus.idle(2);
                let dec = if r == 7 && sz == Size::B { 2 } else { sz.bytes() };
                self.a[r] = self.a[r].wrapping_sub(dec);
                Ea::M(self.a[r])
            }
            5 => {
                let d = self.fetch16(bus) as i16 as i32 as u32;
                Ea::M(self.a[r].wrapping_add(d))
            }
            6 => {
                let base = self.a[r];
                Ea::M(self.index(bus, base))
            }
            _ => match reg {
                0 => Ea::M(self.fetch16(bus) as i16 as i32 as u32),
                1 => Ea::M(self.fetch32(bus)),
                2 => {
                    let base = self.pc;
                    let d = self.fetch16(bus) as i16 as i32 as u32;
                    Ea::M(base.wrapping_add(d))
                }
                3 => {
                    let base = self.pc;
                    Ea::M(self.index(bus, base))
                }
                _ => Ea::Imm(match sz {
                    Size::B => (self.fetch16(bus) & 0xff) as u32,
                    Size::W => self.fetch16(bus) as u32,
                    Size::L => self.fetch32(bus),
                }),
            },
        }
    }

    fn read_ea<B: Bus>(&mut self, bus: &mut B, ea: Ea, sz: Size) -> u32 {
        match ea {
            Ea::D(r) => self.d[r] & sz.mask(),
            Ea::A(r) => self.a[r] & sz.mask(),
            Ea::M(addr) => self.read(bus, addr, sz),
            Ea::Imm(v) => v & sz.mask(),
        }
    }

    fn write_ea<B: Bus>(&mut self, bus: &mut B, ea: Ea, sz: Size, v: u32) {
        match ea {
            Ea::D(r) => {
                let m = sz.mask();
                self.d[r] = (self.d[r] & !m) | (v & m);
            }
            Ea::A(r) => self.a[r] = v,
            Ea::M(addr) => self.write(bus, addr, sz, v),
            Ea::Imm(_) => {}
        }
    }

    // ---- exceptions --------------------------------------------------------

    /// Group 1/2 exception: push PC and SR, jump through `vector`.
    pub fn exception<B: Bus>(&mut self, bus: &mut B, vector: u32, stacked_pc: u32) {
        static LOG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if *LOG.get_or_init(|| std::env::var_os("AMIGA_LOG_EXCEPTIONS").is_some()) && vector < 32 {
            eprintln!("exception vector {vector} at pc {:06x} (instr {:06x})", stacked_pc, self.instr_pc);
        }
        let old_sr = self.sr();
        self.set_supervisor(true);
        self.t = false;
        bus.idle(4);
        if self.is_020() {
            // Format 2 frames carry the address of the faulting instruction.
            let format = match vector {
                VEC_CHK | VEC_TRAPV | VEC_ZERO_DIVIDE | VEC_TRACE => 2,
                _ => 0,
            };
            if format == 2 {
                let ia = self.instr_pc;
                self.push32(bus, ia);
            }
            self.push16(bus, (format << 12 | vector * 4) as u16);
        }
        self.push32(bus, stacked_pc);
        self.push16(bus, old_sr);
        self.pc = self.read32(bus, self.vbr.wrapping_add(vector * 4));
        self.exception_taken = true;
    }

    fn interrupt<B: Bus>(&mut self, bus: &mut B, level: u8) {
        self.stopped = false;
        let old_sr = self.sr();
        self.set_supervisor(true);
        self.t = false;
        self.imask = level;
        // Interrupt acknowledge cycle and internal processing.
        bus.idle(24);
        let vector = 24 + level as u32;
        if self.is_020() {
            self.push16(bus, (vector * 4) as u16);
        }
        let pc = self.pc;
        self.push32(bus, pc);
        self.push16(bus, old_sr);
        self.pc = self.read32(bus, self.vbr.wrapping_add(vector * 4));
    }

    // ---- condition codes and flags ----------------------------------------

    #[inline]
    fn cond(&self, cc: u16) -> bool {
        match cc & 15 {
            0 => true,
            1 => false,
            2 => !self.c && !self.z,
            3 => self.c || self.z,
            4 => !self.c,
            5 => self.c,
            6 => !self.z,
            7 => self.z,
            8 => !self.v,
            9 => self.v,
            10 => !self.n,
            11 => self.n,
            12 => self.n == self.v,
            13 => self.n != self.v,
            14 => !self.z && self.n == self.v,
            _ => self.z || self.n != self.v,
        }
    }

    #[inline]
    fn set_nz(&mut self, v: u32, sz: Size) {
        self.n = v & sz.msb() != 0;
        self.z = v & sz.mask() == 0;
    }

    #[inline]
    fn logic_flags(&mut self, v: u32, sz: Size) {
        self.set_nz(v, sz);
        self.v = false;
        self.c = false;
    }

    /// dst + src (+X). Sets XNZVC. For ADDX, Z is only cleared.
    fn add(&mut self, src: u32, dst: u32, sz: Size, extend: bool) -> u32 {
        let m = sz.mask();
        let (s, d) = (src & m, dst & m);
        let x = (extend && self.x) as u64;
        let wide = s as u64 + d as u64 + x;
        let r = (wide as u32) & m;
        self.c = wide > m as u64;
        self.x = self.c;
        self.v = (s ^ r) & (d ^ r) & sz.msb() != 0;
        self.n = r & sz.msb() != 0;
        if extend {
            if r != 0 {
                self.z = false;
            }
        } else {
            self.z = r == 0;
        }
        r
    }

    /// dst - src (-X). Sets XNZVC unless `cmp`, which leaves X alone.
    fn sub(&mut self, src: u32, dst: u32, sz: Size, extend: bool, cmp: bool) -> u32 {
        let m = sz.mask();
        let (s, d) = (src & m, dst & m);
        let x = (extend && self.x) as u64;
        let r = (d as u64).wrapping_sub(s as u64).wrapping_sub(x) as u32 & m;
        self.c = s as u64 + x > d as u64;
        if !cmp {
            self.x = self.c;
        }
        self.v = (s ^ d) & (r ^ d) & sz.msb() != 0;
        self.n = r & sz.msb() != 0;
        if extend {
            if r != 0 {
                self.z = false;
            }
        } else {
            self.z = r == 0;
        }
        r
    }

    // ---- main step ---------------------------------------------------------

    /// Execute one instruction (or service a pending interrupt).
    pub fn step<B: Bus>(&mut self, bus: &mut B) {
        if self.halted {
            bus.idle(4);
            return;
        }
        if self.trace_pending {
            self.trace_pending = false;
            let pc = self.pc;
            self.exception(bus, VEC_TRACE, pc);
            return;
        }
        let ipl = bus.ipl();
        if ipl > self.imask {
            self.interrupt(bus, ipl);
            return;
        }
        if self.stopped {
            bus.idle(4);
            return;
        }
        let trace = self.t;
        self.exception_taken = false;
        self.instr_pc = self.pc;
        let op = self.fetch16(bus);
        self.execute(bus, op);
        if trace && !self.exception_taken {
            self.trace_pending = true;
        }
    }

    fn illegal<B: Bus>(&mut self, bus: &mut B) {
        let pc = self.instr_pc;
        self.exception(bus, VEC_ILLEGAL, pc);
    }

    fn privilege_violation<B: Bus>(&mut self, bus: &mut B) {
        let pc = self.instr_pc;
        self.exception(bus, VEC_PRIVILEGE, pc);
    }
}
