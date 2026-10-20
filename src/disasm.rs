//! Compact 68000 disassembler for debugging.

/// Disassemble the instruction at `pc`. `rd` reads a word. Returns (text, length in bytes).
pub fn disasm(pc: u32, rd: &dyn Fn(u32) -> u16) -> (String, u32) {
    let mut d = D { at: pc + 2, rd };
    let op = rd(pc);
    let s = d.decode(op);
    (s, d.at - pc)
}

struct D<'a> {
    at: u32,
    rd: &'a dyn Fn(u32) -> u16,
}

const CC: [&str; 16] = ["t", "f", "hi", "ls", "cc", "cs", "ne", "eq", "vc", "vs", "pl", "mi", "ge", "lt", "gt", "le"];

impl D<'_> {
    fn w(&mut self) -> u16 {
        let v = (self.rd)(self.at);
        self.at += 2;
        v
    }
    fn l(&mut self) -> u32 {
        let h = self.w() as u32;
        h << 16 | self.w() as u32
    }

    fn sz(b: u16) -> &'static str {
        match b & 3 {
            0 => ".b",
            1 => ".w",
            2 => ".l",
            _ => ".?",
        }
    }

    fn ea(&mut self, mode: u16, reg: u16, size: u16) -> String {
        match mode {
            0 => format!("d{reg}"),
            1 => format!("a{reg}"),
            2 => format!("(a{reg})"),
            3 => format!("(a{reg})+"),
            4 => format!("-(a{reg})"),
            5 => {
                let d = self.w() as i16;
                format!("{d}(a{reg})")
            }
            6 => {
                let e = self.w();
                format!("{}(a{reg},{})", e as i8, Self::idx(e))
            }
            _ => match reg {
                0 => format!("${:04x}.w", self.w()),
                1 => format!("${:08x}", self.l()),
                2 => {
                    let base = self.at;
                    let d = self.w() as i16 as i32;
                    format!("${:06x}(pc)", base.wrapping_add(d as u32))
                }
                3 => {
                    let base = self.at;
                    let e = self.w();
                    format!("${:06x}(pc,{})", base.wrapping_add(e as i8 as i32 as u32), Self::idx(e))
                }
                4 => match size {
                    0 => format!("#${:02x}", self.w() & 0xff),
                    2 => format!("#${:08x}", self.l()),
                    _ => format!("#${:04x}", self.w()),
                },
                _ => "?".into(),
            },
        }
    }

    fn idx(e: u16) -> String {
        format!(
            "{}{}.{}",
            if e & 0x8000 != 0 { "a" } else { "d" },
            (e >> 12) & 7,
            if e & 0x800 != 0 { "l" } else { "w" }
        )
    }

    fn src(&mut self, op: u16, size: u16) -> String {
        self.ea((op >> 3) & 7, op & 7, size)
    }

    fn regmask(m: u16, predec: bool) -> String {
        let mut v = vec![];
        for i in 0..16 {
            let bit = if predec { 15 - i } else { i };
            if m & (1 << bit) != 0 {
                v.push(if i < 8 { format!("d{i}") } else { format!("a{}", i - 8) });
            }
        }
        v.join("/")
    }

    fn decode(&mut self, op: u16) -> String {
        let reg9 = (op >> 9) & 7;
        let size = (op >> 6) & 3;
        match op >> 12 {
            0 => {
                if op & 0x100 != 0 {
                    if (op >> 3) & 7 == 1 {
                        let d = self.w() as i16;
                        let s = if op & 0x40 != 0 { ".l" } else { ".w" };
                        return if op & 0x80 != 0 {
                            format!("movep{s} d{reg9},{d}(a{})", op & 7)
                        } else {
                            format!("movep{s} {d}(a{}),d{reg9}", op & 7)
                        };
                    }
                    let n = ["btst", "bchg", "bclr", "bset"][size as usize];
                    return format!("{n} d{reg9},{}", self.src(op, 0));
                }
                if op & 0xf00 == 0x800 {
                    let n = ["btst", "bchg", "bclr", "bset"][size as usize];
                    let b = self.w() & 0xff;
                    return format!("{n} #{b},{}", self.src(op, 0));
                }
                let n = ["ori", "andi", "subi", "addi", "?", "eori", "cmpi", "?"][reg9 as usize];
                if op & 0xff == 0x3c {
                    return format!("{n} #${:02x},ccr", self.w() & 0xff);
                }
                if op & 0xff == 0x7c {
                    return format!("{n} #${:04x},sr", self.w());
                }
                let imm = self.ea(7, 4, size);
                format!("{n}{} {imm},{}", Self::sz(size), self.src(op, size))
            }
            1..=3 => {
                let s = match op >> 12 {
                    1 => 0,
                    2 => 2,
                    _ => 1,
                };
                let src = self.src(op, s);
                let dst = self.ea((op >> 6) & 7, reg9, s);
                let a = if (op >> 6) & 7 == 1 { "a" } else { "" };
                format!("move{a}{} {src},{dst}", Self::sz(s))
            }
            4 => self.line4(op),
            5 => {
                if size == 3 {
                    let cc = CC[((op >> 8) & 15) as usize];
                    if (op >> 3) & 7 == 1 {
                        let base = self.at;
                        let d = self.w() as i16 as i32;
                        return format!("db{cc} d{},${:06x}", op & 7, base.wrapping_add(d as u32));
                    }
                    return format!("s{cc} {}", self.src(op, 0));
                }
                let q = if reg9 == 0 { 8 } else { reg9 };
                let n = if op & 0x100 != 0 { "subq" } else { "addq" };
                format!("{n}{} #{q},{}", Self::sz(size), self.src(op, size))
            }
            6 => {
                let base = self.at;
                let mut d = op as i8 as i32;
                if d == 0 {
                    d = self.w() as i16 as i32;
                }
                let n = match (op >> 8) & 15 {
                    0 => "bra".to_string(),
                    1 => "bsr".to_string(),
                    c => format!("b{}", CC[c as usize]),
                };
                format!("{n} ${:06x}", base.wrapping_add(d as u32))
            }
            7 => format!("moveq #{},d{reg9}", op as i8),
            8 | 0xc => {
                let (lg, mul) = if op >> 12 == 8 { ("or", "div") } else { ("and", "mul") };
                if size == 3 {
                    let s = if op & 0x100 != 0 { "s" } else { "u" };
                    return format!("{mul}{s} {},d{reg9}", self.src(op, 1));
                }
                if op & 0x1f0 == 0x100 {
                    let n = if op >> 12 == 8 { "sbcd" } else { "abcd" };
                    return if op & 8 != 0 {
                        format!("{n} -(a{}),-(a{reg9})", op & 7)
                    } else {
                        format!("{n} d{},d{reg9}", op & 7)
                    };
                }
                if op >> 12 == 0xc && op & 0x130 == 0x100 {
                    return format!("exg {}", op & 0x1ff);
                }
                if op & 0x100 != 0 {
                    format!("{lg}{} d{reg9},{}", Self::sz(size), self.src(op, size))
                } else {
                    format!("{lg}{} {},d{reg9}", Self::sz(size), self.src(op, size))
                }
            }
            9 | 0xd => {
                let n = if op >> 12 == 9 { "sub" } else { "add" };
                if size == 3 {
                    let s = if op & 0x100 != 0 { 2 } else { 1 };
                    return format!("{n}a{} {},a{reg9}", Self::sz(s), self.src(op, s));
                }
                if op & 0x130 == 0x100 {
                    return if op & 8 != 0 {
                        format!("{n}x{} -(a{}),-(a{reg9})", Self::sz(size), op & 7)
                    } else {
                        format!("{n}x{} d{},d{reg9}", Self::sz(size), op & 7)
                    };
                }
                if op & 0x100 != 0 {
                    format!("{n}{} d{reg9},{}", Self::sz(size), self.src(op, size))
                } else {
                    format!("{n}{} {},d{reg9}", Self::sz(size), self.src(op, size))
                }
            }
            0xa => format!("dc.w ${op:04x} ; line-a"),
            0xb => {
                if size == 3 {
                    let s = if op & 0x100 != 0 { 2 } else { 1 };
                    return format!("cmpa{} {},a{reg9}", Self::sz(s), self.src(op, s));
                }
                if op & 0x100 != 0 {
                    if (op >> 3) & 7 == 1 {
                        return format!("cmpm{} (a{})+,(a{reg9})+", Self::sz(size), op & 7);
                    }
                    format!("eor{} d{reg9},{}", Self::sz(size), self.src(op, size))
                } else {
                    format!("cmp{} {},d{reg9}", Self::sz(size), self.src(op, size))
                }
            }
            0xe => {
                let names = ["as", "ls", "rox", "ro"];
                let dir = if op & 0x100 != 0 { "l" } else { "r" };
                if size == 3 {
                    let n = names[((op >> 9) & 3) as usize];
                    return format!("{n}{dir}.w {}", self.src(op, 1));
                }
                let n = names[((op >> 3) & 3) as usize];
                let cnt = if op & 0x20 != 0 {
                    format!("d{reg9}")
                } else {
                    format!("#{}", if reg9 == 0 { 8 } else { reg9 })
                };
                format!("{n}{dir}{} {cnt},d{}", Self::sz(size), op & 7)
            }
            _ => format!("dc.w ${op:04x} ; line-f"),
        }
    }

    fn line4(&mut self, op: u16) -> String {
        let reg9 = (op >> 9) & 7;
        let size = (op >> 6) & 3;
        if op & 0x1c0 == 0x1c0 {
            return format!("lea {},a{reg9}", self.src(op, 2));
        }
        if op & 0x1c0 == 0x180 {
            return format!("chk {},d{reg9}", self.src(op, 1));
        }
        match op {
            0x4afc => return "illegal".into(),
            0x4e70 => return "reset".into(),
            0x4e71 => return "nop".into(),
            0x4e72 => return format!("stop #${:04x}", self.w()),
            0x4e73 => return "rte".into(),
            0x4e75 => return "rts".into(),
            0x4e76 => return "trapv".into(),
            0x4e77 => return "rtr".into(),
            _ => {}
        }
        match op & 0xfff0 {
            0x4e40 => return format!("trap #{}", op & 15),
            0x4e50 if op & 8 == 0 => return format!("link a{},#{}", op & 7, self.w() as i16),
            0x4e50 => return format!("unlk a{}", op & 7),
            0x4e60 if op & 8 == 0 => return format!("move a{},usp", op & 7),
            0x4e60 => return format!("move usp,a{}", op & 7),
            _ => {}
        }
        match op & 0xffc0 {
            0x4e80 => return format!("jsr {}", self.src(op, 2)),
            0x4ec0 => return format!("jmp {}", self.src(op, 2)),
            0x40c0 => return format!("move sr,{}", self.src(op, 1)),
            0x44c0 => return format!("move {},ccr", self.src(op, 1)),
            0x46c0 => return format!("move {},sr", self.src(op, 1)),
            0x4800 => return format!("nbcd {}", self.src(op, 0)),
            0x4ac0 => return format!("tas {}", self.src(op, 0)),
            _ => {}
        }
        if op & 0xfff8 == 0x4840 {
            return format!("swap d{}", op & 7);
        }
        if op & 0xffc0 == 0x4840 {
            return format!("pea {}", self.src(op, 2));
        }
        if op & 0xfff8 == 0x4880 {
            return format!("ext.w d{}", op & 7);
        }
        if op & 0xfff8 == 0x48c0 {
            return format!("ext.l d{}", op & 7);
        }
        if op & 0xfb80 == 0x4880 {
            let m = self.w();
            let s = if op & 0x40 != 0 { ".l" } else { ".w" };
            let predec = (op >> 3) & 7 == 4;
            let regs = Self::regmask(m, predec);
            let ea = self.src(op, 2);
            return if op & 0x400 != 0 {
                format!("movem{s} {ea},{regs}")
            } else {
                format!("movem{s} {regs},{ea}")
            };
        }
        let n = match op & 0xff00 {
            0x4000 => "negx",
            0x4200 => "clr",
            0x4400 => "neg",
            0x4600 => "not",
            0x4a00 => "tst",
            _ => return format!("dc.w ${op:04x}"),
        };
        format!("{n}{} {}", Self::sz(size), self.src(op, size))
    }
}

