//! Ghost NTFS partition stream: token grammar, bounded FILE record parsing,
//! and the restore engine that places every stored run at its original LCN.

use crate::Error;
use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom, Write};

const TAG_HEADER: u8 = 0x0e;
const TAG_HEADER_END: u8 = 0x0f;
const TAG_DATA: u8 = 0x0f;
const TAG_DATA_END: u8 = 0x0e;
const TAG_CRC: u8 = 0x0a;
const TAG_CRC_END: u8 = 0x0b;

const KIND_DESCRIPTOR: u8 = 0x01;
const KIND_RUN: u8 = 0x02;
const KIND_RECORD_IN_USE: u8 = 0x04;
const KIND_RECORD_FREE: u8 = 0x08;
const KIND_RECORD_EMPTY: u8 = 0x10;
const KIND_ATTRIBUTE_DATA: u8 = 0x20;
const KIND_ATTRIBUTE_OMITTED: u8 = 0x40;
const FLAGS_RECORD: u8 = 0x80;

const SECTOR_SIZE: u64 = 512;
const MAX_CLUSTER_SIZE: u64 = 2 * 1024 * 1024;
/// `$MFTMirr` covers at least one cluster, so capture up to the largest one.
const MIRROR_CAPTURE_LIMIT: usize = MAX_CLUSTER_SIZE as usize;
const COPY_CHUNK: usize = 1024 * 1024;

const MFT_RECORD: u64 = 0;
const MFT_MIRROR_RECORD: u64 = 1;
const BAD_CLUSTER_RECORD: u64 = 8;
const ATTRIBUTE_FILE_NAME: u32 = 0x30;
const ATTRIBUTE_DATA: u32 = 0x80;
/// BCDBoot can only configure Windows that boots through winload.exe.
const WINDOWS_BOOT_LOADER: &str = "winload.exe";
const ATTRIBUTE_END: u32 = 0xffff_ffff;

/// Geometry of the restored NTFS volume, taken from its boot sector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NtfsGeometry {
    pub bytes_per_sector: u32,
    pub cluster_size: u64,
    pub total_sectors: u64,
    pub mft_lcn: u64,
    pub mft_mirror_lcn: u64,
}

impl NtfsGeometry {
    /// Bytes the target partition needs: every volume sector plus the
    /// trailing backup boot sector NTFS keeps after `total_sectors`.
    pub fn volume_size_bytes(&self) -> u64 {
        self.total_sectors
            .saturating_add(1)
            .saturating_mul(u64::from(self.bytes_per_sector))
    }

    fn total_clusters(&self) -> u64 {
        self.total_sectors
            .saturating_mul(u64::from(self.bytes_per_sector))
            / self.cluster_size
    }
}

/// Random-access sink for restored volume bytes.
pub(crate) trait Target {
    fn write_at(&mut self, offset: u64, bytes: &[u8]) -> Result<(), Error>;
}

/// Performs every validation without writing anything.
pub(crate) struct DryRun;

impl Target for DryRun {
    fn write_at(&mut self, _offset: u64, _bytes: &[u8]) -> Result<(), Error> {
        Ok(())
    }
}

pub(crate) struct SeekTarget<'a, W> {
    pub inner: &'a mut W,
}

impl<W: Write + Seek> Target for SeekTarget<'_, W> {
    fn write_at(&mut self, offset: u64, bytes: &[u8]) -> Result<(), Error> {
        self.inner.seek(SeekFrom::Start(offset))?;
        self.inner.write_all(bytes)?;
        Ok(())
    }
}

/// Counts, limits and mirrors every logical stream byte into `sink`.
pub(crate) struct Tee<'a, R, S> {
    pub inner: R,
    pub sink: &'a mut S,
    pub consumed: u64,
    pub limit: u64,
}

impl<R: Read, S: Write> Tee<'_, R, S> {
    fn read_exact_counted(&mut self, buffer: &mut [u8]) -> Result<(), Error> {
        let next = self
            .consumed
            .checked_add(buffer.len() as u64)
            .ok_or(Error::ExpandedLimit)?;
        if next > self.limit {
            return Err(Error::ExpandedLimit);
        }
        self.inner.read_exact(buffer).map_err(from_io)?;
        self.sink.write_all(buffer)?;
        self.consumed = next;
        Ok(())
    }

    /// Reads one byte, or `None` at a clean end of stream.
    fn read_tag(&mut self) -> Result<Option<u8>, Error> {
        let mut tag = [0_u8; 1];
        loop {
            match self.inner.read(&mut tag) {
                Ok(0) => return Ok(None),
                Ok(_) => break,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(from_io(error)),
            }
        }
        if self.consumed >= self.limit {
            return Err(Error::ExpandedLimit);
        }
        self.sink.write_all(&tag)?;
        self.consumed += 1;
        Ok(Some(tag[0]))
    }
}

fn from_io(error: std::io::Error) -> Error {
    if error.kind() == std::io::ErrorKind::UnexpectedEof {
        return Error::Truncated;
    }
    match error.into_inner() {
        Some(inner) => match inner.downcast::<Error>() {
            Ok(error) => *error,
            Err(other) => Error::Io(std::io::Error::other(other)),
        },
        None => Error::CorruptStream("logical stream read failed"),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Token {
    Header { kind: u8, flags: u8, index: u64 },
    Data { length: u64 },
    Crc,
}

fn read_token<R: Read, S: Write>(stream: &mut Tee<'_, R, S>) -> Result<Option<Token>, Error> {
    let Some(tag) = stream.read_tag()? else {
        return Ok(None);
    };
    match tag {
        TAG_HEADER => {
            let mut body = [0_u8; 15];
            stream.read_exact_counted(&mut body)?;
            if body[14] != TAG_HEADER_END || body[2..6] != [0, 0, 0, 0] {
                return Err(Error::CorruptStream("malformed header packet"));
            }
            let index = u64::from_le_bytes(body[6..14].try_into().expect("fixed slice"));
            Ok(Some(Token::Header {
                kind: body[0],
                flags: body[1],
                index,
            }))
        }
        TAG_DATA => {
            let mut body = [0_u8; 9];
            stream.read_exact_counted(&mut body)?;
            if body[8] != TAG_DATA_END {
                return Err(Error::CorruptStream("malformed data packet"));
            }
            let length = u64::from_le_bytes(body[0..8].try_into().expect("fixed slice"));
            Ok(Some(Token::Data { length }))
        }
        TAG_CRC => {
            let mut body = [0_u8; 9];
            stream.read_exact_counted(&mut body)?;
            if body[4..8] != [0, 0, 0, 0] || body[8] != TAG_CRC_END {
                return Err(Error::CorruptStream("malformed checksum packet"));
            }
            Ok(Some(Token::Crc))
        }
        _ => Err(Error::CorruptStream("unknown packet tag")),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Run {
    /// `None` for a sparse run.
    lcn: Option<u64>,
    clusters: u64,
}

#[derive(Debug, Clone)]
struct NonResident {
    attribute_type: u32,
    name: Vec<u16>,
    start_vcn: u64,
    runs: Vec<Run>,
}

impl NonResident {
    fn is_unnamed_data(&self) -> bool {
        self.attribute_type == ATTRIBUTE_DATA && self.name.is_empty()
    }

    fn is_named(&self, name: &str) -> bool {
        self.attribute_type == ATTRIBUTE_DATA && self.name.iter().copied().eq(name.encode_utf16())
    }
}

fn u16_at(bytes: &[u8], at: usize) -> Result<u16, Error> {
    bytes
        .get(at..at + 2)
        .map(|value| u16::from_le_bytes([value[0], value[1]]))
        .ok_or(Error::CorruptStream("record field out of bounds"))
}

fn u32_at(bytes: &[u8], at: usize) -> Result<u32, Error> {
    bytes
        .get(at..at + 4)
        .map(|value| u32::from_le_bytes(value.try_into().expect("fixed slice")))
        .ok_or(Error::CorruptStream("record field out of bounds"))
}

fn u64_at(bytes: &[u8], at: usize) -> Result<u64, Error> {
    bytes
        .get(at..at + 8)
        .map(|value| u64::from_le_bytes(value.try_into().expect("fixed slice")))
        .ok_or(Error::CorruptStream("record field out of bounds"))
}

struct ParsedRecord {
    non_resident: Vec<NonResident>,
    names_boot_loader: bool,
}

/// Parses an in-use FILE record as Ghost stores it (update sequence already
/// removed) and returns its non-resident attributes in record order.
fn parse_record(record: &[u8], index: u64) -> Result<ParsedRecord, Error> {
    if record.get(0..4) != Some(b"FILE") {
        return Err(Error::CorruptStream("MFT record signature is invalid"));
    }
    let usa_offset = usize::from(u16_at(record, 4)?);
    let usa_count = usize::from(u16_at(record, 6)?);
    let sectors = record.len() / SECTOR_SIZE as usize;
    if usa_count != sectors + 1 || usa_offset < 0x28 || usa_offset + usa_count * 2 > record.len() {
        return Err(Error::CorruptStream(
            "MFT record update sequence is invalid",
        ));
    }
    if u16_at(record, 22)? & 0x0001 == 0 {
        return Err(Error::CorruptStream(
            "in-use MFT record is not marked in use",
        ));
    }
    if usa_offset >= 0x30 && u64::from(u32_at(record, 0x2c)?) != index {
        return Err(Error::CorruptStream(
            "MFT record number does not match its slot",
        ));
    }
    let used = usize::try_from(u32_at(record, 24)?).map_err(|_| Error::InvalidRecord)?;
    if used > record.len() {
        return Err(Error::CorruptStream("MFT record used size is invalid"));
    }
    let mut offset = usize::from(u16_at(record, 20)?);
    let mut attributes = Vec::new();
    let mut names_boot_loader = false;
    loop {
        if offset + 4 > used {
            return Err(Error::CorruptStream(
                "MFT record has no attribute terminator",
            ));
        }
        let attribute_type = u32_at(record, offset)?;
        if attribute_type == ATTRIBUTE_END {
            break;
        }
        let length =
            usize::try_from(u32_at(record, offset + 4)?).map_err(|_| Error::InvalidRecord)?;
        if length < 24 || length % 8 != 0 || offset + length > used {
            return Err(Error::CorruptStream("attribute length is invalid"));
        }
        let attribute = &record[offset..offset + length];
        if attribute[8] != 0 {
            attributes.push(parse_non_resident(attribute_type, attribute)?);
        } else if attribute_type == ATTRIBUTE_FILE_NAME {
            names_boot_loader |= file_name_is(attribute, WINDOWS_BOOT_LOADER)?;
        }
        offset += length;
    }
    Ok(ParsedRecord {
        non_resident: attributes,
        names_boot_loader,
    })
}

/// Compares a resident `$FILE_NAME` value with `expected`, ignoring ASCII case.
fn file_name_is(attribute: &[u8], expected: &str) -> Result<bool, Error> {
    let value_length = usize::try_from(u32_at(attribute, 16)?).map_err(|_| Error::InvalidRecord)?;
    let value_offset = usize::from(u16_at(attribute, 20)?);
    let value = attribute
        .get(value_offset..value_offset.saturating_add(value_length))
        .ok_or(Error::CorruptStream("file name attribute is out of bounds"))?;
    let Some(&name_length) = value.get(64) else {
        return Err(Error::CorruptStream("file name attribute is too short"));
    };
    let name = value
        .get(66..66 + usize::from(name_length) * 2)
        .ok_or(Error::CorruptStream("file name is out of bounds"))?;
    Ok(name.len() == expected.len() * 2
        && name
            .chunks_exact(2)
            .zip(expected.bytes())
            .all(|(pair, byte)| pair[1] == 0 && pair[0].eq_ignore_ascii_case(&byte)))
}

fn parse_non_resident(attribute_type: u32, attribute: &[u8]) -> Result<NonResident, Error> {
    if attribute.len() < 64 {
        return Err(Error::CorruptStream("non-resident attribute is too short"));
    }
    let name_length = usize::from(attribute[9]);
    let name_offset = usize::from(u16_at(attribute, 10)?);
    let name_bytes = attribute
        .get(name_offset..name_offset + name_length * 2)
        .ok_or(Error::CorruptStream("attribute name is out of bounds"))?;
    let name = name_bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect::<Vec<_>>();
    let start_vcn = u64_at(attribute, 16)?;
    let last_vcn = u64_at(attribute, 24)?;
    let runs_offset = usize::from(u16_at(attribute, 32)?);
    if runs_offset < 64 || runs_offset >= attribute.len() {
        return Err(Error::CorruptStream("mapping pairs offset is invalid"));
    }
    let runs = parse_runs(&attribute[runs_offset..])?;
    let clusters = runs
        .iter()
        .try_fold(0_u64, |total, run| total.checked_add(run.clusters))
        .ok_or(Error::CorruptStream("run lengths overflow"))?;
    // An empty attribute stores last VCN = -1 and has no runs.
    let expected = last_vcn.wrapping_sub(start_vcn).wrapping_add(1);
    if clusters != expected {
        return Err(Error::CorruptStream(
            "mapping pairs do not cover the VCN range",
        ));
    }
    Ok(NonResident {
        attribute_type,
        name,
        start_vcn,
        runs,
    })
}

fn parse_runs(bytes: &[u8]) -> Result<Vec<Run>, Error> {
    let mut runs = Vec::new();
    let mut offset = 0;
    let mut lcn = 0_i64;
    loop {
        let header = *bytes
            .get(offset)
            .ok_or(Error::CorruptStream("mapping pairs are unterminated"))?;
        if header == 0 {
            return Ok(runs);
        }
        let length_size = usize::from(header & 0x0f);
        let offset_size = usize::from(header >> 4);
        if length_size == 0 || length_size > 8 || offset_size > 8 {
            return Err(Error::CorruptStream("mapping pair header is invalid"));
        }
        offset += 1;
        let length_bytes = bytes
            .get(offset..offset + length_size)
            .ok_or(Error::CorruptStream("mapping pair is truncated"))?;
        let clusters = little_endian_unsigned(length_bytes);
        if clusters == 0 || clusters > i64::MAX as u64 {
            return Err(Error::CorruptStream("run length is invalid"));
        }
        offset += length_size;
        let run_lcn = if offset_size == 0 {
            None
        } else {
            let delta_bytes = bytes
                .get(offset..offset + offset_size)
                .ok_or(Error::CorruptStream("mapping pair is truncated"))?;
            offset += offset_size;
            lcn = lcn
                .checked_add(little_endian_signed(delta_bytes))
                .filter(|value| *value >= 0)
                .ok_or(Error::CorruptStream("run LCN is invalid"))?;
            Some(lcn as u64)
        };
        runs.push(Run {
            lcn: run_lcn,
            clusters,
        });
        if runs.len() > 1_000_000 {
            return Err(Error::CorruptStream("too many runs in one attribute"));
        }
    }
}

fn little_endian_unsigned(bytes: &[u8]) -> u64 {
    bytes
        .iter()
        .rev()
        .fold(0_u64, |value, byte| (value << 8) | u64::from(*byte))
}

fn little_endian_signed(bytes: &[u8]) -> i64 {
    let unsigned = little_endian_unsigned(bytes);
    let bits = bytes.len() * 8;
    if bits == 64 {
        unsigned as i64
    } else if unsigned & (1 << (bits - 1)) != 0 {
        (unsigned | (!0_u64 << bits)) as i64
    } else {
        unsigned as i64
    }
}

fn parse_boot_sector(sector: &[u8]) -> Result<NtfsGeometry, Error> {
    if sector.len() < SECTOR_SIZE as usize
        || sector[510..512] != [0x55, 0xaa]
        || &sector[3..11] != b"NTFS    "
    {
        return Err(Error::CorruptStream("NTFS boot sector is invalid"));
    }
    let bytes_per_sector = u32::from(u16_at(sector, 0x0b)?);
    if u64::from(bytes_per_sector) != SECTOR_SIZE {
        return Err(Error::UnsupportedVolume(
            "only 512-byte NTFS sectors are supported",
        ));
    }
    let raw = sector[0x0d];
    // 1..=0x80 is a sector count; 0xF4..=0xFF encodes 2^(256 - raw) sectors
    // (up to 4096 sectors = 2 MiB clusters). Everything else is invalid.
    let sectors_per_cluster = match raw {
        1..=0x80 => u64::from(raw),
        0xf4..=0xff => 1_u64 << (256 - u32::from(raw)),
        _ => return Err(Error::CorruptStream("NTFS cluster size is invalid")),
    };
    if !sectors_per_cluster.is_power_of_two() {
        return Err(Error::CorruptStream("NTFS cluster size is invalid"));
    }
    let cluster_size = sectors_per_cluster
        .checked_mul(u64::from(bytes_per_sector))
        .ok_or(Error::CorruptStream("NTFS cluster size is invalid"))?;
    if cluster_size > MAX_CLUSTER_SIZE {
        return Err(Error::UnsupportedVolume("NTFS cluster size is too large"));
    }
    let total_sectors = u64_at(sector, 0x28)?;
    if total_sectors == 0 || total_sectors > u64::MAX / SECTOR_SIZE - 1 {
        return Err(Error::CorruptStream("NTFS volume size is invalid"));
    }
    Ok(NtfsGeometry {
        bytes_per_sector,
        cluster_size,
        total_sectors,
        mft_lcn: u64_at(sector, 0x30)?,
        mft_mirror_lcn: u64_at(sector, 0x38)?,
    })
}

#[derive(Debug, Clone, Copy)]
pub struct RestoreOptions {
    /// Usable bytes on the target partition.
    pub target_size_bytes: u64,
    /// Partition start in sectors; written to the boot sector so legacy BIOS
    /// boot code finds the volume.
    pub hidden_sectors: Option<u32>,
    /// Upper bound on logical (expanded) stream bytes.
    pub expanded_limit: u64,
}

pub(crate) struct Outcome {
    pub geometry: NtfsGeometry,
    pub windows_boot_loader: bool,
}

struct Engine<'t, T> {
    target: &'t mut T,
    options: RestoreOptions,
    geometry: NtfsGeometry,
    descriptor_boot: Vec<u8>,
    record_size: Option<u64>,
    boot_written: bool,
    /// Written cluster ranges, keyed by first cluster.
    written: BTreeMap<u64, u64>,
    mft_prefix: Vec<u8>,
    mft_runs: Option<Vec<Run>>,
    mirror_runs: Option<Vec<Run>>,
    seen_bad_clusters: bool,
    windows_boot_loader: bool,
}

pub(crate) fn run<R: Read, S: Write, T: Target>(
    stream: &mut Tee<'_, R, S>,
    target: &mut T,
    options: RestoreOptions,
) -> Result<Outcome, Error> {
    let descriptor_boot = read_descriptor(stream)?;
    let geometry = parse_boot_sector(&descriptor_boot)?;
    if geometry.volume_size_bytes() > options.target_size_bytes {
        return Err(Error::TargetTooSmall {
            required: geometry.volume_size_bytes(),
            available: options.target_size_bytes,
        });
    }
    let mut engine = Engine {
        target,
        options,
        geometry,
        descriptor_boot,
        record_size: None,
        boot_written: false,
        written: BTreeMap::new(),
        mft_prefix: Vec::new(),
        mft_runs: None,
        mirror_runs: None,
        seen_bad_clusters: false,
        windows_boot_loader: false,
    };
    let mut pending = read_token(stream)?;
    if pending.is_none() {
        return Err(Error::Truncated);
    }
    while let Some(token) = pending {
        pending = engine.read_record(stream, token)?;
    }
    engine.finish()
}

/// Reads the `0e01` partition descriptor and returns the NTFS boot sector
/// Ghost embeds in it.
fn read_descriptor<R: Read, S: Write>(stream: &mut Tee<'_, R, S>) -> Result<Vec<u8>, Error> {
    match read_token(stream)? {
        Some(Token::Header {
            kind: KIND_DESCRIPTOR,
            flags: 0,
            index: 0,
        }) => {}
        _ => return Err(Error::CorruptStream("partition descriptor is missing")),
    }
    let Some(Token::Data { length }) = read_token(stream)? else {
        return Err(Error::CorruptStream("partition descriptor has no payload"));
    };
    if length > crate::container::MAX_DESCRIPTOR_LEN {
        return Err(Error::CorruptStream("partition descriptor is too large"));
    }
    let mut descriptor = vec![0_u8; length as usize];
    stream.read_exact_counted(&mut descriptor)?;
    if descriptor.get(2..6) != Some(b"GHPR".as_slice()) {
        return Err(Error::CorruptStream(
            "partition descriptor signature is invalid",
        ));
    }
    match read_token(stream)? {
        Some(Token::Crc) => {}
        _ => {
            return Err(Error::CorruptStream(
                "partition descriptor checksum is missing",
            ));
        }
    }
    let sector = SECTOR_SIZE as usize;
    let mut found = None;
    for at in 0..descriptor.len().saturating_sub(sector - 1) {
        let window = &descriptor[at..at + sector];
        if &window[3..11] == b"NTFS    " && window[510..512] == [0x55, 0xaa] {
            if found.is_some() {
                return Err(Error::CorruptStream("partition descriptor is ambiguous"));
            }
            found = Some(window.to_vec());
        }
    }
    found.ok_or(Error::UnsupportedFileSystem)
}

impl<T: Target> Engine<'_, T> {
    /// Consumes one record and its attribute sections and returns the token
    /// that follows them.
    fn read_record<R: Read, S: Write>(
        &mut self,
        stream: &mut Tee<'_, R, S>,
        token: Token,
    ) -> Result<Option<Token>, Error> {
        let Token::Header {
            kind,
            flags: FLAGS_RECORD,
            index,
        } = token
        else {
            return Err(Error::CorruptStream("expected an MFT record header"));
        };
        if !matches!(
            kind,
            KIND_RECORD_IN_USE | KIND_RECORD_FREE | KIND_RECORD_EMPTY
        ) {
            return Err(Error::CorruptStream(
                "unexpected packet where an MFT record belongs",
            ));
        }
        if self.mft_runs.is_none() && (kind != KIND_RECORD_IN_USE || index != MFT_RECORD) {
            return Err(Error::CorruptStream(
                "the stream must begin with the $MFT record",
            ));
        }
        let Some(Token::Data { length }) = read_token(stream)? else {
            return Err(Error::CorruptStream("MFT record has no payload"));
        };
        if !matches!(length, 1024 | 2048 | 4096) {
            return Err(Error::CorruptStream("MFT record size is unsupported"));
        }
        if *self.record_size.get_or_insert(length) != length {
            return Err(Error::CorruptStream(
                "MFT record size changed within the stream",
            ));
        }
        let mut record = vec![0_u8; length as usize];
        stream.read_exact_counted(&mut record)?;
        if kind != KIND_RECORD_IN_USE {
            // Free and empty records are already part of the $MFT data.
            return read_token(stream);
        }
        let parsed = parse_record(&record, index)?;
        self.windows_boot_loader |= parsed.names_boot_loader;
        let attributes = parsed.non_resident;
        if index == MFT_RECORD {
            if self.mft_runs.is_some() {
                return Err(Error::CorruptStream("$MFT appears twice"));
            }
            let data = attributes
                .iter()
                .find(|attribute| attribute.is_unnamed_data() && attribute.start_vcn == 0)
                .ok_or(Error::CorruptStream("$MFT has no non-resident $DATA"))?;
            if data.runs.first().and_then(|run| run.lcn) != Some(self.geometry.mft_lcn) {
                return Err(Error::CorruptStream(
                    "boot sector $MFT location differs from $MFT",
                ));
            }
            self.mft_runs = Some(data.runs.clone());
        }
        let mut sections = 0;
        let mut token = read_token(stream)?;
        loop {
            match token {
                Some(Token::Header {
                    kind: KIND_ATTRIBUTE_DATA,
                    flags: FLAGS_RECORD,
                    index: 0,
                }) => {
                    let attribute = attributes.get(sections).ok_or(Error::CorruptStream(
                        "more attribute sections than attributes",
                    ))?;
                    sections += 1;
                    token = self.copy_attribute(stream, index, attribute)?;
                }
                Some(Token::Header {
                    kind: KIND_ATTRIBUTE_OMITTED,
                    flags: FLAGS_RECORD,
                    index: 0,
                }) => {
                    let attribute = attributes.get(sections).ok_or(Error::CorruptStream(
                        "more attribute sections than attributes",
                    ))?;
                    sections += 1;
                    self.omit_attribute(index, attribute)?;
                    token = read_token(stream)?;
                }
                Some(Token::Crc) => {
                    if sections != attributes.len() {
                        return Err(Error::CorruptStream(
                            "attribute sections do not match the MFT record",
                        ));
                    }
                    return read_token(stream);
                }
                Some(_) => {
                    return Err(Error::CorruptStream(
                        "unexpected packet inside an MFT record",
                    ));
                }
                None => return Err(Error::Truncated),
            }
        }
    }

    fn omit_attribute(&mut self, index: u64, attribute: &NonResident) -> Result<(), Error> {
        match index {
            MFT_MIRROR_RECORD if attribute.is_unnamed_data() && attribute.start_vcn == 0 => {
                if self.mirror_runs.replace(attribute.runs.clone()).is_some() {
                    return Err(Error::CorruptStream("$MFTMirr appears twice"));
                }
                Ok(())
            }
            BAD_CLUSTER_RECORD if attribute.is_named("$Bad") => {
                self.seen_bad_clusters = true;
                Ok(())
            }
            _ => Err(Error::UnsupportedVolume(
                "the image omits attribute data that cannot be regenerated",
            )),
        }
    }

    /// Copies an attribute's runs to their LCNs and returns the token after
    /// the last run.
    fn copy_attribute<R: Read, S: Write>(
        &mut self,
        stream: &mut Tee<'_, R, S>,
        index: u64,
        attribute: &NonResident,
    ) -> Result<Option<Token>, Error> {
        let cluster_size = self.geometry.cluster_size;
        let capture_mft =
            index == MFT_RECORD && attribute.is_unnamed_data() && attribute.start_vcn == 0;
        let mut expected_run = 0_u64;
        let mut token = read_token(stream)?;
        while let Some(Token::Header {
            kind: KIND_RUN,
            flags: 0,
            index: run_index,
        }) = token
        {
            if run_index != expected_run {
                return Err(Error::CorruptStream("run sections are out of order"));
            }
            let run = *attribute
                .runs
                .get(usize::try_from(run_index).map_err(|_| Error::InvalidRecord)?)
                .ok_or(Error::CorruptStream("run index is out of range"))?;
            expected_run += 1;
            token = read_token(stream)?;
            let Some(lcn) = run.lcn else {
                if matches!(token, Some(Token::Data { .. })) {
                    return Err(Error::CorruptStream("a sparse run carries data"));
                }
                continue;
            };
            let Some(Token::Data { length }) = token else {
                return Err(Error::CorruptStream("an allocated run has no data"));
            };
            if length != run.clusters {
                return Err(Error::CorruptStream(
                    "run data length differs from the mapping pairs",
                ));
            }
            self.reserve(lcn, run.clusters)?;
            let mut remaining = run.clusters * cluster_size;
            let mut offset = lcn * cluster_size;
            let mut buffer =
                vec![0_u8; COPY_CHUNK.min(usize::try_from(remaining).unwrap_or(COPY_CHUNK))];
            while remaining > 0 {
                let chunk =
                    usize::try_from(remaining.min(buffer.len() as u64)).expect("bounded by buffer");
                stream.read_exact_counted(&mut buffer[..chunk])?;
                if capture_mft && self.mft_prefix.len() < MIRROR_CAPTURE_LIMIT {
                    let wanted = (MIRROR_CAPTURE_LIMIT - self.mft_prefix.len()).min(chunk);
                    self.mft_prefix.extend_from_slice(&buffer[..wanted]);
                }
                if offset == 0 {
                    self.patch_boot_sector(&mut buffer[..chunk])?;
                }
                self.target.write_at(offset, &buffer[..chunk])?;
                offset += chunk as u64;
                remaining -= chunk as u64;
            }
            token = read_token(stream)?;
        }
        if expected_run != attribute.runs.len() as u64 {
            return Err(Error::CorruptStream("attribute runs are incomplete"));
        }
        Ok(token)
    }

    /// Checks the stored boot sector against the descriptor and applies the
    /// target's hidden-sector count.
    fn patch_boot_sector(&mut self, first: &mut [u8]) -> Result<(), Error> {
        let sector = SECTOR_SIZE as usize;
        if first.len() < sector || first[..sector] != self.descriptor_boot[..] {
            return Err(Error::CorruptStream(
                "stored boot sector differs from the partition descriptor",
            ));
        }
        if let Some(hidden) = self.options.hidden_sectors {
            first[0x1c..0x20].copy_from_slice(&hidden.to_le_bytes());
        }
        self.boot_written = true;
        Ok(())
    }

    fn reserve(&mut self, lcn: u64, clusters: u64) -> Result<(), Error> {
        let end = lcn
            .checked_add(clusters)
            .filter(|end| *end <= self.geometry.total_clusters())
            .ok_or(Error::CorruptStream(
                "data lies beyond the end of the volume",
            ))?;
        if let Some((_, previous_end)) = self.written.range(..end).next_back()
            && *previous_end > lcn
        {
            return Err(Error::CorruptStream("two runs claim the same clusters"));
        }
        self.written.insert(lcn, end);
        Ok(())
    }

    fn finish(mut self) -> Result<Outcome, Error> {
        let geometry = self.geometry;
        if !self.boot_written {
            return Err(Error::CorruptStream(
                "the image has no NTFS boot sector data",
            ));
        }
        let mirror_runs = self
            .mirror_runs
            .take()
            .ok_or(Error::CorruptStream("$MFTMirr is missing"))?;
        if mirror_runs.first().and_then(|run| run.lcn) != Some(geometry.mft_mirror_lcn) {
            return Err(Error::CorruptStream(
                "boot sector $MFTMirr location differs",
            ));
        }
        if !self.seen_bad_clusters {
            return Err(Error::CorruptStream("$BadClus is missing"));
        }
        let mut mirror_offset = 0_usize;
        for run in &mirror_runs {
            let lcn = run
                .lcn
                .ok_or(Error::CorruptStream("$MFTMirr has a sparse run"))?;
            self.reserve(lcn, run.clusters)?;
            let bytes = usize::try_from(run.clusters * geometry.cluster_size)
                .map_err(|_| Error::CorruptStream("$MFTMirr is too large"))?;
            let source = self
                .mft_prefix
                .get(mirror_offset..mirror_offset + bytes)
                .ok_or(Error::CorruptStream(
                    "$MFTMirr is larger than the captured $MFT",
                ))?
                .to_vec();
            self.target.write_at(lcn * geometry.cluster_size, &source)?;
            mirror_offset += bytes;
        }
        let mut backup = self.descriptor_boot.clone();
        if let Some(hidden) = self.options.hidden_sectors {
            backup[0x1c..0x20].copy_from_slice(&hidden.to_le_bytes());
        }
        let backup_offset = geometry.total_sectors * u64::from(geometry.bytes_per_sector);
        self.target.write_at(backup_offset, &backup)?;
        Ok(Outcome {
            geometry,
            windows_boot_loader: self.windows_boot_loader,
        })
    }
}

#[cfg(any(test, feature = "fixtures"))]
pub(crate) mod test_support {
    //! Builders for synthetic Ghost NTFS streams used by unit tests.

    pub fn header(kind: u8, flags: u8, index: u64) -> Vec<u8> {
        let mut packet = vec![0x0e, kind, flags, 0, 0, 0, 0];
        packet.extend_from_slice(&index.to_le_bytes());
        packet.push(0x0f);
        packet
    }

    pub fn data(length: u64) -> Vec<u8> {
        let mut packet = vec![0x0f];
        packet.extend_from_slice(&length.to_le_bytes());
        packet.push(0x0e);
        packet
    }

    pub fn crc() -> Vec<u8> {
        vec![0x0a, 1, 2, 3, 4, 0, 0, 0, 0, 0x0b]
    }

    pub fn encode_runs(runs: &[(Option<u64>, u64)]) -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut previous = 0_i64;
        for &(lcn, clusters) in runs {
            let length = clusters.to_le_bytes();
            let length_size = 8 - clusters.leading_zeros() as usize / 8;
            match lcn {
                None => {
                    bytes.push(length_size as u8);
                    bytes.extend_from_slice(&length[..length_size]);
                }
                Some(lcn) => {
                    let delta = lcn as i64 - previous;
                    previous = lcn as i64;
                    let delta_bytes = delta.to_le_bytes();
                    let mut size = 8;
                    while size > 1 {
                        let sign = (delta_bytes[size - 2] & 0x80) != 0;
                        let top = delta_bytes[size - 1];
                        if (top == 0 && !sign) || (top == 0xff && sign) {
                            size -= 1;
                        } else {
                            break;
                        }
                    }
                    bytes.push(((size as u8) << 4) | length_size as u8);
                    bytes.extend_from_slice(&length[..length_size]);
                    bytes.extend_from_slice(&delta_bytes[..size]);
                }
            }
        }
        bytes.push(0);
        bytes
    }

    /// A non-resident attribute with the given runs.
    pub fn attribute(
        kind: u32,
        name: &str,
        runs: &[(Option<u64>, u64)],
        cluster_size: u64,
    ) -> Vec<u8> {
        let name_units = name.encode_utf16().collect::<Vec<_>>();
        let name_offset = 64_usize;
        let runs_offset = (name_offset + name_units.len() * 2).next_multiple_of(8);
        let encoded = encode_runs(runs);
        let length = (runs_offset + encoded.len()).next_multiple_of(8);
        let clusters = runs.iter().map(|run| run.1).sum::<u64>();
        let mut attribute = vec![0_u8; length];
        attribute[0..4].copy_from_slice(&kind.to_le_bytes());
        attribute[4..8].copy_from_slice(&(length as u32).to_le_bytes());
        attribute[8] = 1;
        attribute[9] = name_units.len() as u8;
        attribute[10..12].copy_from_slice(&(name_offset as u16).to_le_bytes());
        attribute[24..32].copy_from_slice(&clusters.wrapping_sub(1).to_le_bytes());
        attribute[32..34].copy_from_slice(&(runs_offset as u16).to_le_bytes());
        let size = clusters * cluster_size;
        attribute[40..48].copy_from_slice(&size.to_le_bytes());
        attribute[48..56].copy_from_slice(&size.to_le_bytes());
        attribute[56..64].copy_from_slice(&size.to_le_bytes());
        for (at, unit) in name_units.iter().enumerate() {
            attribute[name_offset + at * 2..name_offset + at * 2 + 2]
                .copy_from_slice(&unit.to_le_bytes());
        }
        attribute[runs_offset..runs_offset + encoded.len()].copy_from_slice(&encoded);
        attribute
    }

    /// A resident `$FILE_NAME` attribute.
    pub fn file_name(name: &str) -> Vec<u8> {
        let units = name.encode_utf16().collect::<Vec<_>>();
        let value_length = 66 + units.len() * 2;
        let length = (24 + value_length).next_multiple_of(8);
        let mut attribute = vec![0_u8; length];
        attribute[0..4].copy_from_slice(&0x30_u32.to_le_bytes());
        attribute[4..8].copy_from_slice(&(length as u32).to_le_bytes());
        attribute[16..20].copy_from_slice(&(value_length as u32).to_le_bytes());
        attribute[20..22].copy_from_slice(&24_u16.to_le_bytes());
        attribute[24 + 64] = units.len() as u8;
        for (at, unit) in units.iter().enumerate() {
            attribute[24 + 66 + at * 2..24 + 68 + at * 2].copy_from_slice(&unit.to_le_bytes());
        }
        attribute
    }

    /// A 1024-byte in-use FILE record in Ghost's stored form (fixups removed).
    pub fn record(index: u64, attributes: &[Vec<u8>]) -> Vec<u8> {
        let mut record = vec![0_u8; 1024];
        record[0..4].copy_from_slice(b"FILE");
        record[4..6].copy_from_slice(&0x30_u16.to_le_bytes());
        record[6..8].copy_from_slice(&3_u16.to_le_bytes());
        record[20..22].copy_from_slice(&0x38_u16.to_le_bytes());
        record[22..24].copy_from_slice(&1_u16.to_le_bytes());
        record[0x2c..0x30].copy_from_slice(&(index as u32).to_le_bytes());
        let mut offset = 0x38;
        for attribute in attributes {
            record[offset..offset + attribute.len()].copy_from_slice(attribute);
            offset += attribute.len();
        }
        record[offset..offset + 4].copy_from_slice(&0xffff_ffff_u32.to_le_bytes());
        record[24..28].copy_from_slice(&((offset + 8) as u32).to_le_bytes());
        record[28..32].copy_from_slice(&1024_u32.to_le_bytes());
        record
    }

    pub fn boot_sector(
        cluster_size: u64,
        total_sectors: u64,
        mft_lcn: u64,
        mirror_lcn: u64,
    ) -> Vec<u8> {
        let mut sector = vec![0_u8; 512];
        sector[0..3].copy_from_slice(&[0xeb, 0x52, 0x90]);
        sector[3..11].copy_from_slice(b"NTFS    ");
        sector[0x0b..0x0d].copy_from_slice(&512_u16.to_le_bytes());
        sector[0x0d] = (cluster_size / 512) as u8;
        sector[0x1c..0x20].copy_from_slice(&63_u32.to_le_bytes());
        sector[0x28..0x30].copy_from_slice(&total_sectors.to_le_bytes());
        sector[0x30..0x38].copy_from_slice(&mft_lcn.to_le_bytes());
        sector[0x38..0x40].copy_from_slice(&mirror_lcn.to_le_bytes());
        sector[510] = 0x55;
        sector[511] = 0xaa;
        sector
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_mapping_pairs_with_negative_deltas_and_sparse_runs() {
        let encoded = test_support::encode_runs(&[
            (Some(0x1_0000), 4),
            (None, 2),
            (Some(0x20), 300),
            (Some(0x1_0000_0000), 1),
        ]);
        let runs = parse_runs(&encoded).unwrap();
        assert_eq!(
            runs,
            vec![
                Run {
                    lcn: Some(0x1_0000),
                    clusters: 4
                },
                Run {
                    lcn: None,
                    clusters: 2
                },
                Run {
                    lcn: Some(0x20),
                    clusters: 300
                },
                Run {
                    lcn: Some(0x1_0000_0000),
                    clusters: 1
                },
            ]
        );
    }

    #[test]
    fn rejects_every_invalid_sectors_per_cluster_byte_without_panicking() {
        for raw in 0..=u8::MAX {
            let mut sector = test_support::boot_sector(4096, 1024, 4, 2);
            sector[0x0d] = raw;
            let result = parse_boot_sector(&sector);
            let valid = matches!(raw, 1 | 2 | 4 | 8 | 16 | 32 | 64 | 128 | 0xf4..=0xff);
            assert_eq!(result.is_ok(), valid, "raw {raw:#04x}: {result:?}");
            if let Ok(geometry) = result {
                assert!((512..=MAX_CLUSTER_SIZE).contains(&geometry.cluster_size));
            }
        }
    }

    #[test]
    fn rejects_malformed_mapping_pairs() {
        for bytes in [
            vec![0x11, 0x01],             // truncated offset
            vec![0x11, 0x01, 0xff, 0x00], // LCN becomes negative
            vec![0x01, 0x00, 0x00],       // zero-length run
            vec![0x91, 0x01],             // offset size > 8
            vec![0x11, 0x01, 0x05],       // unterminated
        ] {
            assert!(parse_runs(&bytes).is_err(), "{bytes:02x?}");
        }
    }

    #[test]
    fn rejects_records_with_inconsistent_headers() {
        let attribute = test_support::attribute(0x80, "", &[(Some(5), 2)], 4096);
        let good = test_support::record(9, &[attribute]);
        assert_eq!(parse_record(&good, 9).unwrap().non_resident.len(), 1);
        assert!(parse_record(&good, 10).is_err(), "slot number must match");
        let mut not_in_use = good.clone();
        not_in_use[22] = 0;
        assert!(parse_record(&not_in_use, 9).is_err());
        let mut runaway = good.clone();
        runaway[0x38 + 4..0x38 + 8].copy_from_slice(&4000_u32.to_le_bytes());
        assert!(parse_record(&runaway, 9).is_err());
        let mut bad_vcn = good.clone();
        bad_vcn[0x38 + 24..0x38 + 32].copy_from_slice(&5_u64.to_le_bytes());
        assert!(
            parse_record(&bad_vcn, 9).is_err(),
            "runs must cover the VCN range"
        );
    }
}
