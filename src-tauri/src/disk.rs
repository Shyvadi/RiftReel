//! Free space on the drive a folder is on (doc 03 §8: the low-disk guard, the janitor),
//! and whether a drive suits the library.

use std::path::Path;

const GB: u64 = 1_000_000_000;

/// Bytes free for this user on the drive `path` is on. The path need not exist yet: the
/// nearest existing folder above it answers. `None` if that can't be found out.
pub fn free_space(path: &Path) -> Option<u64> {
    let existing = path.ancestors().find(|p| p.exists())?;
    imp::free_space(existing)
}

/// Whether `path` is on a local, fixed drive (not a removable, network or optical one),
/// so a library there is always present. `Err` says what it is instead.
pub fn local_fixed_drive(path: &Path) -> Result<(), String> {
    imp::local_fixed_drive(path)
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
    use std::path::{Component, Path, PathBuf};

    use windows::core::HSTRING;
    use windows::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetDriveTypeW};

    pub fn local_fixed_drive(path: &Path) -> Result<(), String> {
        // The drive's root, `D:\` or `\\server\share\`: what GetDriveTypeW asks for.
        let root: PathBuf = path
            .components()
            .take_while(|c| matches!(c, Component::Prefix(_) | Component::RootDir))
            .collect();
        // SAFETY: a valid string for the call.
        let kind = unsafe { GetDriveTypeW(&HSTRING::from(root.as_os_str())) };
        // DRIVE_* values (winbase.h).
        match kind {
            3 => Ok(()),
            1 => Err("that drive isn't there".into()),
            2 => Err("it is on a removable drive".into()),
            4 => Err("it is on a network drive".into()),
            5 => Err("it is on a CD or DVD drive".into()),
            6 => Err("it is on a RAM disk".into()),
            _ => Err("its drive is of an unknown kind".into()),
        }
    }

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

    pub fn local_fixed_drive(_path: &Path) -> Result<(), String> {
        Ok(())
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
    fn the_temp_folder_is_on_a_local_fixed_drive_and_a_missing_drive_is_not() {
        assert_eq!(local_fixed_drive(&std::env::temp_dir()), Ok(()));
        if cfg!(windows) {
            let missing = ('D'..='Z')
                .rev()
                .map(|letter| format!("{letter}:\\"))
                .find(|root| !Path::new(root).exists())
                .expect("a free drive letter");
            assert_eq!(
                local_fixed_drive(&Path::new(&missing).join("RiftReel")),
                Err("that drive isn't there".to_owned())
            );
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
