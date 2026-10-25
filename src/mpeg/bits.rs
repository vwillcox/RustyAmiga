/// Reads a byte slice most significant bit first. Reading past the end gives
/// zero bits, so a truncated stream ends in a failed start code search rather
/// than a panic.
pub struct Bits<'a> {
    data: &'a [u8],
    /// Position in bits.
    pos: usize,
}

impl<'a> Bits<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Bits { data, pos: 0 }
    }

    /// The next `n` (at most 32) bits, without moving.
    #[inline]
    pub fn peek(&self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        let byte = self.pos >> 3;
        let mut w = [0u8; 8];
        if byte < self.data.len() {
            let end = (byte + 8).min(self.data.len());
            w[..end - byte].copy_from_slice(&self.data[byte..end]);
        }
        let v = u64::from_be_bytes(w) << (self.pos & 7);
        (v >> (64 - n)) as u32
    }

    #[inline]
    pub fn skip(&mut self, n: u32) {
        self.pos += n as usize;
    }

    #[inline]
    pub fn read(&mut self, n: u32) -> u32 {
        let v = self.peek(n);
        self.pos += n as usize;
        v
    }

    #[inline]
    pub fn bit(&mut self) -> bool {
        self.read(1) != 0
    }

    /// Position in bits from the start.
    pub fn pos(&self) -> usize {
        self.pos
    }

    /// Bits left before the end of the data.
    pub fn left(&self) -> usize {
        (self.data.len() * 8).saturating_sub(self.pos)
    }

    pub fn align(&mut self) {
        self.pos = (self.pos + 7) & !7;
    }

    /// At a start code prefix (23 zero bits and a one, byte aligned)?
    pub fn at_start_code(&self) -> bool {
        self.pos & 7 == 0 && self.peek(24) == 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_across_bytes() {
        let mut b = Bits::new(&[0b1010_1100, 0b0101_0011, 0xff]);
        assert_eq!(b.read(3), 0b101);
        assert_eq!(b.read(7), 0b011_0001);
        assert_eq!(b.peek(6), 0b01_0011);
        b.skip(6);
        assert_eq!(b.read(8), 0xff);
        assert_eq!(b.read(5), 0, "past the end reads zeros");
    }
}
