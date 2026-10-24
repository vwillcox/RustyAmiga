//! Expansion boards on the autoconfig chain.
//!
//! Zorro II and Zorro III boards (and the A500/A1000 side slot, which is
//! electrically Zorro II) share one configuration chain. Kickstart's
//! expansion.library configures boards one at a time: the first unconfigured
//! board answers at $E80000 (Zorro II) or $FF000000 (Zorro III), is given an
//! address or told to shut up, and the next board in the chain takes its place.
//! Configured boards are found through a page map, so lookups stay cheap.

pub mod registry;

/// A board's autoconfig identity and address space.
#[derive(Clone, Copy, Debug)]
pub struct Autoconfig {
    pub zorro3: bool,
    /// Link the board's space into the free memory list (RAM boards).
    pub memory: bool,
    /// Offset of the DiagArea (boot ROM) within the board, if it has one.
    pub diag_rom: Option<u16>,
    /// Bytes of address space: 64 KB to 8 MB (Zorro II), up to 1 GB (Zorro III).
    pub size: u32,
    pub manufacturer: u16,
    pub product: u8,
    pub serial: u32,
}

impl Autoconfig {
    pub fn ram(zorro3: bool, size: u32, manufacturer: u16, product: u8) -> Self {
        Autoconfig { zorro3, memory: true, diag_rom: None, size, manufacturer, product, serial: 0 }
    }

    /// The 16 configuration registers (er_Type, er_Product, er_Flags, ...),
    /// before inversion.
    pub fn regs(&self) -> [u8; 16] {
        let mb = self.size >> 20;
        // Size codes: 000 = 8 MB, 001 = 64 KB ... 111 = 4 MB; Zorro III adds the
        // extended codes 000 = 16 MB ... 110 = 1 GB.
        let (code, extended): (u8, bool) = match self.size >> 16 {
            1 => (1, false),
            2 => (2, false),
            4 => (3, false),
            8 => (4, false),
            16 => (5, false),
            32 => (6, false),
            64 => (7, false),
            128 => (0, false),
            _ => match mb {
                16 => (0, true),
                32 => (1, true),
                64 => (2, true),
                128 => (3, true),
                256 => (4, true),
                512 => (5, true),
                _ => (6, true),
            },
        };
        let er_type = if self.zorro3 { 0x80 } else { 0xc0 }
            | if self.memory { 0x20 } else { 0 }
            | if self.diag_rom.is_some() { 0x10 } else { 0 }
            | code;
        // er_Flags: ERFF_MEMSPACE (prefer the 8 MB area) for Zorro II RAM,
        // ERFF_ZORRO_III and ERFF_EXTENDED for Zorro III.
        let flags = if self.zorro3 {
            (if self.memory { 0x80 } else { 0 }) | 0x10 | if extended { 0x20 } else { 0 }
        } else if self.memory {
            0x80
        } else {
            0
        };
        let rom = self.diag_rom.unwrap_or(0);
        let s = self.serial.to_be_bytes();
        [
            er_type,
            self.product,
            flags,
            0,
            (self.manufacturer >> 8) as u8,
            self.manufacturer as u8,
            s[0],
            s[1],
            s[2],
            s[3],
            (rom >> 8) as u8,
            rom as u8,
            0,
            0,
            0,
            0,
        ]
    }
}

/// Memory as a bus-mastering board sees it.
pub trait DmaMem {
    fn read8(&mut self, a: u32) -> u8;
    fn write8(&mut self, a: u32, v: u8);
}

/// An I/O board: registers, a boot ROM, interrupts, and perhaps DMA.
pub trait Controller: Send {
    fn read8(&mut self, off: u32, now: u64) -> u8;
    fn write8(&mut self, off: u32, v: u8, now: u64);
    fn read16(&mut self, off: u32, now: u64) -> u16 {
        (self.read8(off, now) as u16) << 8 | self.read8(off + 1, now) as u16
    }
    fn write16(&mut self, off: u32, v: u16, now: u64) {
        self.write8(off, (v >> 8) as u8, now);
        self.write8(off + 1, v as u8, now);
    }
    /// A side-effect-free read of the boot ROM, for the debugger.
    fn peek16(&self, _off: u32) -> u16 {
        0
    }
    /// The board has been given its base address.
    fn configured(&mut self, _base: u32) {}
    /// Advance to `now`, with access to memory for DMA.
    fn tick(&mut self, _now: u64, _mem: &mut dyn DmaMem) {}
    /// The board's interrupt (INT2) output.
    fn irq(&self) -> bool;
    fn reset(&mut self);
    /// SCSI IDs 0-7, if the board drives hard disks.
    fn disks(&mut self) -> Option<&mut Vec<Option<crate::scsi::HardDisk>>> {
        None
    }
    /// Commands run so far, for the activity light.
    fn activity(&self) -> u64 {
        0
    }
    /// Boot ROM file names the board looks for (Amiga Forever names first).
    fn rom_files(&self) -> &'static [&'static str] {
        &[]
    }
    fn set_rom(&mut self, _rom: Vec<u8>) -> Result<(), String> {
        Ok(())
    }
    fn has_rom(&self) -> bool {
        true
    }
    /// Put a disc in the board's CD drive, if it has one (the CDTV's).
    fn insert_disc(&mut self, _disc: crate::cdimage::Disc) -> bool {
        false
    }
    /// Move CD audio (44.1 kHz stereo) produced so far into `out`.
    fn take_cd_audio(&mut self, _out: &mut std::collections::VecDeque<(i16, i16)>) {}
    /// A CDTV drive the ROM has put in front-panel mode, where it handles the
    /// panel and remote's CD buttons itself instead of the keyboard sending them.
    fn front_panel_active(&self) -> bool {
        false
    }
    /// Press a CD button (0 stop, 1 play/pause, 2 previous, 3 next, 4 rewind,
    /// 5 fast forward) on a drive in front-panel mode.
    fn front_panel(&mut self, _button: u8) {}
}

/// What a card does once configured.
pub enum Device {
    /// Fast RAM.
    Ram(Vec<u8>),
    /// The A590/A2091 SCSI controller, whose state lives in `Board::scsi`.
    A2091,
    /// Any other I/O board.
    Io(Box<dyn Controller>),
    /// Stands in for an `Io` device while it runs (it is taken out to reach memory).
    Busy,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CardState {
    Waiting,
    Configured,
    ShutUp,
}

pub struct Card {
    /// Registry id of the board this card belongs to.
    pub board: &'static str,
    pub ac: Autoconfig,
    pub base: Option<u32>,
    pub state: CardState,
    pub dev: Device,
}

/// Configured Zorro II space is mapped in 64 KB pages.
const PAGE_SHIFT: u32 = 16;

#[derive(Default)]
pub struct Expansion {
    pub cards: Vec<Card>,
    /// 24-bit space: card index + 1 per 64 KB page, 0 if none.
    pages: Vec<u8>,
    /// Zorro III cards above 16 MB: (base, size, card index).
    z3: Vec<(u32, u32, usize)>,
}

impl Expansion {
    pub fn new(cards: Vec<Card>) -> Self {
        Expansion { cards, pages: vec![0; 256], z3: vec![] }
    }

    /// Every board back to unconfigured (reset).
    pub fn reset(&mut self) {
        for c in self.cards.iter_mut() {
            c.state = CardState::Waiting;
            c.base = None;
        }
        self.pages.fill(0);
        self.z3.clear();
    }

    /// The board currently answering configuration cycles.
    fn head(&self) -> Option<usize> {
        self.cards.iter().position(|c| c.state == CardState::Waiting)
    }

    fn head_regs(&self, zorro3: bool) -> Option<[u8; 16]> {
        let c = &self.cards[self.head()?];
        (c.ac.zorro3 == zorro3).then(|| c.ac.regs())
    }

    /// Zorro II configuration space at $E80000: register n's high nibble at
    /// 4n, its low nibble at 4n + 2, all but er_Type inverted.
    pub fn z2_config_read(&self, addr: u32) -> u8 {
        let Some(regs) = self.head_regs(false) else { return 0xff };
        let off = (addr & 0xff) as usize;
        let idx = off / 4;
        let byte = if idx < 16 { regs[idx] } else { 0 };
        let nib = if off & 2 == 0 { byte >> 4 } else { byte & 15 };
        let nib = if idx == 0 { nib } else { !nib & 15 };
        nib << 4 | 0x0f
    }

    /// Zorro III configuration space at $FF000000: register n's high nibble
    /// at 4n, its low nibble at 4n + $100.
    pub fn z3_config_read(&self, addr: u32) -> u8 {
        let Some(regs) = self.head_regs(true) else { return 0xff };
        let idx = (addr & 0xff) as usize / 4;
        let byte = if idx < 16 { regs[idx] } else { 0 };
        let nib = if addr & 0x100 == 0 { byte >> 4 } else { byte & 15 };
        let nib = if idx == 0 || idx >= 16 { nib } else { !nib & 15 };
        nib << 4 | 0x0f
    }

    /// A configuration write at $E80000. Returns the card configured, if one was.
    pub fn z2_config_write(&mut self, addr: u32, v: u8) -> Option<usize> {
        let i = self.head()?;
        if self.cards[i].ac.zorro3 {
            return None;
        }
        let c = &mut self.cards[i];
        match addr & 0xff {
            // A19-A16 come first; writing A23-A20 configures the board.
            0x4a => c.base = Some((c.base.unwrap_or(0) & 0xf0_0000) | (v as u32 >> 4) << 16),
            0x48 => {
                c.base = Some((v as u32 >> 4) << 20 | (c.base.unwrap_or(0) & 0x0f_0000));
                c.state = CardState::Configured;
                self.map(i);
                return Some(i);
            }
            0x4c => c.state = CardState::ShutUp,
            _ => {}
        }
        None
    }

    /// A configuration write at $FF000000.
    pub fn z3_config_write(&mut self, addr: u32, v: u16, word: bool) -> Option<usize> {
        let i = self.head()?;
        if !self.cards[i].ac.zorro3 {
            return None;
        }
        let c = &mut self.cards[i];
        match addr & 0xff {
            // Expansion writes the base address bits A31-A16 as a word.
            0x44 if word => {
                c.base = Some((v as u32) << 16);
                c.state = CardState::Configured;
                self.map(i);
                return Some(i);
            }
            0x44 => c.base = Some((c.base.unwrap_or(0) & 0x00ff_0000) | (v as u32 & 0xff) << 24),
            0x45 => c.base = Some((c.base.unwrap_or(0) & 0xff00_0000) | (v as u32 & 0xff) << 16),
            0x4c => c.state = CardState::ShutUp,
            _ => {}
        }
        None
    }

    fn map(&mut self, i: usize) {
        let c = &self.cards[i];
        let Some(base) = c.base else { return };
        log::debug!("{} configured at {base:08x} ({} KB)", c.board, c.ac.size / 1024);
        if let Device::Io(d) = &mut self.cards[i].dev {
            d.configured(base);
        }
        let c = &self.cards[i];
        if base >> 24 != 0 {
            self.z3.push((base, c.ac.size, i));
            return;
        }
        let first = (base >> PAGE_SHIFT) as usize;
        let n = (c.ac.size >> PAGE_SHIFT).max(1) as usize;
        for p in self.pages.iter_mut().skip(first).take(n) {
            *p = i as u8 + 1;
        }
    }

    /// The configured card covering a 24-bit address: (card, offset).
    #[inline]
    pub fn lookup24(&self, a: u32) -> Option<(usize, u32)> {
        let p = self.pages[((a & 0xff_ffff) >> PAGE_SHIFT) as usize];
        if p == 0 {
            return None;
        }
        let i = p as usize - 1;
        Some((i, a.wrapping_sub(self.cards[i].base?)))
    }

    /// The configured Zorro III card covering a 32-bit address.
    #[inline]
    pub fn lookup32(&self, a: u32) -> Option<(usize, u32)> {
        self.z3.iter().find(|(b, s, _)| a.wrapping_sub(*b) < *s).map(|&(b, _, i)| (i, a - b))
    }

    /// The I/O boards on the chain.
    pub fn io(&mut self) -> impl Iterator<Item = &mut Box<dyn Controller>> {
        self.cards.iter_mut().filter_map(|c| match &mut c.dev {
            Device::Io(d) => Some(d),
            _ => None,
        })
    }

    /// The I/O board of card `i`.
    #[inline]
    pub fn io_at(&mut self, i: usize) -> &mut dyn Controller {
        match &mut self.cards[i].dev {
            Device::Io(d) => d.as_mut(),
            _ => unreachable!("card {i} is not an I/O board"),
        }
    }

    /// RAM of card `i` (which must be a RAM card).
    #[inline]
    pub fn ram_ref(&self, i: usize) -> &Vec<u8> {
        match &self.cards[i].dev {
            Device::Ram(v) => v,
            _ => unreachable!("card {i} has no RAM"),
        }
    }

    /// RAM of card `i`, if it is a RAM card.
    #[inline]
    pub fn ram(&mut self, i: usize) -> Option<&mut Vec<u8>> {
        match &mut self.cards[i].dev {
            Device::Ram(v) => Some(v),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ram_card(zorro3: bool, size: u32) -> Card {
        Card { board: "test", ac: Autoconfig::ram(zorro3, size, 2011, 1), base: None, state: CardState::Waiting, dev: Device::Ram(vec![0; size as usize]) }
    }

    #[test]
    fn size_codes() {
        assert_eq!(Autoconfig::ram(false, 8 << 20, 1, 1).regs()[0], 0xe0);
        assert_eq!(Autoconfig::ram(false, 2 << 20, 1, 1).regs()[0], 0xe6);
        let z3 = Autoconfig::ram(true, 64 << 20, 1, 1).regs();
        assert_eq!((z3[0], z3[2]), (0xa2, 0xb0));
        // The A4091's ROM carries the same values in its own autoconfig nibbles.
        let a4091 = Autoconfig { zorro3: true, memory: false, diag_rom: Some(0x200), size: 16 << 20, manufacturer: 514, product: 0x54, serial: 0x20000 };
        assert_eq!(a4091.regs()[..12], [0x90, 0x54, 0x30, 0, 2, 2, 0, 2, 0, 0, 2, 0]);
    }

    #[test]
    fn chain_configures_boards_in_order() {
        let mut e = Expansion::new(vec![ram_card(false, 2 << 20), ram_card(false, 1 << 20), ram_card(true, 16 << 20)]);
        // er_Type of the first board, read uninverted at $E80000.
        assert_eq!(e.z2_config_read(0xe8_0000) & 0xf0, 0xe0);
        e.z2_config_write(0xe8_004a, 0x00);
        assert_eq!(e.z2_config_write(0xe8_0048, 0x20), Some(0));
        assert_eq!(e.lookup24(0x21_0000), Some((0, 0x1_0000)));
        // The second board now answers; shut it up.
        e.z2_config_write(0xe8_004c, 0);
        assert_eq!(e.cards[1].state, CardState::ShutUp);
        // The Zorro III board answers at $FF000000 only.
        assert_eq!(e.z2_config_read(0xe8_0000), 0xff);
        assert_eq!(e.z3_config_write(0xff00_0044, 0x4000, true), Some(2));
        assert_eq!(e.lookup32(0x4000_1234), Some((2, 0x1234)));
        e.reset();
        assert_eq!(e.lookup24(0x21_0000), None);
    }
}
