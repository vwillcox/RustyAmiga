//! 68881/68882 floating-point coprocessor (also the 68040's built-in FPU).
//!
//! Registers hold f64 rather than 80-bit extended precision. Memory formats
//! (including extended and packed decimal) are converted exactly where f64 can
//! represent the value. Exceptions are recorded in FPSR but never trapped.

use super::*;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FpuModel {
    #[default]
    None,
    M68881,
    M68882,
    /// The 68040's internal FPU.
    M68040,
}

// FPSR condition code bits.
const CC_N: u32 = 1 << 27;
const CC_Z: u32 = 1 << 26;
const CC_I: u32 = 1 << 25;
const CC_NAN: u32 = 1 << 24;
// Exception status / accrued bits.
const EXC_OPERR: u32 = 1 << 13;
const EXC_OVFL: u32 = 1 << 12;
const EXC_DZ: u32 = 1 << 10;
const EXC_INEX2: u32 = 1 << 9;
const ACC_IOP: u32 = 1 << 7;
const ACC_DZ: u32 = 1 << 4;

/// An FPU effective address: like `Ea`, but immediates can be up to 12 bytes.
enum FEa {
    D(usize),
    A(usize),
    M(u32),
    Imm([u8; 12]),
}

/// Bytes per operand in each memory format (L, S, X, P, W, D, B).
fn fmt_bytes(fmt: u16) -> u32 {
    match fmt {
        0 | 1 => 4,
        2 | 3 => 12,
        4 => 2,
        5 => 8,
        _ => 1,
    }
}

/// x * 2^e without intermediate overflow/underflow for large |e|.
fn ldexp(mut x: f64, mut e: i32) -> f64 {
    while e > 1000 {
        x *= 2f64.powi(1000);
        e -= 1000;
    }
    while e < -1000 {
        x *= 2f64.powi(-1000);
        e += 1000;
    }
    x * 2f64.powi(e)
}

/// 96-bit extended precision (sign/exponent, padding, 64-bit mantissa) to f64.
pub fn ext_to_f64(b: &[u8; 12]) -> f64 {
    let neg = b[0] & 0x80 != 0;
    let exp = ((b[0] as i32 & 0x7f) << 8) | b[1] as i32;
    let mant = u64::from_be_bytes(b[4..12].try_into().unwrap());
    let v = if exp == 0x7fff {
        if mant << 1 == 0 {
            f64::INFINITY
        } else {
            f64::NAN
        }
    } else if mant == 0 {
        0.0
    } else {
        ldexp(mant as f64, exp - 16383 - 63)
    };
    if neg {
        -v
    } else {
        v
    }
}

pub fn f64_to_ext(v: f64) -> [u8; 12] {
    let mut b = [0u8; 12];
    let neg = v.is_sign_negative();
    let (exp, mant): (u32, u64) = if v.is_nan() {
        (0x7fff, u64::MAX)
    } else if v.is_infinite() {
        (0x7fff, 0)
    } else if v == 0.0 {
        (0, 0)
    } else {
        let bits = v.abs().to_bits();
        let e = ((bits >> 52) & 0x7ff) as i32;
        let m = bits & ((1 << 52) - 1);
        if e == 0 {
            // Subnormal: normalise.
            let lz = m.leading_zeros() as i32 - 11;
            ((-1022 - lz + 16383) as u32, m << (11 + lz))
        } else {
            ((e - 1023 + 16383) as u32, 1 << 63 | m << 11)
        }
    };
    b[0] = (exp >> 8) as u8 & 0x7f | if neg { 0x80 } else { 0 };
    b[1] = exp as u8;
    b[4..12].copy_from_slice(&mant.to_be_bytes());
    b
}

/// Packed decimal (BCD) to f64.
fn packed_to_f64(b: &[u8; 12]) -> f64 {
    let digit = |i: usize| -> u32 {
        // Mantissa digits: integer digit in byte 3 low nibble, then 16 fraction digits.
        let byte = b[i / 2];
        (if i % 2 == 0 { byte >> 4 } else { byte & 15 }) as u32
    };
    let mut s = String::new();
    if b[0] & 0x80 != 0 {
        s.push('-');
    }
    s.push(char::from_digit(digit(7), 10).unwrap_or('0'));
    s.push('.');
    for i in 8..24 {
        s.push(char::from_digit(digit(i), 10).unwrap_or('0'));
    }
    let e = digit(1) * 100 + digit(2) * 10 + digit(3);
    s.push('e');
    if b[0] & 0x40 != 0 {
        s.push('-');
    }
    s.push_str(&e.to_string());
    s.parse().unwrap_or(f64::NAN)
}

/// f64 to packed decimal with `k` significant digits (k <= 0: digits after the point).
fn f64_to_packed(v: f64, k: i8) -> [u8; 12] {
    let mut b = [0u8; 12];
    if v.is_nan() || v.is_infinite() {
        b[0] = 0x7f;
        b[1] = 0xff;
        if v.is_nan() {
            b[4] = 0xff;
        }
        if v < 0.0 {
            b[0] |= 0x80;
        }
        return b;
    }
    let digits = if k > 0 { k.min(17) as usize } else { 17 };
    let s = format!("{:.*e}", digits - 1, v.abs());
    let (m, e) = s.split_once('e').unwrap();
    let e: i32 = e.parse().unwrap_or(0);
    let m: Vec<u32> = m.chars().filter_map(|c| c.to_digit(10)).collect();
    let set = |b: &mut [u8; 12], i: usize, d: u32| {
        if i % 2 == 0 {
            b[i / 2] |= (d as u8) << 4;
        } else {
            b[i / 2] |= d as u8;
        }
    };
    set(&mut b, 7, m[0]);
    for (j, &d) in m.iter().skip(1).take(16).enumerate() {
        set(&mut b, 8 + j, d);
    }
    let ea = e.unsigned_abs();
    set(&mut b, 1, ea / 100 % 10);
    set(&mut b, 2, ea / 10 % 10);
    set(&mut b, 3, ea % 10);
    if v < 0.0 {
        b[0] |= 0x80;
    }
    if e < 0 {
        b[0] |= 0x40;
    }
    b
}

/// FMOVECR constant ROM.
fn rom_constant(off: u16) -> f64 {
    use std::f64::consts::*;
    match off {
        0x00 => PI,
        0x0b => LOG10_2,
        0x0c => E,
        0x0d => LOG2_E,
        0x0e => LOG10_E,
        0x0f => 0.0,
        0x30 => LN_2,
        0x31 => LN_10,
        0x32 => 1.0,
        0x33 => 1e1,
        0x34 => 1e2,
        0x35 => 1e4,
        0x36 => 1e8,
        0x37 => 1e16,
        0x38 => 1e32,
        0x39 => 1e64,
        0x3a => 1e128,
        0x3b => 1e256,
        0x3c..=0x3f => f64::INFINITY,
        _ => 0.0,
    }
}

impl Cpu {
    #[inline]
    pub(super) fn has_fpu(&self) -> bool {
        self.fpu != FpuModel::None
    }

    fn fpu_ea<B: Bus>(&mut self, bus: &mut B, mode: u16, reg: u16, bytes: u32) -> FEa {
        let r = reg as usize;
        match (mode, reg) {
            (0, _) => FEa::D(r),
            (1, _) => FEa::A(r),
            (3, _) => {
                let a = self.a[r];
                let inc = if r == 7 && bytes == 1 { 2 } else { bytes };
                self.a[r] = a.wrapping_add(inc);
                FEa::M(a)
            }
            (4, _) => {
                let dec = if r == 7 && bytes == 1 { 2 } else { bytes };
                self.a[r] = self.a[r].wrapping_sub(dec);
                FEa::M(self.a[r])
            }
            (7, 4) => {
                let mut b = [0u8; 12];
                // Byte immediates sit in the low byte of a word.
                if bytes == 1 {
                    b[0] = self.fetch16(bus) as u8;
                } else {
                    for i in 0..(bytes as usize / 2) {
                        let w = self.fetch16(bus);
                        b[i * 2..i * 2 + 2].copy_from_slice(&w.to_be_bytes());
                    }
                }
                FEa::Imm(b)
            }
            _ => match self.ea(bus, mode, reg, Size::L) {
                Ea::M(a) => FEa::M(a),
                _ => FEa::M(0),
            },
        }
    }

    fn fea_read<B: Bus>(&mut self, bus: &mut B, ea: &FEa, bytes: u32) -> [u8; 12] {
        let mut b = [0u8; 12];
        match ea {
            FEa::D(r) => b[..4].copy_from_slice(&self.d[*r].to_be_bytes()),
            FEa::A(r) => b[..4].copy_from_slice(&self.a[*r].to_be_bytes()),
            FEa::Imm(v) => b = *v,
            FEa::M(a) => {
                for i in 0..bytes {
                    b[i as usize] = bus.read8(a.wrapping_add(i));
                }
                return b;
            }
        }
        // Register operands: B and W come from the low end of the register.
        if matches!(ea, FEa::D(_) | FEa::A(_)) {
            match bytes {
                1 => b[0] = b[3],
                2 => {
                    b[0] = b[2];
                    b[1] = b[3];
                }
                _ => {}
            }
        }
        b
    }

    fn fea_write<B: Bus>(&mut self, bus: &mut B, ea: &FEa, bytes: u32, b: &[u8; 12]) {
        match ea {
            FEa::D(r) => {
                let v = &mut self.d[*r];
                *v = match bytes {
                    1 => (*v & !0xff) | b[0] as u32,
                    2 => (*v & !0xffff) | u16::from_be_bytes([b[0], b[1]]) as u32,
                    _ => u32::from_be_bytes(b[..4].try_into().unwrap()),
                };
            }
            FEa::A(r) => self.a[*r] = u32::from_be_bytes(b[..4].try_into().unwrap()),
            FEa::M(a) => {
                for i in 0..bytes {
                    bus.write8(a.wrapping_add(i), b[i as usize]);
                }
            }
            FEa::Imm(_) => {}
        }
    }

    /// Convert a memory operand to f64.
    fn from_format(&self, fmt: u16, b: &[u8; 12]) -> f64 {
        let l = u32::from_be_bytes(b[..4].try_into().unwrap());
        match fmt {
            0 => l as i32 as f64,
            1 => f32::from_bits(l) as f64,
            2 => ext_to_f64(b),
            3 => packed_to_f64(b),
            4 => i16::from_be_bytes([b[0], b[1]]) as f64,
            5 => f64::from_bits(u64::from_be_bytes(b[..8].try_into().unwrap())),
            _ => b[0] as i8 as f64,
        }
    }

    /// Round to an integer using the FPCR rounding mode.
    fn fp_round(&self, v: f64) -> f64 {
        match (self.fpcr >> 4) & 3 {
            0 => {
                let r = v.round();
                // Round half to even.
                if (v - v.trunc()).abs() == 0.5 && r % 2.0 != 0.0 {
                    r - v.signum()
                } else {
                    r
                }
            }
            1 => v.trunc(),
            2 => v.floor(),
            _ => v.ceil(),
        }
    }

    /// Convert f64 to a memory format.
    fn to_format(&mut self, fmt: u16, v: f64, k: i8) -> [u8; 12] {
        let mut b = [0u8; 12];
        let int = |c: &mut Cpu, lo: f64, hi: f64| -> i64 {
            if v.is_nan() {
                c.fpsr |= EXC_OPERR | ACC_IOP;
                return -1;
            }
            let r = c.fp_round(v);
            if r < lo || r > hi {
                c.fpsr |= EXC_OPERR | ACC_IOP;
                return if v < 0.0 { lo as i64 } else { hi as i64 };
            }
            if r != v {
                c.fpsr |= EXC_INEX2;
            }
            r as i64
        };
        match fmt {
            0 => b[..4].copy_from_slice(&(int(self, i32::MIN as f64, i32::MAX as f64) as i32).to_be_bytes()),
            1 => b[..4].copy_from_slice(&(v as f32).to_bits().to_be_bytes()),
            2 => b = f64_to_ext(v),
            3 => b = f64_to_packed(v, k),
            4 => b[..2].copy_from_slice(&(int(self, i16::MIN as f64, i16::MAX as f64) as i16).to_be_bytes()),
            5 => b[..8].copy_from_slice(&v.to_bits().to_be_bytes()),
            _ => b[0] = int(self, i8::MIN as f64, i8::MAX as f64) as i8 as u8,
        }
        b
    }

    /// Set the FPSR condition codes from a result.
    fn fp_cc(&mut self, v: f64) {
        self.fpsr &= !(CC_N | CC_Z | CC_I | CC_NAN);
        if v.is_nan() {
            self.fpsr |= CC_NAN;
        } else {
            if v.is_sign_negative() {
                self.fpsr |= CC_N;
            }
            if v == 0.0 {
                self.fpsr |= CC_Z;
            }
            if v.is_infinite() {
                self.fpsr |= CC_I;
            }
        }
    }

    /// Apply the FPCR rounding precision to an arithmetic result.
    fn fp_precision(&self, v: f64) -> f64 {
        match (self.fpcr >> 6) & 3 {
            1 => v as f32 as f64,
            _ => v,
        }
    }

    /// FPSR condition test for FBcc/FScc/FDBcc/FTRAPcc (6-bit predicate).
    fn fp_cond(&mut self, pred: u16) -> bool {
        let n = self.fpsr & CC_N != 0;
        let z = self.fpsr & CC_Z != 0;
        let nan = self.fpsr & CC_NAN != 0;
        match pred & 15 {
            0 => false,
            1 => z,
            2 => !(nan || z || n),
            3 => z || !(nan || n),
            4 => n && !(nan || z),
            5 => z || (n && !nan),
            6 => !(nan || z),
            7 => !nan,
            8 => nan,
            9 => nan || z,
            10 => nan || !(n || z),
            11 => nan || z || !n,
            12 => nan || (n && !z),
            13 => nan || z || n,
            14 => !z,
            _ => true,
        }
    }

    /// Execute a coprocessor instruction for coprocessor ID 1 (op = $F2xx/$F3xx).
    pub(super) fn fpu_op<B: Bus>(&mut self, bus: &mut B, op: u16) {
        let mode = (op >> 3) & 7;
        let reg = op & 7;
        // FSAVE/FRESTORE manage the saved state; anything else puts the FPU in use.
        if !matches!((op >> 6) & 7, 4 | 5) {
            self.fpu_used = true;
        }
        match (op >> 6) & 7 {
            0 => self.fpu_general(bus, op),
            1 => {
                let ext = self.fetch16(bus);
                if mode == 1 {
                    // FDBcc
                    let disp = self.fetch16(bus) as i16 as i32 as u32;
                    if self.fp_cond(ext) {
                        return;
                    }
                    let r = reg as usize;
                    let cnt = (self.d[r] as u16).wrapping_sub(1);
                    self.d[r] = (self.d[r] & 0xffff_0000) | cnt as u32;
                    if cnt != 0xffff {
                        self.pc = self.instr_pc.wrapping_add(4).wrapping_add(disp);
                    }
                } else if mode == 7 && (2..=4).contains(&reg) {
                    // FTRAPcc
                    match reg {
                        2 => {
                            self.fetch16(bus);
                        }
                        3 => {
                            self.fetch32(bus);
                        }
                        _ => {}
                    }
                    if self.fp_cond(ext) {
                        let pc = self.pc;
                        self.exception(bus, VEC_TRAPV, pc);
                    }
                } else {
                    // FScc
                    let ea = self.ea(bus, mode, reg, Size::B);
                    let t = self.fp_cond(ext);
                    self.write_ea(bus, ea, Size::B, if t { 0xff } else { 0 });
                }
            }
            2 | 3 => {
                // FBcc.W / FBcc.L (FNOP is FBF.W #0)
                let base = self.pc;
                let disp = if op & 0x40 != 0 { self.fetch32(bus) } else { self.fetch16(bus) as i16 as i32 as u32 };
                if self.fp_cond(op & 0x3f) {
                    self.pc = base.wrapping_add(disp);
                }
            }
            4 => self.fsave(bus, mode, reg),
            5 => self.frestore(bus, mode, reg),
            _ => {
                let pc = self.instr_pc;
                self.exception(bus, VEC_LINE_F, pc);
            }
        }
    }

    fn fsave<B: Bus>(&mut self, bus: &mut B, mode: u16, reg: u16) {
        if !self.s {
            return self.privilege_violation(bus);
        }
        // Null frame until the FPU has been used, then an idle frame.
        let (header, size): (u32, u32) = match (self.fpu_used, self.fpu) {
            (false, _) => (0, 0),
            (true, FpuModel::M68040) => (0x4100_0000, 0),
            (true, FpuModel::M68881) => (0x1f18_0000, 0x18),
            (true, _) => (0x1f38_0000, 0x38),
        };
        let total = 4 + size;
        let addr = match mode {
            4 => {
                self.a[reg as usize] = self.a[reg as usize].wrapping_sub(total);
                self.a[reg as usize]
            }
            _ => match self.ea(bus, mode, reg, Size::L) {
                Ea::M(a) => a,
                _ => return self.illegal(bus),
            },
        };
        self.write(bus, addr, Size::L, header);
        for i in 0..size / 4 {
            self.write(bus, addr.wrapping_add(4 + i * 4), Size::L, 0);
        }
    }

    fn frestore<B: Bus>(&mut self, bus: &mut B, mode: u16, reg: u16) {
        if !self.s {
            return self.privilege_violation(bus);
        }
        let addr = match mode {
            3 => self.a[reg as usize],
            _ => match self.ea(bus, mode, reg, Size::L) {
                Ea::M(a) => a,
                _ => return self.illegal(bus),
            },
        };
        let header = self.read(bus, addr, Size::L);
        let size = (header >> 16) & 0xff;
        if mode == 3 {
            self.a[reg as usize] = addr.wrapping_add(4 + size);
        }
        if header >> 24 == 0 {
            // Null frame: reset the FPU.
            self.fp = [f64::NAN; 8];
            self.fpcr = 0;
            self.fpsr = 0;
            self.fpiar = 0;
            self.fpu_used = false;
        } else {
            self.fpu_used = true;
        }
    }

    fn fpu_general<B: Bus>(&mut self, bus: &mut B, op: u16) {
        let w = self.fetch16(bus);
        let mode = (op >> 3) & 7;
        let reg = op & 7;
        let fmt = (w >> 10) & 7;
        let dst = ((w >> 7) & 7) as usize;
        match w >> 13 {
            0 | 2 => {
                if w >> 13 == 2 && fmt == 7 {
                    // FMOVECR
                    let v = rom_constant(w & 0x7f);
                    self.fp[dst] = v;
                    self.fp_cc(v);
                    return;
                }
                let src = if w >> 13 == 0 {
                    self.fp[fmt as usize]
                } else {
                    let bytes = fmt_bytes(fmt);
                    let ea = self.fpu_ea(bus, mode, reg, bytes);
                    let b = self.fea_read(bus, &ea, bytes);
                    self.from_format(fmt, &b)
                };
                self.fpu_arith(w & 0x7f, src, dst);
            }
            3 => {
                // FMOVE FPn,<ea>
                let k = (w & 0x7f) as i8;
                let k = if k & 0x40 != 0 { k | !0x7f } else { k };
                let k = if fmt == 7 { self.d[((w >> 4) & 7) as usize] as i8 } else { k };
                let fmt = if fmt == 7 { 3 } else { fmt };
                let src = ((w >> 7) & 7) as usize;
                let v = self.fp[src];
                let b = self.to_format(fmt, v, k);
                let bytes = fmt_bytes(fmt);
                let ea = self.fpu_ea(bus, mode, reg, bytes);
                self.fea_write(bus, &ea, bytes, &b);
            }
            4 | 5 => self.fmovem_ctrl(bus, mode, reg, (w >> 10) & 7, w >> 13 == 5),
            _ => self.fmovem(bus, mode, reg, w),
        }
    }

    fn fpu_arith(&mut self, opmode: u16, src: f64, dst: usize) {
        let d = self.fp[dst];
        // 68040 single/double-rounding variants ($40-$7f) use the same operations.
        let (opm, round): (u16, Option<bool>) = match opmode {
            0x41 | 0x45 => (0x04, Some(opmode == 0x41)),
            0x40.. => (opmode & 0x3b, Some(opmode & 4 == 0)),
            _ => (opmode, None),
        };
        let r = match opm {
            0x00 => src,
            0x01 => self.fp_round(src),
            0x02 => src.sinh(),
            0x03 => src.trunc(),
            0x04 | 0x05 => src.sqrt(),
            0x06 => src.ln_1p(),
            0x08 => src.exp_m1(),
            0x09 => src.tanh(),
            0x0a => src.atan(),
            0x0c => src.asin(),
            0x0d => src.atanh(),
            0x0e => src.sin(),
            0x0f => src.tan(),
            0x10 => src.exp(),
            0x11 => src.exp2(),
            0x12 => 10f64.powf(src),
            0x14 => src.ln(),
            0x15 => src.log10(),
            0x16 => src.log2(),
            0x18 => src.abs(),
            0x19 => src.cosh(),
            0x1a => -src,
            0x1c => src.acos(),
            0x1d => src.cos(),
            0x1e => {
                // FGETEXP
                if src == 0.0 || !src.is_finite() {
                    src
                } else {
                    (((src.abs().to_bits() >> 52) & 0x7ff) as i32 - 1023) as f64
                }
            }
            0x1f => {
                // FGETMAN
                if src == 0.0 || !src.is_finite() {
                    src
                } else {
                    let e = ((src.abs().to_bits() >> 52) & 0x7ff) as i32 - 1023;
                    ldexp(src, -e)
                }
            }
            0x20 => {
                if src == 0.0 && d.is_finite() && d != 0.0 {
                    self.fpsr |= EXC_DZ | ACC_DZ;
                }
                d / src
            }
            0x21 => {
                // FMOD: truncated quotient
                let q = (d / src).trunc();
                self.set_quotient(q);
                d - q * src
            }
            0x22 => d + src,
            0x23 => d * src,
            0x24 => (d as f32 / src as f32) as f64,
            0x25 => {
                // FREM: round-to-nearest quotient
                let q = (d / src).round();
                self.set_quotient(q);
                d - q * src
            }
            0x26 => {
                // FSCALE
                ldexp(d, src.trunc() as i32)
            }
            0x27 => (d as f32 * src as f32) as f64,
            0x28 => d - src,
            0x30..=0x37 => {
                // FSINCOS: sine to FPn, cosine to FPc
                self.fp[(opmode & 7) as usize] = src.cos();
                src.sin()
            }
            0x38 => {
                // FCMP
                self.fp_cmp(d, src);
                return;
            }
            0x3a => {
                // FTST
                self.fp_cc(src);
                return;
            }
            _ => {
                log::debug!("fpu: unimplemented opmode {opmode:02x}");
                src
            }
        };
        let r = match round {
            Some(true) => r as f32 as f64,
            Some(false) => r,
            None => self.fp_precision(r),
        };
        if r.is_infinite() && src.is_finite() && d.is_finite() {
            self.fpsr |= EXC_OVFL;
        }
        if r.is_nan() && !src.is_nan() && !d.is_nan() {
            self.fpsr |= EXC_OPERR | ACC_IOP;
        }
        self.fp[dst] = r;
        self.fp_cc(r);
    }

    fn set_quotient(&mut self, q: f64) {
        let sign = if q < 0.0 { 0x80 } else { 0 };
        let mag = (q.abs() as u64 & 0x7f) as u32;
        self.fpsr = (self.fpsr & !0x00ff_0000) | (sign | mag) << 16;
    }

    fn fp_cmp(&mut self, d: f64, s: f64) {
        self.fpsr &= !(CC_N | CC_Z | CC_I | CC_NAN);
        if d.is_nan() || s.is_nan() {
            self.fpsr |= CC_NAN;
            return;
        }
        if d == s {
            self.fpsr |= CC_Z;
            if d.is_sign_negative() {
                self.fpsr |= CC_N;
            }
        } else if d < s {
            self.fpsr |= CC_N;
        }
    }

    /// FMOVE(M) to/from FPCR, FPSR and FPIAR.
    fn fmovem_ctrl<B: Bus>(&mut self, bus: &mut B, mode: u16, reg: u16, mask: u16, to_ea: bool) {
        let n = mask.count_ones();
        if mode <= 1 && n == 1 {
            // Single register to/from Dn/An.
            let r = reg as usize;
            if to_ea {
                let v = self.fpu_ctrl_get(mask);
                if mode == 0 {
                    self.d[r] = v;
                } else {
                    self.a[r] = v;
                }
            } else {
                let v = if mode == 0 { self.d[r] } else { self.a[r] };
                self.fpu_ctrl_set(mask, v);
            }
            return;
        }
        let bytes = 4 * n.max(1);
        let ea = self.fpu_ea(bus, mode, reg, bytes);
        let mut addr = match ea {
            FEa::M(a) => a,
            FEa::Imm(b) => {
                // Immediate: one long per selected register, in order FPCR, FPSR, FPIAR.
                let mut i = 0;
                for bit in [4u16, 2, 1] {
                    if mask & bit != 0 {
                        let v = u32::from_be_bytes(b[i * 4..i * 4 + 4].try_into().unwrap());
                        self.fpu_ctrl_set(bit, v);
                        i += 1;
                    }
                }
                return;
            }
            _ => return,
        };
        for bit in [4u16, 2, 1] {
            if mask & bit == 0 {
                continue;
            }
            if to_ea {
                let v = self.fpu_ctrl_get(bit);
                self.write(bus, addr, Size::L, v);
            } else {
                let v = self.read(bus, addr, Size::L);
                self.fpu_ctrl_set(bit, v);
            }
            addr = addr.wrapping_add(4);
        }
    }

    fn fpu_ctrl_get(&self, bit: u16) -> u32 {
        match bit {
            4 => self.fpcr,
            2 => self.fpsr,
            _ => self.fpiar,
        }
    }

    fn fpu_ctrl_set(&mut self, bit: u16, v: u32) {
        match bit {
            4 => self.fpcr = v & 0xfff0,
            2 => self.fpsr = v & 0x0fff_fff8,
            _ => self.fpiar = v,
        }
    }

    /// FMOVEM.X: move multiple FP data registers.
    fn fmovem<B: Bus>(&mut self, bus: &mut B, mode: u16, reg: u16, w: u16) {
        let to_mem = w & 0x2000 != 0;
        let list_mode = (w >> 11) & 3;
        let mask = if list_mode & 1 != 0 { self.d[((w >> 4) & 7) as usize] as u8 } else { w as u8 };
        let r = reg as usize;
        if mode == 4 {
            // Predecrement: mask bit n selects FPn; registers are stored from FP7
            // down so FP0 ends up at the lowest address.
            let mut addr = self.a[r];
            for i in (0..8).rev() {
                if mask & (1 << i) != 0 {
                    addr = addr.wrapping_sub(12);
                    let b = f64_to_ext(self.fp[i]);
                    for (k, byte) in b.iter().enumerate() {
                        bus.write8(addr.wrapping_add(k as u32), *byte);
                    }
                }
            }
            self.a[r] = addr;
            return;
        }
        let mut addr = if mode == 3 {
            self.a[r]
        } else {
            match self.ea(bus, mode, reg, Size::L) {
                Ea::M(a) => a,
                _ => return self.illegal(bus),
            }
        };
        for i in 0..8 {
            if mask & (0x80 >> i) == 0 {
                continue;
            }
            if to_mem {
                let b = f64_to_ext(self.fp[i]);
                for (k, byte) in b.iter().enumerate() {
                    bus.write8(addr.wrapping_add(k as u32), *byte);
                }
            } else {
                let mut b = [0u8; 12];
                for (k, byte) in b.iter_mut().enumerate() {
                    *byte = bus.read8(addr.wrapping_add(k as u32));
                }
                self.fp[i] = ext_to_f64(&b);
            }
            addr = addr.wrapping_add(12);
        }
        if mode == 3 {
            self.a[r] = addr;
        }
    }
}
