//! Bounded, cross-platform Windows REGF operations required by EasyDeployMesh.

use regf::{DataType, HiveBuilder, KeyTreeNode, KeyTreeValue, RegistryHive, RegistryValue};
use thiserror::Error;

pub const BOOT_MANAGER_ID: &str = "{9dea862c-5cdd-4e70-acc1-f32b344d4795}";
pub const RAMDISK_OPTIONS_ID: &str = "{ae5534e0-a924-466c-b836-758539a3ee3a}";
pub const WINPE_LOADER_ID: &str = "{9f2f2f90-0f5a-4d8b-9e9f-5decb22bbf0a}";
pub const STANDARD_LOADER_PATH: &str = r"\windows\system32\winload.exe";
pub const SYSTEM32_LOADER_PATH: &str = r"\windows\system32\boot\winload.exe";
pub const AGENT_SHELL: &str = r"X:\EasyDeployMesh\easydeploymesh-shell.exe";

const MAX_HIVE_BYTES: usize = 64 * 1024 * 1024;
const REGF_HEADER_BYTES: usize = 4096;

#[derive(Debug, Error)]
pub enum BcdError {
    #[error("registry hive is empty or larger than the 64 MiB safety limit")]
    InvalidSize,
    #[error("unsupported WinPE loader path: {0}")]
    UnsupportedLoader(String),
    #[error("invalid registry hive: {0}")]
    Registry(String),
    #[error("generated WinPE BCD is missing or has an invalid {0}")]
    InvalidBcd(&'static str),
    #[error("SYSTEM hive has no Setup\\CmdLine REG_SZ value")]
    SetupCmdLineMissing,
    #[error("SYSTEM Setup\\CmdLine cell has insufficient allocated space")]
    SetupCmdLineCapacity,
}

fn registry_error(error: impl core::fmt::Display) -> BcdError {
    BcdError::Registry(error.to_string())
}

fn checked_hive(bytes: &[u8]) -> Result<RegistryHive, BcdError> {
    if bytes.is_empty() || bytes.len() > MAX_HIVE_BYTES {
        return Err(BcdError::InvalidSize);
    }
    RegistryHive::from_bytes(bytes.to_vec()).map_err(registry_error)
}

fn validate_loader(loader_path: &str) -> Result<(), BcdError> {
    if loader_path.eq_ignore_ascii_case(STANDARD_LOADER_PATH)
        || loader_path.eq_ignore_ascii_case(SYSTEM32_LOADER_PATH)
    {
        Ok(())
    } else {
        Err(BcdError::UnsupportedLoader(loader_path.into()))
    }
}

fn object_path(id: &str, suffix: &str) -> String {
    format!(r"Objects\{id}\{suffix}")
}

fn utf16(value: &str) -> Vec<u8> {
    value
        .encode_utf16()
        .chain(core::iter::once(0))
        .flat_map(u16::to_le_bytes)
        .collect()
}

fn value(name: &str, data_type: DataType, data: Vec<u8>) -> KeyTreeValue {
    KeyTreeValue {
        name: name.into(),
        data_type,
        data,
    }
}

fn put(root: &mut KeyTreeNode, path: &str, data_type: DataType, data: Vec<u8>) {
    root.get_or_create_path(path)
        .values
        .push(value("Element", data_type, data));
}

fn describe(root: &mut KeyTreeNode, id: &str, object_type: u32) {
    root.get_or_create_path(&object_path(id, "Description"))
        .values
        .push(value(
            "Type",
            DataType::Dword,
            object_type.to_le_bytes().to_vec(),
        ));
}

fn boot_device_blob() -> Vec<u8> {
    let mut output = vec![0; 16 + 0x48];
    output[16..20].copy_from_slice(&5_u32.to_le_bytes());
    output[24..28].copy_from_slice(&0x48_u32.to_le_bytes());
    output
}

fn guid_bytes(guid: &str) -> Result<[u8; 16], BcdError> {
    let digits: String = guid
        .chars()
        .filter(|character| character.is_ascii_hexdigit())
        .collect();
    if digits.len() != 32 {
        return Err(BcdError::InvalidBcd("GUID"));
    }
    let mut bytes = [0_u8; 16];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&digits[index * 2..index * 2 + 2], 16)
            .map_err(|_| BcdError::InvalidBcd("GUID"))?;
    }
    bytes[0..4].reverse();
    bytes[4..6].reverse();
    bytes[6..8].reverse();
    Ok(bytes)
}

fn ramdisk_device_blob() -> Result<Vec<u8>, BcdError> {
    let path = utf16(r"\Boot\boot.wim");
    let mut output = vec![0_u8; 134 + path.len()];
    let output_len = output.len();
    output[..16].copy_from_slice(&guid_bytes(RAMDISK_OPTIONS_ID)?);
    output[20..24].copy_from_slice(&1_u32.to_le_bytes());
    output[24..28].copy_from_slice(&((output_len - 10) as u32).to_le_bytes());
    output[32..36].copy_from_slice(&3_u32.to_le_bytes());
    output[56..60].copy_from_slice(&1_u32.to_le_bytes());
    output[60..64].copy_from_slice(&((output_len - 50) as u32).to_le_bytes());
    output[67] = 5;
    output[68..72].copy_from_slice(&5_u32.to_le_bytes());
    output[76] = 0x48;
    output[134..].copy_from_slice(&path);
    Ok(output)
}

/// Creates a fresh deterministic BCD store for the managed PXE package.
pub fn create_winpe_bcd(loader_path: &str) -> Result<Vec<u8>, BcdError> {
    validate_loader(loader_path)?;
    let mut root = KeyTreeNode::new("BCD00000001");
    root.get_or_create_path("Description").values.extend([
        value("KeyName", DataType::String, utf16("BCD00000001")),
        value("System", DataType::Dword, 1_u32.to_le_bytes().to_vec()),
    ]);

    describe(&mut root, BOOT_MANAGER_ID, 0x1010_0002);
    put(
        &mut root,
        &object_path(BOOT_MANAGER_ID, "Elements\\12000004"),
        DataType::String,
        utf16("EasyDeployMesh Boot Manager"),
    );
    put(
        &mut root,
        &object_path(BOOT_MANAGER_ID, "Elements\\23000003"),
        DataType::String,
        utf16(WINPE_LOADER_ID),
    );
    let mut display_order = utf16(WINPE_LOADER_ID);
    display_order.extend_from_slice(&[0, 0]);
    put(
        &mut root,
        &object_path(BOOT_MANAGER_ID, "Elements\\24000001"),
        DataType::MultiString,
        display_order,
    );
    put(
        &mut root,
        &object_path(BOOT_MANAGER_ID, "Elements\\25000004"),
        DataType::Binary,
        0_u64.to_le_bytes().to_vec(),
    );

    describe(&mut root, RAMDISK_OPTIONS_ID, 0x3000_0000);
    put(
        &mut root,
        &object_path(RAMDISK_OPTIONS_ID, "Elements\\12000004"),
        DataType::String,
        utf16("EasyDeployMesh RAM disk"),
    );
    put(
        &mut root,
        &object_path(RAMDISK_OPTIONS_ID, "Elements\\31000003"),
        DataType::Binary,
        boot_device_blob(),
    );
    put(
        &mut root,
        &object_path(RAMDISK_OPTIONS_ID, "Elements\\32000004"),
        DataType::String,
        utf16(r"\Boot\boot.sdi"),
    );

    describe(&mut root, WINPE_LOADER_ID, 0x1020_0003);
    for element in ["11000001", "21000001"] {
        put(
            &mut root,
            &object_path(WINPE_LOADER_ID, &format!("Elements\\{element}")),
            DataType::Binary,
            ramdisk_device_blob()?,
        );
    }
    put(
        &mut root,
        &object_path(WINPE_LOADER_ID, "Elements\\12000002"),
        DataType::String,
        utf16(loader_path),
    );
    put(
        &mut root,
        &object_path(WINPE_LOADER_ID, "Elements\\12000004"),
        DataType::String,
        utf16("EasyDeployMesh Windows PE"),
    );
    put(
        &mut root,
        &object_path(WINPE_LOADER_ID, "Elements\\22000002"),
        DataType::String,
        utf16(r"\windows"),
    );
    put(
        &mut root,
        &object_path(WINPE_LOADER_ID, "Elements\\26000010"),
        DataType::Binary,
        vec![1],
    );
    put(
        &mut root,
        &object_path(WINPE_LOADER_ID, "Elements\\26000022"),
        DataType::Binary,
        vec![1],
    );

    let bytes = HiveBuilder::from_tree(root)
        .build()
        .map_err(registry_error)?;
    validate_winpe_bcd(&bytes, loader_path)?;
    Ok(bytes)
}

fn raw_value(hive: &RegistryHive, path: &str, name: &str) -> Result<Vec<u8>, BcdError> {
    hive.open_key(path)
        .and_then(|key| key.value(name))
        .and_then(|entry| entry.raw_data())
        .map_err(registry_error)
}

fn expect(
    hive: &RegistryHive,
    path: String,
    expected: Vec<u8>,
    label: &'static str,
) -> Result<(), BcdError> {
    (raw_value(hive, &path, "Element").ok().as_deref() == Some(expected.as_slice()))
        .then_some(())
        .ok_or(BcdError::InvalidBcd(label))
}

/// Validates every boot-critical value before a generated BCD is published.
pub fn validate_winpe_bcd(bytes: &[u8], loader_path: &str) -> Result<(), BcdError> {
    validate_loader(loader_path)?;
    let hive = checked_hive(bytes)?;
    expect(
        &hive,
        object_path(BOOT_MANAGER_ID, "Elements\\12000004"),
        utf16("EasyDeployMesh Boot Manager"),
        "boot manager description",
    )?;
    expect(
        &hive,
        object_path(BOOT_MANAGER_ID, "Elements\\23000003"),
        utf16(WINPE_LOADER_ID),
        "default loader",
    )?;
    let mut display_order = utf16(WINPE_LOADER_ID);
    display_order.extend_from_slice(&[0, 0]);
    expect(
        &hive,
        object_path(BOOT_MANAGER_ID, "Elements\\24000001"),
        display_order,
        "display order",
    )?;
    expect(
        &hive,
        object_path(BOOT_MANAGER_ID, "Elements\\25000004"),
        0_u64.to_le_bytes().to_vec(),
        "timeout",
    )?;
    expect(
        &hive,
        object_path(RAMDISK_OPTIONS_ID, "Elements\\31000003"),
        boot_device_blob(),
        "boot.sdi device",
    )?;
    expect(
        &hive,
        object_path(RAMDISK_OPTIONS_ID, "Elements\\32000004"),
        utf16(r"\Boot\boot.sdi"),
        "boot.sdi path",
    )?;
    expect(
        &hive,
        object_path(WINPE_LOADER_ID, "Elements\\12000002"),
        utf16(loader_path),
        "loader path",
    )?;
    expect(
        &hive,
        object_path(WINPE_LOADER_ID, "Elements\\22000002"),
        utf16(r"\windows"),
        "system root",
    )?;
    for (element, label) in [("26000010", "detecthal"), ("26000022", "WinPE mode")] {
        expect(
            &hive,
            object_path(WINPE_LOADER_ID, &format!("Elements\\{element}")),
            vec![1],
            label,
        )?;
    }
    for element in ["11000001", "21000001"] {
        expect(
            &hive,
            object_path(WINPE_LOADER_ID, &format!("Elements\\{element}")),
            ramdisk_device_blob()?,
            "ramdisk device",
        )?;
    }
    Ok(())
}

/// Reads the one offline SYSTEM value used by the shell-hook fallback.
pub fn setup_cmdline(bytes: &[u8]) -> Result<String, BcdError> {
    let hive = checked_hive(bytes)?;
    match hive
        .open_key("Setup")
        .and_then(|key| key.value("CmdLine"))
        .and_then(|entry| entry.data())
    {
        Ok(RegistryValue::String(value)) if !value.trim().is_empty() => Ok(value),
        _ => Err(BcdError::SetupCmdLineMissing),
    }
}

fn update_checksum(bytes: &mut [u8]) {
    let sequence = u32::from_le_bytes(bytes[4..8].try_into().unwrap()).wrapping_add(1);
    bytes[4..8].copy_from_slice(&sequence.to_le_bytes());
    bytes[8..12].copy_from_slice(&sequence.to_le_bytes());
    bytes[508..512].fill(0);
    let checksum = bytes[..508].chunks_exact(4).fold(0_u32, |sum, word| {
        sum ^ u32::from_le_bytes(word.try_into().unwrap())
    });
    let checksum = match checksum {
        0 => 1,
        u32::MAX => u32::MAX - 1,
        value => value,
    };
    bytes[508..512].copy_from_slice(&checksum.to_le_bytes());
}

/// Replaces only `Setup\\CmdLine` in-place, preserving every unrelated cell.
pub fn replace_setup_cmdline(bytes: &[u8]) -> Result<(String, Vec<u8>), BcdError> {
    let hive = checked_hive(bytes)?;
    let original = setup_cmdline(bytes)?;
    if original.eq_ignore_ascii_case(AGENT_SHELL) {
        return Ok((original, bytes.to_vec()));
    }
    let entry = hive
        .open_key("Setup")
        .and_then(|key| key.value("CmdLine"))
        .map_err(registry_error)?;
    let raw = entry.raw_value();
    if raw.is_data_resident() {
        return Err(BcdError::SetupCmdLineCapacity);
    }
    let replacement = utf16(AGENT_SHELL);
    let data_cell = REGF_HEADER_BYTES
        .checked_add(raw.data_offset as usize)
        .ok_or(BcdError::SetupCmdLineCapacity)?;
    let value_cell = REGF_HEADER_BYTES
        .checked_add(entry.offset() as usize)
        .ok_or(BcdError::SetupCmdLineCapacity)?;
    if data_cell + 4 > bytes.len() || value_cell + 16 > bytes.len() {
        return Err(BcdError::SetupCmdLineCapacity);
    }
    let allocated = i32::from_le_bytes(bytes[data_cell..data_cell + 4].try_into().unwrap())
        .unsigned_abs() as usize;
    if allocated < 4 || data_cell + allocated > bytes.len() {
        return Err(BcdError::SetupCmdLineCapacity);
    }
    let mut output = bytes.to_vec();
    let target_cell = if replacement.len() <= allocated - 4 {
        data_cell
    } else {
        let required = (replacement.len() + 4 + 7) & !7;
        let free = find_free_cell(&output, required).ok_or(BcdError::SetupCmdLineCapacity)?;
        let free_size = i32::from_le_bytes(output[free..free + 4].try_into().unwrap()) as usize;
        output[free..free + 4].copy_from_slice(&(-(required as i32)).to_le_bytes());
        if free_size > required {
            output[free + required..free + required + 4]
                .copy_from_slice(&((free_size - required) as i32).to_le_bytes());
        }
        output[data_cell..data_cell + 4].copy_from_slice(&(allocated as i32).to_le_bytes());
        output[value_cell + 12..value_cell + 16]
            .copy_from_slice(&((free - REGF_HEADER_BYTES) as u32).to_le_bytes());
        free
    };
    let target_size = i32::from_le_bytes(output[target_cell..target_cell + 4].try_into().unwrap())
        .unsigned_abs() as usize;
    output[target_cell + 4..target_cell + target_size].fill(0);
    output[target_cell + 4..target_cell + 4 + replacement.len()].copy_from_slice(&replacement);
    output[value_cell + 8..value_cell + 12]
        .copy_from_slice(&(replacement.len() as u32).to_le_bytes());
    update_checksum(&mut output);
    if setup_cmdline(&output)? != AGENT_SHELL {
        return Err(BcdError::InvalidBcd("Setup\\CmdLine verification"));
    }
    Ok((original, output))
}

fn find_free_cell(bytes: &[u8], required: usize) -> Option<usize> {
    let mut bin = REGF_HEADER_BYTES;
    while bin.checked_add(32)? <= bytes.len() {
        if &bytes[bin..bin + 4] != b"hbin" {
            return None;
        }
        let bin_size = u32::from_le_bytes(bytes[bin + 8..bin + 12].try_into().ok()?) as usize;
        let end = bin.checked_add(bin_size)?;
        if bin_size < 32 || end > bytes.len() {
            return None;
        }
        let mut cell = bin + 32;
        while cell.checked_add(4)? <= end {
            let signed = i32::from_le_bytes(bytes[cell..cell + 4].try_into().ok()?);
            if signed == i32::MIN || signed == 0 {
                return None;
            }
            let size = signed.unsigned_abs() as usize;
            if size < 8 || size % 8 != 0 || cell.checked_add(size)? > end {
                return None;
            }
            if signed > 0 && size >= required {
                return Some(cell);
            }
            cell += size;
        }
        if cell != end {
            return None;
        }
        bin = end;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_bcd_round_trips_and_rejects_unknown_loader() {
        for loader in [STANDARD_LOADER_PATH, SYSTEM32_LOADER_PATH] {
            let bytes = create_winpe_bcd(loader).unwrap();
            assert!(bytes.starts_with(b"regf"));
            validate_winpe_bcd(&bytes, loader).unwrap();
            let device = ramdisk_device_blob().unwrap();
            assert_eq!(device.len(), 164);
            assert_eq!(&device[134..], utf16(r"\Boot\boot.wim"));
        }
        assert!(matches!(
            create_winpe_bcd(r"\WEPE\B64"),
            Err(BcdError::UnsupportedLoader(_))
        ));
    }

    #[test]
    fn malformed_and_oversized_hives_fail_closed() {
        assert!(validate_winpe_bcd(b"regf", STANDARD_LOADER_PATH).is_err());
        let mut invalid_checksum = create_winpe_bcd(STANDARD_LOADER_PATH).unwrap();
        invalid_checksum[508] ^= 1;
        assert!(validate_winpe_bcd(&invalid_checksum, STANDARD_LOADER_PATH).is_err());
        assert!(matches!(
            checked_hive(&vec![0; MAX_HIVE_BYTES + 1]),
            Err(BcdError::InvalidSize)
        ));
    }

    #[test]
    fn setup_cmdline_update_preserves_unrelated_values() {
        let mut root = KeyTreeNode::new("SYSTEM");
        root.get_or_create_path("Setup").values.push(value(
            "CmdLine",
            DataType::String,
            utf16(&"vendor-shell.exe ".repeat(8)),
        ));
        root.get_or_create_path("Unrelated").values.push(value(
            "Keep",
            DataType::Dword,
            42_u32.to_le_bytes().to_vec(),
        ));
        let input = HiveBuilder::from_tree(root).build().unwrap();
        let (_, output) = replace_setup_cmdline(&input).unwrap();
        assert_eq!(setup_cmdline(&output).unwrap(), AGENT_SHELL);
        let (_, repeated) = replace_setup_cmdline(&output).unwrap();
        assert_eq!(repeated, output);
        let hive = checked_hive(&output).unwrap();
        assert_eq!(
            hive.open_key("Unrelated")
                .unwrap()
                .value("Keep")
                .unwrap()
                .dword_data()
                .unwrap(),
            42
        );
    }

    #[test]
    fn setup_cmdline_can_move_short_data_into_a_bounded_free_cell() {
        let mut root = KeyTreeNode::new("SYSTEM");
        root.get_or_create_path("Setup").values.push(value(
            "CmdLine",
            DataType::String,
            utf16("cmd.exe"),
        ));
        let input = HiveBuilder::from_tree(root).build().unwrap();
        let (_, output) = replace_setup_cmdline(&input).unwrap();
        assert_eq!(setup_cmdline(&output).unwrap(), AGENT_SHELL);
    }
}
