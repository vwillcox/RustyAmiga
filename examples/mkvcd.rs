//! Build a Video CD 2.0 image (BIN/CUE) from an MPEG-1 program stream made
//! of 2324-byte packs (as `ffmpeg -target pal-vcd` or `ntsc-vcd` writes), for
//! testing the CD32 FMV cartridge.
//!
//! mkvcd MOVIE.mpg OUT (writes OUT.bin and OUT.cue)
//!
//! Track 1 holds the ISO 9660 file system with VCD/INFO.VCD and
//! VCD/ENTRIES.VCD at their fixed sectors (150 and 151); track 2 holds the
//! movie, one pack per Mode 2 Form 2 sector, after a two-second pregap and
//! 15 empty sectors. No playback control (PSD) is written.

use std::io::Write;

const PACK: usize = 2324;
const TRACK1: u32 = 300;
const PREGAP: u32 = 150;
const MARGIN: u32 = 15;

fn bcd(v: u32) -> u8 {
    (((v / 10) << 4) | (v % 10)) as u8
}

/// Absolute MSF of a logical sector (with the two-second lead-in).
fn msf(lsn: u32) -> (u32, u32, u32) {
    let f = lsn + 150;
    (f / 4500, f / 75 % 60, f % 75)
}

/// A raw Mode 2 sector: sync, header, subheader (file, channel, submode,
/// coding, twice) and the user data. EDC and ECC are left zero.
fn sector(lsn: u32, sub: [u8; 4], data: &[u8]) -> Vec<u8> {
    let mut s = vec![0u8; 2352];
    s[1..11].fill(0xff);
    let (m, sec, f) = msf(lsn);
    s[12..16].copy_from_slice(&[bcd(m), bcd(sec), bcd(f), 2]);
    s[16..20].copy_from_slice(&sub);
    s[20..24].copy_from_slice(&sub);
    s[24..24 + data.len()].copy_from_slice(data);
    s
}

fn both16(v: u16) -> [u8; 4] {
    let (l, b) = (v.to_le_bytes(), v.to_be_bytes());
    [l[0], l[1], b[0], b[1]]
}

fn both32(v: u32) -> [u8; 8] {
    let (l, b) = (v.to_le_bytes(), v.to_be_bytes());
    [l[0], l[1], l[2], l[3], b[0], b[1], b[2], b[3]]
}

/// An ISO 9660 directory record with the CD-XA system use field.
fn dir_record(name: &[u8], extent: u32, size: u32, dir: bool, xa_attr: u16, file: u8) -> Vec<u8> {
    let mut r = vec![0u8; 33];
    r[2..10].copy_from_slice(&both32(extent));
    r[10..18].copy_from_slice(&both32(size));
    r[18..25].copy_from_slice(&[126, 1, 1, 12, 0, 0, 0]); // 2026-01-01 12:00
    r[25] = if dir { 2 } else { 0 };
    r[28..32].copy_from_slice(&both16(1));
    r[32] = name.len() as u8;
    r.extend_from_slice(name);
    if name.len().is_multiple_of(2) {
        r.push(0);
    }
    r.extend_from_slice(&[0, 0, 0, 0]);
    r.extend_from_slice(&xa_attr.to_be_bytes());
    r.extend_from_slice(b"XA");
    r.extend_from_slice(&[file, 0, 0, 0, 0, 0]);
    r[0] = r.len() as u8;
    r
}

const XA_DIR: u16 = 0x8d55;
const XA_FORM1: u16 = 0x0d55;
const XA_FORM2: u16 = 0x1555;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        eprintln!("usage: mkvcd MOVIE.mpg OUT");
        std::process::exit(1);
    }
    let mpeg = std::fs::read(&args[1]).expect("movie");
    if !mpeg.len().is_multiple_of(PACK) || mpeg.get(..4) != Some(&[0, 0, 1, 0xba]) {
        eprintln!("{}: not a Video CD stream of {PACK}-byte packs", args[1]);
        std::process::exit(1);
    }
    let packs = (mpeg.len() / PACK) as u32;
    let track2 = TRACK1 + PREGAP;
    let movie = track2 + MARGIN;
    let track2_len = MARGIN + packs + MARGIN;
    let total = track2 + track2_len;

    // Directory sectors.
    let (root, cdi, ext, mpegav, segment, vcd) = (20u32, 21, 22, 23, 24, 25);
    let dirs: [(&[u8], u32); 5] = [(b"CDI", cdi), (b"EXT", ext), (b"MPEGAV", mpegav), (b"SEGMENT", segment), (b"VCD", vcd)];
    let dir_sector = |own: u32, parent: u32, entries: &[Vec<u8>]| {
        let mut d = dir_record(&[0], own, 2048, true, XA_DIR, 0);
        d.extend(dir_record(&[1], parent, 2048, true, XA_DIR, 0));
        for e in entries {
            d.extend_from_slice(e);
        }
        d.resize(2048, 0);
        d
    };

    let mut info = vec![0u8; 2048];
    info[..8].copy_from_slice(b"VIDEO_CD");
    info[8] = 2; // version 2.0
    info[10..26].copy_from_slice(b"AMIGA TEST DISC ");
    info[26..28].copy_from_slice(&1u16.to_be_bytes()); // volumes in the album
    info[28..30].copy_from_slice(&1u16.to_be_bytes()); // this volume
    // PAL flag for track 2 (bit 0 of the 98 tracks' flags) when 288 lines.
    let pal = mpeg.windows(8).find(|w| w[..4] == [0, 0, 1, 0xb3]).is_some_and(|w| ((w[5] as u32 & 15) << 8 | w[6] as u32) == 288);
    info[30] = pal as u8;
    info[45] = 8; // offset multiplier

    let mut entries = vec![0u8; 2048];
    entries[..8].copy_from_slice(b"ENTRYVCD");
    entries[8] = 2;
    entries[10..12].copy_from_slice(&1u16.to_be_bytes());
    let (m, s, f) = msf(movie);
    entries[12..16].copy_from_slice(&[bcd(2), bcd(m), bcd(s), bcd(f)]);

    let mut pvd = vec![0u8; 2048];
    pvd[0] = 1;
    pvd[1..6].copy_from_slice(b"CD001");
    pvd[6] = 1;
    let pad = |v: &mut [u8], s: &[u8]| {
        v.fill(b' ');
        v[..s.len()].copy_from_slice(s);
    };
    pad(&mut pvd[8..40], b"CD-RTOS CD-BRIDGE");
    pad(&mut pvd[40..72], b"VIDEOCD");
    pvd[80..88].copy_from_slice(&both32(total));
    pvd[120..124].copy_from_slice(&both16(1));
    pvd[124..128].copy_from_slice(&both16(1));
    pvd[128..132].copy_from_slice(&both16(2048));
    // Path tables: the root, then its five directories.
    let mut path_l = vec![];
    let mut path_m = vec![];
    for (name, extent, parent) in std::iter::once((&[0u8][..], root, 1u16)).chain(dirs.iter().map(|&(n, e)| (n, e, 1))) {
        for (t, be) in [(&mut path_l, false), (&mut path_m, true)] {
            t.push(name.len() as u8);
            t.push(0);
            t.extend_from_slice(&if be { extent.to_be_bytes() } else { extent.to_le_bytes() });
            t.extend_from_slice(&if be { parent.to_be_bytes() } else { parent.to_le_bytes() });
            t.extend_from_slice(name);
            if name.len() % 2 == 1 {
                t.push(0);
            }
        }
    }
    pvd[132..140].copy_from_slice(&both32(path_l.len() as u32));
    pvd[140..144].copy_from_slice(&18u32.to_le_bytes());
    pvd[148..152].copy_from_slice(&19u32.to_be_bytes());
    let root_rec = dir_record(&[0], root, 2048, true, XA_DIR, 0);
    pvd[156..190].copy_from_slice(&root_rec[..34]);
    pvd[156] = 34;
    pad(&mut pvd[190..318], b"");
    pad(&mut pvd[318..446], b"");
    pad(&mut pvd[446..574], b"");
    pad(&mut pvd[574..702], b"CD-RTOS CD-BRIDGE");
    pad(&mut pvd[702..813], b"");
    for at in [813, 830] {
        pvd[at..at + 16].copy_from_slice(b"2026010112000000");
    }
    for at in [847, 864] {
        pvd[at..at + 16].copy_from_slice(b"0000000000000000");
    }
    pvd[881] = 1;
    pvd[1024..1032].copy_from_slice(b"CD-XA001");

    let mut term = vec![0u8; 2048];
    term[0] = 255;
    term[1..6].copy_from_slice(b"CD001");
    term[6] = 1;

    let root_dir: Vec<Vec<u8>> = dirs.iter().map(|&(n, e)| dir_record(n, e, 2048, true, XA_DIR, 0)).collect();
    let av = dir_record(b"AVSEQ01.DAT;1", movie - MARGIN, track2_len * 2048, false, XA_FORM2, 1);
    let vcd_files = [dir_record(b"ENTRIES.VCD;1", 151, 2048, false, XA_FORM1, 0), dir_record(b"INFO.VCD;1", 150, 2048, false, XA_FORM1, 0)];

    let name = &args[2];
    let mut bin = std::io::BufWriter::new(std::fs::File::create(format!("{name}.bin")).expect("bin"));
    const DATA: [u8; 4] = [0, 0, 0x08, 0];
    const DATA_END: [u8; 4] = [0, 0, 0x89, 0];
    const EMPTY: [u8; 4] = [0, 0, 0x20, 0];
    for lsn in 0..total {
        let s = match lsn {
            16 => sector(lsn, DATA_END, &pvd),
            17 => sector(lsn, DATA_END, &term),
            18 => sector(lsn, DATA_END, &path_l),
            19 => sector(lsn, DATA_END, &path_m),
            20 => sector(lsn, DATA_END, &dir_sector(root, root, &root_dir)),
            21 | 22 | 24 => sector(lsn, DATA_END, &dir_sector(lsn, root, &[])),
            23 => sector(lsn, DATA_END, &dir_sector(mpegav, root, std::slice::from_ref(&av))),
            25 => sector(lsn, DATA_END, &dir_sector(vcd, root, &vcd_files)),
            150 => sector(lsn, DATA_END, &info),
            151 => sector(lsn, DATA_END, &entries),
            _ if lsn < TRACK1 => sector(lsn, DATA, &[0; 2048]),
            _ if lsn >= movie && lsn < movie + packs => {
                let pack = &mpeg[(lsn - movie) as usize * PACK..][..PACK];
                // Video and audio sectors are marked as such in the subheader.
                let audio = pack.windows(4).take(64).any(|w| w[..3] == [0, 0, 1] && (0xc0..=0xdf).contains(&w[3]));
                let video = pack.windows(4).take(64).any(|w| w[..3] == [0, 0, 1] && (0xe0..=0xef).contains(&w[3]));
                let sub = match (video, audio) {
                    (true, _) => [1, 1, 0x62, 0x0f],
                    (_, true) => [1, 1, 0x64, 0x7f],
                    _ => [1, 0, 0x60, 0],
                };
                let mut s = sector(lsn, sub, pack);
                if lsn == movie + packs - 1 {
                    s[18] |= 0x81;
                    s[22] |= 0x81;
                }
                s
            }
            _ => sector(lsn, EMPTY, &[]),
        };
        bin.write_all(&s).unwrap();
    }
    bin.flush().unwrap();
    let base = std::path::Path::new(name).file_name().unwrap().to_string_lossy().to_string();
    let t = |lsn: u32| format!("{:02}:{:02}:{:02}", lsn / 4500, lsn / 75 % 60, lsn % 75);
    let cue = format!(
        "FILE \"{base}.bin\" BINARY\n  TRACK 01 MODE2/2352\n    INDEX 01 00:00:00\n  TRACK 02 MODE2/2352\n    INDEX 00 {}\n    INDEX 01 {}\n",
        t(TRACK1),
        t(track2)
    );
    std::fs::write(format!("{name}.cue"), cue).expect("cue");
    eprintln!("{name}.bin: {total} sectors, {packs} packs from sector {movie}");
}
