//! 68000 instruction decode and execution.

use super::*;

impl Cpu {
    pub(super) fn execute<B: Bus>(&mut self, bus: &mut B, op: u16) {
        match op >> 12 {
            0x0 => self.line0(bus, op),
            0x1 => self.op_move(bus, op, Size::B),
            0x2 => self.op_move(bus, op, Size::L),
            0x3 => self.op_move(bus, op, Size::W),
            0x4 => self.line4(bus, op),
            0x5 => self.line5(bus, op),
            0x6 => self.op_bcc(bus, op),
            0x7 => {
                if op & 0x0100 != 0 {
                    return self.illegal(bus);
                }
                let v = op as i8 as i32 as u32;
                self.d[((op >> 9) & 7) as usize] = v;
                self.logic_flags(v, Size::L);
            }
            0x8 => self.line8(bus, op),
            0x9 => self.addsub(bus, op, false),
            0xa => {
                let pc = self.instr_pc;
                self.exception(bus, VEC_LINE_A, pc);
            }
            0xb => self.lineb(bus, op),
            0xc => self.linec(bus, op),
            0xd => self.addsub(bus, op, true),
            0xe => self.linee(bus, op),
            _ => {
                let pc = self.instr_pc;
                self.exception(bus, VEC_LINE_F, pc);
            }
        }
    }

    // ---- line 0: immediate ops, bit ops, MOVEP -----------------------------

    fn line0<B: Bus>(&mut self, bus: &mut B, op: u16) {
        let mode = (op >> 3) & 7;
        let reg = op & 7;
        if op & 0x0100 != 0 {
            if mode == 1 {
                return self.op_movep(bus, op);
            }
            let bit = self.d[((op >> 9) & 7) as usize];
            return self.bitop(bus, op, bit);
        }
        if op & 0x0f00 == 0x0800 {
            let bit = (self.fetch16(bus) & 0xff) as u32;
            return self.bitop(bus, op, bit);
        }
        let kind = (op >> 9) & 7;
        // CCR / SR forms of ORI, ANDI, EORI.
        if op & 0x00ff == 0x003c && matches!(kind, 0 | 1 | 5) {
            let imm = self.fetch16(bus) as u8;
            let ccr = self.ccr();
            let v = match kind {
                0 => ccr | imm,
                1 => ccr & imm,
                _ => ccr ^ imm,
            };
            self.set_ccr(v);
            bus.idle(8);
            return;
        }
        if op & 0x00ff == 0x007c && matches!(kind, 0 | 1 | 5) {
            if !self.s {
                return self.privilege_violation(bus);
            }
            let imm = self.fetch16(bus);
            let sr = self.sr();
            let v = match kind {
                0 => sr | imm,
                1 => sr & imm,
                _ => sr ^ imm,
            };
            self.set_sr(v);
            bus.idle(8);
            return;
        }
        let Some(sz) = Size::from_bits(op >> 6) else {
            return self.illegal(bus);
        };
        if kind == 7 {
            return self.illegal(bus);
        }
        let imm = match sz {
            Size::B => (self.fetch16(bus) & 0xff) as u32,
            Size::W => self.fetch16(bus) as u32,
            Size::L => self.fetch32(bus),
        };
        let ea = self.ea(bus, mode, reg, sz);
        let dst = self.read_ea(bus, ea, sz);
        let is_dreg = mode == 0;
        match kind {
            0 => {
                let r = dst | imm;
                self.logic_flags(r, sz);
                self.write_ea(bus, ea, sz, r);
            }
            1 => {
                let r = dst & imm;
                self.logic_flags(r, sz);
                self.write_ea(bus, ea, sz, r);
            }
            2 => {
                let r = self.sub(imm, dst, sz, false, false);
                self.write_ea(bus, ea, sz, r);
            }
            3 => {
                let r = self.add(imm, dst, sz, false);
                self.write_ea(bus, ea, sz, r);
            }
            5 => {
                let r = dst ^ imm;
                self.logic_flags(r, sz);
                self.write_ea(bus, ea, sz, r);
            }
            6 => {
                self.sub(imm, dst, sz, false, true);
                if is_dreg && sz == Size::L {
                    bus.idle(2);
                }
                return;
            }
            _ => return self.illegal(bus),
        }
        if is_dreg && sz == Size::L {
            bus.idle(4);
        }
    }

    fn bitop<B: Bus>(&mut self, bus: &mut B, op: u16, bit: u32) {
        let mode = (op >> 3) & 7;
        let reg = op & 7;
        let kind = (op >> 6) & 3;
        if mode == 0 {
            let r = reg as usize;
            let mask = 1u32 << (bit & 31);
            self.z = self.d[r] & mask == 0;
            match kind {
                0 => {
                    bus.idle(2);
                    return;
                }
                1 => self.d[r] ^= mask,
                2 => self.d[r] &= !mask,
                _ => self.d[r] |= mask,
            }
            bus.idle(if kind == 2 { 6 } else { 4 });
            return;
        }
        let ea = self.ea(bus, mode, reg, Size::B);
        let v = self.read_ea(bus, ea, Size::B);
        let mask = 1u32 << (bit & 7);
        self.z = v & mask == 0;
        let r = match kind {
            0 => return,
            1 => v ^ mask,
            2 => v & !mask,
            _ => v | mask,
        };
        self.write_ea(bus, ea, Size::B, r);
    }

    fn op_movep<B: Bus>(&mut self, bus: &mut B, op: u16) {
        let dr = ((op >> 9) & 7) as usize;
        let ar = (op & 7) as usize;
        let disp = self.fetch16(bus) as i16 as i32 as u32;
        let mut addr = self.a[ar].wrapping_add(disp);
        let long = op & 0x40 != 0;
        let to_mem = op & 0x80 != 0;
        let n = if long { 4 } else { 2 };
        if to_mem {
            let v = self.d[dr];
            for i in (0..n).rev() {
                bus.write8(addr, (v >> (i * 8)) as u8);
                addr = addr.wrapping_add(2);
            }
        } else {
            let mut v = 0u32;
            for _ in 0..n {
                v = v << 8 | bus.read8(addr) as u32;
                addr = addr.wrapping_add(2);
            }
            if long {
                self.d[dr] = v;
            } else {
                self.d[dr] = (self.d[dr] & 0xffff_0000) | v;
            }
        }
    }

    // ---- MOVE / MOVEA -------------------------------------------------------

    fn op_move<B: Bus>(&mut self, bus: &mut B, op: u16, sz: Size) {
        let src_ea = self.ea(bus, (op >> 3) & 7, op & 7, sz);
        let v = self.read_ea(bus, src_ea, sz);
        let dmode = (op >> 6) & 7;
        let dreg = (op >> 9) & 7;
        if dmode == 1 {
            if sz == Size::B {
                return self.illegal(bus);
            }
            self.a[dreg as usize] = if sz == Size::W { v as i16 as i32 as u32 } else { v };
            return;
        }
        // -(An) destination does not incur the extra two cycles for MOVE.
        let dst_ea = if dmode == 4 {
            let r = dreg as usize;
            let dec = if r == 7 && sz == Size::B { 2 } else { sz.bytes() };
            self.a[r] = self.a[r].wrapping_sub(dec);
            Ea::M(self.a[r])
        } else {
            self.ea(bus, dmode, dreg, sz)
        };
        self.logic_flags(v, sz);
        self.write_ea(bus, dst_ea, sz, v);
    }

    // ---- line 4: miscellaneous ---------------------------------------------

    fn line4<B: Bus>(&mut self, bus: &mut B, op: u16) {
        let mode = (op >> 3) & 7;
        let reg = op & 7;
        if op & 0x01c0 == 0x01c0 {
            // LEA
            let ea = self.ea(bus, mode, reg, Size::L);
            if let Ea::M(addr) = ea {
                self.a[((op >> 9) & 7) as usize] = addr;
            } else {
                return self.illegal(bus);
            }
            return;
        }
        if op & 0x01c0 == 0x0180 {
            // CHK.W
            let ea = self.ea(bus, mode, reg, Size::W);
            let bound = self.read_ea(bus, ea, Size::W) as i16;
            let dn = self.d[((op >> 9) & 7) as usize] as i16;
            bus.idle(6);
            self.z = dn == 0;
            self.n = false;
            self.v = false;
            self.c = false;
            if dn < 0 {
                self.n = true;
                let pc = self.pc;
                self.exception(bus, VEC_CHK, pc);
            } else if dn > bound {
                self.n = false;
                let pc = self.pc;
                self.exception(bus, VEC_CHK, pc);
            }
            return;
        }
        match op & 0x0f00 {
            0x0000 => {
                if op & 0xc0 == 0xc0 {
                    // MOVE from SR (not privileged on the 68000)
                    let ea = self.ea(bus, mode, reg, Size::W);
                    let _ = if mode != 0 { self.read_ea(bus, ea, Size::W) } else { 0 };
                    let sr = self.sr() as u32;
                    self.write_ea(bus, ea, Size::W, sr);
                    if mode == 0 {
                        bus.idle(2);
                    }
                } else {
                    // NEGX
                    let sz = Size::from_bits(op >> 6).unwrap();
                    let ea = self.ea(bus, mode, reg, sz);
                    let v = self.read_ea(bus, ea, sz);
                    let r = self.sub(v, 0, sz, true, false);
                    self.write_ea(bus, ea, sz, r);
                    if mode == 0 && sz == Size::L {
                        bus.idle(2);
                    }
                }
            }
            0x0200 => {
                if op & 0xc0 == 0xc0 {
                    return self.illegal(bus);
                }
                // CLR (the 68000 performs a dummy read first)
                let sz = Size::from_bits(op >> 6).unwrap();
                let ea = self.ea(bus, mode, reg, sz);
                if let Ea::M(_) = ea {
                    let _ = self.read_ea(bus, ea, sz);
                }
                self.write_ea(bus, ea, sz, 0);
                self.n = false;
                self.z = true;
                self.v = false;
                self.c = false;
                if mode == 0 && sz == Size::L {
                    bus.idle(2);
                }
            }
            0x0400 => {
                if op & 0xc0 == 0xc0 {
                    // MOVE to CCR
                    let ea = self.ea(bus, mode, reg, Size::W);
                    let v = self.read_ea(bus, ea, Size::W);
                    self.set_ccr(v as u8);
                    bus.idle(8);
                } else {
                    // NEG
                    let sz = Size::from_bits(op >> 6).unwrap();
                    let ea = self.ea(bus, mode, reg, sz);
                    let v = self.read_ea(bus, ea, sz);
                    let r = self.sub(v, 0, sz, false, false);
                    self.write_ea(bus, ea, sz, r);
                    if mode == 0 && sz == Size::L {
                        bus.idle(2);
                    }
                }
            }
            0x0600 => {
                if op & 0xc0 == 0xc0 {
                    // MOVE to SR
                    if !self.s {
                        return self.privilege_violation(bus);
                    }
                    let ea = self.ea(bus, mode, reg, Size::W);
                    let v = self.read_ea(bus, ea, Size::W);
                    self.set_sr(v as u16);
                    bus.idle(8);
                } else {
                    // NOT
                    let sz = Size::from_bits(op >> 6).unwrap();
                    let ea = self.ea(bus, mode, reg, sz);
                    let v = !self.read_ea(bus, ea, sz) & sz.mask();
                    self.logic_flags(v, sz);
                    self.write_ea(bus, ea, sz, v);
                    if mode == 0 && sz == Size::L {
                        bus.idle(2);
                    }
                }
            }
            0x0800 => match (op >> 6) & 3 {
                0 => {
                    // NBCD
                    let ea = self.ea(bus, mode, reg, Size::B);
                    let dst = self.read_ea(bus, ea, Size::B);
                    let r = self.bcd_sub(0, dst);
                    self.write_ea(bus, ea, Size::B, r);
                    if mode == 0 {
                        bus.idle(2);
                    }
                }
                1 => {
                    if mode == 0 {
                        // SWAP
                        let r = reg as usize;
                        let v = self.d[r].rotate_left(16);
                        self.d[r] = v;
                        self.logic_flags(v, Size::L);
                    } else {
                        // PEA
                        let ea = self.ea(bus, mode, reg, Size::L);
                        if let Ea::M(addr) = ea {
                            self.push32(bus, addr);
                        } else {
                            return self.illegal(bus);
                        }
                    }
                }
                s => {
                    if mode == 0 {
                        // EXT
                        let r = reg as usize;
                        if s == 2 {
                            let v = self.d[r] as i8 as i16 as u16 as u32;
                            self.d[r] = (self.d[r] & 0xffff_0000) | v;
                            self.logic_flags(v, Size::W);
                        } else {
                            let v = self.d[r] as i16 as i32 as u32;
                            self.d[r] = v;
                            self.logic_flags(v, Size::L);
                        }
                    } else {
                        self.movem_to_mem(bus, op);
                    }
                }
            },
            0x0a00 => {
                if op == 0x4afc {
                    return self.illegal(bus);
                }
                if op & 0xc0 == 0xc0 {
                    // TAS
                    let ea = self.ea(bus, mode, reg, Size::B);
                    let v = self.read_ea(bus, ea, Size::B);
                    self.logic_flags(v, Size::B);
                    self.write_ea(bus, ea, Size::B, v | 0x80);
                    if mode != 0 {
                        bus.idle(2);
                    }
                } else {
                    // TST
                    let sz = Size::from_bits(op >> 6).unwrap();
                    let ea = self.ea(bus, mode, reg, sz);
                    let v = self.read_ea(bus, ea, sz);
                    self.logic_flags(v, sz);
                }
            }
            0x0c00 => {
                if op & 0x80 != 0 {
                    self.movem_to_reg(bus, op);
                } else {
                    self.illegal(bus);
                }
            }
            0x0e00 => self.line4e(bus, op),
            _ => self.illegal(bus),
        }
    }

    fn line4e<B: Bus>(&mut self, bus: &mut B, op: u16) {
        let mode = (op >> 3) & 7;
        let reg = op & 7;
        match op & 0xffc0 {
            0x4e80 | 0x4ec0 => {
                // JSR / JMP
                let ea = self.ea(bus, mode, reg, Size::L);
                let Ea::M(target) = ea else {
                    return self.illegal(bus);
                };
                if op & 0x40 == 0 {
                    let pc = self.pc;
                    self.push32(bus, pc);
                }
                self.pc = target;
                return;
            }
            0x4e40 => {}
            _ => return self.illegal(bus),
        }
        match op {
            0x4e40..=0x4e4f => {
                let pc = self.pc;
                self.exception(bus, VEC_TRAP_BASE + (op & 15) as u32, pc);
            }
            0x4e50..=0x4e57 => {
                // LINK
                let r = (op & 7) as usize;
                let disp = self.fetch16(bus) as i16 as i32 as u32;
                let v = self.a[r];
                self.push32(bus, v);
                self.a[r] = self.a[7];
                self.a[7] = self.a[7].wrapping_add(disp);
            }
            0x4e58..=0x4e5f => {
                // UNLK
                let r = (op & 7) as usize;
                self.a[7] = self.a[r];
                let v = self.pop32(bus);
                self.a[r] = v;
            }
            0x4e60..=0x4e67 => {
                if !self.s {
                    return self.privilege_violation(bus);
                }
                self.other_sp = self.a[(op & 7) as usize];
            }
            0x4e68..=0x4e6f => {
                if !self.s {
                    return self.privilege_violation(bus);
                }
                self.a[(op & 7) as usize] = self.other_sp;
            }
            0x4e70 => {
                if !self.s {
                    return self.privilege_violation(bus);
                }
                bus.reset_devices();
                bus.idle(128);
            }
            0x4e71 => {}
            0x4e72 => {
                if !self.s {
                    return self.privilege_violation(bus);
                }
                let v = self.fetch16(bus);
                self.set_sr(v);
                self.stopped = true;
            }
            0x4e73 => {
                if !self.s {
                    return self.privilege_violation(bus);
                }
                let sr = self.pop16(bus);
                let pc = self.pop32(bus);
                self.set_sr(sr);
                self.pc = pc;
            }
            0x4e75 => self.pc = self.pop32(bus),
            0x4e76 => {
                if self.v {
                    let pc = self.pc;
                    self.exception(bus, VEC_TRAPV, pc);
                }
            }
            0x4e77 => {
                let ccr = self.pop16(bus);
                self.set_ccr(ccr as u8);
                self.pc = self.pop32(bus);
            }
            _ => self.illegal(bus),
        }
    }

    fn movem_to_mem<B: Bus>(&mut self, bus: &mut B, op: u16) {
        let mask = self.fetch16(bus);
        let sz = if op & 0x40 != 0 { Size::L } else { Size::W };
        let mode = (op >> 3) & 7;
        let reg = (op & 7) as usize;
        if mode == 4 {
            // Predecrement: mask bit 0 = A7 ... bit 15 = D0.
            let mut addr = self.a[reg];
            let initial = self.a[reg];
            for i in 0..16 {
                if mask & (1 << i) != 0 {
                    let r = 15 - i;
                    let v = if r >= 8 {
                        if r - 8 == reg {
                            initial
                        } else {
                            self.a[r - 8]
                        }
                    } else {
                        self.d[r]
                    };
                    addr = addr.wrapping_sub(sz.bytes());
                    if sz == Size::L {
                        bus.write16(addr.wrapping_add(2), v as u16);
                        bus.write16(addr, (v >> 16) as u16);
                    } else {
                        bus.write16(addr, v as u16);
                    }
                }
            }
            self.a[reg] = addr;
            return;
        }
        let ea = self.ea(bus, mode, reg as u16, sz);
        let Ea::M(mut addr) = ea else {
            return self.illegal(bus);
        };
        for i in 0..16 {
            if mask & (1 << i) != 0 {
                let v = if i >= 8 { self.a[i - 8] } else { self.d[i] };
                self.write(bus, addr, sz, v);
                addr = addr.wrapping_add(sz.bytes());
            }
        }
    }

    fn movem_to_reg<B: Bus>(&mut self, bus: &mut B, op: u16) {
        let mask = self.fetch16(bus);
        let sz = if op & 0x40 != 0 { Size::L } else { Size::W };
        let mode = (op >> 3) & 7;
        let reg = (op & 7) as usize;
        let mut addr = if mode == 3 {
            self.a[reg]
        } else {
            match self.ea(bus, mode, reg as u16, sz) {
                Ea::M(a) => a,
                _ => return self.illegal(bus),
            }
        };
        for i in 0..16 {
            if mask & (1 << i) != 0 {
                let v = self.read(bus, addr, sz);
                let v = if sz == Size::W { v as i16 as i32 as u32 } else { v };
                if i >= 8 {
                    self.a[i - 8] = v;
                } else {
                    self.d[i] = v;
                }
                addr = addr.wrapping_add(sz.bytes());
            }
        }
        // The 68000 performs one extra word read past the end of the list.
        let _ = bus.read16(addr);
        if mode == 3 {
            self.a[reg] = addr;
        }
    }

    // ---- line 5: ADDQ, SUBQ, Scc, DBcc --------------------------------------

    fn line5<B: Bus>(&mut self, bus: &mut B, op: u16) {
        let mode = (op >> 3) & 7;
        let reg = op & 7;
        if op & 0xc0 == 0xc0 {
            let cc = (op >> 8) & 15;
            if mode == 1 {
                // DBcc
                let disp = self.fetch16(bus) as i16 as i32 as u32;
                if self.cond(cc) {
                    bus.idle(4);
                    return;
                }
                let r = reg as usize;
                let cnt = (self.d[r] as u16).wrapping_sub(1);
                self.d[r] = (self.d[r] & 0xffff_0000) | cnt as u32;
                if cnt != 0xffff {
                    self.pc = self.instr_pc.wrapping_add(2).wrapping_add(disp);
                } else {
                    bus.idle(6);
                }
                return;
            }
            // Scc
            let ea = self.ea(bus, mode, reg, Size::B);
            if let Ea::M(_) = ea {
                let _ = self.read_ea(bus, ea, Size::B);
            }
            let t = self.cond(cc);
            self.write_ea(bus, ea, Size::B, if t { 0xff } else { 0 });
            if mode == 0 && t {
                bus.idle(2);
            }
            return;
        }
        let sz = Size::from_bits(op >> 6).unwrap();
        let q = match (op >> 9) & 7 {
            0 => 8,
            n => n as u32,
        };
        let sub = op & 0x100 != 0;
        if mode == 1 {
            let r = reg as usize;
            self.a[r] = if sub { self.a[r].wrapping_sub(q) } else { self.a[r].wrapping_add(q) };
            bus.idle(4);
            return;
        }
        let ea = self.ea(bus, mode, reg, sz);
        let v = self.read_ea(bus, ea, sz);
        let r = if sub { self.sub(q, v, sz, false, false) } else { self.add(q, v, sz, false) };
        self.write_ea(bus, ea, sz, r);
        if mode == 0 && sz == Size::L {
            bus.idle(4);
        }
    }

    // ---- line 6: Bcc / BRA / BSR --------------------------------------------

    fn op_bcc<B: Bus>(&mut self, bus: &mut B, op: u16) {
        let base = self.pc;
        let mut disp = op as i8 as i32 as u32;
        let word = disp == 0;
        if word {
            disp = self.fetch16(bus) as i16 as i32 as u32;
        }
        let target = base.wrapping_add(disp);
        let cc = (op >> 8) & 15;
        if cc == 1 {
            // BSR
            let ret = self.pc;
            self.push32(bus, ret);
            self.pc = target;
            // Refilling the prefetch queue accounts for the branch time.
            return;
        }
        if self.cond(cc) {
            self.pc = target;
            // Refilling the prefetch queue accounts for the branch time.
        } else {
            bus.idle(4);
        }
    }

    // ---- line 8: OR, DIVU, DIVS, SBCD ----------------------------------------

    fn line8<B: Bus>(&mut self, bus: &mut B, op: u16) {
        let mode = (op >> 3) & 7;
        let reg = op & 7;
        let dn = ((op >> 9) & 7) as usize;
        match (op >> 6) & 7 {
            3 => return self.divu(bus, op),
            7 => return self.divs(bus, op),
            4 if mode <= 1 => {
                // SBCD
                if mode == 0 {
                    let src = self.d[reg as usize] & 0xff;
                    let dst = self.d[dn] & 0xff;
                    let r = self.bcd_sub(dst, src);
                    self.d[dn] = (self.d[dn] & !0xff) | r;
                    bus.idle(2);
                } else {
                    let (src, dst, addr) = self.predec_pair(bus, reg as usize, dn, Size::B);
                    let r = self.bcd_sub(dst, src);
                    bus.write8(addr, r as u8);
                }
                return;
            }
            _ => {}
        }
        let sz = Size::from_bits(op >> 6).unwrap();
        let to_ea = op & 0x100 != 0;
        let ea = self.ea(bus, mode, reg, sz);
        let v = self.read_ea(bus, ea, sz);
        let r = (v | self.d[dn]) & sz.mask();
        self.logic_flags(r, sz);
        if to_ea {
            self.write_ea(bus, ea, sz, r);
        } else {
            self.write_ea(bus, Ea::D(dn), sz, r);
            if sz == Size::L {
                bus.idle(if matches!(ea, Ea::D(_) | Ea::Imm(_)) { 4 } else { 2 });
            }
        }
    }

    /// Fetch operands for -(Ay),-(Ax) forms. Returns (src, dst, dst address).
    fn predec_pair<B: Bus>(&mut self, bus: &mut B, ry: usize, rx: usize, sz: Size) -> (u32, u32, u32) {
        let dec = |r: usize| if r == 7 && sz == Size::B { 2 } else { sz.bytes() };
        bus.idle(2);
        self.a[ry] = self.a[ry].wrapping_sub(dec(ry));
        let src = self.read(bus, self.a[ry], sz);
        self.a[rx] = self.a[rx].wrapping_sub(dec(rx));
        let dst = self.read(bus, self.a[rx], sz);
        (src, dst, self.a[rx])
    }

    fn divu<B: Bus>(&mut self, bus: &mut B, op: u16) {
        let ea = self.ea(bus, (op >> 3) & 7, op & 7, Size::W);
        let src = self.read_ea(bus, ea, Size::W);
        let dn = ((op >> 9) & 7) as usize;
        if src == 0 {
            self.c = false;
            self.v = false;
            self.n = false;
            self.z = false;
            let pc = self.pc;
            return self.exception(bus, VEC_ZERO_DIVIDE, pc);
        }
        let dst = self.d[dn];
        let q = dst / src;
        let r = dst % src;
        self.c = false;
        if q > 0xffff {
            self.v = true;
            self.n = true;
            self.z = false;
            bus.idle(6);
            return;
        }
        self.v = false;
        self.n = q & 0x8000 != 0;
        self.z = q == 0;
        self.d[dn] = r << 16 | q;
        bus.idle(136);
    }

    fn divs<B: Bus>(&mut self, bus: &mut B, op: u16) {
        let ea = self.ea(bus, (op >> 3) & 7, op & 7, Size::W);
        let src = self.read_ea(bus, ea, Size::W) as i16 as i32;
        let dn = ((op >> 9) & 7) as usize;
        if src == 0 {
            self.c = false;
            self.v = false;
            self.n = false;
            self.z = false;
            let pc = self.pc;
            return self.exception(bus, VEC_ZERO_DIVIDE, pc);
        }
        let dst = self.d[dn] as i32;
        self.c = false;
        if dst == i32::MIN && src == -1 {
            self.v = true;
            self.n = false;
            self.z = true;
            bus.idle(14);
            return;
        }
        let q = dst / src;
        let r = dst % src;
        if q > i16::MAX as i32 || q < i16::MIN as i32 {
            self.v = true;
            self.n = true;
            self.z = false;
            bus.idle(14);
            return;
        }
        self.v = false;
        self.n = q < 0;
        self.z = q == 0;
        self.d[dn] = (r as u32) << 16 | (q as u32 & 0xffff);
        bus.idle(154);
    }

    // ---- line 9 / D: SUB, ADD and friends -----------------------------------

    fn addsub<B: Bus>(&mut self, bus: &mut B, op: u16, add: bool) {
        let mode = (op >> 3) & 7;
        let reg = op & 7;
        let dn = ((op >> 9) & 7) as usize;
        let opmode = (op >> 6) & 7;
        if opmode == 3 || opmode == 7 {
            // ADDA / SUBA
            let sz = if opmode == 7 { Size::L } else { Size::W };
            let ea = self.ea(bus, mode, reg, sz);
            let mut v = self.read_ea(bus, ea, sz);
            if sz == Size::W {
                v = v as i16 as i32 as u32;
            }
            self.a[dn] = if add { self.a[dn].wrapping_add(v) } else { self.a[dn].wrapping_sub(v) };
            bus.idle(if sz == Size::W || matches!(ea, Ea::D(_) | Ea::A(_) | Ea::Imm(_)) { 4 } else { 2 });
            return;
        }
        let sz = Size::from_bits(opmode).unwrap();
        if opmode >= 4 && mode <= 1 {
            // ADDX / SUBX
            if mode == 0 {
                let src = self.d[reg as usize];
                let dst = self.d[dn];
                let r = if add { self.add(src, dst, sz, true) } else { self.sub(src, dst, sz, true, false) };
                self.write_ea(bus, Ea::D(dn), sz, r);
                if sz == Size::L {
                    bus.idle(4);
                }
            } else {
                let (src, dst, addr) = self.predec_pair(bus, reg as usize, dn, sz);
                let r = if add { self.add(src, dst, sz, true) } else { self.sub(src, dst, sz, true, false) };
                if sz == Size::L {
                    bus.write16(addr.wrapping_add(2), r as u16);
                    bus.write16(addr, (r >> 16) as u16);
                } else {
                    self.write(bus, addr, sz, r);
                }
            }
            return;
        }
        let ea = self.ea(bus, mode, reg, sz);
        let v = self.read_ea(bus, ea, sz);
        if opmode >= 4 {
            // Dn op <ea> -> <ea>
            let r = if add { self.add(self.d[dn], v, sz, false) } else { self.sub(self.d[dn], v, sz, false, false) };
            self.write_ea(bus, ea, sz, r);
        } else {
            let r = if add { self.add(v, self.d[dn], sz, false) } else { self.sub(v, self.d[dn], sz, false, false) };
            self.write_ea(bus, Ea::D(dn), sz, r);
            if sz == Size::L {
                bus.idle(if matches!(ea, Ea::D(_) | Ea::A(_) | Ea::Imm(_)) { 4 } else { 2 });
            }
        }
    }

    // ---- line B: CMP, CMPA, CMPM, EOR ----------------------------------------

    fn lineb<B: Bus>(&mut self, bus: &mut B, op: u16) {
        let mode = (op >> 3) & 7;
        let reg = op & 7;
        let dn = ((op >> 9) & 7) as usize;
        let opmode = (op >> 6) & 7;
        if opmode == 3 || opmode == 7 {
            let sz = if opmode == 7 { Size::L } else { Size::W };
            let ea = self.ea(bus, mode, reg, sz);
            let mut v = self.read_ea(bus, ea, sz);
            if sz == Size::W {
                v = v as i16 as i32 as u32;
            }
            self.sub(v, self.a[dn], Size::L, false, true);
            bus.idle(2);
            return;
        }
        let sz = Size::from_bits(opmode).unwrap();
        if opmode < 4 {
            let ea = self.ea(bus, mode, reg, sz);
            let v = self.read_ea(bus, ea, sz);
            self.sub(v, self.d[dn], sz, false, true);
            if sz == Size::L {
                bus.idle(2);
            }
            return;
        }
        if mode == 1 {
            // CMPM (Ay)+,(Ax)+
            let ry = reg as usize;
            let inc = |r: usize| if r == 7 && sz == Size::B { 2 } else { sz.bytes() };
            let src = self.read(bus, self.a[ry], sz);
            self.a[ry] = self.a[ry].wrapping_add(inc(ry));
            let dst = self.read(bus, self.a[dn], sz);
            self.a[dn] = self.a[dn].wrapping_add(inc(dn));
            self.sub(src, dst, sz, false, true);
            return;
        }
        // EOR Dn,<ea>
        let ea = self.ea(bus, mode, reg, sz);
        let v = self.read_ea(bus, ea, sz);
        let r = (v ^ self.d[dn]) & sz.mask();
        self.logic_flags(r, sz);
        self.write_ea(bus, ea, sz, r);
        if mode == 0 && sz == Size::L {
            bus.idle(4);
        }
    }

    // ---- line C: AND, MULU, MULS, ABCD, EXG ------------------------------------

    fn linec<B: Bus>(&mut self, bus: &mut B, op: u16) {
        let mode = (op >> 3) & 7;
        let reg = op & 7;
        let dn = ((op >> 9) & 7) as usize;
        match (op >> 3) & 0x3f {
            0x28 => {
                self.d.swap(dn, reg as usize);
                bus.idle(2);
                return;
            }
            0x29 => {
                self.a.swap(dn, reg as usize);
                bus.idle(2);
                return;
            }
            0x31 => {
                std::mem::swap(&mut self.d[dn], &mut self.a[reg as usize]);
                bus.idle(2);
                return;
            }
            _ => {}
        }
        match (op >> 6) & 7 {
            3 => {
                // MULU
                let ea = self.ea(bus, mode, reg, Size::W);
                let src = self.read_ea(bus, ea, Size::W);
                let r = (self.d[dn] & 0xffff) * src;
                self.d[dn] = r;
                self.logic_flags(r, Size::L);
                bus.idle(34 + 2 * src.count_ones());
                return;
            }
            7 => {
                // MULS
                let ea = self.ea(bus, mode, reg, Size::W);
                let src = self.read_ea(bus, ea, Size::W);
                let r = (self.d[dn] as i16 as i32).wrapping_mul(src as i16 as i32) as u32;
                self.d[dn] = r;
                self.logic_flags(r, Size::L);
                let bits = ((src << 1) ^ src) & 0xffff;
                bus.idle(34 + 2 * bits.count_ones());
                return;
            }
            4 if mode <= 1 => {
                // ABCD
                if mode == 0 {
                    let src = self.d[reg as usize] & 0xff;
                    let dst = self.d[dn] & 0xff;
                    let r = self.bcd_add(dst, src);
                    self.d[dn] = (self.d[dn] & !0xff) | r;
                    bus.idle(2);
                } else {
                    let (src, dst, addr) = self.predec_pair(bus, reg as usize, dn, Size::B);
                    let r = self.bcd_add(dst, src);
                    bus.write8(addr, r as u8);
                }
                return;
            }
            _ => {}
        }
        let sz = Size::from_bits(op >> 6).unwrap();
        let to_ea = op & 0x100 != 0;
        let ea = self.ea(bus, mode, reg, sz);
        let v = self.read_ea(bus, ea, sz);
        let r = v & self.d[dn] & sz.mask();
        self.logic_flags(r, sz);
        if to_ea {
            self.write_ea(bus, ea, sz, r);
        } else {
            self.write_ea(bus, Ea::D(dn), sz, r);
            if sz == Size::L {
                bus.idle(if matches!(ea, Ea::D(_) | Ea::Imm(_)) { 4 } else { 2 });
            }
        }
    }

    fn bcd_add(&mut self, dst: u32, src: u32) -> u32 {
        let x = self.x as i32;
        let (d, s) = (dst as i32, src as i32);
        let unadjusted = d + s + x;
        let mut res = (d & 0xf) + (s & 0xf) + x;
        res += (d & 0xf0) + (s & 0xf0) + (((9 - res) >> 4) & 0x06);
        res += ((0x9f - res) >> 4) & 0x60;
        self.c = res & !0xff != 0;
        self.x = self.c;
        self.n = res & 0x80 != 0;
        self.v = !unadjusted & res & 0x80 != 0;
        let res = (res & 0xff) as u32;
        if res != 0 {
            self.z = false;
        }
        res
    }

    fn bcd_sub(&mut self, dst: u32, src: u32) -> u32 {
        let x = self.x as i32;
        let (d, s) = (dst as i32, src as i32);
        let unadjusted = d - s - x;
        let lo = (d & 0xf) - (s & 0xf) - x;
        let mut res = unadjusted;
        if lo < 0 {
            res -= 6;
        }
        if unadjusted < 0 {
            res -= 0x60;
        }
        self.c = unadjusted < 0 || res < 0;
        self.x = self.c;
        self.n = res & 0x80 != 0;
        self.v = unadjusted & !res & 0x80 != 0;
        let res = (res & 0xff) as u32;
        if res != 0 {
            self.z = false;
        }
        res
    }

    // ---- line E: shifts and rotates ----------------------------------------

    fn linee<B: Bus>(&mut self, bus: &mut B, op: u16) {
        if op & 0xc0 == 0xc0 {
            // Memory form: word, count 1. Kind in bits 10-9.
            let ea = self.ea(bus, (op >> 3) & 7, op & 7, Size::W);
            let v = self.read_ea(bus, ea, Size::W);
            let left = op & 0x100 != 0;
            let r = self.shift((op >> 9) & 3, left, v, 1, Size::W);
            self.write_ea(bus, ea, Size::W, r);
            return;
        }
        let sz = Size::from_bits(op >> 6).unwrap();
        let r = (op & 7) as usize;
        let cnt_field = ((op >> 9) & 7) as u32;
        let count = if op & 0x20 != 0 {
            self.d[cnt_field as usize] & 63
        } else if cnt_field == 0 {
            8
        } else {
            cnt_field
        };
        let left = op & 0x100 != 0;
        let v = self.d[r] & sz.mask();
        let res = self.shift((op >> 3) & 3, left, v, count, sz);
        self.write_ea(bus, Ea::D(r), sz, res);
        bus.idle(2 + 2 * count + if sz == Size::L { 2 } else { 0 });
    }

    fn shift(&mut self, kind: u16, left: bool, v: u32, count: u32, sz: Size) -> u32 {
        let msb = sz.msb();
        let mask = sz.mask();
        let mut v = v & mask;
        self.v = false;
        match (kind, left) {
            (0, true) => {
                // ASL
                if count == 0 {
                    self.c = false;
                } else {
                    let mut overflow = false;
                    for _ in 0..count {
                        let out = v & msb != 0;
                        v = (v << 1) & mask;
                        if (v & msb != 0) != out {
                            overflow = true;
                        }
                        self.c = out;
                    }
                    self.x = self.c;
                    self.v = overflow;
                }
            }
            (0, false) => {
                // ASR
                if count == 0 {
                    self.c = false;
                } else {
                    for _ in 0..count {
                        self.c = v & 1 != 0;
                        v = (v >> 1) | (v & msb);
                    }
                    self.x = self.c;
                }
            }
            (1, true) => {
                // LSL
                if count == 0 {
                    self.c = false;
                } else {
                    for _ in 0..count {
                        self.c = v & msb != 0;
                        v = (v << 1) & mask;
                    }
                    self.x = self.c;
                }
            }
            (1, false) => {
                // LSR
                if count == 0 {
                    self.c = false;
                } else {
                    for _ in 0..count {
                        self.c = v & 1 != 0;
                        v >>= 1;
                    }
                    self.x = self.c;
                }
            }
            (2, true) => {
                // ROXL
                for _ in 0..count {
                    let out = v & msb != 0;
                    v = ((v << 1) | self.x as u32) & mask;
                    self.x = out;
                }
                self.c = self.x;
            }
            (2, false) => {
                // ROXR
                for _ in 0..count {
                    let out = v & 1 != 0;
                    v = (v >> 1) | if self.x { msb } else { 0 };
                    self.x = out;
                }
                self.c = self.x;
            }
            (_, true) => {
                // ROL
                if count == 0 {
                    self.c = false;
                } else {
                    for _ in 0..count {
                        let out = v & msb != 0;
                        v = ((v << 1) | out as u32) & mask;
                        self.c = out;
                    }
                }
            }
            (_, false) => {
                // ROR
                if count == 0 {
                    self.c = false;
                } else {
                    for _ in 0..count {
                        let out = v & 1 != 0;
                        v = (v >> 1) | if out { msb } else { 0 };
                        self.c = out;
                    }
                }
            }
        }
        self.set_nz(v, sz);
        v
    }
}
