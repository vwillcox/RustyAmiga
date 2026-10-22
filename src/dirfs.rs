//! Host folders as Amiga hard disks.
//!
//! Amiga Forever ships some systems (AROS, the enhanced 3.X installation) as
//! plain folders that UAE mounts through its own filesystem handler. Rather
//! than emulate a handler, the folder is turned into a Fast File System
//! partition image in memory when the machine starts, which Kickstart's own FFS
//! then mounts like any other hard disk. Changes stay in memory.

use std::path::Path;

const BSIZE: usize = 512;
/// Hash table entries in a 512-byte header block.
const HT_SIZE: usize = 72;
/// Blocks reserved at the start of the partition (the boot block).
const RESERVED: u32 = 2;
/// Partition size granularity: one cylinder of the geometry `hdf` gives bare
/// partitions (32 sectors, 1 surface).
const CYL_BLOCKS: u32 = 32;
/// Free space left for the system to write to.
const FREE_BYTES: u64 = 16 << 20;
/// Longest name an FFS header block holds.
const MAX_NAME: usize = 30;

const T_HEADER: u32 = 2;
const T_LIST: u32 = 16;
const ST_ROOT: u32 = 1;
const ST_USERDIR: u32 = 2;
const ST_FILE: u32 = (-3i32) as u32;
/// Protection bit S (script).
const FIBF_SCRIPT: u32 = 1 << 6;

enum Node {
    File { name: Vec<u8>, data: Vec<u8>, date: [u32; 3], prot: u32 },
    Dir { name: Vec<u8>, date: [u32; 3], children: Vec<Node> },
}

/// Build a partition image (international FFS, `DOS\3`) holding the contents of
/// `dir`, with volume name `volume`.
pub fn build(dir: &Path, volume: &str) -> Result<Vec<u8>, String> {
    build_sized(dir, volume, None)
}

/// As `build`, but with `size`, the volume fills exactly that many bytes (a
/// memory card); an error if the contents do not fit.
pub fn build_sized(dir: &Path, volume: &str, size: Option<usize>) -> Result<Vec<u8>, String> {
    let mut stats = Stats::default();
    let children = read_dir(dir, false, &mut stats)?;
    if stats.truncated > 0 {
        log::warn!("{}: {} names longer than {MAX_NAME} characters were shortened", dir.display(), stats.truncated);
    }

    // Size the partition: every file needs a header, data blocks and one
    // extension block per 72 data blocks; directories need one block each.
    fn count(nodes: &[Node]) -> u64 {
        nodes
            .iter()
            .map(|n| match n {
                Node::File { data, .. } => {
                    let d = (data.len() as u64).div_ceil(BSIZE as u64);
                    1 + d + d.saturating_sub(1) / HT_SIZE as u64
                }
                Node::Dir { children, .. } => 1 + count(children),
            })
            .sum()
    }
    let used = count(&children) + 1;
    let blocks = match size {
        Some(bytes) => {
            let blocks = (bytes / BSIZE) as u32;
            // Bitmap blocks and the root come on top of the contents.
            if used + used / 8 + RESERVED as u64 + 4 > blocks as u64 {
                return Err(format!("{}: does not fit in {} KB", dir.display(), bytes / 1024));
            }
            blocks
        }
        None => {
            let want = used + used / 8 + FREE_BYTES / BSIZE as u64 + RESERVED as u64;
            (want.div_ceil(CYL_BLOCKS as u64) * CYL_BLOCKS as u64) as u32
        }
    };
    let mut fs = Fs::new(blocks);

    let now = amiga_date(std::time::SystemTime::now());
    let root = fs.root;
    fs.alloc_at(root);
    for c in &children {
        fs.write_node(c, root);
    }
    fs.finish_root(volume, now);
    log::info!(
        "{}: {} files in {} directories as a {} KB FFS volume \"{volume}\"",
        dir.display(),
        stats.files,
        stats.dirs,
        blocks as usize * BSIZE / 1024
    );
    Ok(fs.img)
}

#[derive(Default)]
struct Stats {
    files: usize,
    dirs: usize,
    truncated: usize,
}

fn read_dir(dir: &Path, in_s: bool, stats: &mut Stats) -> Result<Vec<Node>, String> {
    let mut out = vec![];
    let rd = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut entries: Vec<_> = rd.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let path = e.path();
        let host_name = e.file_name().to_string_lossy().into_owned();
        let mut name = latin1(&host_name);
        if name.len() > MAX_NAME {
            name.truncate(MAX_NAME);
            stats.truncated += 1;
        }
        let meta = std::fs::metadata(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let date = meta.modified().map(amiga_date).unwrap_or([0; 3]);
        if meta.is_dir() {
            stats.dirs += 1;
            let children = read_dir(&path, in_s || host_name.eq_ignore_ascii_case("s"), stats)?;
            out.push(Node::Dir { name, date, children });
        } else if meta.is_file() {
            stats.files += 1;
            let data = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            // No Amiga protection bits survive on the host. Mark scripts so the
            // shell runs them by name: everything in S: and text files that
            // start like an AmigaDOS script.
            let script = data.first().is_some_and(|&b| b == b';' || b == b'.') && !data.starts_with(&[0, 0, 3, 0xf3]);
            let prot = if in_s || script { FIBF_SCRIPT } else { 0 };
            out.push(Node::File { name, data, date, prot });
        }
    }
    Ok(out)
}

/// Host (UTF-8) name to ISO-8859-1, which is what AmigaDOS uses.
fn latin1(s: &str) -> Vec<u8> {
    s.chars()
        .map(|c| match c as u32 {
            0x20..=0x7e | 0xa0..=0xff => c as u8,
            _ => b'_',
        })
        .filter(|&b| b != b'/' && b != b':')
        .collect()
}

/// Days since 1978-01-01, minutes, and ticks (1/50 s).
fn amiga_date(t: std::time::SystemTime) -> [u32; 3] {
    let secs = t.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let secs = secs.saturating_sub(252_460_800); // 1970-01-01 to 1978-01-01
    [(secs / 86400) as u32, (secs % 86400 / 60) as u32, (secs % 60 * 50) as u32]
}

fn intl_upper(c: u8) -> u8 {
    if c.is_ascii_lowercase() || (224..=254).contains(&c) && c != 247 {
        c - 32
    } else {
        c
    }
}

/// The international FFS directory hash.
fn hash(name: &[u8]) -> usize {
    let mut h = name.len() as u32;
    for &c in name {
        h = (h.wrapping_mul(13).wrapping_add(intl_upper(c) as u32)) & 0x7ff;
    }
    h as usize % HT_SIZE
}

struct Fs {
    img: Vec<u8>,
    blocks: u32,
    root: u32,
    used: Vec<bool>,
    next: u32,
    /// File and directory header blocks, checksummed once complete.
    headers: Vec<u32>,
}

impl Fs {
    fn new(blocks: u32) -> Fs {
        let mut img = vec![0u8; blocks as usize * BSIZE];
        img[0..4].copy_from_slice(b"DOS\x03");
        let root = (blocks - 1 + RESERVED) / 2;
        let mut used = vec![false; blocks as usize];
        for u in used.iter_mut().take(RESERVED as usize) {
            *u = true;
        }
        Fs { img, blocks, root, used, next: root + 1, headers: vec![] }
    }

    fn put(&mut self, block: u32, off: usize, v: u32) {
        let o = block as usize * BSIZE + off;
        self.img[o..o + 4].copy_from_slice(&v.to_be_bytes());
    }

    fn get(&self, block: u32, off: usize) -> u32 {
        let o = block as usize * BSIZE + off;
        u32::from_be_bytes(self.img[o..o + 4].try_into().unwrap())
    }

    fn alloc_at(&mut self, b: u32) {
        self.used[b as usize] = true;
    }

    /// Next free block, filling from just after the root block and wrapping
    /// round to the start, as the FFS allocator does.
    fn alloc(&mut self) -> u32 {
        loop {
            if self.next >= self.blocks {
                self.next = RESERVED;
            }
            let b = self.next;
            self.next += 1;
            if !self.used[b as usize] {
                self.used[b as usize] = true;
                return b;
            }
        }
    }

    fn set_name(&mut self, block: u32, name: &[u8]) {
        let o = block as usize * BSIZE + BSIZE - 80;
        self.img[o] = name.len() as u8;
        self.img[o + 1..o + 1 + name.len()].copy_from_slice(name);
    }

    fn set_date(&mut self, block: u32, off: usize, d: [u32; 3]) {
        for (i, v) in d.iter().enumerate() {
            self.put(block, off + i * 4, *v);
        }
    }

    /// Header-block checksum at offset 20: all longs sum to zero.
    fn checksum(&mut self, block: u32, off: usize) {
        self.put(block, off, 0);
        let sum = (0..BSIZE / 4).fold(0u32, |s, i| s.wrapping_add(self.get(block, i * 4)));
        self.put(block, off, sum.wrapping_neg());
    }

    /// Enter `block` (named `name`) in directory `dir`'s hash table.
    fn link(&mut self, dir: u32, block: u32, name: &[u8]) {
        let slot = 24 + hash(name) * 4;
        // Chains are kept in ascending block order, like the FFS does.
        let head = self.get(dir, slot);
        if head == 0 || head > block {
            self.put(block, BSIZE - 16, head);
            self.put(dir, slot, block);
            return;
        }
        let mut cur = head;
        loop {
            let next = self.get(cur, BSIZE - 16);
            if next == 0 || next > block {
                self.put(block, BSIZE - 16, next);
                self.put(cur, BSIZE - 16, block);
                return;
            }
            cur = next;
        }
    }

    fn write_node(&mut self, node: &Node, parent: u32) -> u32 {
        let hdr = self.alloc();
        self.headers.push(hdr);
        match node {
            Node::Dir { name, date, children } => {
                self.put(hdr, 0, T_HEADER);
                self.put(hdr, 4, hdr);
                self.set_date(hdr, BSIZE - 92, *date);
                self.set_name(hdr, name);
                self.put(hdr, BSIZE - 12, parent);
                self.put(hdr, BSIZE - 4, ST_USERDIR);
                for c in children {
                    self.write_node(c, hdr);
                }
                // Children change this block's hash table, so link and sum last.
                self.link(parent, hdr, name);
            }
            Node::File { name, data, date, prot } => {
                let nblocks = data.len().div_ceil(BSIZE);
                let data_blocks: Vec<u32> = (0..nblocks).map(|_| self.alloc()).collect();
                for (i, &b) in data_blocks.iter().enumerate() {
                    let chunk = &data[i * BSIZE..((i + 1) * BSIZE).min(data.len())];
                    let o = b as usize * BSIZE;
                    self.img[o..o + chunk.len()].copy_from_slice(chunk);
                }
                self.put(hdr, 0, T_HEADER);
                self.put(hdr, 4, hdr);
                self.put(hdr, 16, data_blocks.first().copied().unwrap_or(0));
                self.put(hdr, BSIZE - 192, *prot);
                self.put(hdr, BSIZE - 188, data.len() as u32);
                self.set_date(hdr, BSIZE - 92, *date);
                self.set_name(hdr, name);
                self.put(hdr, BSIZE - 12, parent);
                self.put(hdr, BSIZE - 4, ST_FILE);
                // Data block pointers, 72 per block, stored from the end of the
                // table backwards; the rest go in a chain of extension blocks.
                let mut chunks = data_blocks.chunks(HT_SIZE);
                let mut block = hdr;
                let mut first = true;
                loop {
                    let Some(ptrs) = chunks.next() else { break };
                    if !first {
                        let ext = self.alloc();
                        self.put(ext, 0, T_LIST);
                        self.put(ext, 4, ext);
                        self.put(ext, BSIZE - 12, hdr);
                        self.put(ext, BSIZE - 4, ST_FILE);
                        self.put(block, BSIZE - 8, ext);
                        if block != hdr {
                            self.checksum(block, 20);
                        }
                        block = ext;
                    }
                    first = false;
                    self.put(block, 8, ptrs.len() as u32);
                    for (i, &p) in ptrs.iter().enumerate() {
                        self.put(block, 24 + (HT_SIZE - 1 - i) * 4, p);
                    }
                }
                if block != hdr {
                    self.checksum(block, 20);
                }
                self.link(parent, hdr, name);
            }
        }
        hdr
    }

    /// Checksum every header block, then write the bitmap and the root block.
    fn finish_root(&mut self, volume: &str, now: [u32; 3]) {
        let root = self.root;
        self.put(root, 0, T_HEADER);
        self.put(root, 12, HT_SIZE as u32);
        self.put(root, BSIZE - 200, !0); // bitmap valid
        self.set_date(root, BSIZE - 92, now);
        let name = latin1(volume);
        self.set_name(root, &name[..name.len().min(MAX_NAME)]);
        self.set_date(root, BSIZE - 40, now);
        self.set_date(root, BSIZE - 28, now);
        self.put(root, BSIZE - 4, ST_ROOT);

        // Header blocks (directories and files) were only finished once their
        // hash chains were complete; sum them now.
        for b in std::mem::take(&mut self.headers) {
            self.checksum(b, 20);
        }

        // Bitmap: one bit per block after the reserved ones, set when free.
        let bits = (self.blocks - RESERVED) as usize;
        let per_block = (BSIZE / 4 - 1) * 32;
        let nbm = bits.div_ceil(per_block);
        let bm: Vec<u32> = (0..nbm).map(|_| self.alloc()).collect();
        let n_ext = nbm.saturating_sub(25).div_ceil(BSIZE / 4 - 1);
        let ext: Vec<u32> = (0..n_ext).map(|_| self.alloc()).collect();
        for (k, &b) in bm.iter().enumerate() {
            for l in 0..BSIZE / 4 - 1 {
                let mut v = 0u32;
                for bit in 0..32 {
                    let n = k * per_block + l * 32 + bit;
                    if n < bits && !self.used[n + RESERVED as usize] {
                        v |= 1 << bit;
                    }
                }
                self.put(b, 4 + l * 4, v);
            }
            self.checksum(b, 0);
        }
        for (k, &b) in bm.iter().take(25).enumerate() {
            self.put(root, BSIZE - 196 + k * 4, b);
        }
        if let Some(&first) = ext.first() {
            self.put(root, BSIZE - 96, first);
        }
        for (k, &e) in ext.iter().enumerate() {
            let ptrs = bm.iter().skip(25 + k * (BSIZE / 4 - 1)).take(BSIZE / 4 - 1);
            for (i, &p) in ptrs.enumerate() {
                self.put(e, i * 4, p);
            }
            self.put(e, BSIZE - 4, ext.get(k + 1).copied().unwrap_or(0));
        }
        self.checksum(root, 20);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_matches_amigados() {
        // AmigaDOS: hash("S") = (1*13 + 'S') & 0x7ff % 72.
        assert_eq!(hash(b"S"), (13 + b'S' as usize) % 72);
        assert_eq!(hash(b"c"), hash(b"C"));
        // International mode folds Latin-1 letters too: e-acute and E-acute.
        assert_eq!(hash(&[0xe9]), hash(&[0xc9]));
    }

    #[test]
    fn builds_a_volume() {
        let tmp = std::env::temp_dir().join(format!("dirfs-test-{}", std::process::id()));
        std::fs::create_dir_all(tmp.join("S")).unwrap();
        std::fs::write(tmp.join("S/Startup-Sequence"), b"echo hello\n").unwrap();
        std::fs::write(tmp.join("big"), vec![7u8; 100 * 512 + 3]).unwrap();
        let img = build(&tmp, "Test").unwrap();
        std::fs::remove_dir_all(&tmp).unwrap();
        assert_eq!(&img[..4], b"DOS\x03");
        let blocks = (img.len() / BSIZE) as u32;
        let root = (blocks - 1 + RESERVED) / 2;
        let r = &img[root as usize * BSIZE..][..BSIZE];
        let sum = (0..128).fold(0u32, |s, i| s.wrapping_add(u32::from_be_bytes(r[i * 4..i * 4 + 4].try_into().unwrap())));
        assert_eq!(sum, 0, "root checksum");
        assert_eq!(&r[BSIZE - 79..BSIZE - 75], b"Test");
        // "big" is in the root's hash table and has an extension block.
        let slot = 24 + hash(b"big") * 4;
        let hdr = u32::from_be_bytes(r[slot..slot + 4].try_into().unwrap()) as usize;
        let h = &img[hdr * BSIZE..][..BSIZE];
        assert_eq!(u32::from_be_bytes(h[BSIZE - 188..BSIZE - 184].try_into().unwrap()), 100 * 512 + 3);
        assert_ne!(u32::from_be_bytes(h[BSIZE - 8..BSIZE - 4].try_into().unwrap()), 0);
    }
}
