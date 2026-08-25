//! Bounds-checked, read-only Norton Ghost 11.x/12.x partition-image decoder.

use flate2::read::ZlibDecoder;
use std::io::{self, Read, Seek, SeekFrom, Write};
use thiserror::Error;

pub const PARSER_VERSION: u32 = 3;
const HEADER_SIZE: u64 = 512;
const RECORD_HEADER_SIZE: usize = 10;
const RECORD_MAGIC: u32 = 0x012f_18d8;
const BLOCK_SIZE: usize = 32 * 1024;
const MAX_STORED_LEN: usize = 33_002;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    None,
    Fast,
    High(u8),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageInfo {
    pub compression: Compression,
    pub partition_count: u32,
    pub source_partition: u32,
    pub encrypted: bool,
    pub spanned: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedPartition {
    pub info: ImageInfo,
    pub expanded_size_bytes: u64,
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
    #[error("GHO compressed block is invalid")]
    CorruptBlock,
    #[error("GHO expanded data exceeds the configured limit")]
    ExpandedLimit,
    #[error(transparent)]
    Io(#[from] io::Error),
}

#[derive(Debug, Clone)]
struct Partition {
    spans: Vec<(u64, u64)>,
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
        if u16::from_le_bytes([primary_header[0], primary_header[1]]) != 0xeffe
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
                if u16::from_le_bytes([header[0], header[1]]) != 0xeffe
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

pub fn inspect<R: Read + Seek>(reader: &mut R) -> Result<ImageInfo, Error> {
    let (_, info) = parse(reader)?;
    Ok(info)
}

pub fn verify<R: Read + Seek>(
    reader: &mut R,
    expanded_limit: u64,
) -> Result<VerifiedPartition, Error> {
    let mut sink = io::sink();
    let (info, expanded_size_bytes) = decode_partition(reader, 1, &mut sink, expanded_limit)?;
    Ok(VerifiedPartition {
        info,
        expanded_size_bytes,
    })
}

pub fn decode_partition<R: Read + Seek, W: Write>(
    reader: &mut R,
    source_partition: u32,
    output: &mut W,
    expanded_limit: u64,
) -> Result<(ImageInfo, u64), Error> {
    let (partitions, info) = parse(reader)?;
    if source_partition == 0 || source_partition > info.partition_count {
        return Err(Error::PartitionCount(info.partition_count));
    }
    let partition = partitions
        .get((source_partition - 1) as usize)
        .ok_or(Error::PartitionCount(info.partition_count))?;
    let mut total = 0_u64;
    let mut compressed = vec![0_u8; MAX_STORED_LEN];
    let mut expanded = vec![0_u8; BLOCK_SIZE + 1024];
    for &(start, end) in &partition.spans {
        let mut offset = start;
        while offset.checked_add(2).is_some_and(|next| next <= end) {
            reader.seek(SeekFrom::Start(offset))?;
            let mut length = [0_u8; 2];
            reader.read_exact(&mut length).map_err(map_eof)?;
            let stored = usize::from(u16::from_le_bytes(length));
            if stored == 0 {
                break;
            }
            if !(3..=MAX_STORED_LEN).contains(&stored) {
                return Err(Error::CorruptBlock);
            }
            let data_len = stored - 2;
            let next = offset
                .checked_add(stored as u64)
                .ok_or(Error::CorruptBlock)?;
            if next > end {
                return Err(Error::Truncated);
            }
            reader
                .read_exact(&mut compressed[..data_len])
                .map_err(map_eof)?;
            let count = match info.compression {
                Compression::None => {
                    if data_len > expanded.len() {
                        return Err(Error::CorruptBlock);
                    }
                    expanded[..data_len].copy_from_slice(&compressed[..data_len]);
                    data_len
                }
                Compression::Fast => fast_lz(&compressed[..data_len], &mut expanded)?,
                Compression::High(_) => zlib(&compressed[..data_len], &mut expanded)?,
            };
            total = total
                .checked_add(count as u64)
                .ok_or(Error::ExpandedLimit)?;
            if total > expanded_limit {
                return Err(Error::ExpandedLimit);
            }
            output.write_all(&expanded[..count])?;
            offset = next;
        }
    }
    Ok((info, total))
}

fn parse<R: Read + Seek>(reader: &mut R) -> Result<(Vec<Partition>, ImageInfo), Error> {
    let file_len = reader.seek(SeekFrom::End(0))?;
    if file_len < HEADER_SIZE {
        return Err(Error::Truncated);
    }
    reader.seek(SeekFrom::Start(0))?;
    let mut header = [0_u8; HEADER_SIZE as usize];
    reader.read_exact(&mut header).map_err(map_eof)?;
    if u16::from_le_bytes([header[0], header[1]]) != 0xeffe {
        return Err(Error::InvalidMagic);
    }
    if header[2] != 1 {
        return Err(Error::SpannedUnsupported);
    }
    if header[12] & 0x02 != 0 {
        return Err(Error::EncryptedUnsupported);
    }
    let compression = match header[3] {
        0 => Compression::None,
        2 => Compression::Fast,
        value @ 3..=9 => Compression::High(value),
        // Some Ghost 11/12 producers encode Z9 as tag 10. Decompression is
        // still zlib; normalize the displayed level without rejecting it.
        10 => Compression::High(9),
        value => return Err(Error::UnsupportedCompression(value)),
    };
    let mut partitions: Vec<Partition> = Vec::new();
    let mut offset = HEADER_SIZE;
    while offset < file_len {
        let record = find_record(reader, offset, file_len)?;
        let Some((record_offset, kind, body_len)) = record else {
            break;
        };
        let body_end = record_offset
            .checked_add(RECORD_HEADER_SIZE as u64)
            .and_then(|v| v.checked_add(body_len as u64))
            .ok_or(Error::InvalidRecord)?;
        if body_end > file_len {
            return Err(Error::Truncated);
        }
        match kind {
            0x0603 => {
                let data_start = body_end
                    .checked_add(HEADER_SIZE)
                    .ok_or(Error::InvalidRecord)?;
                if data_start > file_len {
                    return Err(Error::Truncated);
                }
                reader.seek(SeekFrom::Start(body_end))?;
                let mut magic = [0_u8; 2];
                reader.read_exact(&mut magic).map_err(map_eof)?;
                if u16::from_le_bytes(magic) != 0xeffe {
                    return Err(Error::InvalidMagic);
                }
                let data_end = find_record(reader, data_start, file_len)?.map_or(file_len, |v| v.0);
                partitions.push(Partition {
                    spans: vec![(data_start, data_end)],
                });
                offset = data_end;
            }
            0x0703 => {
                let mut data_start = body_end;
                if data_start + HEADER_SIZE <= file_len {
                    reader.seek(SeekFrom::Start(data_start))?;
                    let mut magic = [0_u8; 2];
                    reader.read_exact(&mut magic).map_err(map_eof)?;
                    if u16::from_le_bytes(magic) == 0xeffe {
                        data_start += HEADER_SIZE;
                    }
                }
                let data_end = find_record(reader, data_start, file_len)?.map_or(file_len, |v| v.0);
                partitions
                    .last_mut()
                    .ok_or(Error::InvalidRecord)?
                    .spans
                    .push((data_start, data_end));
                offset = data_end;
            }
            0x0023 => break,
            _ => offset = body_end,
        }
    }
    let count = u32::try_from(partitions.len()).map_err(|_| Error::InvalidRecord)?;
    if count == 0 {
        return Err(Error::PartitionCount(count));
    }
    Ok((
        partitions,
        ImageInfo {
            compression,
            partition_count: count,
            // Inspection describes the container. Callers must explicitly choose
            // a one-based partition when more than one stream is present.
            source_partition: 1,
            encrypted: false,
            spanned: false,
        },
    ))
}

fn find_record<R: Read + Seek>(
    reader: &mut R,
    start: u64,
    end: u64,
) -> Result<Option<(u64, u16, u16)>, Error> {
    let mut offset = start;
    let mut buffer = vec![0_u8; 64 * 1024 + RECORD_HEADER_SIZE];
    while offset < end {
        reader.seek(SeekFrom::Start(offset))?;
        let remaining =
            usize::try_from((end - offset).min(buffer.len() as u64)).unwrap_or(buffer.len());
        let read = reader.read(&mut buffer[..remaining])?;
        if read < RECORD_HEADER_SIZE {
            return Ok(None);
        }
        for i in 0..=read - RECORD_HEADER_SIZE {
            if u32::from_le_bytes(buffer[i + 4..i + 8].try_into().unwrap()) == RECORD_MAGIC {
                let kind = u16::from_le_bytes(buffer[i..i + 2].try_into().unwrap());
                if matches!(kind, 0x0006 | 0x0603 | 0x0703 | 0x0023) {
                    let len = u16::from_le_bytes(buffer[i + 8..i + 10].try_into().unwrap());
                    return Ok(Some((offset + i as u64, kind, len)));
                }
            }
        }
        offset = offset
            .checked_add((read - (RECORD_HEADER_SIZE - 1)) as u64)
            .ok_or(Error::InvalidRecord)?;
    }
    Ok(None)
}

fn fast_lz(data: &[u8], out: &mut [u8]) -> Result<usize, Error> {
    if data.len() < 4 {
        return Err(Error::CorruptBlock);
    }
    if data[0] == 1 {
        let raw = &data[4..];
        if raw.len() > out.len() {
            return Err(Error::CorruptBlock);
        }
        out[..raw.len()].copy_from_slice(raw);
        return Ok(raw.len());
    }
    let mut table = [-1_i32; 4096];
    let sentinel = b"123456789012345678";
    let (mut src, mut dst, mut control) = (4_usize, 0_usize, 1_u32);
    let (mut literal_run, mut previous_run) = (0_u16, 0_u16);
    while src < data.len() {
        if control == 1 {
            if src + 1 >= data.len() {
                break;
            }
            control = u32::from(data[src]) | (u32::from(data[src + 1]) << 8) | 0x10000;
            src += 2;
        }
        let tokens = if data.len().saturating_sub(32) < src {
            1
        } else {
            16
        };
        for _ in 0..tokens {
            if src >= data.len() {
                break;
            }
            if control & 1 != 0 {
                if src + 1 >= data.len() {
                    return Err(Error::Truncated);
                }
                let index = usize::from(data[src + 1]) | (usize::from(data[src] & 0xf0) << 4);
                let extra = usize::from(data[src] & 0x0f);
                let match_pos = table[index];
                let match_start = dst;
                for j in 0..3 + extra {
                    if dst >= out.len() {
                        return Err(Error::CorruptBlock);
                    }
                    out[dst] = if match_pos < 0 {
                        sentinel.get(j).copied().unwrap_or(0)
                    } else {
                        let at = usize::try_from(match_pos)
                            .unwrap()
                            .checked_add(j)
                            .ok_or(Error::CorruptBlock)?;
                        if at >= dst {
                            return Err(Error::CorruptBlock);
                        }
                        out[at]
                    };
                    dst += 1;
                }
                src += 2;
                if literal_run > 0 {
                    let pos = match_start
                        .checked_sub(usize::from(literal_run))
                        .ok_or(Error::CorruptBlock)?;
                    if pos + 2 < dst {
                        table[hash(out[pos], out[pos + 1], out[pos + 2])] = pos as i32;
                    }
                    if previous_run == 2 && pos + 3 < dst {
                        table[hash(out[pos + 1], out[pos + 2], out[pos + 3])] = (pos + 1) as i32;
                    }
                    literal_run = 0;
                    previous_run = 0;
                }
                table[index] = match_start as i32;
            } else {
                if dst >= out.len() {
                    return Err(Error::CorruptBlock);
                }
                out[dst] = data[src];
                dst += 1;
                src += 1;
                literal_run += 1;
                previous_run = literal_run;
                if literal_run == 3 {
                    let pos = dst - 3;
                    table[hash(out[pos], out[pos + 1], out[pos + 2])] = pos as i32;
                    literal_run = 2;
                    previous_run = 2;
                }
            }
            control >>= 1;
            if control == 1 {
                break;
            }
        }
    }
    Ok(dst)
}

fn hash(a: u8, b: u8, c: u8) -> usize {
    let value = i32::from(c) ^ (16 * (i32::from(b) ^ (16 * i32::from(a))));
    (((-24993_i32).wrapping_mul(value) as u32 >> 4) & 0xfff) as usize
}

fn zlib(data: &[u8], out: &mut [u8]) -> Result<usize, Error> {
    if data.len() < 4 {
        return Err(Error::CorruptBlock);
    }
    if data[0] == 1 {
        let raw = &data[4..];
        if raw.len() > out.len() {
            return Err(Error::CorruptBlock);
        }
        out[..raw.len()].copy_from_slice(raw);
        return Ok(raw.len());
    }
    let mut decoder = ZlibDecoder::new(&data[4..]);
    let mut count = 0;
    while count < out.len() {
        let n = decoder
            .read(&mut out[count..])
            .map_err(|_| Error::CorruptBlock)?;
        if n == 0 {
            break;
        }
        count += n;
    }
    let mut extra = [0_u8; 1];
    if decoder.read(&mut extra).map_err(|_| Error::CorruptBlock)? != 0 {
        return Err(Error::CorruptBlock);
    }
    Ok(count)
}

fn map_eof(error: io::Error) -> Error {
    if error.kind() == io::ErrorKind::UnexpectedEof {
        Error::Truncated
    } else {
        Error::Io(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn image(compression: u8, block: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0_u8; 512];
        bytes[0..4].copy_from_slice(&[0xfe, 0xef, 1, compression]);
        bytes.extend_from_slice(&0x0603_u32.to_le_bytes());
        bytes.extend_from_slice(&RECORD_MAGIC.to_le_bytes());
        bytes.extend_from_slice(&20_u16.to_le_bytes());
        bytes.extend_from_slice(&[0; 20]);
        let mut ph = vec![0_u8; 512];
        ph[0..2].copy_from_slice(&0xeffe_u16.to_le_bytes());
        bytes.extend(ph);
        bytes.extend_from_slice(&u16::try_from(block.len() + 2).unwrap().to_le_bytes());
        bytes.extend_from_slice(block);
        bytes.extend_from_slice(&0x23_u32.to_le_bytes());
        bytes.extend_from_slice(&RECORD_MAGIC.to_le_bytes());
        bytes.extend_from_slice(&0_u16.to_le_bytes());
        bytes
    }

    fn multi_partition_image(first: &[u8], second: &[u8]) -> Vec<u8> {
        let mut bytes = image(0, first);
        bytes.truncate(bytes.len() - RECORD_HEADER_SIZE);
        bytes.extend_from_slice(&0x0603_u32.to_le_bytes());
        bytes.extend_from_slice(&RECORD_MAGIC.to_le_bytes());
        bytes.extend_from_slice(&20_u16.to_le_bytes());
        bytes.extend_from_slice(&[0; 20]);
        let mut header = vec![0_u8; HEADER_SIZE as usize];
        header[0..2].copy_from_slice(&0xeffe_u16.to_le_bytes());
        bytes.extend(header);
        bytes.extend_from_slice(&u16::try_from(second.len() + 2).unwrap().to_le_bytes());
        bytes.extend_from_slice(second);
        bytes.extend_from_slice(&0x23_u32.to_le_bytes());
        bytes.extend_from_slice(&RECORD_MAGIC.to_le_bytes());
        bytes.extend_from_slice(&0_u16.to_le_bytes());
        bytes
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
    fn decodes_uncompressed_partition() {
        let bytes = image(0, b"hello");
        let verified = verify(&mut Cursor::new(bytes), 100).unwrap();
        assert_eq!(verified.expanded_size_bytes, 5);
    }

    #[test]
    fn decodes_an_explicit_partition_from_a_multi_partition_image() {
        let bytes = multi_partition_image(b"first", b"second");
        let info = inspect(&mut Cursor::new(&bytes)).unwrap();
        assert_eq!(info.partition_count, 2);

        let mut out = Vec::new();
        decode_partition(&mut Cursor::new(bytes), 2, &mut out, 100).unwrap();
        assert_eq!(out, b"second");
    }

    #[test]
    fn rejects_an_out_of_range_partition_selection() {
        let bytes = multi_partition_image(b"first", b"second");
        assert!(matches!(
            decode_partition(&mut Cursor::new(bytes), 3, &mut Vec::new(), 100),
            Err(Error::PartitionCount(2))
        ));
    }

    #[test]
    fn span_reader_skips_validated_continuation_headers() {
        let primary = image(0, b"first");
        let (primary, continuation) = span(&primary, b"second", [1, 2, 3, 4]);
        let expected = [primary.as_slice(), b"second"].concat();
        let mut reader = SpanReader::new(vec![Cursor::new(primary), Cursor::new(continuation)])
            .expect("matching spans should form a virtual stream");
        let mut actual = Vec::new();
        reader.read_to_end(&mut actual).unwrap();
        assert_eq!(actual, expected);
        assert_eq!(reader.len(), expected.len() as u64);
    }

    #[test]
    fn span_reader_rejects_a_mismatched_image_id() {
        let primary = image(0, b"first");
        let (primary, mut continuation) = span(&primary, b"second", [1, 2, 3, 4]);
        continuation[4] = 9;
        assert!(matches!(
            SpanReader::new(vec![Cursor::new(primary), Cursor::new(continuation)]),
            Err(Error::SpanMismatch)
        ));
    }
    #[test]
    fn rejects_encryption_spans_and_unknown_compression() {
        for (file_type, flags, compression) in [(9, 0, 0), (1, 2, 0), (1, 0, 1)] {
            let mut bytes = image(compression, b"x");
            bytes[2] = file_type;
            bytes[12] = flags;
            assert!(inspect(&mut Cursor::new(bytes)).is_err());
        }
    }
    #[test]
    fn rejects_every_truncation_without_panicking() {
        let bytes = image(0, b"abcdef");
        for length in 0..bytes.len() {
            let _ = inspect(&mut Cursor::new(&bytes[..length]));
        }
    }
    #[test]
    fn enforces_expanded_limit() {
        let bytes = image(0, b"abcdef");
        assert!(matches!(
            verify(&mut Cursor::new(bytes), 5),
            Err(Error::ExpandedLimit)
        ));
    }
    #[test]
    fn decodes_fast_uncompressed_block() {
        let bytes = image(2, b"\x01\0\0\0hello");
        let mut out = Vec::new();
        decode_partition(&mut Cursor::new(bytes), 1, &mut out, 100).unwrap();
        assert_eq!(out, b"hello");
    }
    #[test]
    fn decodes_zlib_block() {
        use flate2::{Compression as Level, write::ZlibEncoder};
        let mut encoder = ZlibEncoder::new(Vec::new(), Level::new(6));
        encoder.write_all(b"hello zlib").unwrap();
        let mut block = vec![0, 0, 0, 0];
        block.extend(encoder.finish().unwrap());
        let mut out = Vec::new();
        decode_partition(&mut Cursor::new(image(3, &block)), 1, &mut out, 100).unwrap();
        assert_eq!(out, b"hello zlib");
    }

    #[test]
    fn maps_ghost_header_compression_tag_to_z_level() {
        let z3 = inspect(&mut Cursor::new(image(3, b"unused"))).unwrap();
        assert_eq!(z3.compression, Compression::High(3));
        let z9 = inspect(&mut Cursor::new(image(9, b"unused"))).unwrap();
        assert_eq!(z9.compression, Compression::High(9));
        let legacy_z9 = inspect(&mut Cursor::new(image(10, b"unused"))).unwrap();
        assert_eq!(legacy_z9.compression, Compression::High(9));
    }

}
