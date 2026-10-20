//! Preparing HDF images for real-hardware controllers.
//!
//! Commodore's drivers (A590/A2091 ROM, Gayle scsi.device) only mount disks
//! that carry a Rigid Disk Block. Some images are bare partitions (a DOS boot
//! block at block 0), which emulators normally mount with a fixed geometry. For
//! those, a one-cylinder RDB describing the partition is placed in front of the
//! image, much as HDToolBox would partition a real drive.

/// Geometry Amiga Forever documents for its RDB-less hardfiles.
const SECTORS: u32 = 32;
const SURFACES: u32 = 1;
const RESERVED: u32 = 2;

fn put(b: &mut [u8], off: usize, v: u32) {
    b[off..off + 4].copy_from_slice(&v.to_be_bytes());
}

fn get(b: &[u8], off: usize) -> u32 {
    u32::from_be_bytes(b[off..off + 4].try_into().unwrap())
}

/// Fill in the checksum of an RDB block (SummedLongs at +4, checksum at +8).
fn checksum(block: &mut [u8]) {
    let longs = (get(block, 4) as usize).min(128);
    put(block, 8, 0);
    let sum = (0..longs).fold(0u32, |s, i| s.wrapping_add(get(block, i * 4)));
    put(block, 8, sum.wrapping_neg());
}

fn find_rdb(data: &[u8]) -> Option<usize> {
    (0..16).map(|b| b * 512).find(|&o| data.len() >= o + 512 && &data[o..o + 4] == b"RDSK")
}

/// Clear the RDB's "no more disks after this one" flags (RDBFF_LAST,
/// RDBFF_LASTLUN, RDBFF_LASTTID), as HDToolBox would, so the driver goes on to
/// scan later units. Returns the old flags if they changed.
pub fn clear_last_flags(data: &mut [u8]) -> Option<u32> {
    let o = find_rdb(data)?;
    let flags = get(data, o + 20);
    if flags & 7 == 0 {
        return None;
    }
    put(data, o + 20, flags & !7);
    checksum(&mut data[o..o + 512]);
    Some(flags)
}

/// Give a bare partition image an RDB. `boot_pri` is the partition's boot
/// priority and `name` its device name. Images that already have an RDB, or
/// that are not recognisably a DOS partition, are returned unchanged.
pub fn wrap_partition(data: Vec<u8>, name: &str, bootable: bool, boot_pri: i32) -> Vec<u8> {
    if find_rdb(&data).is_some() || data.len() < 1024 || &data[..3] != b"DOS" {
        return data;
    }
    let cyl_bytes = (SECTORS * SURFACES * 512) as usize;
    let cyls = (data.len() / cyl_bytes) as u32;
    let dostype = get(&data, 0);
    let mut out = vec![0u8; cyl_bytes];

    let rdsk = &mut out[0..512];
    rdsk[0..4].copy_from_slice(b"RDSK");
    put(rdsk, 4, 64);
    put(rdsk, 12, 7); // host ID
    put(rdsk, 16, 512);
    put(rdsk, 20, 0);
    put(rdsk, 24, !0); // no bad blocks
    put(rdsk, 28, 1); // partition list at block 1
    put(rdsk, 32, !0); // no filesystems
    put(rdsk, 36, !0);
    for i in 0..6 {
        put(rdsk, 40 + i * 4, !0);
    }
    put(rdsk, 64, cyls + 1);
    put(rdsk, 68, SECTORS);
    put(rdsk, 72, SURFACES);
    put(rdsk, 76, 1);
    put(rdsk, 80, cyls + 1);
    put(rdsk, 128, 0);
    put(rdsk, 132, SECTORS * SURFACES - 1);
    put(rdsk, 136, 1);
    put(rdsk, 140, cyls);
    put(rdsk, 144, SECTORS * SURFACES);
    put(rdsk, 152, 1);
    rdsk[160..168].copy_from_slice(b"AMIGA   ");
    rdsk[168..184].copy_from_slice(b"HARDFILE        ");
    rdsk[184..188].copy_from_slice(b"1.0 ");
    checksum(rdsk);

    let part = &mut out[512..1024];
    part[0..4].copy_from_slice(b"PART");
    put(part, 4, 64);
    put(part, 12, 7);
    put(part, 16, !0); // last partition
    put(part, 20, bootable as u32); // PBFF_BOOTABLE
    let n = name.as_bytes();
    part[36] = n.len() as u8;
    part[37..37 + n.len()].copy_from_slice(n);
    let env = [
        16,              // table size
        128,             // longs per block
        0,               // sector origin
        SURFACES,        // surfaces
        1,               // sectors per block
        SECTORS,         // blocks per track
        RESERVED,        // reserved blocks
        0,               // prealloc
        0,               // interleave
        1,               // low cylinder
        cyls,            // high cylinder
        30,              // buffers
        0,               // buffer memory type
        0x00ff_ffff,     // max transfer
        0x7fff_fffe,     // address mask
        boot_pri as u32, // boot priority
        dostype,
    ];
    for (i, v) in env.iter().enumerate() {
        put(part, 128 + i * 4, *v);
    }
    checksum(part);

    log::info!("{name}: bare partition image ({:08x}), added an RDB in front of it", dostype);
    out.extend(data);
    out
}
