use gpt_disk_io::{BlockIo, BlockIoAdapter, gpt_disk_types::BlockSize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};
use thiserror::Error;

const ISO_SECTOR_BYTES: u64 = 2048;
const MAX_ISO_BYTES: u64 = 32 * 1024 * 1024 * 1024;
const MAX_DIRECTORY_ENTRIES: usize = 4096;
const MAX_INFO_BYTES: u64 = 8 * 1024;
const MIN_KERNEL_BYTES: u64 = 1024 * 1024;
const MAX_KERNEL_BYTES: u64 = 256 * 1024 * 1024;
const MIN_INITRD_BYTES: u64 = 1024 * 1024;
const MAX_INITRD_BYTES: u64 = 2 * 1024 * 1024 * 1024;

pub(crate) const MANAGED_KERNEL_NAME: &str = "installer-vmlinuz";
pub(crate) const MANAGED_INITRD_NAME: &str = "installer-initrd";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InspectedAsset {
    pub(crate) basename: &'static str,
    pub(crate) size_bytes: u64,
    pub(crate) sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InspectedUbuntuIso {
    pub(crate) release: String,
    pub(crate) kernel: InspectedAsset,
    pub(crate) initrd: InspectedAsset,
}

#[derive(Debug, Error)]
pub(crate) enum LinuxIsoError {
    #[error("could not access the ISO: {0}")]
    Io(#[from] io::Error),
    #[error("{0}")]
    Invalid(String),
}

/// Inspects content rather than the source filename and copies only the two
/// bounded boot assets needed by the network installer into the staged object.
pub(crate) fn inspect_and_extract(
    iso_path: &Path,
    staged_object_dir: &Path,
) -> Result<InspectedUbuntuIso, LinuxIsoError> {
    inspect(iso_path, Some(staged_object_dir))
}

/// Re-reads the embedded metadata and boot assets without trusting the
/// persisted capability manifest.
pub(crate) fn inspect_managed(iso_path: &Path) -> Result<InspectedUbuntuIso, LinuxIsoError> {
    inspect(iso_path, None)
}

fn inspect(
    iso_path: &Path,
    staged_object_dir: Option<&Path>,
) -> Result<InspectedUbuntuIso, LinuxIsoError> {
    let media_len = fs::metadata(iso_path)?.len();
    if !(ISO_SECTOR_BYTES * 17..=MAX_ISO_BYTES).contains(&media_len) {
        return Err(LinuxIsoError::Invalid(format!(
            "ISO size {media_len} is outside the supported range"
        )));
    }

    let block_size = BlockSize::new(ISO_SECTOR_BYTES as u32)
        .ok_or_else(|| LinuxIsoError::Invalid("invalid ISO sector size".to_owned()))?;
    let media = File::open(iso_path)?;
    let mut media = BlockIoAdapter::new(media, block_size);
    let volume = iso9660::mount(&mut media, 0)
        .map_err(|error| LinuxIsoError::Invalid(format!("could not mount ISO9660: {error}")))?;
    validate_extent(
        volume.root_extent_lba,
        u64::from(volume.root_extent_len),
        media_len,
        "root directory",
    )?;

    let disk_dir = find_child(
        &mut media,
        volume.root_extent_lba,
        volume.root_extent_len,
        ".disk",
        media_len,
    )?;
    require_directory(&disk_dir, ".disk")?;
    let info = find_child(
        &mut media,
        disk_dir.extent_lba,
        disk_dir.data_length,
        "info",
        media_len,
    )?;
    require_regular_file(&info, ".disk/info", 1, MAX_INFO_BYTES, media_len)?;
    let info_bytes = read_bounded_file(&mut media, info, MAX_INFO_BYTES)?;
    let info_text = std::str::from_utf8(&info_bytes)
        .map_err(|_| LinuxIsoError::Invalid(".disk/info is not valid UTF-8".to_owned()))?;
    validate_ubuntu_server_info(info_text)?;

    let casper_dir = find_child(
        &mut media,
        volume.root_extent_lba,
        volume.root_extent_len,
        "casper",
        media_len,
    )?;
    require_directory(&casper_dir, "casper")?;
    let kernel = find_child(
        &mut media,
        casper_dir.extent_lba,
        casper_dir.data_length,
        "vmlinuz",
        media_len,
    )?;
    require_regular_file(
        &kernel,
        "casper/vmlinuz",
        MIN_KERNEL_BYTES,
        MAX_KERNEL_BYTES,
        media_len,
    )?;
    let initrd = find_child(
        &mut media,
        casper_dir.extent_lba,
        casper_dir.data_length,
        "initrd",
        media_len,
    )?;
    require_regular_file(
        &initrd,
        "casper/initrd",
        MIN_INITRD_BYTES,
        MAX_INITRD_BYTES,
        media_len,
    )?;

    let kernel_path = staged_object_dir.map(|directory| directory.join(MANAGED_KERNEL_NAME));
    let kernel_metadata = copy_file_and_hash(&mut media, kernel, kernel_path.as_deref())?;
    validate_linux_kernel_header(&kernel_metadata.2)?;
    let initrd_path = staged_object_dir.map(|directory| directory.join(MANAGED_INITRD_NAME));
    let initrd_metadata = copy_file_and_hash(&mut media, initrd, initrd_path.as_deref())?;

    Ok(InspectedUbuntuIso {
        release: "24.04".to_owned(),
        kernel: InspectedAsset {
            basename: MANAGED_KERNEL_NAME,
            size_bytes: kernel_metadata.0,
            sha256: kernel_metadata.1,
        },
        initrd: InspectedAsset {
            basename: MANAGED_INITRD_NAME,
            size_bytes: initrd_metadata.0,
            sha256: initrd_metadata.1,
        },
    })
}

fn validate_ubuntu_server_info(info: &str) -> Result<(), LinuxIsoError> {
    let info = info.trim();
    let release_prefix = "Ubuntu-Server 24.04";
    let release_boundary = info.as_bytes().get(release_prefix.len()).copied();
    let correct_release = info.starts_with(release_prefix)
        && release_boundary.is_none_or(|byte| byte == b'.' || byte.is_ascii_whitespace());
    let amd64 = info
        .split_ascii_whitespace()
        .any(|field| field.trim_matches(['(', ')', ',', ';']) == "amd64");
    if !correct_release || !amd64 || !info.contains(" LTS") {
        return Err(LinuxIsoError::Invalid(
            "only Ubuntu Server 24.04 LTS amd64 installation media is supported".to_owned(),
        ));
    }
    Ok(())
}

fn find_child<B: BlockIo>(
    media: &mut B,
    directory_lba: u32,
    directory_len: u32,
    expected_name: &str,
    media_len: u64,
) -> Result<iso9660::FileEntry, LinuxIsoError> {
    validate_extent(
        directory_lba,
        u64::from(directory_len),
        media_len,
        "directory",
    )?;
    let mut found = None;
    for (index, entry) in
        iso9660::DirectoryIterator::new(media, directory_lba, directory_len).enumerate()
    {
        if index >= MAX_DIRECTORY_ENTRIES {
            return Err(LinuxIsoError::Invalid(format!(
                "ISO directory contains more than {MAX_DIRECTORY_ENTRIES} entries"
            )));
        }
        let entry = entry.map_err(|error| {
            LinuxIsoError::Invalid(format!("could not read ISO directory: {error}"))
        })?;
        if entry.name.contains(['/', '\\', '\0']) {
            return Err(LinuxIsoError::Invalid(format!(
                "ISO contains an unsafe directory entry {:?}",
                entry.name
            )));
        }
        let is_expected = entry.name.eq_ignore_ascii_case(expected_name)
            || (expected_name == ".disk" && entry.name.eq_ignore_ascii_case("_disk"));
        if is_expected {
            if found.is_some() {
                return Err(LinuxIsoError::Invalid(format!(
                    "ISO contains multiple case-conflicting entries for {expected_name}"
                )));
            }
            found = Some(entry);
        }
    }
    found.ok_or_else(|| LinuxIsoError::Invalid(format!("ISO is missing {expected_name}")))
}

fn require_directory(entry: &iso9660::FileEntry, name: &str) -> Result<(), LinuxIsoError> {
    if !entry.flags.directory {
        return Err(LinuxIsoError::Invalid(format!(
            "ISO entry {name} is not a directory"
        )));
    }
    Ok(())
}

fn require_regular_file(
    entry: &iso9660::FileEntry,
    name: &str,
    minimum: u64,
    maximum: u64,
    media_len: u64,
) -> Result<(), LinuxIsoError> {
    if entry.flags.directory || !(minimum..=maximum).contains(&entry.size) {
        return Err(LinuxIsoError::Invalid(format!(
            "ISO entry {name} has an unsupported type or size"
        )));
    }
    validate_extent(entry.extent_lba, entry.size, media_len, name)
}

fn validate_extent(
    extent_lba: u32,
    length: u64,
    media_len: u64,
    name: &str,
) -> Result<(), LinuxIsoError> {
    let start = u64::from(extent_lba)
        .checked_mul(ISO_SECTOR_BYTES)
        .ok_or_else(|| LinuxIsoError::Invalid(format!("ISO extent for {name} overflows")))?;
    let padded_length = length
        .checked_add(ISO_SECTOR_BYTES - 1)
        .map(|value| value / ISO_SECTOR_BYTES * ISO_SECTOR_BYTES)
        .ok_or_else(|| LinuxIsoError::Invalid(format!("ISO length for {name} overflows")))?;
    if length == 0
        || start
            .checked_add(padded_length)
            .is_none_or(|end| end > media_len)
    {
        return Err(LinuxIsoError::Invalid(format!(
            "ISO extent for {name} lies outside the image"
        )));
    }
    Ok(())
}

fn read_bounded_file<B: BlockIo>(
    media: &mut B,
    entry: iso9660::FileEntry,
    maximum: u64,
) -> Result<Vec<u8>, LinuxIsoError> {
    if entry.size > maximum {
        return Err(LinuxIsoError::Invalid(
            "ISO metadata file exceeds its read limit".to_owned(),
        ));
    }
    let capacity = usize::try_from(entry.size)
        .map_err(|_| LinuxIsoError::Invalid("ISO metadata size is unsupported".to_owned()))?;
    let mut contents = Vec::with_capacity(capacity);
    let mut reader = iso9660::FileReader::new(media, entry);
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        let count = reader
            .read(&mut buffer)
            .map_err(|error| LinuxIsoError::Invalid(format!("could not read ISO file: {error}")))?;
        if count == 0 {
            break;
        }
        contents.extend_from_slice(&buffer[..count]);
        if contents.len() > capacity {
            return Err(LinuxIsoError::Invalid(
                "ISO file exceeded its declared size".to_owned(),
            ));
        }
    }
    if contents.len() != capacity {
        return Err(LinuxIsoError::Invalid(
            "ISO file ended before its declared size".to_owned(),
        ));
    }
    Ok(contents)
}

fn copy_file_and_hash<B: BlockIo>(
    media: &mut B,
    entry: iso9660::FileEntry,
    destination: Option<&Path>,
) -> Result<(u64, String, Vec<u8>), LinuxIsoError> {
    let declared_size = entry.size;
    let mut source = iso9660::FileReader::new(media, entry);
    let mut output = destination
        .map(|path| OpenOptions::new().create_new(true).write(true).open(path))
        .transpose()?;
    let mut hasher = Sha256::new();
    let mut copied = 0_u64;
    let mut prefix = Vec::with_capacity(0x208);
    let mut buffer = vec![0_u8; 128 * 1024];
    loop {
        let count = source.read(&mut buffer).map_err(|error| {
            LinuxIsoError::Invalid(format!("could not extract ISO boot asset: {error}"))
        })?;
        if count == 0 {
            break;
        }
        copied = copied
            .checked_add(count as u64)
            .ok_or_else(|| LinuxIsoError::Invalid("boot asset size overflow".to_owned()))?;
        if copied > declared_size {
            return Err(LinuxIsoError::Invalid(
                "boot asset exceeded its declared size".to_owned(),
            ));
        }
        if prefix.len() < 0x208 {
            let prefix_count = (0x208 - prefix.len()).min(count);
            prefix.extend_from_slice(&buffer[..prefix_count]);
        }
        if let Some(output) = &mut output {
            output.write_all(&buffer[..count])?;
        }
        hasher.update(&buffer[..count]);
    }
    if copied != declared_size {
        return Err(LinuxIsoError::Invalid(
            "boot asset ended before its declared size".to_owned(),
        ));
    }
    if let Some(output) = output {
        output.sync_all()?;
    }
    Ok((copied, format!("{:x}", hasher.finalize()), prefix))
}

fn validate_linux_kernel_header(prefix: &[u8]) -> Result<(), LinuxIsoError> {
    let boot_signature = prefix.get(0x1fe..0x200) == Some([0x55, 0xaa].as_slice());
    let header_signature = prefix.get(0x202..0x206) == Some(b"HdrS".as_slice());
    let protocol_version = prefix
        .get(0x206..0x208)
        .and_then(|bytes| <[u8; 2]>::try_from(bytes).ok())
        .map(u16::from_le_bytes);
    if !boot_signature
        || !header_signature
        || protocol_version.is_none_or(|version| version < 0x020b)
    {
        return Err(LinuxIsoError::Invalid(
            "casper/vmlinuz is not an x86 Linux boot kernel".to_owned(),
        ));
    }
    Ok(())
}

pub(crate) fn managed_asset_path(directory: &Path, basename: &str) -> PathBuf {
    directory.join(basename)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_extents_that_overflow_or_leave_the_iso() {
        assert!(validate_extent(u32::MAX, 2048, 1024 * 1024, "fixture").is_err());
        assert!(validate_extent(1, u64::MAX, u64::MAX, "fixture").is_err());
        assert!(validate_extent(8, 2048, 8 * 2048, "fixture").is_err());
        assert!(validate_extent(8, 2048, 9 * 2048, "fixture").is_ok());
    }

    #[test]
    fn kernel_probe_requires_boot_signature_header_and_modern_protocol() {
        let mut header = vec![0_u8; 0x208];
        header[0x1fe..0x200].copy_from_slice(&[0x55, 0xaa]);
        header[0x202..0x206].copy_from_slice(b"HdrS");
        header[0x206..0x208].copy_from_slice(&0x020a_u16.to_le_bytes());
        assert!(validate_linux_kernel_header(&header).is_err());

        header[0x206..0x208].copy_from_slice(&0x020b_u16.to_le_bytes());
        assert!(validate_linux_kernel_header(&header).is_ok());
    }
}
