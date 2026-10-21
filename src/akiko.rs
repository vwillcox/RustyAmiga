//! Akiko, the CD32's custom chip at $B80000: the chip ID, the chunky-to-planar
//! converter, the I2C lines of the settings NVRAM, and the CD-ROM controller.
//!
//! No CD drive is emulated yet: its registers read as idle, so the CD32 boots
//! to its "insert a disc" screen.
//!
//! Registers (byte offsets from $B80000):
//! - $02-$05: ID, $C0CACAFE
//! - $30-$33: NVRAM I2C port (direction at $32, data at $33: bit 7 = SCL, bit 6 = SDA)
//! - $38-$3B: C2P. Write eight longs of chunky pixels (four pixels per long),
//!   then read back eight longs: one 32-pixel row of each of the eight planes.

/// Settings NVRAM (a 24C08: 1 KB behind I2C).
const NVRAM_SIZE: usize = 1024;

#[derive(Default)]
enum I2c {
    #[default]
    Idle,
    /// Receiving a byte from the CPU; `bits` so far.
    Rx { byte: u8, bits: u8 },
    /// Sending `byte` to the CPU, MSB first.
    Tx { byte: u8, bits: u8 },
    /// Clock pulse for the acknowledge bit after a received byte.
    AckOut,
    /// Waiting for the CPU's acknowledge after a sent byte.
    AckIn,
}

pub struct Akiko {
    c2p_in: [u32; 8],
    c2p_out: [u32; 8],
    write_idx: usize,
    read_idx: usize,

    nvram: Vec<u8>,
    io_dir: u8,
    io_out: u8,
    i2c: I2c,
    /// Bytes received since START: device address, then word address.
    rx_count: u32,
    reading: bool,
    addr: usize,
    /// SDA as driven by the EEPROM (true = released, reads high).
    sda_eeprom: bool,
    scl: bool,
    sda: bool,
}

impl Default for Akiko {
    fn default() -> Self {
        Self::new()
    }
}

impl Akiko {
    pub fn new() -> Self {
        Akiko {
            c2p_in: [0; 8],
            c2p_out: [0; 8],
            write_idx: 0,
            read_idx: 0,
            nvram: vec![0; NVRAM_SIZE],
            io_dir: 0,
            io_out: 0xff,
            i2c: I2c::Idle,
            rx_count: 0,
            reading: false,
            addr: 0,
            sda_eeprom: true,
            scl: true,
            sda: true,
        }
    }

    pub fn read8(&mut self, off: u32) -> u8 {
        match off & 0xff {
            0x02 => 0xc0,
            0x03 => 0xca,
            0x04 => 0xca,
            0x05 => 0xfe,
            0x32 => self.io_dir,
            0x33 => {
                // Lines set as outputs read back what the CPU drives; SDA as an
                // input reads the EEPROM (and the bus pull-up).
                let sda_in = if self.io_dir & 0x40 != 0 { self.sda } else { self.sda && self.sda_eeprom };
                (self.io_out & 0x3f) | (self.scl as u8) << 7 | (sda_in as u8) << 6
            }
            o @ 0x38..=0x3b => self.c2p_read(o - 0x38),
            _ => 0,
        }
    }

    pub fn write8(&mut self, off: u32, v: u8) {
        match off & 0xff {
            0x32 => {
                self.io_dir = v;
                self.update_lines();
            }
            0x33 => {
                self.io_out = v;
                self.update_lines();
            }
            o @ 0x38..=0x3b => self.c2p_write(o - 0x38, v),
            _ => {}
        }
    }

    // ---- C2P ------------------------------------------------------------------

    fn c2p_write(&mut self, byte: u32, v: u8) {
        let shift = 8 * (3 - byte);
        let w = &mut self.c2p_in[self.write_idx];
        *w = (*w & !(0xff << shift)) | (v as u32) << shift;
        if byte == 3 {
            self.write_idx = (self.write_idx + 1) & 7;
        }
        self.read_idx = 0;
    }

    fn c2p_read(&mut self, byte: u32) -> u8 {
        if self.read_idx == 0 && byte == 0 {
            self.convert();
        }
        self.write_idx = 0;
        let v = (self.c2p_out[self.read_idx] >> (8 * (3 - byte))) as u8;
        if byte == 3 {
            self.read_idx = (self.read_idx + 1) & 7;
        }
        v
    }

    /// 32 chunky pixels (one byte each, eight longs in) to 32 bits of each of
    /// the eight planes (eight longs out, plane 0 first).
    fn convert(&mut self) {
        self.c2p_out = [0; 8];
        for (px, chunk) in self.c2p_in.iter().flat_map(|l| l.to_be_bytes()).enumerate() {
            for (plane, out) in self.c2p_out.iter_mut().enumerate() {
                if chunk & (1 << plane) != 0 {
                    *out |= 1 << (31 - px);
                }
            }
        }
    }

    // ---- NVRAM (I2C EEPROM) ---------------------------------------------------

    fn update_lines(&mut self) {
        let scl = self.io_dir & 0x80 == 0 || self.io_out & 0x80 != 0;
        let sda = self.io_dir & 0x40 == 0 || self.io_out & 0x40 != 0;
        let (old_scl, old_sda) = (self.scl, self.sda);
        self.scl = scl;
        self.sda = sda;
        if scl && old_scl && sda != old_sda {
            if !sda {
                // START
                self.i2c = I2c::Rx { byte: 0, bits: 0 };
                self.rx_count = 0;
                self.sda_eeprom = true;
            } else {
                // STOP
                self.i2c = I2c::Idle;
                self.sda_eeprom = true;
            }
            return;
        }
        if scl && !old_scl {
            self.clock_rise();
        } else if !scl && old_scl {
            self.clock_fall();
        }
    }

    /// Data is sampled while SCL is high.
    fn clock_rise(&mut self) {
        match &mut self.i2c {
            I2c::Rx { byte, bits } => {
                *byte = *byte << 1 | self.sda as u8;
                *bits += 1;
            }
            I2c::AckIn => {
                // A NAK ends a read; an ACK asks for the next byte.
                if self.sda {
                    self.i2c = I2c::Idle;
                }
            }
            _ => {}
        }
    }

    /// The EEPROM changes SDA while SCL is low.
    fn clock_fall(&mut self) {
        match std::mem::take(&mut self.i2c) {
            I2c::Rx { byte, bits: 8 } => {
                self.received(byte);
                self.sda_eeprom = false; // ACK
                self.i2c = I2c::AckOut;
            }
            I2c::AckOut => {
                self.sda_eeprom = true;
                if self.reading {
                    self.send_next();
                } else {
                    self.i2c = I2c::Rx { byte: 0, bits: 0 };
                }
            }
            I2c::Tx { byte, bits } if bits < 8 => {
                self.sda_eeprom = byte & (0x80 >> bits) != 0;
                self.i2c = I2c::Tx { byte, bits: bits + 1 };
            }
            I2c::Tx { .. } => {
                self.sda_eeprom = true;
                self.i2c = I2c::AckIn;
            }
            I2c::AckIn => self.send_next(),
            other => self.i2c = other,
        }
    }

    fn send_next(&mut self) {
        let byte = self.nvram[self.addr];
        self.addr = (self.addr + 1) % NVRAM_SIZE;
        self.sda_eeprom = byte & 0x80 != 0;
        self.i2c = I2c::Tx { byte, bits: 1 };
    }

    fn received(&mut self, byte: u8) {
        if self.rx_count == 0 {
            // Device address 1010 A9 A8 x R/W: the top address bits ride along.
            self.reading = byte & 1 != 0;
            self.addr = (self.addr & 0xff) | ((byte as usize >> 1) & 3) << 8;
        } else if self.rx_count == 1 && !self.reading {
            self.addr = (self.addr & 0x300) | byte as usize;
        } else if !self.reading {
            self.nvram[self.addr] = byte;
            // Page writes wrap within 16 bytes.
            self.addr = (self.addr & !15) | ((self.addr + 1) & 15);
        }
        self.rx_count += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn c2p_converts_a_row() {
        let mut a = Akiko::new();
        // Pixel 0 = colour 1 (plane 0), pixel 31 = colour $80 (plane 7).
        let chunky = [0x0100_0000u32, 0, 0, 0, 0, 0, 0, 0x0000_0080];
        for l in chunky {
            for b in 0..4 {
                a.write8(0x38 + b, (l >> (24 - 8 * b)) as u8);
            }
        }
        let mut out = [0u32; 8];
        for o in out.iter_mut() {
            for b in 0..4 {
                *o = *o << 8 | a.read8(0x38 + b) as u32;
            }
        }
        assert_eq!(out[0], 0x8000_0000);
        assert_eq!(out[7], 0x0000_0001);
        assert_eq!(out[1..7], [0; 6]);
    }

    #[test]
    fn id() {
        let mut a = Akiko::new();
        let id = (2..6).fold(0u32, |v, o| v << 8 | a.read8(o) as u32);
        assert_eq!(id, 0xc0ca_cafe);
    }
}
