//! Free space on the drive a folder is on (doc 03 §8: the low-disk guard, the janitor).

use std::path::Path;

const GB: u64 = 1_000_000_000;

/// Bytes free for this user on the drive `path` is on. The path need not exist yet: the
/// nearest existing folder above it answers. `None` if that can't be found out.
pub fn free_space(path: &Path) -> Option<u64> {
    let existing = path.ancestors().find(|p| p.exists())?;
    imp::free_space(existing)
}

/// `bytes` as gigabytes for messages (`12.3 GB`).
pub fn gb(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / GB as f64)
}

/// Gigabytes from the settings as bytes (negative or not finite: 0).
pub fn bytes_of_gb(gb: f64) -> u64 {
    if gb.is_finite() && gb > 0.0 {
        (gb * GB as f64) as u64
    } else {
        0
    }
}

#[cfg(windows)]
mod imp {
    use std::path::Path;

    use windows::core::HSTRING;
    use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    pub fn free_space(path: &Path) -> Option<u64> {
        let mut available = 0u64;
        // SAFETY: the out-pointer is a live u64; the others are not asked for.
        unsafe {
            GetDiskFreeSpaceExW(
                &HSTRING::from(path.as_os_str()),
                Some(&mut available),
                None,
                None,
            )
        }
        .ok()?;
        Some(available)
    }
}

#[cfg(not(windows))]
mod imp {
    use std::path::Path;

    pub fn free_space(_path: &Path) -> Option<u64> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_space_of_a_folder_not_made_yet_is_its_drives() {
        let here = std::env::temp_dir();
        let free = free_space(&here);
        if cfg!(windows) {
            let free = free.expect("the temp folder's drive");
            assert!(free > 0);
            let later = free_space(&here.join("riftreel-not-made").join("yet")).unwrap();
            // The same drive (other programs may write in between).
            assert!(later.abs_diff(free) < 1_000_000_000, "{later} vs {free}");
        }
    }

    #[test]
    fn gigabytes() {
        assert_eq!(gb(12_345_678_901), "12.3 GB");
        assert_eq!(bytes_of_gb(20.0), 20_000_000_000);
        assert_eq!(bytes_of_gb(-1.0), 0);
        assert_eq!(bytes_of_gb(f64::NAN), 0);
    }
}
