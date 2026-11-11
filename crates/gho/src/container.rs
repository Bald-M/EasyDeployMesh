//! GHO container: file header, end record, the uncompressed partition
//! descriptor segment, and the zlib packet chain that carries the rest of the
//! logical partition stream.

use crate::{Compression, Error, ImageInfo, map_eof};
use flate2::{Decompress, FlushDecompress, Status};
use std::io::{self, Read, Seek, SeekFrom};

pub(crate) const HEADER_SIZE: u64 = 512;
const FILE_MAGIC: u16 = 0xeffe;
const FILE_TYPE_PRIMARY: u8 = 1;
const FILE_TYPE_SPAN: u8 = 9;
const FLAG_ENCRYPTED: u8 = 0x02;

pub(crate) const RECORD_MAGIC: u32 = 0x012f_18d8;
const END_RECORD_KIND: u16 = 0x0023;
const END_RECORD_BODY: u16 = 24;
const END_RECORD_SIZE: u64 = 10 + END_RECORD_BODY as u64;

/// Ghost places the partition descriptor near the start of the image. The
/// search window is bounded so a hostile file cannot force a full scan.
const DESCRIPTOR_SEARCH_LIMIT: u64 = 1024 * 1024;
pub(crate) const MAX_DESCRIPTOR_LEN: u64 = 64 * 1024;
const DESCRIPTOR_MAGIC: &[u8; 4] = b"GHPR";

/// Largest expanded payload Ghost writes per packet (its transfer buffer is
/// 32 KiB); anything larger is treated as corruption.
const MAX_PACKET_EXPANDED: usize = 64 * 1024;

#[derive(Debug, Clone, Copy)]
pub(crate) struct Layout {
    pub info: ImageInfo,
    /// Offset of the uncompressed `0e01` descriptor packet.
    pub stream_start: u64,
    /// Offset of the first `[u16 length][zlib]` packet.
    pub packets_start: u64,
    /// Offset of the zero-length packet that terminates the chain.
    pub terminator: u64,
}

pub(crate) fn read_header<R: Read + Seek>(reader: &mut R) -> Result<(ImageInfo, u64), Error> {
    let file_len = reader.seek(SeekFrom::End(0))?;
    if file_len < HEADER_SIZE {
        return Err(Error::Truncated);
    }
    reader.seek(SeekFrom::Start(0))?;
    let mut header = [0_u8; HEADER_SIZE as usize];
    reader.read_exact(&mut header).map_err(map_eof)?;
    if u16::from_le_bytes([header[0], header[1]]) != FILE_MAGIC {
        return Err(Error::InvalidMagic);
    }
    match header[2] {
        FILE_TYPE_PRIMARY => {}
        FILE_TYPE_SPAN => return Err(Error::SpannedUnsupported),
        _ => return Err(Error::InvalidMagic),
    }
    if header[12] & FLAG_ENCRYPTED != 0 {
        return Err(Error::EncryptedUnsupported);
    }
    let compression = match header[3] {
        0 => Compression::None,
        2 => Compression::Fast,
        // The tag is Ghost's -z level plus one: fast (-z1) is 2 and the
        // zlib levels -z2..-z9 are 3..10.
        value @ 3..=10 => Compression::High(value - 1),
        value => return Err(Error::UnsupportedCompression(value)),
    };
    Ok((
        ImageInfo {
            compression,
            partition_count: 1,
            source_partition: 1,
            encrypted: false,
            spanned: false,
        },
        file_len,
    ))
}

pub(crate) fn locate<R: Read + Seek>(reader: &mut R) -> Result<Layout, Error> {
    let (info, file_len) = read_header(reader)?;
    // Only zlib packet chains have been verified against real Ghost images.
    // Uncompressed and "fast" images are rejected rather than guessed at.
    match info.compression {
        Compression::High(_) => {}
        Compression::None => return Err(Error::UnsupportedCompression(0)),
        Compression::Fast => return Err(Error::UnsupportedCompression(2)),
    }
    let terminator = read_end_record(reader, file_len)?;
    let (stream_start, packets_start) = find_descriptor(reader, file_len.min(terminator))?;
    if packets_start > terminator {
        return Err(Error::InvalidRecord);
    }
    Ok(Layout {
        info,
        stream_start,
        packets_start,
        terminator,
    })
}

/// Validates the trailing `0x0023` record and returns the offset of the
/// zero-length packet that terminates the compressed chain.
fn read_end_record<R: Read + Seek>(reader: &mut R, file_len: u64) -> Result<u64, Error> {
    if file_len < HEADER_SIZE + END_RECORD_SIZE + 2 {
        return Err(Error::Truncated);
    }
    let record_start = file_len - END_RECORD_SIZE;
    reader.seek(SeekFrom::Start(record_start))?;
    let mut record = [0_u8; END_RECORD_SIZE as usize];
    reader.read_exact(&mut record).map_err(map_eof)?;
    let kind = u16::from_le_bytes([record[0], record[1]]);
    let magic = u32::from_le_bytes(record[4..8].try_into().expect("fixed slice"));
    let body = u16::from_le_bytes([record[8], record[9]]);
    if kind != END_RECORD_KIND || magic != RECORD_MAGIC || body != END_RECORD_BODY {
        return Err(Error::InvalidRecord);
    }
    let distance = u64::from_le_bytes(record[18..26].try_into().expect("fixed slice"));
    let terminator = record_start
        .checked_sub(distance)
        .filter(|offset| *offset >= HEADER_SIZE)
        .ok_or(Error::InvalidRecord)?;
    reader.seek(SeekFrom::Start(terminator))?;
    let mut marker = [0_u8; 2];
    reader.read_exact(&mut marker).map_err(map_eof)?;
    if marker != [0, 0] {
        return Err(Error::InvalidRecord);
    }
    Ok(terminator)
}

/// Finds the uncompressed descriptor segment:
/// `0e 01 00.. 0f` + `0f <u64 len> 0e` + `<len bytes starting "..GHPR">` +
/// `0a <u32 crc> 00 00 00 00 0b`.
fn find_descriptor<R: Read + Seek>(reader: &mut R, limit: u64) -> Result<(u64, u64), Error> {
    let end = limit.min(HEADER_SIZE + DESCRIPTOR_SEARCH_LIMIT);
    if end <= HEADER_SIZE {
        return Err(Error::Truncated);
    }
    let window = usize::try_from(end - HEADER_SIZE).map_err(|_| Error::InvalidRecord)?;
    let mut buffer = vec![0_u8; window];
    reader.seek(SeekFrom::Start(HEADER_SIZE))?;
    reader.read_exact(&mut buffer).map_err(map_eof)?;
    let mut candidate = None;
    for at in 0..buffer.len().saturating_sub(26 + 6) {
        let packet = &buffer[at..at + 16];
        if packet[0] != 0x0e || packet[1] != 0x01 || packet[15] != 0x0f {
            continue;
        }
        if packet[2..15].iter().any(|byte| *byte != 0) {
            continue;
        }
        let data = &buffer[at + 16..at + 26];
        if data[0] != 0x0f || data[9] != 0x0e {
            continue;
        }
        let length = u64::from_le_bytes(data[1..9].try_into().expect("fixed slice"));
        if !(6..=MAX_DESCRIPTOR_LEN).contains(&length) {
            continue;
        }
        let payload = at + 26;
        if buffer.get(payload + 2..payload + 6) != Some(DESCRIPTOR_MAGIC.as_slice()) {
            continue;
        }
        if candidate.is_some() {
            // Multiple descriptors means a disk image or an unknown layout.
            return Err(Error::PartitionCount(2));
        }
        candidate = Some((at, length));
    }
    let (at, length) = candidate.ok_or(Error::InvalidRecord)?;
    let crc = at + 26 + length as usize;
    let crc_packet = buffer.get(crc..crc + 10).ok_or(Error::Truncated)?;
    if crc_packet[0] != 0x0a || crc_packet[5..9] != [0, 0, 0, 0] || crc_packet[9] != 0x0b {
        return Err(Error::InvalidRecord);
    }
    let stream_start = HEADER_SIZE + at as u64;
    Ok((stream_start, HEADER_SIZE + (crc + 10) as u64))
}

/// Presents the uncompressed descriptor segment followed by every
/// decompressed packet as one logical byte stream.
pub(crate) struct LogicalStream<'a, R> {
    reader: &'a mut R,
    raw_position: u64,
    raw_end: u64,
    packet_position: u64,
    terminator: u64,
    compressed: Vec<u8>,
    expanded: Vec<u8>,
    expanded_len: usize,
    expanded_pos: usize,
    finished: bool,
}

impl<'a, R: Read + Seek> LogicalStream<'a, R> {
    pub(crate) fn new(reader: &'a mut R, layout: &Layout) -> Self {
        Self {
            reader,
            raw_position: layout.stream_start,
            raw_end: layout.packets_start,
            packet_position: layout.packets_start,
            terminator: layout.terminator,
            compressed: vec![0_u8; usize::from(u16::MAX)],
            expanded: vec![0_u8; MAX_PACKET_EXPANDED],
            expanded_len: 0,
            expanded_pos: 0,
            finished: false,
        }
    }

    fn next_packet(&mut self) -> Result<bool, Error> {
        if self.finished {
            return Ok(false);
        }
        if self.packet_position == self.terminator {
            self.finished = true;
            return Ok(false);
        }
        let header_end = self
            .packet_position
            .checked_add(2)
            .ok_or(Error::CorruptBlock)?;
        if header_end > self.terminator {
            return Err(Error::CorruptBlock);
        }
        self.reader.seek(SeekFrom::Start(self.packet_position))?;
        let mut length = [0_u8; 2];
        self.reader.read_exact(&mut length).map_err(map_eof)?;
        let stored = usize::from(u16::from_le_bytes(length));
        if stored < 3 {
            // A zero length before the recorded terminator means the chain
            // and the end record disagree.
            return Err(Error::CorruptBlock);
        }
        let next = self.packet_position + stored as u64;
        if next > self.terminator {
            return Err(Error::CorruptBlock);
        }
        let payload = stored - 2;
        self.reader
            .read_exact(&mut self.compressed[..payload])
            .map_err(map_eof)?;
        let mut inflater = Decompress::new(true);
        let status = inflater
            .decompress(
                &self.compressed[..payload],
                &mut self.expanded,
                FlushDecompress::Finish,
            )
            .map_err(|_| Error::CorruptBlock)?;
        if status != Status::StreamEnd || inflater.total_in() != payload as u64 {
            return Err(Error::CorruptBlock);
        }
        self.expanded_len =
            usize::try_from(inflater.total_out()).map_err(|_| Error::CorruptBlock)?;
        if self.expanded_len == 0 {
            return Err(Error::CorruptBlock);
        }
        self.expanded_pos = 0;
        self.packet_position = next;
        Ok(true)
    }
}

impl<R: Read + Seek> Read for LogicalStream<'_, R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        if self.raw_position < self.raw_end {
            let wanted = usize::try_from(self.raw_end - self.raw_position)
                .unwrap_or(usize::MAX)
                .min(output.len());
            self.reader.seek(SeekFrom::Start(self.raw_position))?;
            self.reader.read_exact(&mut output[..wanted])?;
            self.raw_position += wanted as u64;
            return Ok(wanted);
        }
        if self.expanded_pos == self.expanded_len
            && !self.next_packet().map_err(io::Error::other)?
        {
            return Ok(0);
        }
        let wanted = (self.expanded_len - self.expanded_pos).min(output.len());
        output[..wanted]
            .copy_from_slice(&self.expanded[self.expanded_pos..self.expanded_pos + wanted]);
        self.expanded_pos += wanted;
        Ok(wanted)
    }
}
