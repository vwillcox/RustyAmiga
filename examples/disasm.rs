//! Disassemble part of a raw 68k binary: `disasm FILE START END [BASE]` (hex).
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let d = std::fs::read(&a[1]).expect("read");
    let h = |s: &str| u32::from_str_radix(s.trim_start_matches("0x"), 16).unwrap();
    let (mut pc, end) = (h(&a[2]), h(&a[3]));
    let base = a.get(4).map_or(0, |s| h(s));
    let rd = |p: u32| {
        let o = p.wrapping_sub(base) as usize;
        if o + 1 < d.len() { u16::from_be_bytes([d[o], d[o + 1]]) } else { 0 }
    };
    while pc < end {
        let (s, next) = amiga::disasm::disasm(pc, &rd);
        println!("{pc:06x}  {s}");
        pc += next;
    }
}
