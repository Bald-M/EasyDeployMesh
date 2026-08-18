#[cfg(target_os = "windows")]
use easydeploymesh_core::{Disk, SystemDetails};
#[cfg(target_os = "windows")]
use serde::Deserialize;
use std::{
    collections::BTreeSet,
    mem::{offset_of, size_of},
};
#[cfg(target_os = "windows")]
use std::{
    error::Error,
    ffi::c_void,
    fs::{self, OpenOptions},
    io::{self, Write},
    path::PathBuf,
    process::Command,
};
#[cfg(target_os = "windows")]
use windows::{
    Win32::{
        Foundation::{ERROR_INSUFFICIENT_BUFFER, GENERIC_READ, HANDLE},
        Storage::FileSystem::{
            CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
            QueryDosDeviceW,
        },
        System::{
            IO::DeviceIoControl,
            Ioctl::{
                DISK_EXTENT, GET_LENGTH_INFORMATION, IOCTL_DISK_GET_LENGTH_INFO,
                IOCTL_STORAGE_QUERY_PROPERTY, PropertyStandardQuery, STORAGE_PROPERTY_QUERY,
                StorageDeviceProperty, VOLUME_DISK_EXTENTS,
            },
        },
    },
    core::{Owned, PCWSTR},
};

#[cfg(target_os = "windows")]
const MAX_DOS_DEVICE_CHARS: usize = 1024 * 1024;

#[repr(C)]
struct StorageDeviceDescriptorPrefix {
    version: u32,
    size: u32,
    device_type: u8,
    device_type_modifier: u8,
    removable_media: u8,
    command_queueing: u8,
    vendor_id_offset: u32,
    product_id_offset: u32,
    product_revision_offset: u32,
    serial_number_offset: u32,
    bus_type: i32,
    raw_properties_length: u32,
}

const STORAGE_DESCRIPTOR_PREFIX_LEN: usize = size_of::<StorageDeviceDescriptorPrefix>();
const VENDOR_ID_OFFSET_FIELD: usize = offset_of!(StorageDeviceDescriptorPrefix, vendor_id_offset);
const PRODUCT_ID_OFFSET_FIELD: usize = offset_of!(StorageDeviceDescriptorPrefix, product_id_offset);
const SERIAL_NUMBER_OFFSET_FIELD: usize =
    offset_of!(StorageDeviceDescriptorPrefix, serial_number_offset);
#[cfg(target_os = "windows")]
const MAX_STORAGE_DESCRIPTOR_BYTES: usize = 64 * 1024;
#[cfg(target_os = "windows")]
const MAX_VOLUME_EXTENTS: usize = 1024;

#[cfg(target_os = "windows")]
#[derive(Debug, Default)]
pub(crate) struct WindowsHardwareSummary {
    pub model: Option<String>,
    pub serial: Option<String>,
    pub cpu_model: Option<String>,
    pub physical_core_count: Option<u32>,
    pub logical_processor_count: u32,
    pub memory_bytes: u64,
    pub system_details: SystemDetails,
}

#[cfg(target_os = "windows")]
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WindowsHardwareProbe {
    model: Option<String>,
    serial: Option<String>,
    cpu_model: Option<String>,
    physical_core_count: Option<u32>,
    logical_processor_count: Option<u32>,
    memory_bytes: Option<u64>,
    system_details: SystemDetails,
}

#[cfg(target_os = "windows")]
pub(crate) fn collect_hardware_summary() -> Result<WindowsHardwareSummary, Box<dyn Error>> {
    const SCRIPT: &str = r#"
$ErrorActionPreference = 'SilentlyContinue'
$computer = Get-CimInstance Win32_ComputerSystem | Select-Object -First 1
$bios = Get-CimInstance Win32_BIOS | Select-Object -First 1
$board = Get-CimInstance Win32_BaseBoard | Select-Object -First 1
$os = Get-CimInstance Win32_OperatingSystem | Select-Object -First 1
$cpu = Get-CimInstance Win32_Processor | Select-Object -First 1
$memory = @(Get-CimInstance Win32_PhysicalMemory)
$gpus = @(Get-CimInstance Win32_VideoController)
$displays = @(Get-CimInstance Win32_DesktopMonitor)
$audio = @(Get-CimInstance Win32_SoundDevice)
$network = @(Get-CimInstance Win32_NetworkAdapter | Where-Object { $_.PhysicalAdapter -eq $true })

$model = (@($computer.Manufacturer, $computer.Model) | Where-Object { $_ -and $_.Trim() } | Select-Object -Unique) -join ' '
$motherboard = (@($board.Manufacturer, $board.Product) | Where-Object { $_ -and $_.Trim() } | Select-Object -Unique) -join ' '
$uptime = if ($os.LastBootUpTime) { [uint64]((Get-Date) - $os.LastBootUpTime).TotalSeconds } else { $null }
$result = [ordered]@{
  model = if ($model) { $model.Trim() } else { $null }
  serial = if ($bios.SerialNumber) { $bios.SerialNumber.Trim() } else { $null }
  cpuModel = if ($cpu.Name) { $cpu.Name.Trim() } else { $null }
  physicalCoreCount = if ($cpu.NumberOfCores) { [uint32]$cpu.NumberOfCores } else { $null }
  logicalProcessorCount = if ($cpu.NumberOfLogicalProcessors) { [uint32]$cpu.NumberOfLogicalProcessors } else { $null }
  memoryBytes = if ($computer.TotalPhysicalMemory) { [uint64]$computer.TotalPhysicalMemory } else { $null }
  systemDetails = [ordered]@{
    osName = if ($os.Caption) { $os.Caption.Trim() } else { $null }
    osVersion = if ($os.Version) { $os.Version.Trim() } else { $null }
    uptimeSeconds = $uptime
    motherboard = if ($motherboard) { $motherboard.Trim() } else { $null }
    memoryModules = @($memory | ForEach-Object { [ordered]@{
      manufacturer = if ($_.Manufacturer) { $_.Manufacturer.Trim() } else { $null }
      partNumber = if ($_.PartNumber) { $_.PartNumber.Trim() } else { $null }
      capacityBytes = [uint64]$_.Capacity
      speedMhz = if ($_.ConfiguredClockSpeed) { [uint32]$_.ConfiguredClockSpeed } elseif ($_.Speed) { [uint32]$_.Speed } else { $null }
    }})
    gpus = @($gpus | Where-Object Name | ForEach-Object { [ordered]@{
      name = $_.Name.Trim(); manufacturer = if ($_.AdapterCompatibility) { $_.AdapterCompatibility.Trim() } else { $null }; memoryBytes = if ($_.AdapterRAM) { [uint64]$_.AdapterRAM } else { $null }
    }})
    displays = @($displays | Where-Object Name | ForEach-Object { [ordered]@{
      name = $_.Name.Trim(); manufacturer = if ($_.MonitorManufacturer) { $_.MonitorManufacturer.Trim() } else { $null }; memoryBytes = $null
    }})
    audioDevices = @($audio | Where-Object Name | ForEach-Object { [ordered]@{
      name = $_.Name.Trim(); manufacturer = if ($_.Manufacturer) { $_.Manufacturer.Trim() } else { $null }; memoryBytes = $null
    }})
    networkAdapters = @($network | Where-Object Name | ForEach-Object { [ordered]@{
      name = $_.Name.Trim(); manufacturer = if ($_.Manufacturer) { $_.Manufacturer.Trim() } else { $null }; macAddress = $_.MACAddress; speedBps = if ($_.Speed) { [uint64]$_.Speed } else { $null }
    }})
  }
}
$result | ConvertTo-Json -Depth 6 -Compress
"#;

    let mut last_error = None;
    for executable in ["powershell.exe", "powershell"] {
        match Command::new(executable)
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                SCRIPT,
            ])
            .output()
        {
            Ok(output) if output.status.success() => {
                let probe: WindowsHardwareProbe = serde_json::from_slice(&output.stdout)?;
                return Ok(WindowsHardwareSummary {
                    model: probe.model.filter(|value| !value.is_empty()),
                    serial: probe.serial.filter(|value| !value.is_empty()),
                    cpu_model: probe.cpu_model.filter(|value| !value.is_empty()),
                    physical_core_count: probe.physical_core_count,
                    logical_processor_count: probe.logical_processor_count.unwrap_or_default(),
                    memory_bytes: probe.memory_bytes.unwrap_or_default(),
                    system_details: probe.system_details,
                });
            }
            Ok(output) => {
                last_error = Some(String::from_utf8_lossy(&output.stderr).trim().to_owned());
            }
            Err(error) => last_error = Some(error.to_string()),
        }
    }
    Err(format!(
        "Windows hardware inventory probe failed: {}",
        last_error.unwrap_or_else(|| "PowerShell is unavailable".to_owned())
    )
    .into())
}

#[cfg(target_os = "windows")]
pub(crate) fn collect_disks() -> Result<Vec<Disk>, Box<dyn Error>> {
    let numbers = enumerate_disk_numbers()?;
    let system_disks = system_disk_numbers().unwrap_or_default();
    let mut disks = Vec::with_capacity(numbers.len());
    let mut last_error = None;

    for number in numbers {
        match read_disk(number, system_disks.contains(&number)) {
            Ok(disk) => disks.push(disk),
            Err(error) => {
                eprintln!("Skipping PhysicalDrive{number} during inventory: {error}");
                last_error = Some(error.to_string());
            }
        }
    }

    if disks.is_empty() {
        let detail = last_error
            .map(|error| format!(": {error}"))
            .unwrap_or_default();
        return Err(format!("no readable physical disks were reported by Windows{detail}").into());
    }
    Ok(disks)
}

#[cfg(target_os = "windows")]
fn enumerate_disk_numbers() -> Result<Vec<u32>, Box<dyn Error>> {
    match query_dos_device_numbers() {
        Ok(numbers) if !numbers.is_empty() => Ok(numbers),
        Ok(_) => diskpart_disk_numbers()
            .map_err(|error| format!("native disk enumeration was empty; {error}").into()),
        Err(native_error) => diskpart_disk_numbers().map_err(|fallback_error| {
            format!(
                "native disk enumeration failed ({native_error}); DiskPart fallback failed ({fallback_error})"
            )
            .into()
        }),
    }
}

#[cfg(target_os = "windows")]
fn query_dos_device_numbers() -> io::Result<Vec<u32>> {
    let mut capacity = 32 * 1024;
    loop {
        let mut buffer = vec![0_u16; capacity];
        let copied = unsafe { QueryDosDeviceW(PCWSTR::null(), Some(&mut buffer)) };
        if copied != 0 {
            let copied = usize::try_from(copied)
                .map_err(|_| io::Error::other("QueryDosDeviceW returned an invalid length"))?;
            if copied > buffer.len() || buffer.get(copied - 1) != Some(&0) {
                return Err(io::Error::other(
                    "QueryDosDeviceW returned an invalid MULTI_SZ buffer",
                ));
            }
            return Ok(parse_dos_device_numbers(&buffer[..copied]));
        }

        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER.0 as i32)
            || capacity >= MAX_DOS_DEVICE_CHARS
        {
            return Err(error);
        }
        capacity = (capacity * 2).min(MAX_DOS_DEVICE_CHARS);
    }
}

fn parse_dos_device_numbers(names: &[u16]) -> Vec<u32> {
    names
        .split(|character| *character == 0)
        .filter_map(|name| String::from_utf16(name).ok())
        .filter_map(|name| {
            let prefix = "PhysicalDrive";
            let (head, suffix) = name.split_at_checked(prefix.len())?;
            if !head.eq_ignore_ascii_case(prefix)
                || suffix.is_empty()
                || !suffix.bytes().all(|byte| byte.is_ascii_digit())
            {
                return None;
            }
            suffix.parse::<u32>().ok()
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

#[cfg(target_os = "windows")]
fn read_disk(number: u32, is_system: bool) -> Result<Disk, Box<dyn Error>> {
    let id = format!(r"\\.\PhysicalDrive{number}");
    let handle = open_device(&id)?;
    let size_bytes = disk_length(&handle)?;
    if size_bytes == 0 {
        return Err("disk reported a zero-byte capacity".into());
    }

    let (model, serial) = identity_fields(storage_identity(&handle).unwrap_or_default(), number);
    Ok(Disk {
        id,
        model,
        serial,
        size_bytes,
        is_system,
    })
}

#[cfg(target_os = "windows")]
fn open_device(path: &str) -> windows::core::Result<Owned<HANDLE>> {
    let wide = path.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
    let handle = unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            GENERIC_READ.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )?
    };
    Ok(unsafe { Owned::new(handle) })
}

#[cfg(target_os = "windows")]
fn disk_length(handle: &Owned<HANDLE>) -> Result<u64, Box<dyn Error>> {
    let mut output = GET_LENGTH_INFORMATION::default();
    let mut returned = 0_u32;
    unsafe {
        DeviceIoControl(
            **handle,
            IOCTL_DISK_GET_LENGTH_INFO,
            None,
            0,
            Some((&raw mut output).cast::<c_void>()),
            u32::try_from(size_of::<GET_LENGTH_INFORMATION>())?,
            Some(&mut returned),
            None,
        )?;
    }
    if returned as usize != size_of::<GET_LENGTH_INFORMATION>() {
        return Err(format!(
            "IOCTL_DISK_GET_LENGTH_INFO returned {returned} bytes instead of {}",
            size_of::<GET_LENGTH_INFORMATION>()
        )
        .into());
    }
    u64::try_from(output.Length).map_err(|_| "disk reported a negative capacity".into())
}

#[derive(Default)]
struct StorageIdentity {
    model: Option<String>,
    serial: Option<String>,
}

fn identity_fields(identity: StorageIdentity, number: u32) -> (String, Option<String>) {
    (
        identity
            .model
            .unwrap_or_else(|| format!("Physical Disk {number}")),
        identity.serial,
    )
}

#[cfg(target_os = "windows")]
fn storage_identity(handle: &Owned<HANDLE>) -> Result<StorageIdentity, Box<dyn Error>> {
    let query = STORAGE_PROPERTY_QUERY {
        PropertyId: StorageDeviceProperty,
        QueryType: PropertyStandardQuery,
        AdditionalParameters: [0],
    };
    let mut header = [0_u8; 8];
    let mut returned = 0_u32;
    unsafe {
        DeviceIoControl(
            **handle,
            IOCTL_STORAGE_QUERY_PROPERTY,
            Some((&raw const query).cast::<c_void>()),
            u32::try_from(size_of::<STORAGE_PROPERTY_QUERY>())?,
            Some(header.as_mut_ptr().cast::<c_void>()),
            u32::try_from(header.len())?,
            Some(&mut returned),
            None,
        )?;
    }
    if returned as usize != header.len() {
        return Err(format!(
            "storage descriptor header returned {returned} bytes instead of {}",
            header.len()
        )
        .into());
    }

    let descriptor_len = usize::try_from(read_u32(&header, 4).ok_or("invalid descriptor header")?)?;
    if !(STORAGE_DESCRIPTOR_PREFIX_LEN..=MAX_STORAGE_DESCRIPTOR_BYTES).contains(&descriptor_len) {
        return Err(format!("storage descriptor length {descriptor_len} is out of bounds").into());
    }

    let mut descriptor = vec![0_u8; descriptor_len];
    returned = 0;
    unsafe {
        DeviceIoControl(
            **handle,
            IOCTL_STORAGE_QUERY_PROPERTY,
            Some((&raw const query).cast::<c_void>()),
            u32::try_from(size_of::<STORAGE_PROPERTY_QUERY>())?,
            Some(descriptor.as_mut_ptr().cast::<c_void>()),
            u32::try_from(descriptor.len())?,
            Some(&mut returned),
            None,
        )?;
    }
    let returned = usize::try_from(returned)?;
    if returned > descriptor.len() {
        return Err("storage descriptor returned more bytes than requested".into());
    }
    descriptor.truncate(returned);
    parse_storage_descriptor(&descriptor).map_err(Into::into)
}

fn parse_storage_descriptor(bytes: &[u8]) -> Result<StorageIdentity, &'static str> {
    if bytes.len() < STORAGE_DESCRIPTOR_PREFIX_LEN {
        return Err("storage descriptor is truncated");
    }
    let declared_len = read_u32(bytes, 4).ok_or("storage descriptor is truncated")? as usize;
    if declared_len < STORAGE_DESCRIPTOR_PREFIX_LEN || declared_len > bytes.len() {
        return Err("storage descriptor has an invalid declared length");
    }
    let bytes = &bytes[..declared_len];
    let vendor = descriptor_string(bytes, VENDOR_ID_OFFSET_FIELD);
    let product = descriptor_string(bytes, PRODUCT_ID_OFFSET_FIELD);
    let serial = descriptor_string(bytes, SERIAL_NUMBER_OFFSET_FIELD);
    let model = match (vendor, product) {
        (Some(vendor), Some(product)) if starts_with_word_ignore_ascii_case(&product, &vendor) => {
            Some(product)
        }
        (Some(vendor), Some(product)) => Some(format!("{vendor} {product}")),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    };
    Ok(StorageIdentity { model, serial })
}

fn descriptor_string(bytes: &[u8], offset_field: usize) -> Option<String> {
    let offset = read_u32(bytes, offset_field)? as usize;
    if offset < STORAGE_DESCRIPTOR_PREFIX_LEN || offset >= bytes.len() {
        return None;
    }
    let length = bytes[offset..].iter().position(|byte| *byte == 0)?;
    let value = String::from_utf8_lossy(&bytes[offset..offset + length]);
    let value = value.trim().to_owned();
    (!value.is_empty()).then_some(value)
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let value = bytes.get(offset..offset.checked_add(4)?)?;
    Some(u32::from_le_bytes(value.try_into().ok()?))
}

fn starts_with_word_ignore_ascii_case(value: &str, prefix: &str) -> bool {
    let Some(head) = value.get(..prefix.len()) else {
        return false;
    };
    head.eq_ignore_ascii_case(prefix)
        && value
            .as_bytes()
            .get(prefix.len())
            .is_none_or(u8::is_ascii_whitespace)
}

#[cfg(target_os = "windows")]
fn system_disk_numbers() -> Result<BTreeSet<u32>, Box<dyn Error>> {
    let system_drive = std::env::var("SystemDrive")?;
    let system_drive = system_drive.trim().trim_end_matches(['\\', '/']);
    if system_drive.len() != 2
        || system_drive.as_bytes()[1] != b':'
        || !system_drive.as_bytes()[0].is_ascii_alphabetic()
    {
        return Err("SystemDrive is not a drive-letter volume".into());
    }

    let handle = open_device(&format!(r"\\.\{system_drive}"))?;
    let extent_offset = offset_of!(VOLUME_DISK_EXTENTS, Extents);
    let extent_size = size_of::<DISK_EXTENT>();
    let buffer_len = extent_offset
        .checked_add(
            MAX_VOLUME_EXTENTS
                .checked_mul(extent_size)
                .ok_or("volume extent buffer overflow")?,
        )
        .ok_or("volume extent buffer overflow")?;
    let mut buffer = vec![0_u8; buffer_len];
    let mut returned = 0_u32;
    unsafe {
        DeviceIoControl(
            *handle,
            windows::Win32::Storage::FileSystem::IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS,
            None,
            0,
            Some(buffer.as_mut_ptr().cast::<c_void>()),
            u32::try_from(buffer.len())?,
            Some(&mut returned),
            None,
        )?;
    }
    let returned = usize::try_from(returned)?;
    if returned < extent_offset || returned > buffer.len() {
        return Err("volume extent response has an invalid length".into());
    }
    let count = usize::try_from(read_u32(&buffer[..returned], 0).ok_or("missing extent count")?)?;
    if count > MAX_VOLUME_EXTENTS {
        return Err("volume reported too many disk extents".into());
    }
    let required = extent_offset
        .checked_add(
            count
                .checked_mul(extent_size)
                .ok_or("volume extent overflow")?,
        )
        .ok_or("volume extent overflow")?;
    if required > returned {
        return Err("volume extent response is truncated".into());
    }

    let mut numbers = BTreeSet::new();
    for index in 0..count {
        let offset = extent_offset + index * extent_size;
        numbers.insert(read_u32(&buffer[..returned], offset).ok_or("invalid disk extent")?);
    }
    Ok(numbers)
}

#[cfg(target_os = "windows")]
fn diskpart_disk_numbers() -> Result<Vec<u32>, Box<dyn Error>> {
    let script_path = diskpart_script()?;
    let _cleanup = TempScript(script_path.clone());
    let output = Command::new("diskpart.exe")
        .arg("/s")
        .arg(&script_path)
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "DiskPart list disk exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    parse_diskpart_disk_numbers(&output.stdout)
        .map_err(|error| format!("DiskPart list disk output was invalid: {error}").into())
}

#[cfg(target_os = "windows")]
fn diskpart_script() -> io::Result<PathBuf> {
    for suffix in 0..16_u8 {
        let path = std::env::temp_dir().join(format!(
            "easydeploymesh-diskpart-{}-{suffix}.txt",
            std::process::id()
        ));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                if let Err(error) = file
                    .write_all(b"list disk\r\nexit\r\n")
                    .and_then(|()| file.flush())
                {
                    drop(file);
                    let _ = fs::remove_file(&path);
                    return Err(error);
                }
                return Ok(path);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not reserve a temporary DiskPart script",
    ))
}

#[cfg(target_os = "windows")]
struct TempScript(PathBuf);

#[cfg(target_os = "windows")]
impl Drop for TempScript {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn parse_diskpart_disk_numbers(output: &[u8]) -> Result<Vec<u32>, &'static str> {
    let lines = output.split(|byte| *byte == b'\n').collect::<Vec<_>>();
    let separator = lines
        .iter()
        .enumerate()
        .find(|(index, line)| {
            is_diskpart_separator(line)
                && lines[..*index]
                    .iter()
                    .rev()
                    .map(|line| trim_ascii(line))
                    .find(|line| !line.is_empty())
                    .is_some_and(|header| header.windows(3).any(|value| value == b"###"))
        })
        .map(|(index, _)| index)
        .ok_or("disk table separator was not found")?;
    let mut numbers = BTreeSet::new();

    for line in &lines[separator + 1..] {
        let line = trim_ascii(line);
        if line.is_empty() {
            if !numbers.is_empty() {
                break;
            }
            continue;
        }
        if let Some(number) = parse_diskpart_row(line) {
            numbers.insert(number);
        }
    }

    if numbers.is_empty() {
        return Err("no disk rows were found");
    }
    Ok(numbers.into_iter().collect())
}

fn is_diskpart_separator(line: &[u8]) -> bool {
    let line = trim_ascii(line);
    line.iter()
        .all(|byte| byte.is_ascii_whitespace() || *byte == b'-')
        && line
            .split(|byte| byte.is_ascii_whitespace())
            .filter(|group| !group.is_empty())
            .filter(|group| group.iter().all(|byte| *byte == b'-') && group.len() >= 3)
            .count()
            >= 2
}

fn parse_diskpart_row(line: &[u8]) -> Option<u32> {
    let cell_end = line
        .windows(2)
        .position(|pair| pair.iter().all(u8::is_ascii_whitespace))?;
    let first_cell = trim_ascii(&line[..cell_end]);
    let digit_start = first_cell.iter().position(u8::is_ascii_digit)?;
    if digit_start == 0 || !first_cell[digit_start - 1].is_ascii_whitespace() {
        return None;
    }
    let prefix = trim_ascii(&first_cell[..digit_start]);
    if prefix.is_empty() || prefix.iter().any(u8::is_ascii_digit) {
        return None;
    }
    let digit_end = first_cell[digit_start..]
        .iter()
        .position(|byte| !byte.is_ascii_digit())
        .map_or(first_cell.len(), |offset| digit_start + offset);
    if !trim_ascii(&first_cell[digit_end..]).is_empty() {
        return None;
    }
    let number = std::str::from_utf8(&first_cell[digit_start..digit_end])
        .ok()?
        .parse::<u32>()
        .ok()?;
    contains_size_unit(&line[cell_end..]).then_some(number)
}

fn contains_size_unit(bytes: &[u8]) -> bool {
    bytes
        .split(u8::is_ascii_whitespace)
        .filter(|token| !token.is_empty())
        .any(|token| {
            matches!(
                token,
                [b'B' | b'b']
                    | [
                        b'K' | b'k' | b'M' | b'm' | b'G' | b'g' | b'T' | b't' | b'P' | b'p',
                        b'B' | b'b'
                    ]
            )
        })
}

fn trim_ascii(mut bytes: &[u8]) -> &[u8] {
    while bytes.first().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[1..];
    }
    while bytes.last().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_english_diskpart_output_without_banner_numbers() {
        let output = br#"Microsoft DiskPart version 10.0.26100.1
Copyright (C) Microsoft Corporation.
On computer: EASYDEPLOYMESH

  Disk ###  Status         Size     Free     Dyn  Gpt
  --------  -------------  -------  -------  ---  ---
* Disk 0    Online          476 GB  1024 KB        *
  Disk 12   No Media           0 B      0 B
  Disk M9   Missing             0 B      0 B
"#;

        assert_eq!(parse_diskpart_disk_numbers(output), Ok(vec![0, 12]));
    }

    #[test]
    fn parses_zh_cn_utf8_diskpart_output() {
        let output = "Microsoft DiskPart 版本 10.0.26100.1\r\n\
            在计算机上: EASYDEPLOYMESH\r\n\r\n\
              磁盘 ###  状态           大小     可用     Dyn  Gpt\r\n\
              --------  -------------  -------  -------  ---  ---\r\n\
              磁盘 0    联机            476 GB  1024 KB        *\r\n\
              磁盘 2    脱机             64 GB      0 B\r\n";

        assert_eq!(
            parse_diskpart_disk_numbers(output.as_bytes()),
            Ok(vec![0, 2])
        );
    }

    #[test]
    fn parses_zh_cn_cp936_diskpart_output() {
        let output = b"Microsoft DiskPart \xb0\xe6\xb1\xbe 10.0.26100.1\r\n\r\n\
          \xb4\xc5\xc5\xcc ###  \xd7\xb4\xcc\xac           \xb4\xf3\xd0\xa1     \xbf\xc9\xd3\xc3     Dyn  Gpt\r\n\
          --------  -------------  -------  -------  ---  ---\r\n\
          \xb4\xc5\xc5\xcc 1    \xc1\xaa\xbb\xfa            238 GB      0 B        *\r\n";

        assert_eq!(parse_diskpart_disk_numbers(output), Ok(vec![1]));
    }

    #[test]
    fn rejects_malformed_rows_and_banner_only_matches() {
        let output = br#"Disk 7 is mentioned in this banner.
Microsoft DiskPart version 10.0.26100.1

  Disk ###  Status         Size     Free
  --------  -------------  -------  -------
  Disk X    Online          476 GB      0 B
  Disk 3    Online          unknown
"#;

        assert_eq!(
            parse_diskpart_disk_numbers(output),
            Err("no disk rows were found")
        );
        assert_eq!(
            parse_diskpart_disk_numbers(b"Disk 4 Online 100 GB\r\n"),
            Err("disk table separator was not found")
        );
    }

    #[test]
    fn storage_descriptor_validates_offsets_and_builds_identity() {
        let mut descriptor = vec![0_u8; 64];
        descriptor[4..8].copy_from_slice(&(64_u32).to_le_bytes());
        descriptor[12..16].copy_from_slice(&(36_u32).to_le_bytes());
        descriptor[16..20].copy_from_slice(&(43_u32).to_le_bytes());
        descriptor[24..28].copy_from_slice(&(55_u32).to_le_bytes());
        descriptor[36..43].copy_from_slice(b"Vendor\0");
        descriptor[43..55].copy_from_slice(b"Fast Disk\0\0\0");
        descriptor[55..64].copy_from_slice(b" SN-42 \0\0");

        let identity = parse_storage_descriptor(&descriptor).expect("valid descriptor");
        assert_eq!(identity.model.as_deref(), Some("Vendor Fast Disk"));
        assert_eq!(identity.serial.as_deref(), Some("SN-42"));

        descriptor[16..20].copy_from_slice(&(64_u32).to_le_bytes());
        let identity = parse_storage_descriptor(&descriptor).expect("bounded descriptor");
        assert_eq!(identity.model.as_deref(), Some("Vendor"));
    }

    #[test]
    fn storage_descriptor_rejects_truncation() {
        let mut descriptor = vec![0_u8; STORAGE_DESCRIPTOR_PREFIX_LEN];
        descriptor[4..8].copy_from_slice(&(64_u32).to_le_bytes());
        assert!(parse_storage_descriptor(&descriptor).is_err());
    }

    #[test]
    fn parses_only_numbered_physical_drive_device_names() {
        let names = "C:\0PhysicalDrive12\0physicaldrive0\0PhysicalDriveX\0PhysicalDrive12\0\0"
            .encode_utf16()
            .collect::<Vec<_>>();

        assert_eq!(parse_dos_device_numbers(&names), vec![0, 12]);
    }

    #[test]
    fn unavailable_identity_uses_stable_fallbacks() {
        let (model, serial) = identity_fields(StorageIdentity::default(), 7);

        assert_eq!(model, "Physical Disk 7");
        assert_eq!(serial, None);
    }
}
