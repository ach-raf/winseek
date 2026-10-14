//! Drive letter enumeration.

use crate::sys::*;

const DRIVE_REMOVABLE: u32 = 2;
const DRIVE_FIXED: u32 = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeDesc {
    pub letter: char,
    /// As `GetVolumeInformationW` reports: "NTFS", "FAT32", "exFAT", "ReFS", "FAT".
    pub fs: String,
    pub serial: u32,
    pub removable: bool,
}

/// `GetLogicalDrives` bitmask (bit 0 = A:).
pub fn logical_drive_mask() -> u32 {
    // SAFETY: no arguments, no memory access.
    unsafe { GetLogicalDrives() }
}

/// Every fixed or removable drive letter with a readable filesystem. Works unelevated.
pub fn list_volumes() -> Vec<VolumeDesc> {
    let mask = logical_drive_mask();
    let mut out = Vec::new();
    for i in 0..26u32 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let letter = (b'A' + i as u8) as char;
        let root = wide(&format!("{letter}:\\"));
        // SAFETY: `root` is NUL-terminated.
        let ty = unsafe { GetDriveTypeW(root.as_ptr()) };
        if ty != DRIVE_FIXED && ty != DRIVE_REMOVABLE {
            continue;
        }
        let mut serial: u32 = 0;
        let mut fsname = [0u16; 64];
        // SAFETY: `root` NUL-terminated; out pointers are valid for the stated sizes; unused
        // outputs are null (allowed by the API).
        let ok = unsafe {
            GetVolumeInformationW(
                root.as_ptr(),
                std::ptr::null_mut(),
                0,
                &mut serial,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                fsname.as_mut_ptr(),
                fsname.len() as u32,
            )
        };
        if ok == 0 {
            continue;
        }
        out.push(VolumeDesc {
            letter,
            fs: from_wide_nul(&fsname),
            serial,
            removable: ty == DRIVE_REMOVABLE,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_system_drive() {
        let v = list_volumes();
        assert!(!v.is_empty());
        for d in &v {
            assert!(d.letter.is_ascii_uppercase());
            assert!(!d.fs.is_empty());
            assert!(logical_drive_mask() & (1 << (d.letter as u8 - b'A')) != 0);
        }
    }
}
