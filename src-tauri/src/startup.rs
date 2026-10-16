//! Starting with Windows (`[startup] with_windows`): a value named `RiftReel` under the
//! user's `Run` key, which Windows runs when the user signs in, starts RiftReel in the
//! tray. Task Manager lists it under Startup apps, where it can be turned off as well.

use std::io;
use std::path::{Path, PathBuf};

use crate::app::TRAY_ARG;

/// The key Windows reads at sign-in, under `HKEY_CURRENT_USER`.
#[cfg(windows)]
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
/// RiftReel's value under it.
#[cfg(windows)]
const VALUE: &str = "RiftReel";

/// The command Windows runs at sign-in: `exe`, in the tray.
pub fn command(exe: &Path) -> String {
    format!("\"{}\" {TRAY_ARG}", exe.display())
}

/// The exe to start: this one, under its installed name. A copy that a reinstall renamed
/// aside while it ran (`riftreel.old-….exe`) points at the one that replaced it.
pub fn this_exe() -> io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    Ok(exe.with_file_name(concat!(env!("CARGO_PKG_NAME"), ".exe")))
}

/// The command registered under RiftReel's name, if there is one.
#[cfg(windows)]
pub fn registered() -> io::Result<Option<String>> {
    use windows::core::HSTRING;
    use windows::Win32::Foundation::ERROR_FILE_NOT_FOUND;
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_SZ};

    let (key, value) = (HSTRING::from(RUN_KEY), HSTRING::from(VALUE));
    let mut bytes = 0u32;
    // SAFETY: valid strings; this call only asks for the size.
    let status = unsafe {
        RegGetValueW(HKEY_CURRENT_USER, &key, &value, RRF_RT_REG_SZ, None, None, Some(&mut bytes))
    };
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    status.ok().map_err(io::Error::other)?;
    let mut text = vec![0u16; (bytes as usize).div_ceil(2)];
    // SAFETY: `text` holds `bytes` bytes.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            &key,
            &value,
            RRF_RT_REG_SZ,
            None,
            Some(text.as_mut_ptr().cast()),
            Some(&mut bytes),
        )
    };
    status.ok().map_err(io::Error::other)?;
    let end = text.iter().position(|&unit| unit == 0).unwrap_or(text.len());
    Ok(Some(String::from_utf16_lossy(&text[..end])))
}

/// Registers `exe` to start in the tray when the user signs in, or removes that.
#[cfg(windows)]
pub fn set(enabled: bool, exe: &Path) -> io::Result<()> {
    use windows::core::HSTRING;
    use windows::Win32::Foundation::ERROR_FILE_NOT_FOUND;
    use windows::Win32::System::Registry::{
        RegDeleteKeyValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ,
    };

    let (key, value) = (HSTRING::from(RUN_KEY), HSTRING::from(VALUE));
    if !enabled {
        // SAFETY: valid strings.
        let status = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, &key, &value) };
        if status == ERROR_FILE_NOT_FOUND {
            return Ok(());
        }
        return status.ok().map_err(io::Error::other);
    }
    let data: Vec<u16> = command(exe).encode_utf16().chain([0]).collect();
    // SAFETY: `data` is a NUL-terminated UTF-16 string, `data.len() * 2` bytes long.
    let status = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            &key,
            &value,
            REG_SZ.0,
            Some(data.as_ptr().cast()),
            (data.len() * 2) as u32,
        )
    };
    status.ok().map_err(io::Error::other)
}

#[cfg(not(windows))]
pub fn registered() -> io::Result<Option<String>> {
    Ok(None)
}

#[cfg(not(windows))]
pub fn set(_enabled: bool, _exe: &Path) -> io::Result<()> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "starting with Windows needs Windows"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_the_exe_in_the_tray() {
        let exe = Path::new(r"C:\Users\Someone\AppData\Local\Programs\RiftReel\riftreel.exe");
        assert_eq!(
            command(exe),
            r#""C:\Users\Someone\AppData\Local\Programs\RiftReel\riftreel.exe" --tray"#
        );
    }
}
