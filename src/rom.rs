//! Kickstart ROM loading, including Cloanto "AMIROMTYPE1" encrypted images.

use std::path::Path;

const CLOANTO_MAGIC: &[u8] = b"AMIROMTYPE1";

#[derive(Debug)]
pub struct Rom {
    pub data: Vec<u8>,
    pub version: u16,
    pub revision: u16,
}

#[derive(Debug, thiserror::Error)]
pub enum RomError {
    #[error("i/o error reading {0}: {1}")]
    Io(String, std::io::Error),
    #[error("ROM is encrypted but no rom.key was found next to it (or given with --rom-key)")]
    NoKey,
    #[error("unsupported ROM size {0} bytes (expected 256K, 512K or 1M)")]
    BadSize(usize),
}

/// XOR-decrypt a Cloanto ROM body with the repeating key.
pub fn decrypt(body: &[u8], key: &[u8]) -> Vec<u8> {
    body.iter().zip(key.iter().cycle()).map(|(b, k)| b ^ k).collect()
}

/// Kickstart checksum: 32-bit sum with end-around carry should be 0xffffffff.
pub fn checksum_ok(data: &[u8]) -> bool {
    let mut sum: u32 = 0;
    for w in data.chunks_exact(4) {
        let v = u32::from_be_bytes([w[0], w[1], w[2], w[3]]);
        let (s, carry) = sum.overflowing_add(v);
        sum = s.wrapping_add(carry as u32);
    }
    sum == 0xffff_ffff
}

pub fn load(path: &Path, key_path: Option<&Path>) -> Result<Rom, RomError> {
    let raw = std::fs::read(path).map_err(|e| RomError::Io(path.display().to_string(), e))?;
    let mut data = if raw.starts_with(CLOANTO_MAGIC) {
        let key_file = match key_path {
            Some(k) => k.to_path_buf(),
            None => path.with_file_name("rom.key"),
        };
        let key = std::fs::read(&key_file).map_err(|_| RomError::NoKey)?;
        if key.is_empty() {
            return Err(RomError::NoKey);
        }
        decrypt(&raw[CLOANTO_MAGIC.len()..], &key)
    } else {
        raw
    };
    // Some dumps are byte-swapped or carry a 512-byte header; handle the header case.
    if data.len() % 1024 == 512 {
        data.drain(..512);
    }
    if !matches!(data.len(), 0x40000 | 0x80000 | 0x100000) {
        return Err(RomError::BadSize(data.len()));
    }
    let version = u16::from_be_bytes([data[12], data[13]]);
    let revision = u16::from_be_bytes([data[14], data[15]]);
    if !checksum_ok(&data) {
        log::warn!("ROM checksum mismatch for {} (continuing anyway)", path.display());
    }
    Ok(Rom { data, version, revision })
}
