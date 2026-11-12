//! Bounds-checked Norton Ghost 11.x partition-image reader and NTFS restorer.
//!
//! A Ghost NTFS image is not a raw volume dump. It is a stream of MFT records
//! and the cluster runs of each record's non-resident attributes. Restoring
//! places every stored run at its original LCN on the target volume,
//! regenerates `$MFTMirr` and the backup boot sector, and leaves growing the
//! file system to the operating system. See `docs/gho-format.md`.

mod container;
#[cfg(any(test, feature = "fixtures"))]
pub mod fixtures;
mod ntfs;
mod readback;

pub use ntfs::{NtfsGeometry, RestoreOptions};
pub use readback::WrittenRegions;

use std::io::{self, Read, Seek, SeekFrom, Write};
use thiserror::Error;

/// Bumped whenever accepted images or restore semantics change, so cached
/// capability metadata is re-verified.
pub const PARSER_VERSION: u32 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    None,
    Fast,
    High(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileSystem {
    Ntfs,
}

impl FileSystem {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ntfs => "ntfs",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageInfo {
    pub compression: Compression,
    pub partition_count: u32,
    pub source_partition: u32,
    pub encrypted: bool,
    pub spanned: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerifiedPartition {
    pub info: ImageInfo,
    pub file_system: FileSystem,
    pub geometry: NtfsGeometry,
    /// Bytes in the logical (expanded) partition stream.
    pub expanded_size_bytes: u64,
    /// Whether an in-use MFT record is named `winload.exe`, i.e. the volume
    /// holds Windows that BCDBoot can configure (Vista and later).
    pub windows_boot_loader: bool,
}

impl VerifiedPartition {
    /// Minimum target partition size.
    pub fn volume_size_bytes(&self) -> u64 {
        self.geometry.volume_size_bytes()
    }
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("GHO input is truncated")]
    Truncated,
    #[error("GHO file magic is invalid")]
    InvalidMagic,
    #[error("GHO record structure is invalid")]
    InvalidRecord,
    #[error("GHO compression type {0} is unsupported")]
    UnsupportedCompression(u8),
    #[error("a GHS continuation file cannot be opened as the primary GHO")]
    SpannedUnsupported,
    #[error("GHO span header does not match the primary image")]
    SpanMismatch,
    #[error("encrypted GHO images are unsupported")]
    EncryptedUnsupported,
    #[error("GHO partition selection is invalid for an image containing {0} partitions")]
    PartitionCount(u32),
    #[error("GHO compressed packet is invalid")]
    CorruptBlock,
    #[error("GHO partition stream is invalid: {0}")]
    CorruptStream(&'static str),
    #[error("GHO partition does not contain an NTFS file system")]
    UnsupportedFileSystem,
    #[error("GHO NTFS volume is unsupported: {0}")]
    UnsupportedVolume(&'static str),
    #[error("target partition holds {available} bytes but the GHO volume needs {required}")]
    TargetTooSmall { required: u64, available: u64 },
    #[error("GHO expanded data exceeds the configured limit")]
    ExpandedLimit,
    #[error("restored volume does not read back the bytes that were written")]
    ReadBackMismatch,
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Presents a primary GHO and its ordered GHS files as one seekable stream.
/// Ghost span headers are validated and omitted from the virtual byte range.
pub struct SpanReader<R> {
    readers: Vec<R>,
    starts: Vec<u64>,
    lengths: Vec<u64>,
    position: u64,
    total: u64,
}

const FILE_MAGIC: u16 = 0xeffe;
const HEADER_SIZE: u64 = container::HEADER_SIZE;

impl<R: Read + Seek> SpanReader<R> {
    pub fn new(mut readers: Vec<R>) -> Result<Self, Error> {
        if readers.is_empty() {
            return Err(Error::Truncated);
        }
        let mut starts = Vec::with_capacity(readers.len());
        let mut lengths = Vec::with_capacity(readers.len());
        let mut total = 0_u64;
        let mut primary_header = [0_u8; HEADER_SIZE as usize];
        readers[0].seek(SeekFrom::Start(0))?;
        readers[0]
            .read_exact(&mut primary_header)
            .map_err(map_eof)?;
        if u16::from_le_bytes([primary_header[0], primary_header[1]]) != FILE_MAGIC
            || primary_header[2] != 1
        {
            return Err(Error::InvalidMagic);
        }
        let primary_id = &primary_header[4..8];
        let compression = primary_header[3];
        for (index, reader) in readers.iter_mut().enumerate() {
            let file_len = reader.seek(SeekFrom::End(0))?;
            let skipped = if index == 0 { 0 } else { HEADER_SIZE };
            if file_len < HEADER_SIZE {
                return Err(Error::Truncated);
            }
            if index > 0 {
                reader.seek(SeekFrom::Start(0))?;
                let mut header = [0_u8; HEADER_SIZE as usize];
                reader.read_exact(&mut header).map_err(map_eof)?;
                if u16::from_le_bytes([header[0], header[1]]) != FILE_MAGIC
                    || header[2] != 9
                    || header[3] != compression
                    || &header[4..8] != primary_id
                {
                    return Err(Error::SpanMismatch);
                }
            }
            let length = file_len - skipped;
            starts.push(total);
            lengths.push(length);
            total = total.checked_add(length).ok_or(Error::InvalidRecord)?;
        }
        Ok(Self {
            readers,
            starts,
            lengths,
            position: 0,
            total,
        })
    }

    pub fn len(&self) -> u64 {
        self.total
    }

    pub fn is_empty(&self) -> bool {
        self.total == 0
    }
}

impl<R: Read + Seek> Read for SpanReader<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let mut written = 0;
        while written < output.len() && self.position < self.total {
            let index = self
                .starts
                .partition_point(|start| *start <= self.position)
                .saturating_sub(1);
            let relative = self.position - self.starts[index];
            let available = self.lengths[index] - relative;
            let wanted = usize::try_from(available.min((output.len() - written) as u64))
                .unwrap_or(output.len() - written);
            let skipped = if index == 0 { 0 } else { HEADER_SIZE };
            self.readers[index].seek(SeekFrom::Start(skipped + relative))?;
            let count = self.readers[index].read(&mut output[written..written + wanted])?;
            if count == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "GHO span ended before its measured length",
                ));
            }
            written += count;
            self.position += count as u64;
        }
        Ok(written)
    }
}

impl<R: Read + Seek> Seek for SpanReader<R> {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        let next = match position {
            SeekFrom::Start(value) => i128::from(value),
            SeekFrom::End(value) => i128::from(self.total) + i128::from(value),
            SeekFrom::Current(value) => i128::from(self.position) + i128::from(value),
        };
        if !(0..=i128::from(self.total)).contains(&next) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid GHO span seek",
            ));
        }
        self.position = next as u64;
        Ok(self.position)
    }
}

/// Reads the container structure without decoding partition data.
pub fn inspect<R: Read + Seek>(reader: &mut R) -> Result<ImageInfo, Error> {
    Ok(container::locate(reader)?.info)
}

/// Decodes and validates the whole partition stream without writing.
pub fn verify<R: Read + Seek>(
    reader: &mut R,
    expanded_limit: u64,
) -> Result<VerifiedPartition, Error> {
    analyze_partition(reader, 1, &mut io::sink(), expanded_limit)
}

/// Validates the partition stream as a dry-run restore and copies every
/// logical stream byte into `sink` (for example a digest).
pub fn analyze_partition<R: Read + Seek, S: Write>(
    reader: &mut R,
    source_partition: u32,
    sink: &mut S,
    expanded_limit: u64,
) -> Result<VerifiedPartition, Error> {
    process(
        reader,
        source_partition,
        sink,
        &mut ntfs::DryRun,
        RestoreOptions {
            target_size_bytes: u64::MAX,
            hidden_sectors: None,
            expanded_limit,
        },
    )
}

/// Result of a restore: the validated partition and a record of every region
/// written, for read-back verification once the writes are durable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoredPartition {
    pub partition: VerifiedPartition,
    pub written: WrittenRegions,
}

/// Restores the partition onto `target`, which must address the target
/// volume from its first byte. Every logical stream byte is also copied into
/// `sink`. On error the target may be partially written.
pub fn restore_partition<R: Read + Seek, W: Write + Seek, S: Write>(
    reader: &mut R,
    source_partition: u32,
    target: &mut W,
    sink: &mut S,
    options: RestoreOptions,
) -> Result<RestoredPartition, Error> {
    let mut target = readback::Recording::new(ntfs::SeekTarget { inner: target });
    let partition = process(reader, source_partition, sink, &mut target, options)?;
    let (target, written) = target.finish();
    target.inner.flush()?;
    Ok(RestoredPartition { partition, written })
}

fn process<R: Read + Seek, S: Write, T: ntfs::Target>(
    reader: &mut R,
    source_partition: u32,
    sink: &mut S,
    target: &mut T,
    options: RestoreOptions,
) -> Result<VerifiedPartition, Error> {
    let layout = container::locate(reader)?;
    if source_partition != 1 {
        return Err(Error::PartitionCount(layout.info.partition_count));
    }
    let mut stream = ntfs::Tee {
        inner: container::LogicalStream::new(reader, &layout),
        sink,
        consumed: 0,
        limit: options.expanded_limit,
    };
    let outcome = ntfs::run(&mut stream, target, options)?;
    Ok(VerifiedPartition {
        info: layout.info,
        file_system: FileSystem::Ntfs,
        geometry: outcome.geometry,
        expanded_size_bytes: stream.consumed,
        windows_boot_loader: outcome.windows_boot_loader,
    })
}

pub(crate) fn map_eof(error: io::Error) -> Error {
    if error.kind() == io::ErrorKind::UnexpectedEof {
        Error::Truncated
    } else {
        Error::Io(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::*;
    use crate::ntfs::test_support::*;
    use std::io::Cursor;

    fn options(target_size_bytes: u64) -> RestoreOptions {
        RestoreOptions {
            target_size_bytes,
            hidden_sectors: Some(2048),
            expanded_limit: u64::MAX,
        }
    }

    fn restore(bytes: Vec<u8>) -> Result<(VerifiedPartition, Vec<u8>, Vec<u8>), Error> {
        let mut target = Cursor::new(vec![0xcc_u8; VOLUME_BYTES as usize]);
        let mut logical = Vec::new();
        let restored = restore_partition(
            &mut Cursor::new(bytes),
            1,
            &mut target,
            &mut logical,
            options(VOLUME_BYTES),
        )?;
        let mut volume = Cursor::new(target.into_inner());
        restored
            .written
            .verify(&mut volume, &mut |_| Ok(()))
            .expect("a restored volume must read back what was written");
        Ok((restored.partition, volume.into_inner(), logical))
    }

    fn cluster(volume: &[u8], lcn: u64, count: u64) -> &[u8] {
        &volume[(lcn * CLUSTER) as usize..((lcn + count) * CLUSTER) as usize]
    }

    #[test]
    fn restores_runs_at_their_lcns_and_regenerates_metadata() {
        let parts = parts();
        let (verified, volume, logical) = restore(image_with(&body(&parts))).unwrap();
        assert_eq!(verified.file_system, FileSystem::Ntfs);
        assert_eq!(verified.geometry.cluster_size, CLUSTER);
        assert_eq!(verified.volume_size_bytes(), VOLUME_BYTES);
        assert_eq!(verified.info.compression, Compression::High(9));
        assert_eq!(verified.expanded_size_bytes, logical.len() as u64);

        assert_eq!(cluster(&volume, 4, 2), parts.mft_blob.as_slice());
        assert_eq!(cluster(&volume, 10, 1), parts.user_data[0].as_slice());
        assert_eq!(cluster(&volume, 20, 3), parts.user_data[1].as_slice());
        // Sparse clusters and the free record's run are untouched.
        assert!(cluster(&volume, 11, 2).iter().all(|byte| *byte == 0xcc));
        assert!(cluster(&volume, 50, 1).iter().all(|byte| *byte == 0xcc));
        // $MFTMirr mirrors the first $MFT cluster.
        assert_eq!(cluster(&volume, 2, 1), &parts.mft_blob[..CLUSTER as usize]);
        // Boot sector and its backup carry the target's hidden sectors.
        let mut expected_boot = boot();
        expected_boot[0x1c..0x20].copy_from_slice(&2048_u32.to_le_bytes());
        assert_eq!(&volume[..512], expected_boot.as_slice());
        assert_eq!(&volume[512..2 * CLUSTER as usize], &parts.boot_data[512..]);
        let backup = (TOTAL_SECTORS * 512) as usize;
        assert_eq!(&volume[backup..backup + 512], expected_boot.as_slice());
    }

    #[test]
    fn read_back_covers_runs_mirror_and_backup_boot_sector() {
        let parts = parts();
        let mut target = Cursor::new(vec![0xcc_u8; VOLUME_BYTES as usize]);
        let restored = restore_partition(
            &mut Cursor::new(image_with(&body(&parts))),
            1,
            &mut target,
            &mut io::sink(),
            options(VOLUME_BYTES),
        )
        .unwrap();
        let intact = target.into_inner();
        let backup = (TOTAL_SECTORS * 512) as usize;
        // A user-data run, the regenerated $MFTMirr, the patched boot sector
        // and its backup are all covered by the read-back.
        for offset in [
            (20 * CLUSTER) as usize,
            (2 * CLUSTER) as usize,
            0x1c,
            backup + 0x1c,
        ] {
            let mut volume = intact.clone();
            volume[offset] ^= 0xff;
            let error = restored
                .written
                .verify(&mut Cursor::new(volume), &mut |_| Ok(()))
                .unwrap_err();
            assert!(matches!(error, Error::ReadBackMismatch), "{offset}");
        }
        // Sparse and unallocated clusters were never written.
        let mut volume = intact.clone();
        volume[(11 * CLUSTER) as usize] ^= 0xff;
        restored
            .written
            .verify(&mut Cursor::new(volume), &mut |_| Ok(()))
            .unwrap();
    }

    #[test]
    fn dry_run_matches_restore_and_logical_stream() {
        let bytes = image();
        let (restored, _, logical) = restore(bytes.clone()).unwrap();
        let mut sink = Vec::new();
        let analyzed = analyze_partition(&mut Cursor::new(&bytes), 1, &mut sink, u64::MAX).unwrap();
        assert_eq!(analyzed, restored);
        assert_eq!(sink, logical);
        assert_eq!(
            verify(&mut Cursor::new(&bytes), u64::MAX).unwrap(),
            restored
        );
        let descriptor = descriptor_segment(&boot());
        assert_eq!(&logical[..descriptor.len()], descriptor.as_slice());
    }

    #[test]
    fn reports_whether_the_volume_holds_a_windows_boot_loader() {
        assert!(
            verify(&mut Cursor::new(image()), u64::MAX)
                .unwrap()
                .windows_boot_loader
        );
        let legacy = verify(&mut Cursor::new(image_without_boot_loader()), u64::MAX).unwrap();
        assert!(!legacy.windows_boot_loader);
    }

    #[test]
    fn inspects_container_metadata() {
        let info = inspect(&mut Cursor::new(image())).unwrap();
        assert_eq!(info.partition_count, 1);
        assert_eq!(info.compression, Compression::High(9));
    }

    #[test]
    fn rejects_a_target_smaller_than_the_volume() {
        let mut target = Cursor::new(vec![0_u8; VOLUME_BYTES as usize]);
        let error = restore_partition(
            &mut Cursor::new(image()),
            1,
            &mut target,
            &mut io::sink(),
            options(VOLUME_BYTES - 1),
        )
        .unwrap_err();
        assert!(matches!(error, Error::TargetTooSmall { .. }));
        assert!(
            target.into_inner().iter().all(|byte| *byte == 0),
            "nothing may be written"
        );
    }

    #[test]
    fn rejects_other_partitions_and_unverified_compression() {
        assert!(matches!(
            verify_partition(image(), 2),
            Err(Error::PartitionCount(1))
        ));
        for (tag, expected) in [(0, 0), (2, 2), (1, 1), (11, 11)] {
            let bytes = image_from(&descriptor_segment(&boot()), &body(&parts()), tag);
            assert!(
                matches!(inspect(&mut Cursor::new(bytes)), Err(Error::UnsupportedCompression(value)) if value == expected),
                "tag {tag}"
            );
        }
    }

    fn verify_partition(bytes: Vec<u8>, partition: u32) -> Result<VerifiedPartition, Error> {
        analyze_partition(
            &mut Cursor::new(bytes),
            partition,
            &mut io::sink(),
            u64::MAX,
        )
    }

    #[test]
    fn rejects_encrypted_and_continuation_files() {
        let mut encrypted = image();
        encrypted[12] = 0x02;
        assert!(matches!(
            inspect(&mut Cursor::new(encrypted)),
            Err(Error::EncryptedUnsupported)
        ));
        let mut span = image();
        span[2] = 9;
        assert!(matches!(
            inspect(&mut Cursor::new(span)),
            Err(Error::SpannedUnsupported)
        ));
    }

    #[test]
    fn rejects_a_non_ntfs_descriptor() {
        let mut fat = boot();
        fat[3..11].copy_from_slice(b"MSDOS5.0");
        let bytes = image_from(&descriptor_segment(&fat), &body(&parts()), 10);
        assert!(matches!(
            verify_partition(bytes, 1),
            Err(Error::UnsupportedFileSystem)
        ));
    }

    #[test]
    fn rejects_a_boot_sector_that_differs_from_the_descriptor() {
        let mut parts = parts();
        parts.boot_data[0x30] ^= 1;
        assert!(matches!(
            verify_partition(image_with(&body(&parts)), 1),
            Err(Error::CorruptStream(_))
        ));
    }

    fn replace(stream: &mut Vec<u8>, from: &[u8], to: &[u8]) {
        let at = stream
            .windows(from.len())
            .position(|window| window == from)
            .expect("pattern present");
        stream.splice(at..at + from.len(), to.iter().copied());
    }

    fn assert_corrupt(stream: Vec<u8>) {
        let result = verify_partition(image_with(&stream), 1);
        assert!(
            matches!(result, Err(Error::CorruptStream(_) | Error::Truncated)),
            "{result:?}"
        );
    }

    #[test]
    fn rejects_structural_stream_corruption() {
        let parts = parts();
        let original = body(&parts);

        // Data length disagrees with the mapping pairs.
        let mut stream = original.clone();
        let run = [header(0x02, 0, 0), data(1)].concat();
        let wrong = [header(0x02, 0, 0), data(2)].concat();
        let at = stream
            .windows(run.len())
            .rposition(|window| window == run.as_slice())
            .unwrap();
        stream.splice(at..at + run.len(), wrong);
        assert_corrupt(stream);

        // Runs out of order.
        let mut stream = original.clone();
        replace(&mut stream, &header(0x02, 0, 1), &header(0x02, 0, 5));
        assert_corrupt(stream);

        // A sparse run carrying data.
        let mut stream = original.clone();
        let sparse = [header(0x02, 0, 1), header(0x02, 0, 2)].concat();
        let with_data = [
            header(0x02, 0, 1),
            data(2),
            vec![0; 2 * CLUSTER as usize],
            header(0x02, 0, 2),
        ]
        .concat();
        replace(&mut stream, &sparse, &with_data);
        assert_corrupt(stream);

        // Unknown packet tag.
        let mut stream = original.clone();
        stream.push(0x61);
        assert_corrupt(stream);

        // Stream that does not start with $MFT.
        let mut stream = original.clone();
        stream[1] = 0x10;
        assert_corrupt(stream);

        // Missing CRC terminator: record ends with another record header.
        let mut stream = original.clone();
        stream.truncate(stream.len() - 10);
        assert_corrupt(stream);
    }

    #[test]
    fn rejects_runs_that_overlap_or_leave_the_volume() {
        let parts = parts();
        let original = body(&parts);
        let user = user_record("WINLOAD.EXE", &[(Some(10), 1), (None, 2), (Some(20), 3)]);

        let overlapping = user_record("WINLOAD.EXE", &[(Some(4), 1), (None, 2), (Some(20), 3)]);
        let mut stream = original.clone();
        replace(&mut stream, &user, &overlapping);
        assert_corrupt(stream);

        let outside = user_record("WINLOAD.EXE", &[(Some(10), 1), (None, 2), (Some(62), 3)]);
        let mut stream = original.clone();
        replace(&mut stream, &user, &outside);
        assert_corrupt(stream);
    }

    #[test]
    fn rejects_omitted_data_that_cannot_be_regenerated() {
        let parts = parts();
        let mut stream = body(&parts);
        let section = [header(0x20, 0x80, 0), header(0x02, 0, 0), data(1)].concat();
        let at = stream
            .windows(section.len())
            .rposition(|window| window == section.as_slice())
            .unwrap();
        // Replace the user file's whole data section with an omission marker.
        let end = stream.len() - 10;
        stream.splice(at..end, header(0x40, 0x80, 0));
        assert!(matches!(
            verify_partition(image_with(&stream), 1),
            Err(Error::UnsupportedVolume(_))
        ));
    }

    #[test]
    fn rejects_corrupt_packets_and_end_records() {
        let bytes = image();
        let descriptor_end = 512 + 0x400 + descriptor_segment(&boot()).len();

        let mut flipped = bytes.clone();
        flipped[descriptor_end + 8] ^= 0xff;
        assert!(verify_partition(flipped, 1).is_err());

        let mut bad_length = bytes.clone();
        bad_length[descriptor_end..descriptor_end + 2].copy_from_slice(&2_u16.to_le_bytes());
        assert!(matches!(
            verify_partition(bad_length, 1),
            Err(Error::CorruptBlock)
        ));

        let mut bad_end = bytes.clone();
        let len = bad_end.len();
        bad_end[len - 16] ^= 1;
        assert!(matches!(
            inspect(&mut Cursor::new(bad_end)),
            Err(Error::InvalidRecord)
        ));
    }

    #[test]
    fn enforces_the_expanded_limit() {
        let bytes = image();
        let full = verify(&mut Cursor::new(&bytes), u64::MAX)
            .unwrap()
            .expanded_size_bytes;
        assert!(matches!(
            verify(&mut Cursor::new(&bytes), full - 1),
            Err(Error::ExpandedLimit)
        ));
        assert!(verify(&mut Cursor::new(&bytes), full).is_ok());
    }

    #[test]
    fn rejects_every_truncation_without_panicking() {
        let bytes = image();
        for length in (0..bytes.len()).step_by(97) {
            assert!(verify(&mut Cursor::new(&bytes[..length]), u64::MAX).is_err());
        }
    }

    fn span(primary: &[u8], payload: &[u8], image_id: [u8; 4]) -> (Vec<u8>, Vec<u8>) {
        let mut primary = primary.to_vec();
        primary[4..8].copy_from_slice(&image_id);
        let mut continuation = vec![0_u8; HEADER_SIZE as usize];
        continuation[0..4].copy_from_slice(&[0xfe, 0xef, 9, primary[3]]);
        continuation[4..8].copy_from_slice(&image_id);
        continuation.extend_from_slice(payload);
        (primary, continuation)
    }

    #[test]
    fn restores_from_a_split_span_set() {
        let whole = image();
        let split = whole.len() / 2;
        let (primary, continuation) = span(&whole[..split], &whole[split..], [1, 2, 3, 4]);
        let mut reader = SpanReader::new(vec![Cursor::new(primary), Cursor::new(continuation)])
            .expect("matching spans should form a virtual stream");
        assert_eq!(reader.len(), whole.len() as u64);
        let joined = verify(&mut reader, u64::MAX).unwrap();
        assert_eq!(joined, verify(&mut Cursor::new(&whole), u64::MAX).unwrap());
    }

    #[test]
    fn span_reader_rejects_a_mismatched_image_id() {
        let primary = image();
        let (primary, mut continuation) = span(&primary, b"second", [1, 2, 3, 4]);
        continuation[4] = 9;
        assert!(matches!(
            SpanReader::new(vec![Cursor::new(primary), Cursor::new(continuation)]),
            Err(Error::SpanMismatch)
        ));
    }

    /// Run with `EASYDEPLOYMESH_GHO_PATH=/path/to/image.gho cargo test -p
    /// easydeploymesh-gho -- --ignored` to validate a real Ghost image.
    #[test]
    #[ignore = "requires EASYDEPLOYMESH_GHO_PATH to point to a real Ghost GHO image"]
    fn verifies_a_real_ghost_image() {
        let path =
            std::env::var_os("EASYDEPLOYMESH_GHO_PATH").expect("set EASYDEPLOYMESH_GHO_PATH");
        let mut file = std::fs::File::open(path).unwrap();
        let verified = verify(&mut file, u64::MAX).unwrap();
        assert_eq!(verified.file_system, FileSystem::Ntfs);
        assert!(verified.volume_size_bytes() > 0);
    }

    /// Restores a real image into a file in the temp directory (sparse on
    /// APFS) and reads every written region back.
    #[test]
    #[ignore = "requires EASYDEPLOYMESH_GHO_PATH and free space for the restored data"]
    fn restores_and_reads_back_a_real_ghost_image() {
        let path =
            std::env::var_os("EASYDEPLOYMESH_GHO_PATH").expect("set EASYDEPLOYMESH_GHO_PATH");
        let volume_path = std::env::temp_dir().join(format!(
            "easydeploymesh-gho-readback-{}.img",
            std::process::id()
        ));
        let mut volume = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&volume_path)
            .unwrap();
        let result = (|| {
            let restored = restore_partition(
                &mut std::fs::File::open(&path)?,
                1,
                &mut volume,
                &mut io::sink(),
                RestoreOptions {
                    target_size_bytes: u64::MAX,
                    hidden_sectors: Some(2048),
                    expanded_limit: u64::MAX,
                },
            )?;
            volume.sync_all()?;
            restored.written.verify(&mut volume, &mut |_| Ok(()))?;
            Ok::<_, Error>(restored.written.bytes())
        })();
        std::fs::remove_file(&volume_path).unwrap();
        assert!(result.unwrap() > 0);
    }
}
