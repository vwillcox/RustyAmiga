//! Floppy drives and ADF <-> MFM track conversion.

use std::path::PathBuf;

pub const SYNC: u16 = 0x4489;
const SECTOR_MFM_WORDS: usize = 544;
/// Track length in MFM words for a double-density disk (12668 bytes).
pub const DD_TRACK_WORDS: usize = 6334;

/// One raw MFM track: a circular bitstream of `bits` bits packed MSB-first.
#[derive(Clone)]
pub struct Track {
    pub words: Vec<u16>,
    pub bits: usize,
}

impl Track {
    fn from_words(words: Vec<u16>) -> Track {
        let bits = words.len() * 16;
        Track { words, bits }
    }

    #[inline]
    pub fn bit(&self, pos: usize) -> u16 {
        (self.words[pos >> 4] >> (15 - (pos & 15))) & 1
    }

    #[inline]
    pub fn set_bit(&mut self, pos: usize, v: u16) {
        let m = 1 << (15 - (pos & 15));
        if v != 0 {
            self.words[pos >> 4] |= m;
        } else {
            self.words[pos >> 4] &= !m;
        }
    }
}

pub struct Disk {
    pub data: Vec<u8>,
    pub sectors: usize,
    pub path: Option<PathBuf>,
    pub write_protected: bool,
    tracks: Vec<Option<Track>>,
    dirty: Vec<bool>,
    /// Extended ADFs hold raw tracks that cannot be written back as a plain ADF.
    extended: bool,
}

impl Disk {
    /// Load a standard or extended ("UAE-1ADF") ADF image.
    pub fn from_adf(data: Vec<u8>, path: Option<PathBuf>) -> Result<Disk, String> {
        if data.starts_with(b"UAE-1ADF") {
            return Self::from_extended(&data, path);
        }
        if data.starts_with(b"UAE--ADF") {
            return Err("old-style UAE--ADF images are not supported".into());
        }
        let sectors = match data.len() {
            901120 => 11,
            1802240 => 22,
            n if n % (512 * 11) == 0 && n / (512 * 11) <= 168 => 11,
            n => return Err(format!("unsupported ADF size {} bytes", n)),
        };
        let ntracks = (data.len() / (512 * sectors)).max(160);
        Ok(Disk {
            data,
            sectors,
            path,
            write_protected: false,
            tracks: vec![None; ntracks],
            dirty: vec![false; ntracks],
            extended: false,
        })
    }

    fn from_extended(img: &[u8], path: Option<PathBuf>) -> Result<Disk, String> {
        let be16 = |o: usize| u16::from_be_bytes([img[o], img[o + 1]]) as usize;
        let be32 = |o: usize| u32::from_be_bytes([img[o], img[o + 1], img[o + 2], img[o + 3]]) as usize;
        if img.len() < 12 {
            return Err("truncated extended ADF".into());
        }
        let ntracks = be16(10);
        let mut off = 12 + ntracks * 12;
        if off > img.len() {
            return Err("truncated extended ADF header".into());
        }
        let mut data = vec![0u8; ntracks.max(160) * 11 * 512];
        let mut tracks: Vec<Option<Track>> = vec![None; ntracks.max(160)];
        for t in 0..ntracks {
            let h = 12 + t * 12;
            let kind = be16(h + 2);
            let len = be32(h + 4);
            let bits = be32(h + 8);
            let body = img.get(off..off + len).ok_or("truncated extended ADF track")?;
            off += len;
            if kind == 0 {
                let n = len.min(11 * 512);
                data[t * 11 * 512..t * 11 * 512 + n].copy_from_slice(&body[..n]);
            } else {
                if bits == 0 {
                    continue;
                }
                let mut words: Vec<u16> = body.chunks(2).map(|c| (c[0] as u16) << 8 | *c.get(1).unwrap_or(&0) as u16).collect();
                words.resize(bits.div_ceil(16), 0);
                tracks[t] = Some(Track { words, bits });
            }
        }
        let n = tracks.len();
        Ok(Disk {
            data,
            sectors: 11,
            path,
            write_protected: false,
            tracks,
            dirty: vec![false; n],
            extended: true,
        })
    }

    pub fn track_words(&self) -> usize {
        DD_TRACK_WORDS * self.sectors / 11
    }

    pub fn track(&mut self, t: usize) -> &mut Track {
        if t >= self.tracks.len() {
            self.tracks.resize(t + 1, None);
            self.dirty.resize(t + 1, false);
        }
        if self.tracks[t].is_none() {
            let enc = encode_track(&self.data, t, self.sectors, self.track_words());
            self.tracks[t] = Some(Track::from_words(enc));
        }
        self.tracks[t].as_mut().unwrap()
    }

    pub fn mark_dirty(&mut self, t: usize) {
        if t < self.dirty.len() {
            self.dirty[t] = true;
        }
    }

    /// Decode written tracks back into the ADF image.
    pub fn flush(&mut self) -> bool {
        let mut changed = false;
        for t in 0..self.tracks.len() {
            if !self.dirty[t] {
                continue;
            }
            self.dirty[t] = false;
            if let Some(tr) = &self.tracks[t] {
                for (sec, bytes) in decode_track(&tr.words, t, self.sectors) {
                    let off = (t * self.sectors + sec) * 512;
                    if off + 512 <= self.data.len() {
                        self.data[off..off + 512].copy_from_slice(&bytes);
                        changed = true;
                    }
                }
            }
        }
        changed
    }

    pub fn save(&mut self) -> std::io::Result<()> {
        if self.extended {
            // Raw tracks can't be represented in a plain ADF; keep the original file.
            return Ok(());
        }
        if self.flush() {
            if let Some(p) = &self.path {
                std::fs::write(p, &self.data)?;
            }
        }
        Ok(())
    }
}

fn odd_even_long(v: u32) -> (u32, u32) {
    ((v >> 1) & 0x5555_5555, v & 0x5555_5555)
}

fn encode_track(adf: &[u8], track: usize, sectors: usize, words: usize) -> Vec<u16> {
    // Build the track as raw data bits first (clock bits zero), then add clocks.
    let mut longs: Vec<u32> = Vec::with_capacity(words / 2 + 1);
    let mut is_sync: Vec<bool> = Vec::new();
    let mut push = |v: u32, sync: bool, longs: &mut Vec<u32>| {
        longs.push(v);
        is_sync.push(sync);
    };
    for s in 0..sectors {
        let off = (track * sectors + s) * 512;
        let blank = [0u8; 512];
        let data = adf.get(off..off + 512).unwrap_or(&blank);
        push(0, false, &mut longs);
        push((SYNC as u32) << 16 | SYNC as u32, true, &mut longs);
        let info = 0xff00_0000 | (track as u32) << 16 | (s as u32) << 8 | (sectors - s) as u32;
        let (io, ie) = odd_even_long(info);
        push(io, false, &mut longs);
        push(ie, false, &mut longs);
        // Label: 16 zero bytes, odd then even.
        for _ in 0..8 {
            push(0, false, &mut longs);
        }
        let hsum = io ^ ie;
        let (ho, he) = odd_even_long(hsum);
        push(ho, false, &mut longs);
        push(he, false, &mut longs);
        let mut odd = [0u32; 128];
        let mut even = [0u32; 128];
        let mut dsum = 0u32;
        for i in 0..128 {
            let v = u32::from_be_bytes([data[i * 4], data[i * 4 + 1], data[i * 4 + 2], data[i * 4 + 3]]);
            let (o, e) = odd_even_long(v);
            odd[i] = o;
            even[i] = e;
            dsum ^= o ^ e;
        }
        let (dso, dse) = odd_even_long(dsum);
        push(dso, false, &mut longs);
        push(dse, false, &mut longs);
        for &o in &odd {
            push(o, false, &mut longs);
        }
        for &e in &even {
            push(e, false, &mut longs);
        }
    }
    let mut out: Vec<u16> = Vec::with_capacity(words);
    let mut sync_w: Vec<bool> = Vec::with_capacity(words);
    for (l, s) in longs.iter().zip(is_sync.iter()) {
        out.push((l >> 16) as u16);
        out.push(*l as u16);
        sync_w.push(*s);
        sync_w.push(*s);
    }
    debug_assert_eq!(out.len(), sectors * SECTOR_MFM_WORDS);
    while out.len() < words {
        out.push(0);
        sync_w.push(false);
    }
    // Insert MFM clock bits: a clock 1 goes between two zero data bits.
    let mut prev_bit = 0u16;
    for (w, &s) in out.iter_mut().zip(sync_w.iter()) {
        if s {
            prev_bit = *w & 1;
            continue;
        }
        let mut v = *w & 0x5555;
        for bit in (0..8).rev() {
            let data_pos = bit * 2;
            let clock_pos = data_pos + 1;
            let d = (v >> data_pos) & 1;
            if d == 0 && prev_bit == 0 {
                v |= 1 << clock_pos;
            }
            prev_bit = d;
        }
        *w = v;
    }
    out
}

/// Decode AmigaDOS sectors found in an MFM track. Returns (sector, data) pairs.
pub fn decode_track(mfm: &[u16], track: usize, sectors: usize) -> Vec<(usize, Vec<u8>)> {
    let n = mfm.len();
    let get_long = |i: usize| -> u32 { (mfm[i % n] as u32) << 16 | mfm[(i + 1) % n] as u32 };
    let mut found = vec![];
    let mut i = 0;
    while i < n {
        if mfm[i] == SYNC && mfm[(i + 1) % n] != SYNC {
            let p = i + 1;
            let io = get_long(p) & 0x5555_5555;
            let ie = get_long(p + 2) & 0x5555_5555;
            let info = io << 1 | ie;
            let t = ((info >> 16) & 0xff) as usize;
            let s = ((info >> 8) & 0xff) as usize;
            if info >> 24 == 0xff && t == track && s < sectors {
                let dp = p + 4 + 16 + 4 + 4;
                let mut bytes = vec![0u8; 512];
                for k in 0..128 {
                    let o = get_long(dp + k * 2) & 0x5555_5555;
                    let e = get_long(dp + 256 + k * 2) & 0x5555_5555;
                    let v = o << 1 | e;
                    bytes[k * 4..k * 4 + 4].copy_from_slice(&v.to_be_bytes());
                }
                found.push((s, bytes));
            }
            i += SECTOR_MFM_WORDS - 4;
        } else {
            i += 1;
        }
    }
    found
}

pub struct Drive {
    pub present: bool,
    pub disk: Option<Disk>,
    pub cyl: u8,
    pub side: u8,
    pub motor: bool,
    pub selected: bool,
    /// Disk-change latch: true while /CHNG is asserted (after eject / at power up).
    pub changed: bool,
    pub pos: usize,
    pub step_line: bool,
    /// High-density drive (identifies as such when an HD disk is inserted).
    pub hd: bool,
    /// Position in the 32-bit drive ID shifted out on /RDY while the motor is off.
    pub id_pos: u8,
}

impl Drive {
    pub fn new(present: bool) -> Self {
        Drive {
            present,
            disk: None,
            cyl: 0,
            side: 0,
            motor: false,
            selected: false,
            changed: true,
            pos: 0,
            step_line: true,
            hd: false,
            id_pos: 0,
        }
    }

    /// Drive identification as disk.resource sees it (a 1 bit is /RDY high):
    /// $00000000 for a 3.5" DD drive (or an HD drive holding a DD disk),
    /// $AAAAAAAA for an HD drive with an HD disk (it spins at 150 RPM).
    pub fn id(&self) -> u32 {
        if self.hd && self.disk.as_ref().is_some_and(|d| d.sectors == 22) {
            0xaaaa_aaaa
        } else {
            0
        }
    }

    /// /RDY asserted (low): ready while the motor runs, otherwise the next ID bit.
    pub fn ready(&self) -> bool {
        self.motor || self.id() & (0x8000_0000 >> (self.id_pos & 31)) == 0
    }

    pub fn track_index(&self) -> usize {
        self.cyl as usize * 2 + self.side as usize
    }

    pub fn insert(&mut self, disk: Disk) {
        // An HD image needs an HD drive; fit one rather than refuse the disk.
        if disk.sectors == 22 {
            self.hd = true;
        }
        self.disk = Some(disk);
        self.changed = true;
    }

    pub fn eject(&mut self) -> Option<Disk> {
        self.changed = true;
        let mut d = self.disk.take();
        if let Some(d) = d.as_mut() {
            if let Err(e) = d.save() {
                log::error!("failed to save disk: {e}");
            }
        }
        d
    }

    /// Length of the current track in bits.
    pub fn track_bits(&mut self) -> usize {
        let t = self.track_index();
        match self.disk.as_mut() {
            Some(d) => d.track(t).bits,
            None => DD_TRACK_WORDS * 16,
        }
    }

    /// Advance the head by one bit cell without reading. Returns the index pulse.
    pub fn rotate(&mut self) -> bool {
        let len = self.track_bits();
        self.pos += 1;
        if self.pos >= len {
            self.pos = 0;
            return true;
        }
        false
    }

    /// Read the bit under the head and advance. Returns (bit, index pulse).
    pub fn read_bit(&mut self) -> (u16, bool) {
        let t = self.track_index();
        let b = match self.disk.as_mut() {
            Some(d) => {
                let tr = d.track(t);
                tr.bit(self.pos % tr.bits)
            }
            None => 0,
        };
        (b, self.rotate())
    }

    /// Write a 16-bit word at the head. Returns true if the index was passed.
    pub fn write_word(&mut self, v: u16) -> bool {
        let t = self.track_index();
        let mut index = false;
        for i in 0..16 {
            if let Some(d) = self.disk.as_mut() {
                if !d.write_protected {
                    let pos = self.pos;
                    let tr = d.track(t);
                    let p = pos % tr.bits;
                    tr.set_bit(p, (v >> (15 - i)) & 1);
                    d.mark_dirty(t);
                }
            }
            index |= self.rotate();
        }
        index
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mfm_roundtrip() {
        let mut adf = vec![0u8; 901120];
        for (i, b) in adf.iter_mut().enumerate() {
            *b = (i * 7 + i / 512) as u8;
        }
        for t in [0usize, 1, 80, 159] {
            let enc = encode_track(&adf, t, 11, DD_TRACK_WORDS);
            assert_eq!(enc.len(), DD_TRACK_WORDS);
            let dec = decode_track(&enc, t, 11);
            assert_eq!(dec.len(), 11);
            for (s, bytes) in dec {
                let off = (t * 11 + s) * 512;
                assert_eq!(&bytes[..], &adf[off..off + 512]);
            }
        }
    }
}
