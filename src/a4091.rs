//! Commodore A4091: a Zorro III SCSI-2 controller with an NCR 53C710, driven
//! by its boot ROM (`amiga-boot-a4091.rom`, 32 KB); and the A4000T's built-in
//! SCSI, which is the same chip on the motherboard.
//!
//! The 16 MB board space holds the ROM nibble-wide from offset 0 (byte n's high
//! nibble at 4n, its low nibble at 4n + 2, each in the top of the byte), the
//! 53C710 registers at $800000 (mirrored up to $8BFFFF) and the DIP switches
//! at $8C0003. The chip is wired big-endian.

use crate::expansion::{Controller, DmaMem};
use crate::ncr710::Ncr710;
use crate::scsi::HardDisk;

const ROM_SIZE: usize = 0x8000;
const NCR: u32 = 0x80_0000;
const NCR_END: u32 = 0x8c_0000;
const DIP: u32 = 0x8c_0003;

pub struct A4091 {
    chip: Ncr710,
    rom: Vec<u8>,
    /// DIP switches as read (a closed switch reads 0): bits 0-2 the
    /// controller's own SCSI ID, bit 7 set skips scanning LUNs.
    dip: u8,
}

impl Default for A4091 {
    fn default() -> Self {
        A4091 { chip: Ncr710::default(), rom: vec![], dip: 0xff }
    }
}

impl A4091 {
    fn rom_nibble(&self, off: u32) -> u8 {
        let b = self.rom.get((off as usize >> 2) & (ROM_SIZE - 1)).copied().unwrap_or(0xff);
        match off & 3 {
            0 => b | 0x0f,
            2 => b << 4 | 0x0f,
            _ => 0xff,
        }
    }
}

impl Controller for A4091 {
    fn read8(&mut self, off: u32, _now: u64) -> u8 {
        match off {
            0..NCR => self.rom_nibble(off),
            NCR..NCR_END => self.chip.read((off as usize & 0x3f) ^ 3),
            DIP => self.dip,
            _ => 0xff,
        }
    }

    fn write8(&mut self, off: u32, v: u8, now: u64) {
        if (NCR..NCR_END).contains(&off) {
            self.chip.write((off as usize & 0x3f) ^ 3, v, now);
        }
    }

    fn peek16(&self, off: u32) -> u16 {
        if off < NCR {
            (self.rom_nibble(off) as u16) << 8 | self.rom_nibble(off + 1) as u16
        } else {
            0
        }
    }

    fn configured(&mut self, base: u32) {
        self.chip.window = (base + NCR, NCR_END - NCR);
    }

    fn tick(&mut self, now: u64, mem: &mut dyn DmaMem) {
        self.chip.tick(now, mem);
    }

    fn irq(&self) -> bool {
        self.chip.irq()
    }

    fn reset(&mut self) {
        self.chip.reset();
        self.chip.bus.reset();
    }

    fn disks(&mut self) -> Option<&mut Vec<Option<HardDisk>>> {
        Some(&mut self.chip.bus.disks)
    }

    fn activity(&self) -> u64 {
        self.chip.bus.activity
    }

    fn rom_files(&self) -> &'static [&'static str] {
        &["amiga-boot-a4091.rom"]
    }

    fn set_rom(&mut self, rom: Vec<u8>) -> Result<(), String> {
        if rom.len() != ROM_SIZE {
            return Err(format!("expected a 32 KB A4091 boot ROM, got {} bytes", rom.len()));
        }
        self.rom = rom;
        Ok(())
    }

    fn has_rom(&self) -> bool {
        !self.rom.is_empty()
    }
}

/// The A4000T's built-in SCSI: the same chip, big-endian, with its registers
/// at $DD0040-$DD007F, mirrored at $DD00C0 (where Kickstart's driver starts
/// its scripts), and no ROM of its own.
pub struct A4000tScsi {
    chip: Ncr710,
}

impl A4000tScsi {
    pub const BASE: u32 = 0xdd_0040;

    pub fn new() -> Self {
        let mut chip = Ncr710::default();
        chip.window = (Self::BASE, 0xc0);
        A4000tScsi { chip }
    }
}

impl Controller for A4000tScsi {
    /// `off` is the offset from $DD0040.
    fn read8(&mut self, off: u32, _now: u64) -> u8 {
        self.chip.read((off as usize & 0x3f) ^ 3)
    }

    fn write8(&mut self, off: u32, v: u8, now: u64) {
        self.chip.write((off as usize & 0x3f) ^ 3, v, now);
    }

    fn tick(&mut self, now: u64, mem: &mut dyn DmaMem) {
        self.chip.tick(now, mem);
    }

    fn irq(&self) -> bool {
        self.chip.irq()
    }

    fn reset(&mut self) {
        self.chip.reset();
        self.chip.bus.reset();
    }

    fn disks(&mut self) -> Option<&mut Vec<Option<HardDisk>>> {
        Some(&mut self.chip.bus.disks)
    }

    fn activity(&self) -> u64 {
        self.chip.bus.activity
    }
}
