//! Variable-length code tables of ISO 11172-2 Annex B, decoded through lookup
//! tables indexed by the next bits of the stream.

use super::Bits;
use std::sync::OnceLock;

/// The escape code in `DCT_COEFF`.
pub(super) const ESCAPE: u16 = 0xffff;
/// Address increment values for macroblock_stuffing and macroblock_escape.
pub(super) const MB_STUFFING: i16 = 34;
pub(super) const MB_ESCAPE: i16 = 35;

/// A code table expanded so that the next `bits` bits of the stream index the
/// code's length and value (length 0: not a valid code).
pub(super) struct Vlc<T> {
    bits: u32,
    lut: Vec<(u8, T)>,
}

impl<T: Copy + Default> Vlc<T> {
    fn new(codes: &[(u8, u16, T)]) -> Self {
        let bits = codes.iter().map(|c| c.0 as u32).max().unwrap_or(1);
        let mut lut = vec![(0, T::default()); 1 << bits];
        for &(len, code, value) in codes {
            let shift = bits - len as u32;
            let first = (code as usize) << shift;
            for e in &mut lut[first..first + (1 << shift)] {
                *e = (len, value);
            }
        }
        Vlc { bits, lut }
    }

    #[inline]
    pub(super) fn read(&self, b: &mut Bits) -> Option<T> {
        let (len, v) = self.lut[b.peek(self.bits) as usize];
        if len == 0 {
            return None;
        }
        b.skip(len as u32);
        Some(v)
    }
}

pub(super) struct Tables {
    pub mb_address_increment: Vlc<i16>,
    /// macroblock_type for I, P, B and D pictures (by picture_coding_type - 1).
    pub mb_type: [Vlc<u8>; 4],
    pub coded_block_pattern: Vlc<u8>,
    pub motion_code: Vlc<i16>,
    pub dc_size: [Vlc<u8>; 2],
    pub dct_coeff: Vlc<u16>,
}

pub(super) fn tables() -> &'static Tables {
    static T: OnceLock<Tables> = OnceLock::new();
    T.get_or_init(|| Tables {
        mb_address_increment: Vlc::new(MB_ADDRESS_INCREMENT),
        mb_type: [Vlc::new(MB_TYPE_I), Vlc::new(MB_TYPE_P), Vlc::new(MB_TYPE_B), Vlc::new(MB_TYPE_D)],
        coded_block_pattern: Vlc::new(CODED_BLOCK_PATTERN),
        motion_code: Vlc::new(MOTION_CODE),
        dc_size: [Vlc::new(DCT_DC_SIZE_LUMA), Vlc::new(DCT_DC_SIZE_CHROMA)],
        dct_coeff: Vlc::new(DCT_COEFF),
    })
}

// macroblock_type flags.
pub(super) const MB_INTRA: u8 = 0x01;
pub(super) const MB_PATTERN: u8 = 0x02;
pub(super) const MB_BACKWARD: u8 = 0x04;
pub(super) const MB_FORWARD: u8 = 0x08;
pub(super) const MB_QUANT: u8 = 0x10;

/// macroblock_type in D-pictures (DC coefficients only).
const MB_TYPE_D: &[(u8, u16, u8)] = &[(1, 0b1, MB_INTRA)];

/// Table B.1: macroblock_address_increment (34 is stuffing, 35 escape).
const MB_ADDRESS_INCREMENT: &[(u8, u16, i16)] = &[
    (1, 0b1, 1),
    (3, 0b010, 3),
    (3, 0b011, 2),
    (4, 0b0010, 5),
    (4, 0b0011, 4),
    (5, 0b00010, 7),
    (5, 0b00011, 6),
    (7, 0b0000110, 9),
    (7, 0b0000111, 8),
    (8, 0b00000110, 15),
    (8, 0b00000111, 14),
    (8, 0b00001000, 13),
    (8, 0b00001001, 12),
    (8, 0b00001010, 11),
    (8, 0b00001011, 10),
    (10, 0b0000010010, 21),
    (10, 0b0000010011, 20),
    (10, 0b0000010100, 19),
    (10, 0b0000010101, 18),
    (10, 0b0000010110, 17),
    (10, 0b0000010111, 16),
    (11, 0b00000001000, 35),
    (11, 0b00000001111, 34),
    (11, 0b00000011000, 33),
    (11, 0b00000011001, 32),
    (11, 0b00000011010, 31),
    (11, 0b00000011011, 30),
    (11, 0b00000011100, 29),
    (11, 0b00000011101, 28),
    (11, 0b00000011110, 27),
    (11, 0b00000011111, 26),
    (11, 0b00000100000, 25),
    (11, 0b00000100001, 24),
    (11, 0b00000100010, 23),
    (11, 0b00000100011, 22),
];

/// Table B.2a: macroblock_type in I-pictures.
const MB_TYPE_I: &[(u8, u16, u8)] = &[
    (1, 0b1, MB_INTRA),
    (2, 0b01, MB_QUANT | MB_INTRA),
];

/// Table B.2b: macroblock_type in P-pictures.
const MB_TYPE_P: &[(u8, u16, u8)] = &[
    (1, 0b1, MB_FORWARD | MB_PATTERN),
    (2, 0b01, MB_PATTERN),
    (3, 0b001, MB_FORWARD),
    (5, 0b00001, MB_QUANT | MB_PATTERN),
    (5, 0b00010, MB_QUANT | MB_FORWARD | MB_PATTERN),
    (5, 0b00011, MB_INTRA),
    (6, 0b000001, MB_QUANT | MB_INTRA),
];

/// Table B.2c: macroblock_type in B-pictures.
const MB_TYPE_B: &[(u8, u16, u8)] = &[
    (2, 0b10, MB_FORWARD | MB_BACKWARD),
    (2, 0b11, MB_FORWARD | MB_BACKWARD | MB_PATTERN),
    (3, 0b010, MB_BACKWARD),
    (3, 0b011, MB_BACKWARD | MB_PATTERN),
    (4, 0b0010, MB_FORWARD),
    (4, 0b0011, MB_FORWARD | MB_PATTERN),
    (5, 0b00010, MB_QUANT | MB_FORWARD | MB_BACKWARD | MB_PATTERN),
    (5, 0b00011, MB_INTRA),
    (6, 0b000001, MB_QUANT | MB_INTRA),
    (6, 0b000010, MB_QUANT | MB_BACKWARD | MB_PATTERN),
    (6, 0b000011, MB_QUANT | MB_FORWARD | MB_PATTERN),
];

/// Table B.3: coded_block_pattern.
const CODED_BLOCK_PATTERN: &[(u8, u16, u8)] = &[
    (3, 0b111, 60),
    (4, 0b1010, 32),
    (4, 0b1011, 16),
    (4, 0b1100, 8),
    (4, 0b1101, 4),
    (5, 0b01000, 62),
    (5, 0b01001, 2),
    (5, 0b01010, 61),
    (5, 0b01011, 1),
    (5, 0b01100, 56),
    (5, 0b01101, 52),
    (5, 0b01110, 44),
    (5, 0b01111, 28),
    (5, 0b10000, 40),
    (5, 0b10001, 20),
    (5, 0b10010, 48),
    (5, 0b10011, 12),
    (6, 0b001100, 63),
    (6, 0b001101, 3),
    (6, 0b001110, 36),
    (6, 0b001111, 24),
    (7, 0b0010000, 34),
    (7, 0b0010001, 18),
    (7, 0b0010010, 10),
    (7, 0b0010011, 6),
    (7, 0b0010100, 33),
    (7, 0b0010101, 17),
    (7, 0b0010110, 9),
    (7, 0b0010111, 5),
    (8, 0b00000100, 58),
    (8, 0b00000101, 54),
    (8, 0b00000110, 46),
    (8, 0b00000111, 30),
    (8, 0b00001000, 57),
    (8, 0b00001001, 53),
    (8, 0b00001010, 45),
    (8, 0b00001011, 29),
    (8, 0b00001100, 38),
    (8, 0b00001101, 26),
    (8, 0b00001110, 37),
    (8, 0b00001111, 25),
    (8, 0b00010000, 43),
    (8, 0b00010001, 23),
    (8, 0b00010010, 51),
    (8, 0b00010011, 15),
    (8, 0b00010100, 42),
    (8, 0b00010101, 22),
    (8, 0b00010110, 50),
    (8, 0b00010111, 14),
    (8, 0b00011000, 41),
    (8, 0b00011001, 21),
    (8, 0b00011010, 49),
    (8, 0b00011011, 13),
    (8, 0b00011100, 35),
    (8, 0b00011101, 19),
    (8, 0b00011110, 11),
    (8, 0b00011111, 7),
    (9, 0b000000010, 39),
    (9, 0b000000011, 27),
    (9, 0b000000100, 59),
    (9, 0b000000101, 55),
    (9, 0b000000110, 47),
    (9, 0b000000111, 31),
];

/// Table B.4: motion_code, with its sign bit.
const MOTION_CODE: &[(u8, u16, i16)] = &[
    (1, 0b1, 0),
    (3, 0b010, 1),
    (3, 0b011, -1),
    (4, 0b0010, 2),
    (4, 0b0011, -2),
    (5, 0b00010, 3),
    (5, 0b00011, -3),
    (7, 0b0000110, 4),
    (7, 0b0000111, -4),
    (8, 0b00000110, 7),
    (8, 0b00000111, -7),
    (8, 0b00001000, 6),
    (8, 0b00001001, -6),
    (8, 0b00001010, 5),
    (8, 0b00001011, -5),
    (10, 0b0000010010, 10),
    (10, 0b0000010011, -10),
    (10, 0b0000010100, 9),
    (10, 0b0000010101, -9),
    (10, 0b0000010110, 8),
    (10, 0b0000010111, -8),
    (11, 0b00000011000, 16),
    (11, 0b00000011001, -16),
    (11, 0b00000011010, 15),
    (11, 0b00000011011, -15),
    (11, 0b00000011100, 14),
    (11, 0b00000011101, -14),
    (11, 0b00000011110, 13),
    (11, 0b00000011111, -13),
    (11, 0b00000100000, 12),
    (11, 0b00000100001, -12),
    (11, 0b00000100010, 11),
    (11, 0b00000100011, -11),
];

/// Table B.5a: dct_dc_size_luminance.
const DCT_DC_SIZE_LUMA: &[(u8, u16, u8)] = &[
    (2, 0b00, 1),
    (2, 0b01, 2),
    (3, 0b100, 0),
    (3, 0b101, 3),
    (3, 0b110, 4),
    (4, 0b1110, 5),
    (5, 0b11110, 6),
    (6, 0b111110, 7),
    (7, 0b1111110, 8),
];

/// Table B.5b: dct_dc_size_chrominance.
const DCT_DC_SIZE_CHROMA: &[(u8, u16, u8)] = &[
    (2, 0b00, 0),
    (2, 0b01, 1),
    (2, 0b10, 2),
    (3, 0b110, 3),
    (4, 0b1110, 4),
    (5, 0b11110, 5),
    (6, 0b111110, 6),
    (7, 0b1111110, 7),
    (8, 0b11111110, 8),
];

/// Table B.5c: dct_coeff_first/next as run << 8 | level, without the sign bit. ESCAPE
/// is followed by a 6-bit run and the level; "1" is run 0, level 1 as a first
/// coefficient and the start of "10" (end of block) or "11" afterwards.
const DCT_COEFF: &[(u8, u16, u16)] = &[
    (1, 0b1, 0x1),
    (3, 0b011, 0x101),
    (4, 0b0100, 0x2),
    (4, 0b0101, 0x201),
    (5, 0b00101, 0x3),
    (5, 0b00110, 0x401),
    (5, 0b00111, 0x301),
    (6, 0b000001, ESCAPE),
    (6, 0b000100, 0x701),
    (6, 0b000101, 0x601),
    (6, 0b000110, 0x102),
    (6, 0b000111, 0x501),
    (7, 0b0000100, 0x202),
    (7, 0b0000101, 0x901),
    (7, 0b0000110, 0x4),
    (7, 0b0000111, 0x801),
    (8, 0b00100000, 0xd01),
    (8, 0b00100001, 0x6),
    (8, 0b00100010, 0xc01),
    (8, 0b00100011, 0xb01),
    (8, 0b00100100, 0x302),
    (8, 0b00100101, 0x103),
    (8, 0b00100110, 0x5),
    (8, 0b00100111, 0xa01),
    (10, 0b0000001000, 0x1001),
    (10, 0b0000001001, 0x502),
    (10, 0b0000001010, 0x7),
    (10, 0b0000001011, 0x203),
    (10, 0b0000001100, 0x104),
    (10, 0b0000001101, 0xf01),
    (10, 0b0000001110, 0xe01),
    (10, 0b0000001111, 0x402),
    (12, 0b000000010000, 0xb),
    (12, 0b000000010001, 0x802),
    (12, 0b000000010010, 0x403),
    (12, 0b000000010011, 0xa),
    (12, 0b000000010100, 0x204),
    (12, 0b000000010101, 0x702),
    (12, 0b000000010110, 0x1501),
    (12, 0b000000010111, 0x1401),
    (12, 0b000000011000, 0x9),
    (12, 0b000000011001, 0x1301),
    (12, 0b000000011010, 0x1201),
    (12, 0b000000011011, 0x105),
    (12, 0b000000011100, 0x303),
    (12, 0b000000011101, 0x8),
    (12, 0b000000011110, 0x602),
    (12, 0b000000011111, 0x1101),
    (13, 0b0000000010000, 0xa02),
    (13, 0b0000000010001, 0x902),
    (13, 0b0000000010010, 0x503),
    (13, 0b0000000010011, 0x304),
    (13, 0b0000000010100, 0x205),
    (13, 0b0000000010101, 0x107),
    (13, 0b0000000010110, 0x106),
    (13, 0b0000000010111, 0xf),
    (13, 0b0000000011000, 0xe),
    (13, 0b0000000011001, 0xd),
    (13, 0b0000000011010, 0xc),
    (13, 0b0000000011011, 0x1a01),
    (13, 0b0000000011100, 0x1901),
    (13, 0b0000000011101, 0x1801),
    (13, 0b0000000011110, 0x1701),
    (13, 0b0000000011111, 0x1601),
    (14, 0b00000000010000, 0x1f),
    (14, 0b00000000010001, 0x1e),
    (14, 0b00000000010010, 0x1d),
    (14, 0b00000000010011, 0x1c),
    (14, 0b00000000010100, 0x1b),
    (14, 0b00000000010101, 0x1a),
    (14, 0b00000000010110, 0x19),
    (14, 0b00000000010111, 0x18),
    (14, 0b00000000011000, 0x17),
    (14, 0b00000000011001, 0x16),
    (14, 0b00000000011010, 0x15),
    (14, 0b00000000011011, 0x14),
    (14, 0b00000000011100, 0x13),
    (14, 0b00000000011101, 0x12),
    (14, 0b00000000011110, 0x11),
    (14, 0b00000000011111, 0x10),
    (15, 0b000000000010000, 0x28),
    (15, 0b000000000010001, 0x27),
    (15, 0b000000000010010, 0x26),
    (15, 0b000000000010011, 0x25),
    (15, 0b000000000010100, 0x24),
    (15, 0b000000000010101, 0x23),
    (15, 0b000000000010110, 0x22),
    (15, 0b000000000010111, 0x21),
    (15, 0b000000000011000, 0x20),
    (15, 0b000000000011001, 0x10e),
    (15, 0b000000000011010, 0x10d),
    (15, 0b000000000011011, 0x10c),
    (15, 0b000000000011100, 0x10b),
    (15, 0b000000000011101, 0x10a),
    (15, 0b000000000011110, 0x109),
    (15, 0b000000000011111, 0x108),
    (16, 0b0000000000010000, 0x112),
    (16, 0b0000000000010001, 0x111),
    (16, 0b0000000000010010, 0x110),
    (16, 0b0000000000010011, 0x10f),
    (16, 0b0000000000010100, 0x603),
    (16, 0b0000000000010101, 0x1002),
    (16, 0b0000000000010110, 0xf02),
    (16, 0b0000000000010111, 0xe02),
    (16, 0b0000000000011000, 0xd02),
    (16, 0b0000000000011001, 0xc02),
    (16, 0b0000000000011010, 0xb02),
    (16, 0b0000000000011011, 0x1f01),
    (16, 0b0000000000011100, 0x1e01),
    (16, 0b0000000000011101, 0x1d01),
    (16, 0b0000000000011110, 0x1c01),
    (16, 0b0000000000011111, 0x1b01),
];
