//! CD images: ISO 9660 files (one data track of 2048-byte sectors) and CUE
//! sheets with their BIN files (data and audio tracks, raw 2352-byte sectors).
//!
//! Positions are logical sector numbers (LSN): track 1 starts at LSN 0, which
//! is MSF 00:02:00 on the disc.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Bytes in a raw sector (and in a sector of audio: 588 stereo frames).
pub const RAW: usize = 2352;

#[derive(Clone, Debug)]
pub struct Track {
    pub number: u8,
    pub audio: bool,
    /// First sector (INDEX 01).
    pub start: u32,
    /// Sectors up to the next track (or the lead-out).
    pub len: u32,
    file: usize,
    /// Byte offset of the track's first sector in its file.
    offset: u64,
    /// 2048 (cooked data) or 2352 (raw) bytes per sector in the file.
    sector_size: u32,
}

pub struct Disc {
    pub name: String,
    files: Vec<File>,
    pub tracks: Vec<Track>,
    /// First sector of the lead-out.
    pub leadout: u32,
}

/// LSN to minutes/seconds/frames (with the 2-second lead-in offset).
pub fn lsn_to_msf(lsn: i32) -> (u8, u8, u8) {
    let f = (lsn + 150).max(0) as u32;
    ((f / 4500) as u8, (f / 75 % 60) as u8, (f % 75) as u8)
}

pub fn msf_to_lsn(m: u8, s: u8, f: u8) -> i32 {
    (m as i32 * 60 + s as i32) * 75 + f as i32 - 150
}

pub fn bcd(v: u8) -> u8 {
    (v / 10) << 4 | v % 10
}

pub fn from_bcd(v: u8) -> u8 {
    (v >> 4) * 10 + (v & 15)
}

impl Disc {
    /// Open an ISO or a CUE sheet (by extension).
    pub fn open(path: &Path) -> Result<Disc, String> {
        let name = path.display().to_string();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
        let err = |e: std::io::Error| format!("{name}: {e}");
        if ext == "cue" {
            let text = std::fs::read_to_string(path).map_err(err)?;
            return Self::from_cue(&name, path.parent().unwrap_or(Path::new(".")), &text);
        }
        let file = File::open(path).map_err(err)?;
        let len = file.metadata().map_err(err)?.len();
        let (sector_size, sectors) = if len % RAW as u64 == 0 && len % 2048 != 0 { (RAW as u32, len / RAW as u64) } else { (2048, len / 2048) };
        let track = Track { number: 1, audio: false, start: 0, len: sectors as u32, file: 0, offset: 0, sector_size };
        Ok(Disc { name, files: vec![file], tracks: vec![track], leadout: sectors as u32 })
    }

    fn from_cue(name: &str, dir: &Path, text: &str) -> Result<Disc, String> {
        let bad = |line: &str| format!("{name}: cannot read '{line}'");
        let msf = |s: &str| -> Option<u32> {
            let v: Vec<u32> = s.split(':').map(|p| p.parse().ok()).collect::<Option<_>>()?;
            (v.len() == 3).then(|| (v[0] * 60 + v[1]) * 75 + v[2])
        };
        let mut files: Vec<(File, u64)> = vec![];
        let mut tracks: Vec<Track> = vec![];
        // Sectors before the current file (earlier files plus all pregaps).
        let mut file_base = 0u32;
        let mut pregap = 0u32;
        for line in text.lines().map(str::trim) {
            let word = line.split_whitespace().next().unwrap_or("").to_ascii_uppercase();
            match word.as_str() {
                "FILE" => {
                    let fname = if let (Some(a), Some(b)) = (line.find('"'), line.rfind('"')) {
                        line[a + 1..b].to_string()
                    } else {
                        line.split_whitespace().nth(1).ok_or_else(|| bad(line))?.to_string()
                    };
                    let p: PathBuf = dir.join(&fname);
                    let f = File::open(&p).map_err(|e| format!("{}: {e}", p.display()))?;
                    let len = f.metadata().map_err(|e| format!("{}: {e}", p.display()))?.len();
                    if let Some((_, prev_len)) = files.last() {
                        let size = tracks.last().map_or(RAW as u64, |t| t.sector_size as u64);
                        file_base += (prev_len / size) as u32;
                    }
                    files.push((f, len));
                }
                "TRACK" => {
                    let mut it = line.split_whitespace().skip(1);
                    let number: u8 = it.next().and_then(|n| n.parse().ok()).ok_or_else(|| bad(line))?;
                    let mode = it.next().ok_or_else(|| bad(line))?.to_ascii_uppercase();
                    let audio = mode == "AUDIO";
                    let sector_size = if mode.ends_with("/2048") { 2048 } else { RAW as u32 };
                    if files.is_empty() {
                        return Err(bad(line));
                    }
                    tracks.push(Track { number, audio, start: u32::MAX, len: 0, file: files.len() - 1, offset: 0, sector_size });
                }
                "PREGAP" => {
                    let v = line.split_whitespace().nth(1).and_then(msf).ok_or_else(|| bad(line))?;
                    pregap += v;
                }
                "INDEX" => {
                    let mut it = line.split_whitespace().skip(1);
                    let idx: u32 = it.next().and_then(|n| n.parse().ok()).ok_or_else(|| bad(line))?;
                    let pos = it.next().and_then(msf).ok_or_else(|| bad(line))?;
                    let t = tracks.last_mut().ok_or_else(|| bad(line))?;
                    if idx == 1 {
                        t.start = file_base + pregap + pos;
                        t.offset = pos as u64 * t.sector_size as u64;
                    }
                }
                _ => {}
            }
        }
        if tracks.is_empty() || tracks.iter().any(|t| t.start == u32::MAX) {
            return Err(format!("{name}: no tracks"));
        }
        // Each track runs to the next one, or to the end of its file.
        for i in 0..tracks.len() {
            let end = match tracks.get(i + 1) {
                Some(n) if n.file == tracks[i].file => n.start,
                _ => {
                    let t = &tracks[i];
                    let left = (files[t.file].1.saturating_sub(t.offset) / t.sector_size as u64) as u32;
                    t.start + left
                }
            };
            tracks[i].len = end.saturating_sub(tracks[i].start);
        }
        let leadout = tracks.last().map_or(0, |t| t.start + t.len);
        Ok(Disc { name: name.to_string(), files: files.into_iter().map(|f| f.0).collect(), tracks, leadout })
    }

    /// The track holding `lsn`.
    pub fn track_at(&self, lsn: u32) -> Option<&Track> {
        self.tracks.iter().find(|t| lsn >= t.start && lsn < t.start + t.len)
    }

    pub fn is_data(&self, lsn: u32) -> bool {
        self.track_at(lsn).is_some_and(|t| !t.audio)
    }

    /// The sector as stored in the image (2048 or 2352 bytes).
    fn read_stored(&mut self, lsn: u32) -> Option<(Vec<u8>, bool, u32)> {
        let t = self.track_at(lsn)?.clone();
        let size = t.sector_size as usize;
        let mut buf = vec![0u8; size];
        let f = &mut self.files[t.file];
        f.seek(SeekFrom::Start(t.offset + (lsn - t.start) as u64 * size as u64)).ok()?;
        f.read_exact(&mut buf).ok()?;
        Some((buf, t.audio, t.sector_size))
    }

    /// A raw 2352-byte sector: audio samples, or a mode 1 data sector with its
    /// sync pattern and header (EDC/ECC are left zero when the image has none).
    pub fn read_raw(&mut self, lsn: u32) -> Option<Vec<u8>> {
        let (data, _audio, size) = self.read_stored(lsn)?;
        if size == RAW as u32 {
            return Some(data);
        }
        let mut raw = vec![0u8; RAW];
        raw[1..11].fill(0xff);
        let (m, s, f) = lsn_to_msf(lsn as i32);
        raw[12..16].copy_from_slice(&[bcd(m), bcd(s), bcd(f), 1]);
        raw[16..16 + 2048].copy_from_slice(&data);
        Some(raw)
    }

    /// The 2048 user data bytes of a data sector.
    pub fn read_data(&mut self, lsn: u32) -> Option<Vec<u8>> {
        let (data, audio, size) = self.read_stored(lsn)?;
        if audio {
            return None;
        }
        if size == 2048 {
            return Some(data);
        }
        // Mode 2 (XA) form 1 keeps a subheader before the data.
        let at = if data[15] == 2 { 24 } else { 16 };
        Some(data[at..at + 2048].to_vec())
    }

    /// Q subchannel for `lsn` (mode 1: position), 12 bytes with CRC.
    pub fn q_subchannel(&self, lsn: u32) -> [u8; 12] {
        let mut q = [0u8; 12];
        let (num, audio, rel) = match self.track_at(lsn) {
            Some(t) => (t.number, t.audio, lsn - t.start),
            None => (0xaa, false, 0),
        };
        q[0] = if audio { 0x01 } else { 0x41 };
        q[1] = if num == 0xaa { 0xaa } else { bcd(num) };
        q[2] = 1;
        let (m, s, f) = lsn_to_msf(rel as i32 - 150);
        q[3..6].copy_from_slice(&[bcd(m), bcd(s), bcd(f)]);
        let (m, s, f) = lsn_to_msf(lsn as i32);
        q[7..10].copy_from_slice(&[bcd(m), bcd(s), bcd(f)]);
        let crc = !crc16_ccitt(&q[..10]);
        q[10..12].copy_from_slice(&crc.to_be_bytes());
        q
    }
}

fn crc16_ccitt(data: &[u8]) -> u16 {
    let mut crc = 0u16;
    for &b in data {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 { crc << 1 ^ 0x1021 } else { crc << 1 };
        }
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn msf_round_trip() {
        assert_eq!(lsn_to_msf(0), (0, 2, 0));
        assert_eq!(msf_to_lsn(0, 2, 0), 0);
        assert_eq!(msf_to_lsn(0, 0, 0), -150);
        let (m, s, f) = lsn_to_msf(123456);
        assert_eq!(msf_to_lsn(m, s, f), 123456);
    }

    #[test]
    fn cue_with_audio_tracks() {
        let dir = std::env::temp_dir().join(format!("cdimage-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Track 1: 10 data sectors; track 2: audio with a 2-second INDEX 00 gap.
        std::fs::write(dir.join("game.bin"), vec![0u8; RAW * (10 + 150 + 20)]).unwrap();
        let cue = "FILE \"game.bin\" BINARY\n  TRACK 01 MODE1/2352\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    INDEX 00 00:00:10\n    INDEX 01 00:02:10\n";
        let d = Disc::from_cue("game.cue", &dir, cue).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(d.tracks.len(), 2);
        assert_eq!((d.tracks[0].start, d.tracks[0].len), (0, 160));
        assert_eq!((d.tracks[1].start, d.tracks[1].len, d.tracks[1].audio), (160, 20, true));
        assert_eq!(d.leadout, 180);
    }
}
