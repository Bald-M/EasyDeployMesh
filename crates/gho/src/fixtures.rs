//! Synthetic, structurally valid Ghost NTFS partition images for tests.
//! Enabled with the `fixtures` feature; never used by production code.

use crate::ntfs::test_support::*;
use flate2::{Compression as Level, write::ZlibEncoder};
use std::io::Write;

pub const CLUSTER: u64 = 4096;
pub const TOTAL_SECTORS: u64 = 64 * 8;
pub const VOLUME_BYTES: u64 = (TOTAL_SECTORS + 1) * 512;

pub fn boot() -> Vec<u8> {
    boot_sector(CLUSTER, TOTAL_SECTORS, 4, 2)
}

pub fn descriptor_segment(boot: &[u8]) -> Vec<u8> {
    let mut payload = vec![0x5a_u8; 2048];
    payload[0..2].copy_from_slice(&[4, 0]);
    payload[2..6].copy_from_slice(b"GHPR");
    payload[532..532 + 512].copy_from_slice(boot);
    payload[2044..2048].copy_from_slice(b"RPHG");
    let mut segment = header(0x01, 0, 0);
    segment.extend(data(payload.len() as u64));
    segment.extend(payload);
    segment.extend(crc());
    segment
}

pub fn fill(seed: u8, len: u64) -> Vec<u8> {
    (0..len)
        .map(|at| seed.wrapping_add((at % 251) as u8))
        .collect()
}

pub fn file_record(index: u64, runs: &[(Option<u64>, u64)]) -> Vec<u8> {
    record(index, &[attribute(0x80, "", runs, CLUSTER)])
}

/// Record 9: a named user file with one non-resident `$DATA`.
pub fn user_record(name: &str, runs: &[(Option<u64>, u64)]) -> Vec<u8> {
    record(9, &[file_name(name), attribute(0x80, "", runs, CLUSTER)])
}

/// The logical stream after the descriptor, built from parts so tests
/// can corrupt individual pieces.
pub struct Parts {
    pub mft_blob: Vec<u8>,
    pub boot_data: Vec<u8>,
    pub user_data: Vec<Vec<u8>>,
    /// Name of the user file; `winload.exe` marks a BCDBoot-capable system.
    pub file_name: String,
}

pub fn parts() -> Parts {
    let mut boot_data = boot();
    boot_data.extend(fill(7, 2 * CLUSTER - 512));
    Parts {
        mft_blob: fill(1, 2 * CLUSTER),
        boot_data,
        user_data: vec![fill(30, CLUSTER), fill(40, 3 * CLUSTER)],
        file_name: "WINLOAD.EXE".to_owned(),
    }
}

pub fn body(parts: &Parts) -> Vec<u8> {
    let mut stream = Vec::new();
    // $MFT with its data.
    stream.extend(header(0x04, 0x80, 0));
    stream.extend(data(1024));
    stream.extend(file_record(0, &[(Some(4), 2)]));
    stream.extend(header(0x20, 0x80, 0));
    stream.extend(header(0x02, 0, 0));
    stream.extend(data(2));
    stream.extend(&parts.mft_blob);
    stream.extend(crc());
    // $Boot.
    stream.extend(header(0x04, 0x80, 7));
    stream.extend(data(1024));
    stream.extend(file_record(7, &[(Some(0), 2)]));
    stream.extend(header(0x20, 0x80, 0));
    stream.extend(header(0x02, 0, 0));
    stream.extend(data(2));
    stream.extend(&parts.boot_data);
    stream.extend(crc());
    // $MFTMirr, regenerated.
    stream.extend(header(0x04, 0x80, 1));
    stream.extend(data(1024));
    stream.extend(file_record(1, &[(Some(2), 1)]));
    stream.extend(header(0x40, 0x80, 0));
    stream.extend(crc());
    // An empty slot and a free record.
    stream.extend(header(0x10, 0x80, 2));
    stream.extend(data(1024));
    stream.extend(vec![0_u8; 1024]);
    stream.extend(header(0x08, 0x80, 3));
    stream.extend(data(1024));
    stream.extend(file_record(3, &[(Some(50), 1)]));
    // $BadClus with its sparse $Bad stream.
    stream.extend(header(0x04, 0x80, 8));
    stream.extend(data(1024));
    stream.extend(record(
        8,
        &[attribute(0x80, "$Bad", &[(None, 64)], CLUSTER)],
    ));
    stream.extend(header(0x40, 0x80, 0));
    stream.extend(crc());
    // A fragmented user file with a sparse hole.
    stream.extend(header(0x04, 0x80, 9));
    stream.extend(data(1024));
    stream.extend(user_record(
        &parts.file_name,
        &[(Some(10), 1), (None, 2), (Some(20), 3)],
    ));
    stream.extend(header(0x20, 0x80, 0));
    stream.extend(header(0x02, 0, 0));
    stream.extend(data(1));
    stream.extend(&parts.user_data[0]);
    stream.extend(header(0x02, 0, 1));
    stream.extend(header(0x02, 0, 2));
    stream.extend(data(3));
    stream.extend(&parts.user_data[1]);
    stream.extend(crc());
    stream
}

pub fn image_from(descriptor: &[u8], body: &[u8], compression: u8) -> Vec<u8> {
    let mut bytes = vec![0_u8; 512];
    bytes[0..4].copy_from_slice(&[0xfe, 0xef, 1, compression]);
    bytes[4..8].copy_from_slice(&[1, 2, 3, 4]);
    bytes.extend(vec![0_u8; 0x400]);
    bytes.extend_from_slice(descriptor);
    for chunk in body.chunks(32 * 1024) {
        let mut encoder = ZlibEncoder::new(Vec::new(), Level::new(6));
        encoder.write_all(chunk).unwrap();
        let packet = encoder.finish().unwrap();
        bytes.extend_from_slice(&u16::try_from(packet.len() + 2).unwrap().to_le_bytes());
        bytes.extend(packet);
    }
    let terminator = bytes.len() as u64;
    bytes.extend_from_slice(&[0, 0]);
    bytes.extend_from_slice(&[0x77; 40]);
    let end_record = bytes.len() as u64;
    bytes.extend_from_slice(&0x0023_u32.to_le_bytes());
    bytes.extend_from_slice(&crate::container::RECORD_MAGIC.to_le_bytes());
    bytes.extend_from_slice(&24_u16.to_le_bytes());
    bytes.extend_from_slice(&0_u64.to_le_bytes());
    bytes.extend_from_slice(&(end_record - terminator).to_le_bytes());
    bytes.extend_from_slice(&0_u64.to_le_bytes());
    bytes
}

pub fn image_with(body: &[u8]) -> Vec<u8> {
    image_from(&descriptor_segment(&boot()), body, 10)
}

pub fn image() -> Vec<u8> {
    image_with(&body(&parts()))
}

/// A partition image whose descriptor holds a FAT32 boot sector.
pub fn non_ntfs_image() -> Vec<u8> {
    let mut fat = boot();
    fat[3..11].copy_from_slice(b"MSDOS5.0");
    image_from(&descriptor_segment(&fat), &body(&parts()), 10)
}

/// A valid NTFS image whose volume has no Windows boot loader (like XP).
pub fn image_without_boot_loader() -> Vec<u8> {
    let mut parts = parts();
    parts.file_name = "ntldr".to_owned();
    image_with(&body(&parts))
}
