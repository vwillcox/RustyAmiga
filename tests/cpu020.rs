//! 68020 instruction tests: hand-assembled code against the M68000 Family
//! Programmer's Reference Manual.

use amiga::bus::Bus;
use amiga::cpu::{Cpu, CpuModel};

struct Mem(Vec<u8>);

impl Bus for Mem {
    fn read8(&mut self, a: u32) -> u8 {
        self.0[(a & 0xffff) as usize]
    }
    fn read16(&mut self, a: u32) -> u16 {
        (self.read8(a) as u16) << 8 | self.read8(a + 1) as u16
    }
    fn write8(&mut self, a: u32, v: u8) {
        self.0[(a & 0xffff) as usize] = v;
    }
    fn write16(&mut self, a: u32, v: u16) {
        self.write8(a, (v >> 8) as u8);
        self.write8(a + 1, v as u8);
    }
}

impl Mem {
    fn long(&mut self, a: u32, v: u32) {
        self.write16(a, (v >> 16) as u16);
        self.write16(a + 2, v as u16);
    }
    fn get_long(&mut self, a: u32) -> u32 {
        (self.read16(a) as u32) << 16 | self.read16(a + 2) as u32
    }
}

const CODE: u32 = 0x1000;

/// Build a 68020 in supervisor mode with `code` at $1000.
fn setup(code: &[u16]) -> (Cpu, Mem) {
    let mut m = Mem(vec![0; 0x10000]);
    for (i, w) in code.iter().enumerate() {
        m.write16(CODE + i as u32 * 2, *w);
    }
    let mut c = Cpu::with_model(CpuModel::M68020);
    c.pc = CODE;
    c.a[7] = 0x8000;
    c.imask = 7;
    (c, m)
}

fn run(c: &mut Cpu, m: &mut Mem, n: usize) {
    for _ in 0..n {
        c.step(m);
    }
}

#[test]
fn extb() {
    let (mut c, mut m) = setup(&[0x49c0]);
    c.d[0] = 0x1234_5680;
    run(&mut c, &mut m, 1);
    assert_eq!(c.d[0], 0xffff_ff80);
    assert!(c.n);
}

#[test]
fn mul_long() {
    // MULU.L D1,D0 (32-bit result, overflows)
    let (mut c, mut m) = setup(&[0x4c01, 0x0000]);
    c.d[0] = 0x10000;
    c.d[1] = 0x10000;
    run(&mut c, &mut m, 1);
    assert_eq!(c.d[0], 0);
    assert!(c.v);
    // MULU.L D1,D2:D0 (64-bit)
    let (mut c, mut m) = setup(&[0x4c01, 0x0402]);
    c.d[0] = 0x10000;
    c.d[1] = 0x10000;
    run(&mut c, &mut m, 1);
    assert_eq!((c.d[2], c.d[0]), (1, 0));
    assert!(!c.v);
    // MULS.L D1,D0
    let (mut c, mut m) = setup(&[0x4c01, 0x0800]);
    c.d[0] = -3i32 as u32;
    c.d[1] = 7;
    run(&mut c, &mut m, 1);
    assert_eq!(c.d[0] as i32, -21);
    assert!(c.n && !c.v);
}

#[test]
fn div_long() {
    // DIVU.L D1,D0
    let (mut c, mut m) = setup(&[0x4c41, 0x0000]);
    c.d[0] = 100;
    c.d[1] = 7;
    run(&mut c, &mut m, 1);
    assert_eq!(c.d[0], 14);
    // DIVUL.L D1,D2:D0 (32-bit dividend, remainder in D2)
    let (mut c, mut m) = setup(&[0x4c41, 0x0002]);
    c.d[0] = 100;
    c.d[1] = 7;
    run(&mut c, &mut m, 1);
    assert_eq!((c.d[0], c.d[2]), (14, 2));
    // DIVS.L D1,D2:D0 (64-bit dividend)
    let (mut c, mut m) = setup(&[0x4c41, 0x0c02]);
    c.d[2] = 0xffff_ffff;
    c.d[0] = -100i32 as u32;
    c.d[1] = 7;
    run(&mut c, &mut m, 1);
    assert_eq!((c.d[0] as i32, c.d[2] as i32), (-14, -2));
    assert!(c.n);
}

#[test]
fn bitfields() {
    // BFEXTU D0{4:8},D1
    let (mut c, mut m) = setup(&[0xe9c0, 0x1108]);
    c.d[0] = 0x1234_5678;
    run(&mut c, &mut m, 1);
    assert_eq!(c.d[1], 0x23);
    // BFINS D1,(A0){12:12}
    let (mut c, mut m) = setup(&[0xefd0, 0x130c]);
    c.a[0] = 0x2000;
    c.d[1] = 0xabc;
    run(&mut c, &mut m, 1);
    assert_eq!(m.get_long(0x2000), 0x000a_bc00);
    // BFFFO D0{0:32},D1
    let (mut c, mut m) = setup(&[0xedc0, 0x1000]);
    c.d[0] = 0x0001_0000;
    run(&mut c, &mut m, 1);
    assert_eq!(c.d[1], 15);
    // BFEXTS (A0){D2:8},D1 with a negative offset
    let (mut c, mut m) = setup(&[0xebd0, 0x1888]);
    c.a[0] = 0x2001;
    c.d[2] = -4i32 as u32;
    m.write8(0x2000, 0x0f);
    m.write8(0x2001, 0x80);
    run(&mut c, &mut m, 1);
    assert_eq!(c.d[1] as i32, -8);
    assert!(c.n);
    // BFCHG D0{28:8} wraps around the register
    let (mut c, mut m) = setup(&[0xeac0, 0x0708]);
    c.d[0] = 0;
    run(&mut c, &mut m, 1);
    assert_eq!(c.d[0], 0xf000_000f);
    assert!(c.z);
}

#[test]
fn full_extension_word() {
    // LEA ([4,A0],D1.L*4,8),A1: post-indexed memory indirect
    let (mut c, mut m) = setup(&[0x43f0, 0x1d26, 0x0004, 0x0008]);
    c.a[0] = 0x2000;
    c.d[1] = 2;
    m.long(0x2004, 0x3000);
    run(&mut c, &mut m, 1);
    assert_eq!(c.a[1], 0x3010);
    // Brief format with scale: LEA (2,A0,D1.W*8),A1
    let (mut c, mut m) = setup(&[0x43f0, 0x1602]);
    c.a[0] = 0x2000;
    c.d[1] = 3;
    run(&mut c, &mut m, 1);
    assert_eq!(c.a[1], 0x2000 + 24 + 2);
}

#[test]
fn branch_long() {
    let (mut c, mut m) = setup(&[0x60ff, 0x0000, 0x0100]);
    run(&mut c, &mut m, 1);
    assert_eq!(c.pc, CODE + 2 + 0x100);
}

#[test]
fn vbr_and_frames() {
    // MOVEC D0,VBR; TRAP #0; (handler) RTE
    let (mut c, mut m) = setup(&[0x4e7b, 0x0801, 0x4e40, 0x4e71]);
    c.d[0] = 0x4000;
    m.long(0x4000 + 32 * 4, 0x1100);
    m.write16(0x1100, 0x4e73);
    run(&mut c, &mut m, 2);
    assert_eq!(c.pc, 0x1100);
    assert_eq!(c.a[7], 0x8000 - 8);
    assert_eq!(m.read16(0x8000 - 2), 0x0080, "format 0, vector offset $80");
    assert_eq!(m.get_long(0x8000 - 6), CODE + 6);
    run(&mut c, &mut m, 1);
    assert_eq!((c.pc, c.a[7]), (CODE + 6, 0x8000));
}

#[test]
fn trapcc_format2() {
    // TRAPT (always) -> vector 7 with a format 2 frame
    let (mut c, mut m) = setup(&[0x50fc]);
    m.long(7 * 4, 0x1200);
    run(&mut c, &mut m, 1);
    assert_eq!(c.pc, 0x1200);
    assert_eq!(m.read16(0x8000 - 6), 0x201c);
    assert_eq!(m.get_long(0x8000 - 4), CODE);
}

#[test]
fn cacr_reads_back_like_68020() {
    // MOVEC D0,CACR; MOVEC CACR,D1
    let (mut c, mut m) = setup(&[0x4e7b, 0x0002, 0x4e7a, 0x1002]);
    c.d[0] = 0x0000_0109;
    run(&mut c, &mut m, 2);
    assert_eq!(c.d[1], 1);
}

#[test]
fn pack_unpack() {
    let (mut c, mut m) = setup(&[0x8340, 0x0000]);
    c.d[0] = 0x0304;
    run(&mut c, &mut m, 1);
    assert_eq!(c.d[1] & 0xff, 0x34);
    let (mut c, mut m) = setup(&[0x8380, 0x3030]);
    c.d[0] = 0x34;
    run(&mut c, &mut m, 1);
    assert_eq!(c.d[1] & 0xffff, 0x3334);
}

#[test]
fn cas_and_cmp2() {
    // CAS.L D1,D2,(A0)
    let (mut c, mut m) = setup(&[0x0ed0, 0x0081]);
    c.a[0] = 0x2000;
    m.long(0x2000, 5);
    c.d[1] = 5;
    c.d[2] = 99;
    run(&mut c, &mut m, 1);
    assert_eq!(m.get_long(0x2000), 99);
    assert!(c.z);
    // CMP2.W (A0),D0
    for (d0, cflag, zflag) in [(25, true, false), (10, false, true), (15, false, false)] {
        let (mut c, mut m) = setup(&[0x02d0, 0x0000]);
        c.a[0] = 0x2000;
        m.write16(0x2000, 10);
        m.write16(0x2002, 20);
        c.d[0] = d0;
        run(&mut c, &mut m, 1);
        assert_eq!((c.c, c.z), (cflag, zflag), "d0={d0}");
    }
}

#[test]
fn unaligned_long_read() {
    // MOVE.L (A0),D0 at an odd address
    let (mut c, mut m) = setup(&[0x2010]);
    c.a[0] = 0x2001;
    m.long(0x2000, 0x0011_2233);
    m.write8(0x2004, 0x44);
    run(&mut c, &mut m, 1);
    assert_eq!(c.d[0], 0x1122_3344);
}
