//! Instructions added by the 68010 and 68020.

use super::*;

impl Cpu {
    // ---- system control ---------------------------------------------------------

    /// RTE: the 68010+ reads the stack frame format word to know the frame size.
    pub(super) fn rte_020<B: Bus>(&mut self, bus: &mut B) {
        let sp = self.a[7];
        let sr = self.read16(bus, sp);
        let pc = self.read32(bus, sp.wrapping_add(2));
        let fv = self.read16(bus, sp.wrapping_add(6));
        let size = match fv >> 12 {
            0 | 1 => 8,
            2 => 12,
            9 => 20,
            0xa => 32,
            0xb => 92,
            _ => {
                let pc = self.instr_pc;
                return self.exception(bus, VEC_FORMAT_ERROR, pc);
            }
        };
        self.a[7] = sp.wrapping_add(size);
        self.set_sr(sr);
        self.pc = pc;
        if fv >> 12 == 1 {
            // Throwaway frame: continue with the frame on the new stack.
            self.rte_020(bus);
        }
    }

    pub(super) fn movec<B: Bus>(&mut self, bus: &mut B, to_ctl: bool) {
        if !self.s {
            return self.privilege_violation(bus);
        }
        let ext = self.fetch16(bus);
        let r = ((ext >> 12) & 7) as usize;
        let is_a = ext & 0x8000 != 0;
        let ctl = ext & 0xfff;
        if to_ctl {
            let v = if is_a { self.a[r] } else { self.d[r] };
            match ctl {
                0x000 => self.sfc = (v & 7) as u8,
                0x001 => self.dfc = (v & 7) as u8,
                // CACR keeps only the bits each CPU implements; command bits
                // read back as zero. Exec relies on this to tell the CPUs apart.
                0x002 => {
                    self.cacr = v & match self.model {
                        CpuModel::M68040 => 0x8000_8000,
                        CpuModel::M68030 => 0x0000_3313,
                        _ => 0x0000_0003,
                    }
                }
                0x800 => self.set_usp(v),
                0x801 => self.vbr = v,
                0x802 if self.model != CpuModel::M68040 => self.caar = v,
                0x803 => self.msp = v,
                0x804 => self.set_ssp(v),
                0x003..=0x007 | 0x805..=0x807 if self.model == CpuModel::M68040 => {
                    self.mmu[Self::mmu040_index(ctl)] = v as u64;
                }
                _ => return self.illegal(bus),
            }
        } else {
            let v = match ctl {
                0x000 => self.sfc as u32,
                0x001 => self.dfc as u32,
                0x002 => self.cacr,
                0x800 => self.usp(),
                0x801 => self.vbr,
                0x802 if self.model != CpuModel::M68040 => self.caar,
                0x803 => self.msp,
                0x804 => self.ssp(),
                0x003..=0x007 | 0x805..=0x807 if self.model == CpuModel::M68040 => {
                    self.mmu[Self::mmu040_index(ctl)] as u32
                }
                _ => return self.illegal(bus),
            };
            if is_a {
                self.a[r] = v;
            } else {
                self.d[r] = v;
            }
        }
    }

    /// Slot in `mmu` for a 68040 MOVEC control register.
    fn mmu040_index(ctl: u16) -> usize {
        match ctl {
            0x003 => 0, // TC
            0x004 => 1, // ITT0
            0x005 => 2, // ITT1
            0x006 => 3, // DTT0
            0x007 => 4, // DTT1
            0x805 => 5, // MMUSR
            0x806 => 6, // URP
            _ => 7,     // SRP
        }
    }

    /// 68030 MMU instructions (coprocessor ID 0): PMOVE, PFLUSH, PTEST, PLOAD.
    /// Register contents are kept so software can read them back.
    pub(super) fn mmu030<B: Bus>(&mut self, bus: &mut B, op: u16) {
        if !self.s {
            return self.privilege_violation(bus);
        }
        let ext = self.fetch16(bus);
        let mode = (op >> 3) & 7;
        let reg = op & 7;
        match ext >> 13 {
            // PMOVE to/from TT0/TT1 (000), TC/SRP/CRP (010), MMUSR (011).
            0 | 2 | 3 => {
                let preg = (ext >> 10) & 7;
                let (slot, bytes) = match (ext >> 13, preg) {
                    (0, 2) => (1, 4), // TT0
                    (0, 3) => (2, 4), // TT1
                    (2, 0) => (0, 4), // TC
                    (2, 2) => (6, 8), // SRP
                    (2, 3) => (7, 8), // CRP
                    (3, _) => (5, 2), // MMUSR
                    _ => return,
                };
                let to_mem = ext & 0x0200 != 0;
                let sz = if bytes == 2 { Size::W } else { Size::L };
                let ea = self.ea(bus, mode, reg, sz);
                let addr = match ea {
                    Ea::M(a) => a,
                    _ => {
                        // Register operands (TC, TT, MMUSR to Dn/An).
                        if to_mem {
                            let v = self.mmu[slot] as u32;
                            self.write_ea(bus, ea, sz, v);
                        } else {
                            self.mmu[slot] = self.read_ea(bus, ea, sz) as u64;
                        }
                        return;
                    }
                };
                if to_mem {
                    let v = self.mmu[slot];
                    if bytes == 8 {
                        self.write(bus, addr, Size::L, (v >> 32) as u32);
                        self.write(bus, addr.wrapping_add(4), Size::L, v as u32);
                    } else {
                        self.write(bus, addr, sz, v as u32);
                    }
                } else if bytes == 8 {
                    let hi = self.read(bus, addr, Size::L) as u64;
                    let lo = self.read(bus, addr.wrapping_add(4), Size::L) as u64;
                    self.mmu[slot] = hi << 32 | lo;
                } else {
                    self.mmu[slot] = self.read(bus, addr, sz) as u64;
                }
            }
            // PFLUSH / PLOAD / PTEST: no translation cache to manage.
            _ => {
                if mode >= 2 {
                    let _ = self.ea(bus, mode, reg, Size::L);
                }
            }
        }
    }

    /// 68040 cache and MMU maintenance and MOVE16 (op $F4xx-$F6xx).
    pub(super) fn line_f_040<B: Bus>(&mut self, bus: &mut B, op: u16) {
        match op & 0xff00 {
            // CINV/CPUSH and PFLUSH/PTEST: caches and MMU are not modelled.
            0xf400 | 0xf500 => {
                if !self.s {
                    self.privilege_violation(bus);
                }
            }
            _ => {
                // MOVE16: copy one 16-byte line.
                let (src, dst) = if op & 0xfff8 == 0xf620 {
                    let ext = self.fetch16(bus);
                    let ax = (op & 7) as usize;
                    let ay = ((ext >> 12) & 7) as usize;
                    let (s, d) = (self.a[ax], self.a[ay]);
                    self.a[ax] = s.wrapping_add(16);
                    if ay != ax {
                        self.a[ay] = d.wrapping_add(16);
                    }
                    (s, d)
                } else {
                    let ay = (op & 7) as usize;
                    let abs = self.fetch32(bus);
                    let a = self.a[ay];
                    match (op >> 3) & 3 {
                        0 => {
                            self.a[ay] = a.wrapping_add(16);
                            (a, abs)
                        }
                        1 => {
                            self.a[ay] = a.wrapping_add(16);
                            (abs, a)
                        }
                        2 => (a, abs),
                        _ => (abs, a),
                    }
                };
                let (src, dst) = (src & !15, dst & !15);
                for i in 0..4 {
                    let v = self.read(bus, src + i * 4, Size::L);
                    self.write(bus, dst + i * 4, Size::L, v);
                }
            }
        }
    }

    /// MOVES: move to/from an address space; function codes are not modelled.
    pub(super) fn moves<B: Bus>(&mut self, bus: &mut B, op: u16) {
        if !self.s {
            return self.privilege_violation(bus);
        }
        let Some(sz) = Size::from_bits(op >> 6) else {
            return self.illegal(bus);
        };
        let ext = self.fetch16(bus);
        let r = ((ext >> 12) & 7) as usize;
        let ea = self.ea(bus, (op >> 3) & 7, op & 7, sz);
        if ext & 0x0800 != 0 {
            let v = if ext & 0x8000 != 0 { self.a[r] } else { self.d[r] };
            self.write_ea(bus, ea, sz, v);
        } else {
            let v = self.read_ea(bus, ea, sz);
            if ext & 0x8000 != 0 {
                self.a[r] = match sz {
                    Size::B => v as i8 as i32 as u32,
                    Size::W => v as i16 as i32 as u32,
                    Size::L => v,
                };
            } else {
                self.write_ea(bus, Ea::D(r), sz, v);
            }
        }
    }

    pub(super) fn trapcc<B: Bus>(&mut self, bus: &mut B, op: u16) {
        match op & 7 {
            2 => {
                self.fetch16(bus);
            }
            3 => {
                self.fetch32(bus);
            }
            _ => {}
        }
        if self.cond((op >> 8) & 15) {
            let pc = self.pc;
            self.exception(bus, VEC_TRAPV, pc);
        }
    }

    // ---- arithmetic -------------------------------------------------------------

    pub(super) fn chk_l<B: Bus>(&mut self, bus: &mut B, op: u16) {
        let ea = self.ea(bus, (op >> 3) & 7, op & 7, Size::L);
        let bound = self.read_ea(bus, ea, Size::L) as i32;
        let dn = self.d[((op >> 9) & 7) as usize] as i32;
        self.z = dn == 0;
        self.v = false;
        self.c = false;
        if dn < 0 || dn > bound {
            self.n = dn < 0;
            let pc = self.pc;
            self.exception(bus, VEC_CHK, pc);
        }
    }

    /// CMP2 / CHK2 (op 0000 0ss0 11 <ea>).
    pub(super) fn cmp2<B: Bus>(&mut self, bus: &mut B, op: u16) {
        let sz = match (op >> 9) & 3 {
            0 => Size::B,
            1 => Size::W,
            _ => Size::L,
        };
        let ext = self.fetch16(bus);
        let Ea::M(addr) = self.ea(bus, (op >> 3) & 7, op & 7, sz) else {
            return self.illegal(bus);
        };
        let lo = self.read(bus, addr, sz);
        let hi = self.read(bus, addr.wrapping_add(sz.bytes()), sz);
        let r = ((ext >> 12) & 7) as usize;
        let sext = |v: u32| match sz {
            Size::B => v as i8 as i32 as u32,
            Size::W => v as i16 as i32 as u32,
            Size::L => v,
        };
        // Address registers compare against sign-extended bounds as longs.
        let (lo, hi, v) = if ext & 0x8000 != 0 {
            (sext(lo), sext(hi), self.a[r])
        } else {
            (lo, hi, self.d[r] & sz.mask())
        };
        self.z = v == lo || v == hi;
        self.c = if lo <= hi { v < lo || v > hi } else { v > hi && v < lo };
        if self.c && ext & 0x0800 != 0 {
            let pc = self.pc;
            self.exception(bus, VEC_CHK, pc);
        }
    }

    /// MULU.L / MULS.L (op 0100 1100 00 <ea>).
    pub(super) fn mul_l<B: Bus>(&mut self, bus: &mut B, op: u16) {
        let ext = self.fetch16(bus);
        let ea = self.ea(bus, (op >> 3) & 7, op & 7, Size::L);
        let src = self.read_ea(bus, ea, Size::L);
        let dl = ((ext >> 12) & 7) as usize;
        let dh = (ext & 7) as usize;
        let signed = ext & 0x0800 != 0;
        let wide = ext & 0x0400 != 0;
        let r: u64 = if signed {
            (self.d[dl] as i32 as i64).wrapping_mul(src as i32 as i64) as u64
        } else {
            self.d[dl] as u64 * src as u64
        };
        self.c = false;
        if wide {
            self.d[dh] = (r >> 32) as u32;
            self.d[dl] = r as u32;
            self.n = r >> 63 != 0;
            self.z = r == 0;
            self.v = false;
        } else {
            self.d[dl] = r as u32;
            self.n = r & 0x8000_0000 != 0;
            self.z = r as u32 == 0;
            self.v = if signed { r as i64 != r as u32 as i32 as i64 } else { r >> 32 != 0 };
        }
        bus.idle(40);
    }

    /// DIVU.L / DIVS.L (op 0100 1100 01 <ea>).
    pub(super) fn div_l<B: Bus>(&mut self, bus: &mut B, op: u16) {
        let ext = self.fetch16(bus);
        let ea = self.ea(bus, (op >> 3) & 7, op & 7, Size::L);
        let src = self.read_ea(bus, ea, Size::L);
        let dq = ((ext >> 12) & 7) as usize;
        let dr = (ext & 7) as usize;
        let signed = ext & 0x0800 != 0;
        let wide = ext & 0x0400 != 0;
        if src == 0 {
            self.c = false;
            let pc = self.pc;
            return self.exception(bus, VEC_ZERO_DIVIDE, pc);
        }
        self.c = false;
        bus.idle(70);
        if signed {
            let dividend: i64 = if wide { ((self.d[dr] as u64) << 32 | self.d[dq] as u64) as i64 } else { self.d[dq] as i32 as i64 };
            let divisor = src as i32 as i64;
            if dividend == i64::MIN && divisor == -1 {
                self.v = true;
                return;
            }
            let q = dividend / divisor;
            let r = dividend % divisor;
            if q < i32::MIN as i64 || q > i32::MAX as i64 {
                self.v = true;
                return;
            }
            self.v = false;
            self.n = q < 0;
            self.z = q == 0;
            if dr != dq {
                self.d[dr] = r as u32;
            }
            self.d[dq] = q as u32;
        } else {
            let dividend: u64 = if wide { (self.d[dr] as u64) << 32 | self.d[dq] as u64 } else { self.d[dq] as u64 };
            let q = dividend / src as u64;
            let r = dividend % src as u64;
            if q > u32::MAX as u64 {
                self.v = true;
                return;
            }
            self.v = false;
            self.n = q & 0x8000_0000 != 0;
            self.z = q == 0;
            if dr != dq {
                self.d[dr] = r as u32;
            }
            self.d[dq] = q as u32;
        }
    }

    /// CAS Dc,Du,<ea>.
    pub(super) fn cas<B: Bus>(&mut self, bus: &mut B, op: u16) {
        let sz = match (op >> 9) & 3 {
            1 => Size::B,
            2 => Size::W,
            _ => Size::L,
        };
        let ext = self.fetch16(bus);
        let ea = self.ea(bus, (op >> 3) & 7, op & 7, sz);
        let v = self.read_ea(bus, ea, sz);
        let dc = (ext & 7) as usize;
        let du = ((ext >> 6) & 7) as usize;
        self.sub(self.d[dc], v, sz, false, true);
        if self.z {
            let u = self.d[du];
            self.write_ea(bus, ea, sz, u);
        } else {
            self.write_ea(bus, Ea::D(dc), sz, v);
        }
    }

    /// CAS2 Dc1:Dc2,Du1:Du2,(Rn1):(Rn2).
    pub(super) fn cas2<B: Bus>(&mut self, bus: &mut B, op: u16) {
        let sz = if op & 0x0200 != 0 { Size::L } else { Size::W };
        let e1 = self.fetch16(bus);
        let e2 = self.fetch16(bus);
        let reg = |c: &Cpu, e: u16| {
            let r = ((e >> 12) & 7) as usize;
            if e & 0x8000 != 0 { c.a[r] } else { c.d[r] }
        };
        let (a1, a2) = (reg(self, e1), reg(self, e2));
        let v1 = self.read(bus, a1, sz);
        let v2 = self.read(bus, a2, sz);
        let (dc1, dc2) = ((e1 & 7) as usize, (e2 & 7) as usize);
        self.sub(self.d[dc1], v1, sz, false, true);
        if self.z {
            self.sub(self.d[dc2], v2, sz, false, true);
        }
        if self.z {
            let (u1, u2) = (self.d[((e1 >> 6) & 7) as usize], self.d[((e2 >> 6) & 7) as usize]);
            self.write(bus, a1, sz, u1);
            self.write(bus, a2, sz, u2);
        } else {
            self.write_ea(bus, Ea::D(dc1), sz, v1);
            self.write_ea(bus, Ea::D(dc2), sz, v2);
        }
    }

    /// PACK (unpack = false) or UNPK.
    pub(super) fn pack<B: Bus>(&mut self, bus: &mut B, op: u16, unpack: bool) {
        let adj = self.fetch16(bus);
        let rx = (op & 7) as usize;
        let ry = ((op >> 9) & 7) as usize;
        let mem = op & 8 != 0;
        let dec = |r: usize| if r == 7 { 2 } else { 1 };
        if !unpack {
            let src = if mem {
                self.a[rx] = self.a[rx].wrapping_sub(dec(rx));
                let lo = self.bus_read8(bus, self.a[rx]) as u16;
                self.a[rx] = self.a[rx].wrapping_sub(dec(rx));
                let hi = self.bus_read8(bus, self.a[rx]) as u16;
                hi << 8 | lo
            } else {
                self.d[rx] as u16
            };
            let v = src.wrapping_add(adj);
            let b = ((v >> 4) & 0xf0 | v & 0x0f) as u8;
            if mem {
                self.a[ry] = self.a[ry].wrapping_sub(dec(ry));
                self.bus_write8(bus, self.a[ry], b);
            } else {
                self.d[ry] = (self.d[ry] & !0xff) | b as u32;
            }
        } else {
            let src = if mem {
                self.a[rx] = self.a[rx].wrapping_sub(dec(rx));
                self.bus_read8(bus, self.a[rx]) as u16
            } else {
                self.d[rx] as u16 & 0xff
            };
            let v = ((src & 0xf0) << 4 | src & 0x0f).wrapping_add(adj);
            if mem {
                self.a[ry] = self.a[ry].wrapping_sub(dec(ry));
                self.bus_write8(bus, self.a[ry], v as u8);
                self.a[ry] = self.a[ry].wrapping_sub(dec(ry));
                self.bus_write8(bus, self.a[ry], (v >> 8) as u8);
            } else {
                self.d[ry] = (self.d[ry] & !0xffff) | v as u32;
            }
        }
    }

    // ---- bit fields -------------------------------------------------------------

    /// BFTST/BFEXTU/BFCHG/BFEXTS/BFCLR/BFFFO/BFSET/BFINS (op 1110 1kkk 11 <ea>).
    pub(super) fn bitfield<B: Bus>(&mut self, bus: &mut B, op: u16) {
        let kind = (op >> 8) & 7;
        let ext = self.fetch16(bus);
        let offset: i32 = if ext & 0x0800 != 0 { self.d[((ext >> 6) & 7) as usize] as i32 } else { ((ext >> 6) & 31) as i32 };
        let width: u32 = {
            let w = if ext & 0x20 != 0 { self.d[(ext & 7) as usize] & 31 } else { (ext & 31) as u32 };
            if w == 0 { 32 } else { w }
        };
        let dn = ((ext >> 12) & 7) as usize;
        let fmask: u32 = if width == 32 { !0 } else { (1u32 << width) - 1 };
        let ea = self.ea(bus, (op >> 3) & 7, op & 7, Size::B);

        // Fetch the field: `field` holds it right-aligned.
        let field;
        enum Loc {
            Reg(usize, u32),
            Mem(u32, u32, u32),
        }
        let loc = match ea {
            Ea::D(r) => {
                let off = (offset as u32) & 31;
                let rot = self.d[r].rotate_left(off);
                field = rot >> (32 - width) & fmask;
                Loc::Reg(r, off)
            }
            Ea::M(base) => {
                let addr = base.wrapping_add((offset >> 3) as u32);
                let bitoff = (offset & 7) as u32;
                let nbytes = (bitoff + width).div_ceil(8);
                let mut raw: u64 = 0;
                for i in 0..nbytes {
                    raw = raw << 8 | self.bus_read8(bus, addr.wrapping_add(i)) as u64;
                }
                let shift = nbytes * 8 - bitoff - width;
                field = (raw >> shift) as u32 & fmask;
                Loc::Mem(addr, nbytes, shift)
            }
            _ => return self.illegal(bus),
        };
        let msb = 1u32 << (width - 1);
        self.n = field & msb != 0;
        self.z = field == 0;
        self.v = false;
        self.c = false;

        let new = match kind {
            0 => None,                                // BFTST
            1 => {
                self.d[dn] = field;                   // BFEXTU
                None
            }
            2 => Some(!field & fmask),                // BFCHG
            3 => {
                // BFEXTS
                self.d[dn] = if self.n { field | !fmask } else { field };
                None
            }
            4 => Some(0),                             // BFCLR
            5 => {
                // BFFFO
                let lz = if field == 0 { width } else { field.leading_zeros() - (32 - width) };
                self.d[dn] = (offset as u32).wrapping_add(lz);
                None
            }
            6 => Some(fmask),                         // BFSET
            _ => {
                // BFINS: flags come from the inserted value.
                let v = self.d[dn] & fmask;
                self.n = v & msb != 0;
                self.z = v == 0;
                Some(v)
            }
        };
        let Some(new) = new else { return };
        match loc {
            Loc::Reg(r, off) => {
                let m = fmask << (32 - width);
                let rot = self.d[r].rotate_left(off);
                let rot = (rot & !m) | (new << (32 - width) & m);
                self.d[r] = rot.rotate_right(off);
            }
            Loc::Mem(addr, nbytes, shift) => {
                let mut raw: u64 = 0;
                for i in 0..nbytes {
                    raw = raw << 8 | self.bus_read8(bus, addr.wrapping_add(i)) as u64;
                }
                let m = (fmask as u64) << shift;
                raw = (raw & !m) | ((new as u64) << shift & m);
                for i in 0..nbytes {
                    self.bus_write8(bus, addr.wrapping_add(i), (raw >> ((nbytes - 1 - i) * 8)) as u8);
                }
            }
        }
    }
}
