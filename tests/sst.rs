//! Runs the SingleStepTests 68000 suite (https://github.com/SingleStepTests/m68000).
//! Set SST_DIR to the directory containing the v1/*.json.bin files.

use amiga::bus::Bus;
use amiga::cpu::Cpu;
use std::collections::HashMap;

struct TestBus {
    mem: HashMap<u32, u8>,
}

impl Bus for TestBus {
    fn read8(&mut self, a: u32) -> u8 {
        *self.mem.get(&(a & 0xff_ffff)).unwrap_or(&0)
    }
    fn read16(&mut self, a: u32) -> u16 {
        (self.read8(a) as u16) << 8 | self.read8(a.wrapping_add(1)) as u16
    }
    fn write8(&mut self, a: u32, v: u8) {
        self.mem.insert(a & 0xff_ffff, v);
    }
    fn write16(&mut self, a: u32, v: u16) {
        self.write8(a, (v >> 8) as u8);
        self.write8(a.wrapping_add(1), v as u8);
    }
}

struct State {
    regs: [u32; 19],
    prefetch: [u32; 2],
    ram: Vec<(u32, u8)>,
}

struct Reader<'a> {
    b: &'a [u8],
    p: usize,
}

impl Reader<'_> {
    fn u8(&mut self) -> u8 {
        self.p += 1;
        self.b[self.p - 1]
    }
    fn u16(&mut self) -> u16 {
        let v = u16::from_le_bytes([self.b[self.p], self.b[self.p + 1]]);
        self.p += 2;
        v
    }
    fn u32(&mut self) -> u32 {
        let v = u32::from_le_bytes(self.b[self.p..self.p + 4].try_into().unwrap());
        self.p += 4;
        v
    }
    fn state(&mut self) -> State {
        self.u32();
        assert_eq!(self.u32(), 0x01234567);
        let mut regs = [0; 19];
        for r in regs.iter_mut() {
            *r = self.u32();
        }
        let prefetch = [self.u32(), self.u32()];
        let n = self.u32();
        let mut ram = vec![];
        for _ in 0..n {
            let a = self.u32();
            let d = self.u16();
            ram.push((a, (d >> 8) as u8));
            ram.push((a | 1, d as u8));
        }
        State { regs, prefetch, ram }
    }
}

const NAMES: [&str; 19] = [
    "d0", "d1", "d2", "d3", "d4", "d5", "d6", "d7", "a0", "a1", "a2", "a3", "a4", "a5", "a6", "usp", "ssp", "sr",
    "pc",
];

fn run_file(path: &std::path::Path, verbose: usize) -> (usize, usize, usize) {
    let data = std::fs::read(path).unwrap();
    let mut r = Reader { b: &data, p: 0 };
    assert_eq!(r.u32(), 0x1A3F5D71);
    let n = r.u32();
    let (mut pass, mut fail, mut skip) = (0, 0, 0);
    for _ in 0..n {
        r.u32();
        assert_eq!(r.u32(), 0xABC12367);
        r.u32();
        r.u32();
        let len = r.u32() as usize;
        let name = String::from_utf8_lossy(&r.b[r.p..r.p + len]).to_string();
        r.p += len;
        let init = r.state();
        let fin = r.state();
        r.u32();
        r.u32();
        let _cycles = r.u32();
        let ntx = r.u32();
        let mut addr_err = false;
        for _ in 0..ntx {
            let tw = r.u8();
            r.u32();
            if tw != 0 {
                for _ in 0..5 {
                    r.u32();
                }
            }
            if tw == 4 || tw == 5 {
                addr_err = true;
            }
        }
        if addr_err {
            skip += 1;
            continue;
        }

        let mut bus = TestBus { mem: HashMap::new() };
        let pc = init.regs[18];
        bus.write16(pc.wrapping_sub(4), init.prefetch[0] as u16);
        bus.write16(pc.wrapping_sub(2), init.prefetch[1] as u16);
        for &(a, d) in &init.ram {
            bus.write8(a, d);
        }
        let mut cpu = Cpu::new();
        cpu.d.copy_from_slice(&init.regs[0..8]);
        cpu.a[..7].copy_from_slice(&init.regs[8..15]);
        cpu.s = false;
        cpu.a[7] = init.regs[15];
        cpu.other_sp = init.regs[16];
        cpu.set_sr(init.regs[17] as u16);
        cpu.pc = pc.wrapping_sub(4);
        cpu.step(&mut bus);

        let mut got = [0u32; 19];
        got[..8].copy_from_slice(&cpu.d);
        got[8..15].copy_from_slice(&cpu.a[..7]);
        got[15] = cpu.usp();
        got[16] = cpu.ssp();
        got[17] = cpu.sr() as u32;
        got[18] = cpu.pc.wrapping_add(4);
        let mut errs = vec![];
        for i in 0..19 {
            let (mut g, mut e) = (got[i], fin.regs[i]);
            if i == 18 {
                g &= 0xff_ffff;
                e &= 0xff_ffff;
            }
            if g != e {
                errs.push(format!("{}: got {:08x} want {:08x}", NAMES[i], g, e));
            }
        }
        for &(a, d) in &fin.ram {
            let g = bus.read8(a);
            if g != d {
                errs.push(format!("ram {:06x}: got {:02x} want {:02x}", a, g, d));
            }
        }
        if errs.is_empty() {
            pass += 1;
        } else {
            if fail < verbose {
                eprintln!("FAIL {} [{}]", name, errs.join(", "));
            }
            fail += 1;
        }
    }
    (pass, fail, skip)
}

#[test]
fn single_step_tests() {
    let Ok(dir) = std::env::var("SST_DIR") else {
        eprintln!("SST_DIR not set; skipping");
        return;
    };
    let filter = std::env::var("SST_FILTER").unwrap_or_default();
    let verbose: usize = std::env::var("SST_VERBOSE").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.to_string_lossy().ends_with(".json.bin"))
        .filter(|p| p.to_string_lossy().contains(&filter))
        .collect();
    entries.sort();
    let (mut tp, mut tf) = (0, 0);
    for p in entries {
        let (pass, fail, skip) = run_file(&p, verbose);
        let name = p.file_name().unwrap().to_string_lossy().replace(".json.bin", "");
        println!("{:16} pass {:6} fail {:6} skip(addr err) {:5}", name, pass, fail, skip);
        tp += pass;
        tf += fail;
    }
    println!("TOTAL pass {} fail {}", tp, tf);
}
