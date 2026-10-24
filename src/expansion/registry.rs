//! Every add-on board the emulator offers, and how to build it.
//!
//! A board is chosen with `--board ID[:option=value,...]`. It contributes one
//! or more cards to the autoconfig chain (an A2091 with RAM is two: the SCSI
//! controller and its memory). Manufacturer and product numbers are the ones
//! the real boards report, so their own drivers and diagnostics recognise them.

use super::{Autoconfig, Card, CardState, Device};
use crate::config::Model;
use crate::rtc::RtcKind;

/// Where a board plugs in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Slot {
    /// A Zorro II slot, or the A500/A1000 86-pin side slot (same bus).
    Zorro2,
    /// A Zorro III slot (A3000/A4000); Zorro II boards fit there too.
    Zorro3,
    /// The trapdoor (memory/CPU expansion slot underneath) of one model.
    Trapdoor(Model),
}

impl Slot {
    pub fn fits(self, model: Model) -> bool {
        match self {
            // Every Amiga but the CD32 takes Zorro II boards: in slots, through
            // the A500/A1000 side expansion, or via a sidecar/bridge on the
            // A600/A1200 (which is how these boards were used there).
            Slot::Zorro2 => model != Model::Cd32,
            Slot::Zorro3 => model.addr32(),
            Slot::Trapdoor(m) => m == model,
        }
    }

    pub fn describe(self) -> String {
        match self {
            Slot::Zorro2 => "Zorro II".into(),
            Slot::Zorro3 => "Zorro III".into(),
            Slot::Trapdoor(m) => format!("{m:?} trapdoor"),
        }
    }
}

/// What a board adds to the machine.
#[derive(Default)]
pub struct Parts {
    /// Cards on the autoconfig chain.
    pub cards: Vec<Card>,
    /// Trapdoor slow RAM ($C00000) in KB.
    pub slow_kb: Option<u32>,
    /// Chip RAM in KB (boards that add chip RAM).
    pub chip_kb: Option<u32>,
    /// A battery-backed clock.
    pub rtc: Option<RtcKind>,
}

impl From<Vec<Card>> for Parts {
    fn from(cards: Vec<Card>) -> Self {
        Parts { cards, ..Default::default() }
    }
}

pub struct BoardInfo {
    pub id: &'static str,
    pub name: &'static str,
    pub maker: &'static str,
    pub year: u16,
    pub slot: Slot,
    /// What the options mean, for `--list-boards`.
    pub options: &'static str,
    build: fn(&Options) -> Result<Parts, String>,
}

impl BoardInfo {
    /// What the board adds, with the given options.
    pub fn build(&'static self, opts: &Options) -> Result<Parts, String> {
        (self.build)(opts)
    }
}

/// `key=value` options given after the board id.
#[derive(Default, Clone, Debug)]
pub struct Options(pub Vec<(String, String)>);

impl Options {
    pub fn parse(s: &str) -> Result<Self, String> {
        let mut v = vec![];
        for part in s.split(',').filter(|p| !p.is_empty()) {
            let (k, val) = part.split_once('=').ok_or_else(|| format!("option '{part}' should be key=value"))?;
            v.push((k.trim().to_ascii_lowercase(), val.trim().to_string()));
        }
        Ok(Options(v))
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    /// A size in KB from `key` ("2048", "2M", "512K"), one of `allowed`.
    fn size_kb(&self, key: &str, default: u32, allowed: &[u32]) -> Result<u32, String> {
        let Some(v) = self.get(key) else { return Ok(default) };
        let v = v.to_ascii_uppercase();
        let kb = if let Some(m) = v.strip_suffix('M').or_else(|| v.strip_suffix("MB")) {
            m.parse::<u32>().map(|m| m * 1024)
        } else {
            v.trim_end_matches("KB").trim_end_matches('K').parse::<u32>()
        }
        .map_err(|_| format!("bad size '{v}'"))?;
        if allowed.contains(&kb) {
            Ok(kb)
        } else {
            let list: Vec<String> = allowed.iter().map(|k| if k % 1024 == 0 { format!("{}M", k / 1024) } else { format!("{k}K") }).collect();
            Err(format!("{key}={v}: this board takes {}", list.join(", ")))
        }
    }
}

/// A RAM card of `kb` kilobytes.
fn ram_card(board: &'static str, zorro3: bool, kb: u32, manufacturer: u16, product: u8) -> Card {
    let size = kb * 1024;
    Card {
        board,
        ac: Autoconfig::ram(zorro3, size, manufacturer, product),
        base: None,
        state: CardState::Waiting,
        dev: Device::Ram(vec![0; size as usize]),
    }
}

const Z2_SIZES: &[u32] = &[64, 128, 256, 512, 1024, 2048, 4096, 8192];
const Z3_SIZES: &[u32] = &[4096, 8192, 16384, 32768, 65536, 131072, 262144, 524288, 1048576];

/// Manufacturer numbers.
const COMMODORE_WC: u16 = 514;
/// The id the generic boards use (as UAE's own expansions do).
const GENERIC: u16 = 2011;
const GVP: u16 = 2017;
const SUPRA: u16 = 1056;
const MICROBOTICS: u16 = 1010;
const BSC: u16 = 2092;

macro_rules! ram_board {
    ($id:literal, $name:literal, $maker:literal, $year:literal, $slot:expr, $manuf:expr, $prod:literal, $default:literal, $sizes:expr) => {
        BoardInfo {
            id: $id,
            name: $name,
            maker: $maker,
            year: $year,
            slot: $slot,
            options: concat!("ram=SIZE (default ", $default, "K)"),
            build: |o| Ok(vec![ram_card($id, $slot == Slot::Zorro3, o.size_kb("ram", $default, $sizes)?, $manuf, $prod)].into()),
        }
    };
}

pub static BOARDS: &[BoardInfo] = &[
    ram_board!("z2ram", "Zorro II fast RAM", "generic", 1986, Slot::Zorro2, GENERIC, 1, 8192, Z2_SIZES),
    ram_board!("z3ram", "Zorro III fast RAM", "generic", 1990, Slot::Zorro3, GENERIC, 2, 65536, Z3_SIZES),
    ram_board!("a2052", "A2052 2 MB RAM", "Commodore", 1987, Slot::Zorro2, COMMODORE_WC, 10, 2048, &[512, 1024, 2048]),
    ram_board!("a2058", "A2058 RAM", "Commodore", 1988, Slot::Zorro2, COMMODORE_WC, 10, 8192, &[2048, 4096, 8192]),
    ram_board!("a560", "A560 RAM (A500 side slot)", "Commodore", 1988, Slot::Zorro2, COMMODORE_WC, 32, 2048, &[512, 1024, 2048]),
    ram_board!("supra500rx", "Supra 500RX", "Supra", 1989, Slot::Zorro2, SUPRA, 10, 8192, &[512, 1024, 2048, 4096, 8192]),
    ram_board!("supra2000", "SupraRAM 2000", "Supra", 1989, Slot::Zorro2, SUPRA, 9, 8192, &[2048, 4096, 8192]),
    ram_board!("microbotics8up", "8-Up! (Rev Z)", "Microbotics", 1989, Slot::Zorro2, MICROBOTICS, 4, 8192, &[2048, 4096, 8192]),
    ram_board!("gvpimpact2ram", "Impact Series II RAM", "GVP", 1990, Slot::Zorro2, GVP, 10, 8192, &[2048, 4096, 8192]),
    ram_board!("oktagon2008ram", "Oktagon 2008 RAM", "BSC", 1992, Slot::Zorro2, BSC, 8, 8192, &[2048, 4096, 8192]),
    BoardInfo {
        id: "a2091",
        name: "A2091 / A590 SCSI controller",
        maker: "Commodore",
        year: 1989,
        slot: Slot::Zorro2,
        options: "ram=SIZE (0, 512K, 1M or 2M fitted on the board; default 0)",
        build: |o| {
            let mut cards = vec![Card {
                board: "a2091",
                ac: Autoconfig {
                    zorro3: false,
                    memory: false,
                    diag_rom: Some(crate::a2091::ROM_OFFSET as u16),
                    size: 0x1_0000,
                    manufacturer: COMMODORE_WC,
                    product: 3,
                    serial: 0,
                },
                base: None,
                state: CardState::Waiting,
                dev: Device::A2091,
            }];
            let kb = o.size_kb("ram", 0, &[0, 512, 1024, 2048])?;
            if kb > 0 {
                cards.push(ram_card("a2091", false, kb, COMMODORE_WC, 10));
            }
            Ok(cards.into())
        },
    },
    BoardInfo {
        id: "a4091",
        name: "A4091 SCSI-2 controller (NCR 53C710)",
        maker: "Commodore",
        year: 1993,
        slot: Slot::Zorro3,
        options: "none",
        build: |_| {
            Ok(vec![Card {
                board: "a4091",
                ac: Autoconfig {
                    zorro3: true,
                    memory: false,
                    diag_rom: Some(0x200),
                    size: 16 << 20,
                    manufacturer: COMMODORE_WC,
                    product: 0x54,
                    serial: 0x2_0000,
                },
                base: None,
                state: CardState::Waiting,
                dev: Device::Io(Box::<crate::a4091::A4091>::default()),
            }]
            .into())
        },
    },
    BoardInfo {
        id: "uaegfx",
        name: "UAE virtual graphics card (Picasso96 uaegfx)",
        maker: "UAE",
        year: 2000,
        slot: Slot::Zorro3,
        options: "mem=SIZE video memory (4M to 256M, default 16M)",
        build: |o| {
            let kb = o.size_kb("mem", 16384, &Z3_SIZES[..7])?;
            Ok(vec![Card {
                board: "uaegfx",
                ac: Autoconfig {
                    zorro3: true,
                    memory: false,
                    diag_rom: None,
                    size: kb * 1024,
                    manufacturer: crate::rtg::MANUFACTURER,
                    product: crate::rtg::PRODUCT,
                    serial: 0,
                },
                base: None,
                state: CardState::Waiting,
                dev: Device::Ram(vec![0; kb as usize * 1024]),
            }]
            .into())
        },
    },
    BoardInfo {
        id: "a501",
        name: "A501 512 KB RAM and clock",
        maker: "Commodore",
        year: 1987,
        slot: Slot::Trapdoor(Model::A500),
        options: "none",
        build: |_| Ok(Parts { slow_kb: Some(512), rtc: Some(RtcKind::Msm6242), ..Default::default() }),
    },
    BoardInfo {
        id: "a601",
        name: "A601 1 MB chip RAM and clock",
        maker: "Commodore",
        year: 1992,
        slot: Slot::Trapdoor(Model::A600),
        options: "none",
        build: |_| Ok(Parts { chip_kb: Some(2048), rtc: Some(RtcKind::Msm6242), ..Default::default() }),
    },
    BoardInfo {
        id: "mbx1200z",
        name: "MBX 1200z (RAM, FPU socket and clock)",
        maker: "Microbotics",
        year: 1993,
        slot: Slot::Trapdoor(Model::A1200),
        options: "ram=SIZE (1M, 2M, 4M or 8M; default 4M)",
        build: |o| {
            let kb = o.size_kb("ram", 4096, &[1024, 2048, 4096, 8192])?;
            Ok(Parts { cards: vec![ram_card("mbx1200z", false, kb, MICROBOTICS, 0x81)], rtc: Some(RtcKind::Msm6242), ..Default::default() })
        },
    },
];

pub fn find(id: &str) -> Option<&'static BoardInfo> {
    BOARDS.iter().find(|b| b.id.eq_ignore_ascii_case(id))
}

/// Parse `ID[:option=value,...]`.
pub fn parse_spec(spec: &str) -> Result<(&'static BoardInfo, Options), String> {
    let (id, opts) = spec.split_once(':').unwrap_or((spec, ""));
    let info = find(id).ok_or_else(|| format!("unknown board '{id}' (--list-boards lists them)"))?;
    Ok((info, Options::parse(opts)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specs_parse_and_build() {
        let (b, o) = parse_spec("a2058:ram=4M").unwrap();
        let cards = b.build(&o).unwrap().cards;
        assert_eq!(cards[0].ac.size, 4 << 20);
        assert!(parse_spec("a2058:ram=3M").and_then(|(b, o)| b.build(&o)).is_err());
        let (b, o) = parse_spec("a2091:ram=2M").unwrap();
        assert_eq!(b.build(&o).unwrap().cards.len(), 2);
        assert!(parse_spec("nonesuch").is_err());
    }

    #[test]
    fn ids_are_unique() {
        for (i, a) in BOARDS.iter().enumerate() {
            assert!(BOARDS[i + 1..].iter().all(|b| b.id != a.id), "duplicate {}", a.id);
        }
    }
}
