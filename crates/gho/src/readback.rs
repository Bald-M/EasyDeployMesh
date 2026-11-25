//! Read-back verification of a restored volume.
//!
//! Restore writes are disjoint: every cluster run is reserved before it is
//! written, `$MFTMirr` is reserved the same way, and the backup boot sector
//! lies past the last cluster. So the bytes a restore wrote, read back from
//! the same offsets in the same order, must reproduce the write digest.

use crate::Error;
use crate::ntfs::Target;
use sha2::{Digest, Sha256};
use std::io::{Read, Seek, SeekFrom};

const READ_CHUNK: usize = 1024 * 1024;

/// Where a restore wrote and a digest of what it wrote, in write order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrittenRegions {
    /// `(offset, length)` in write order; contiguous writes are merged.
    extents: Vec<(u64, u64)>,
    digest: [u8; 32],
    bytes: u64,
}

impl WrittenRegions {
    /// Total bytes written to the target.
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Re-reads every written region from `volume` and fails unless it holds
    /// exactly the bytes the restore wrote. `progress` receives the bytes
    /// verified so far after each chunk and may abort by returning an error.
    pub fn verify<R: Read + Seek>(
        &self,
        volume: &mut R,
        progress: &mut dyn FnMut(u64) -> Result<(), Error>,
    ) -> Result<(), Error> {
        let mut digest = Sha256::new();
        let mut buffer = vec![0_u8; READ_CHUNK];
        let mut verified = 0_u64;
        for &(offset, length) in &self.extents {
            volume.seek(SeekFrom::Start(offset))?;
            let mut remaining = length;
            while remaining > 0 {
                let chunk = usize::try_from(remaining.min(READ_CHUNK as u64))
                    .expect("bounded by the read chunk");
                volume.read_exact(&mut buffer[..chunk])?;
                digest.update(&buffer[..chunk]);
                remaining -= chunk as u64;
                verified += chunk as u64;
                progress(verified)?;
            }
        }
        // `bytes` is the extents' sum; the length check is purely defensive.
        if verified != self.bytes || digest.finalize().as_slice() != self.digest {
            return Err(Error::ReadBackMismatch);
        }
        Ok(())
    }
}

/// Forwards writes to the real target while recording them for read-back.
pub(crate) struct Recording<T> {
    inner: T,
    extents: Vec<(u64, u64)>,
    digest: Sha256,
    bytes: u64,
}

impl<T> Recording<T> {
    pub fn new(inner: T) -> Self {
        Self {
            inner,
            extents: Vec::new(),
            digest: Sha256::new(),
            bytes: 0,
        }
    }

    pub fn finish(self) -> (T, WrittenRegions) {
        (
            self.inner,
            WrittenRegions {
                extents: self.extents,
                digest: self.digest.finalize().into(),
                bytes: self.bytes,
            },
        )
    }
}

impl<T: Target> Target for Recording<T> {
    fn write_at(&mut self, offset: u64, bytes: &[u8]) -> Result<(), Error> {
        self.inner.write_at(offset, bytes)?;
        if bytes.is_empty() {
            return Ok(());
        }
        let length = bytes.len() as u64;
        match self.extents.last_mut() {
            Some((start, previous)) if *start + *previous == offset => *previous += length,
            _ => self.extents.push((offset, length)),
        }
        self.digest.update(bytes);
        self.bytes += length;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ntfs::SeekTarget;
    use std::io::Cursor;

    fn record(volume: &mut Cursor<Vec<u8>>) -> WrittenRegions {
        let mut recording = Recording::new(SeekTarget { inner: volume });
        recording.write_at(512, &[1; 512]).unwrap();
        recording.write_at(1024, &[2; 1024]).unwrap();
        recording.write_at(8192, &[3; 512]).unwrap();
        recording.write_at(0, &[4; 512]).unwrap();
        recording.finish().1
    }

    #[test]
    fn merges_contiguous_writes_and_keeps_write_order() {
        let mut volume = Cursor::new(vec![0_u8; 16 * 1024]);
        let written = record(&mut volume);
        assert_eq!(written.extents, vec![(512, 1536), (8192, 512), (0, 512)]);
        assert_eq!(written.bytes(), 2560);
    }

    #[test]
    fn read_back_accepts_an_intact_volume_and_reports_progress() {
        let mut volume = Cursor::new(vec![0_u8; 16 * 1024]);
        let written = record(&mut volume);
        let mut reported = Vec::new();
        written
            .verify(&mut volume, &mut |done| {
                reported.push(done);
                Ok(())
            })
            .unwrap();
        assert_eq!(reported, vec![1536, 2048, 2560]);
    }

    #[test]
    fn read_back_rejects_a_changed_byte_in_any_written_region() {
        for corrupt in [512, 2047, 8192, 511] {
            let mut volume = Cursor::new(vec![0_u8; 16 * 1024]);
            let written = record(&mut volume);
            volume.get_mut()[corrupt] ^= 0xff;
            let error = written.verify(&mut volume, &mut |_| Ok(())).unwrap_err();
            assert!(matches!(error, Error::ReadBackMismatch), "{corrupt}");
        }
    }

    #[test]
    fn read_back_ignores_bytes_outside_the_written_regions() {
        let mut volume = Cursor::new(vec![0_u8; 16 * 1024]);
        let written = record(&mut volume);
        volume.get_mut()[4096] = 0xaa;
        written.verify(&mut volume, &mut |_| Ok(())).unwrap();
    }

    #[test]
    fn read_back_fails_on_a_short_volume_and_honours_abort() {
        let mut volume = Cursor::new(vec![0_u8; 16 * 1024]);
        let written = record(&mut volume);
        let mut short = Cursor::new(volume.get_ref()[..8192].to_vec());
        assert!(matches!(
            written.verify(&mut short, &mut |_| Ok(())).unwrap_err(),
            Error::Io(_)
        ));
        let aborted = written
            .verify(&mut volume, &mut |_| Err(Error::CorruptStream("cancelled")))
            .unwrap_err();
        assert!(matches!(aborted, Error::CorruptStream("cancelled")));
    }
}
