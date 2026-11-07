//! Storage media detection for the speed report.
//!
//! The report must say whether a number came from an NVMe SSD, a SATA SSD or a
//! spinning disk, because the same scan is an order of magnitude apart between
//! them. Detection is best-effort: anything it cannot determine is reported as
//! "unknown" instead of being guessed.

use serde::Serialize;
use std::path::Path;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct DriveInfo {
    /// Drive root the answer is for (`C:`), or empty when unknown.
    pub root: String,
    /// Bus the device sits on: NVMe, SATA, ATA, USB, RAID, Virtual, unknown.
    pub bus: String,
    /// "SSD", "HDD" or "unknown".
    pub media: String,
    /// Vendor and product string from the device descriptor.
    pub model: String,
}

impl DriveInfo {
    /// Short label for tables, e.g. "NVMe SSD", "SATA SSD", "SATA HDD".
    pub fn label(&self) -> String {
        match (self.bus.as_str(), self.media.as_str()) {
            ("unknown", "unknown") => "unknown".to_string(),
            (bus, "unknown") => bus.to_string(),
            ("unknown", media) => media.to_string(),
            (bus, media) => format!("{bus} {media}"),
        }
    }
}

/// Maps a `STORAGE_BUS_TYPE` value to a readable name.
pub fn bus_name(bus_type: u32) -> &'static str {
    match bus_type {
        1 => "SCSI",
        2 => "ATAPI",
        3 => "ATA",
        4 => "IEEE1394",
        5 => "SSA",
        6 => "Fibre",
        7 => "USB",
        8 => "RAID",
        9 => "iSCSI",
        10 => "SAS",
        11 => "SATA",
        12 => "SD",
        13 => "MMC",
        14 => "Virtual",
        15 => "FileBackedVirtual",
        16 => "Spaces",
        17 => "NVMe",
        18 => "SCM",
        19 => "UFS",
        _ => "unknown",
    }
}

/// Describes the drive that holds `path`.
pub fn describe(path: &Path) -> DriveInfo {
    #[cfg(windows)]
    {
        windows_impl::describe(path)
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        DriveInfo {
            root: String::new(),
            bus: "unknown".into(),
            media: "unknown".into(),
            model: String::new(),
        }
    }
}

#[cfg(windows)]
mod windows_impl {
    use super::{DriveInfo, bus_name};
    use std::os::windows::ffi::OsStrExt;
    use std::path::{Component, Path, Prefix};
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::IO::DeviceIoControl;

    /// `CTL_CODE(IOCTL_STORAGE_BASE, 0x0500, METHOD_BUFFERED, FILE_ANY_ACCESS)`.
    const IOCTL_STORAGE_QUERY_PROPERTY: u32 = 0x002D_1400;
    const STORAGE_DEVICE_PROPERTY: u32 = 0;
    const STORAGE_DEVICE_SEEK_PENALTY_PROPERTY: u32 = 7;
    const PROPERTY_STANDARD_QUERY: u32 = 0;

    fn drive_letter(path: &Path) -> Option<char> {
        match path.components().next()? {
            Component::Prefix(p) => match p.kind() {
                Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => {
                    Some((letter as char).to_ascii_uppercase())
                }
                _ => None,
            },
            _ => None,
        }
    }

    /// Runs `IOCTL_STORAGE_QUERY_PROPERTY` against an open volume handle.
    fn query(handle: isize, property_id: u32) -> Option<Vec<u8>> {
        // STORAGE_PROPERTY_QUERY: PropertyId, QueryType, one byte of
        // AdditionalParameters (padded to 12 bytes).
        let mut input = [0u8; 12];
        input[0..4].copy_from_slice(&property_id.to_le_bytes());
        input[4..8].copy_from_slice(&PROPERTY_STANDARD_QUERY.to_le_bytes());
        let mut out = vec![0u8; 1024];
        let mut returned = 0u32;
        // SAFETY: both buffers are valid for the lengths passed, the handle
        // is open for the duration of the call, and no overlapped I/O is used.
        let ok = unsafe {
            DeviceIoControl(
                handle as _,
                IOCTL_STORAGE_QUERY_PROPERTY,
                input.as_ptr().cast(),
                input.len() as u32,
                out.as_mut_ptr().cast(),
                out.len() as u32,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return None;
        }
        out.truncate(returned as usize);
        Some(out)
    }

    fn u32_at(buf: &[u8], at: usize) -> Option<u32> {
        buf.get(at..at + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn c_string_at(buf: &[u8], offset: u32) -> String {
        let start = offset as usize;
        if offset == 0 || start >= buf.len() {
            return String::new();
        }
        let tail = &buf[start..];
        let end = tail.iter().position(|b| *b == 0).unwrap_or(tail.len());
        String::from_utf8_lossy(&tail[..end]).trim().to_string()
    }

    pub fn describe(path: &Path) -> DriveInfo {
        let unknown = DriveInfo {
            root: String::new(),
            bus: "unknown".into(),
            media: "unknown".into(),
            model: String::new(),
        };
        let Some(letter) = drive_letter(path) else {
            return unknown;
        };
        let mut info = DriveInfo {
            root: format!("{letter}:"),
            ..unknown
        };
        if let Some(handle) = open(&format!(r"\\.\{letter}:")) {
            fill(&mut info, handle.0);
            // Dynamic or spanned volumes do not answer the storage query
            // themselves; ask which physical disk backs the volume instead.
            if info.bus == "unknown"
                && let Some(number) = disk_number(handle.0)
                && let Some(physical) = open(&format!(r"\\.\PhysicalDrive{number}"))
            {
                fill(&mut info, physical.0);
            }
        }
        info
    }

    /// Owns an open device handle and closes it on drop.
    struct Handle(isize);

    impl Drop for Handle {
        fn drop(&mut self) {
            // SAFETY: the handle came from a successful CreateFileW.
            unsafe { CloseHandle(self.0 as _) };
        }
    }

    fn open(device_path: &str) -> Option<Handle> {
        let device: Vec<u16> = std::ffi::OsString::from(device_path)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        // Zero desired access: enough to ask the driver about the device and
        // needs no administrator rights.
        // SAFETY: `device` is a NUL-terminated UTF-16 string that outlives the call.
        let handle = unsafe {
            CreateFileW(
                device.as_ptr(),
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        (handle != INVALID_HANDLE_VALUE).then_some(Handle(handle as isize))
    }

    /// Fills bus, model and media type from the two storage queries.
    fn fill(info: &mut DriveInfo, raw: isize) {
        // STORAGE_DEVICE_DESCRIPTOR: VendorIdOffset@12, ProductIdOffset@16,
        // BusType@28.
        if let Some(desc) = query(raw, STORAGE_DEVICE_PROPERTY) {
            if let Some(bus) = u32_at(&desc, 28) {
                info.bus = bus_name(bus).to_string();
            }
            let vendor = u32_at(&desc, 12).map(|o| c_string_at(&desc, o));
            let product = u32_at(&desc, 16).map(|o| c_string_at(&desc, o));
            info.model = [vendor, product]
                .into_iter()
                .flatten()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
        }
        // DEVICE_SEEK_PENALTY_DESCRIPTOR: IncursSeekPenalty is the byte at 8.
        if let Some(seek) = query(raw, STORAGE_DEVICE_SEEK_PENALTY_PROPERTY)
            && seek.len() > 8
        {
            info.media = if seek[8] == 0 { "SSD" } else { "HDD" }.to_string();
        }
    }

    /// First physical disk number behind a volume
    /// (`IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS`).
    fn disk_number(raw: isize) -> Option<u32> {
        const IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS: u32 = 0x0056_0000;
        let mut out = vec![0u8; 256];
        let mut returned = 0u32;
        // SAFETY: the output buffer is valid for its length; no input is used.
        let ok = unsafe {
            DeviceIoControl(
                raw as _,
                IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS,
                std::ptr::null(),
                0,
                out.as_mut_ptr().cast(),
                out.len() as u32,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 || u32_at(&out, 0)? == 0 {
            return None;
        }
        // VOLUME_DISK_EXTENTS: count (padded to 8), then DISK_EXTENT.DiskNumber.
        u32_at(&out, 8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bus_names_cover_the_common_cases() {
        assert_eq!(bus_name(17), "NVMe");
        assert_eq!(bus_name(11), "SATA");
        assert_eq!(bus_name(7), "USB");
        assert_eq!(bus_name(999), "unknown");
    }

    #[test]
    fn label_degrades_gracefully() {
        let mut d = DriveInfo {
            bus: "NVMe".into(),
            media: "SSD".into(),
            ..DriveInfo::default()
        };
        assert_eq!(d.label(), "NVMe SSD");
        d.media = "unknown".into();
        assert_eq!(d.label(), "NVMe");
        d.bus = "unknown".into();
        assert_eq!(d.label(), "unknown");
    }

    /// The probe must never panic, whatever the machine looks like.
    #[test]
    fn describing_the_current_drive_does_not_panic() {
        let info = describe(&std::env::current_dir().unwrap());
        eprintln!("current drive: {info:?} => {}", info.label());
        for letter in 'A'..='Z' {
            let root = format!("{letter}:\\");
            if Path::new(&root).exists() {
                let info = describe(Path::new(&root));
                eprintln!("{root} => {} [{}]", info.label(), info.model);
            }
        }
    }
}
