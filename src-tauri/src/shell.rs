//! Windows shell actions on clip files (doc 03 §9): send to the Recycle Bin, put on the
//! clipboard as a file, "Save as…", show in Explorer. Each runs on a thread of its own
//! with COM in a single-threaded apartment, owned by the window that asked (`owner`, a
//! raw window handle), and returns when the shell is done.

use std::path::{Path, PathBuf};

/// The clipboard's file-list format (`CF_HDROP`): a `DROPFILES` header, then each path in
/// UTF-16 ending with a NUL, then one more NUL.
pub fn drop_files(paths: &[PathBuf]) -> Vec<u8> {
    // DROPFILES: pFiles (offset of the list), pt (x, y), fNC, fWide.
    const HEADER: u32 = 20;
    let mut bytes = Vec::with_capacity(HEADER as usize + 512);
    bytes.extend_from_slice(&HEADER.to_le_bytes());
    bytes.extend_from_slice(&[0; 12]);
    bytes.extend_from_slice(&1u32.to_le_bytes());
    for path in paths {
        for unit in path.as_os_str().encode_wide_lossless() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        bytes.extend_from_slice(&[0, 0]);
    }
    bytes.extend_from_slice(&[0, 0]);
    bytes
}

/// UTF-16 of a path (Windows paths are UTF-16 already; elsewhere, from UTF-8).
trait EncodeWide {
    fn encode_wide_lossless(&self) -> Vec<u16>;
}

impl EncodeWide for std::ffi::OsStr {
    fn encode_wide_lossless(&self) -> Vec<u16> {
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            self.encode_wide().collect()
        }
        #[cfg(not(windows))]
        {
            self.to_string_lossy().encode_utf16().collect()
        }
    }
}

/// Sends `paths` to the Recycle Bin, with undo. Never deletes for good: should Windows be
/// unable to recycle a file (too big for the bin, a drive without one), it asks first,
/// and saying no keeps the file. Returns `false` when the user cancelled.
pub fn recycle(paths: &[PathBuf], owner: Option<isize>) -> Result<bool, String> {
    imp::recycle(paths.to_vec(), owner)
}

/// Puts `paths` on the clipboard as files, to paste in Explorer, Discord, a chat…
pub fn copy_files(paths: &[PathBuf], owner: Option<isize>) -> Result<(), String> {
    imp::copy_files(drop_files(paths), owner)
}

/// The "Save as" dialog for an MP4, starting on `name`. `None` when cancelled.
pub fn save_as(owner: Option<isize>, name: &str) -> Result<Option<PathBuf>, String> {
    imp::save_as(owner, name.to_owned())
}

/// Opens Explorer on `path`, selected.
pub fn reveal(path: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // Explorer takes `/select,"<path>"` as written; a path can't contain a quote.
        std::process::Command::new("explorer.exe")
            .raw_arg(format!("/select,\"{}\"", path.display()))
            .spawn()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Err("showing files is only supported on Windows".into())
    }
}

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;
    use std::path::PathBuf;

    use windows::core::{w, HSTRING};
    use windows::Win32::Foundation::{GlobalFree, ERROR_CANCELLED, HANDLE, HGLOBAL, HWND};
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_INPROC_SERVER,
        COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
    };
    use windows::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
    };
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    use windows::Win32::System::Ole::{CF_HDROP, DROPEFFECT_COPY};
    use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
    use windows::Win32::UI::Shell::{
        FileOperation, FileSaveDialog, IFileOperation, IFileSaveDialog, IShellItem,
        SHCreateItemFromParsingName, FOFX_RECYCLEONDELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION,
        FOF_SILENT, FOF_WANTNUKEWARNING, FOS_FORCEFILESYSTEM, FOS_OVERWRITEPROMPT,
        FOS_PATHMUSTEXIST, SIGDN_FILESYSPATH,
    };

    fn window(owner: Option<isize>) -> Option<HWND> {
        owner.map(|raw| HWND(raw as *mut c_void))
    }

    /// Runs `work` on a fresh thread with COM single-threaded, as the shell wants.
    fn on_sta<T: Send + 'static>(
        work: impl FnOnce() -> Result<T, String> + Send + 'static,
    ) -> Result<T, String> {
        std::thread::spawn(move || {
            // SAFETY: balanced by CoUninitialize below, on this thread.
            unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) }
                .ok()
                .map_err(|e| e.to_string())?;
            let result = work();
            // SAFETY: COM was initialised above on this thread.
            unsafe { CoUninitialize() };
            result
        })
        .join()
        .map_err(|_| "the shell thread failed".to_owned())?
    }

    pub fn recycle(paths: Vec<PathBuf>, owner: Option<isize>) -> Result<bool, String> {
        on_sta(move || {
            // SAFETY: COM is initialised on this thread; every pointer outlives its call.
            unsafe {
                let operation: IFileOperation =
                    CoCreateInstance(&FileOperation, None, CLSCTX_INPROC_SERVER)
                        .map_err(|e| e.to_string())?;
                operation
                    .SetOperationFlags(
                        FOF_ALLOWUNDO
                            | FOFX_RECYCLEONDELETE
                            | FOF_NOCONFIRMATION
                            | FOF_WANTNUKEWARNING
                            | FOF_SILENT,
                    )
                    .map_err(|e| e.to_string())?;
                if let Some(owner) = window(owner) {
                    let _ = operation.SetOwnerWindow(owner);
                }
                for path in &paths {
                    let item: IShellItem =
                        SHCreateItemFromParsingName(&HSTRING::from(path.as_os_str()), None)
                            .map_err(|e| format!("{}: {e}", path.display()))?;
                    operation
                        .DeleteItem(&item, None)
                        .map_err(|e| e.to_string())?;
                }
                match operation.PerformOperations() {
                    Ok(()) => {}
                    Err(e) if e.code() == ERROR_CANCELLED.to_hresult() => return Ok(false),
                    Err(e) => return Err(e.to_string()),
                }
                let aborted = operation
                    .GetAnyOperationsAborted()
                    .map_err(|e| e.to_string())?;
                Ok(!aborted.as_bool())
            }
        })
    }

    pub fn copy_files(list: Vec<u8>, owner: Option<isize>) -> Result<(), String> {
        on_sta(move || {
            // SAFETY: the memory handed to SetClipboardData is the system's afterwards; on
            // failure it is freed here. The clipboard is closed on every path.
            unsafe {
                let files = global(&list)?;
                let effect = global(&DROPEFFECT_COPY.0.to_le_bytes())?;
                if let Err(e) = OpenClipboard(window(owner)) {
                    let _ = GlobalFree(Some(files));
                    let _ = GlobalFree(Some(effect));
                    return Err(e.to_string());
                }
                let result = (|| {
                    EmptyClipboard().map_err(|e| e.to_string())?;
                    SetClipboardData(u32::from(CF_HDROP.0), Some(HANDLE(files.0)))
                        .map_err(|e| e.to_string())?;
                    // Paste copies (rather than moves) the file.
                    let format = RegisterClipboardFormatW(w!("Preferred DropEffect"));
                    if format == 0 || SetClipboardData(format, Some(HANDLE(effect.0))).is_err() {
                        let _ = GlobalFree(Some(effect));
                    }
                    Ok(())
                })();
                if result.is_err() {
                    let _ = GlobalFree(Some(files));
                    let _ = GlobalFree(Some(effect));
                }
                let _ = CloseClipboard();
                result
            }
        })
    }

    /// Movable global memory holding `bytes`.
    unsafe fn global(bytes: &[u8]) -> Result<HGLOBAL, String> {
        let memory = GlobalAlloc(GMEM_MOVEABLE, bytes.len()).map_err(|e| e.to_string())?;
        let at = GlobalLock(memory) as *mut u8;
        if at.is_null() {
            let _ = GlobalFree(Some(memory));
            return Err("could not lock clipboard memory".into());
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), at, bytes.len());
        let _ = GlobalUnlock(memory);
        Ok(memory)
    }

    pub fn save_as(owner: Option<isize>, name: String) -> Result<Option<PathBuf>, String> {
        on_sta(move || {
            // SAFETY: COM is initialised on this thread; the dialog's strings outlive it.
            unsafe {
                let dialog: IFileSaveDialog =
                    CoCreateInstance(&FileSaveDialog, None, CLSCTX_INPROC_SERVER)
                        .map_err(|e| e.to_string())?;
                let options = dialog.GetOptions().map_err(|e| e.to_string())?;
                dialog
                    .SetOptions(
                        options | FOS_OVERWRITEPROMPT | FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST,
                    )
                    .map_err(|e| e.to_string())?;
                let types = [COMDLG_FILTERSPEC {
                    pszName: w!("MP4 video"),
                    pszSpec: w!("*.mp4"),
                }];
                dialog.SetFileTypes(&types).map_err(|e| e.to_string())?;
                dialog
                    .SetDefaultExtension(w!("mp4"))
                    .map_err(|e| e.to_string())?;
                dialog
                    .SetTitle(w!("Save clip as"))
                    .map_err(|e| e.to_string())?;
                dialog
                    .SetFileName(&HSTRING::from(name.as_str()))
                    .map_err(|e| e.to_string())?;
                match dialog.Show(window(owner)) {
                    Ok(()) => {}
                    Err(e) if e.code() == ERROR_CANCELLED.to_hresult() => return Ok(None),
                    Err(e) => return Err(e.to_string()),
                }
                let item = dialog.GetResult().map_err(|e| e.to_string())?;
                let raw = item
                    .GetDisplayName(SIGDN_FILESYSPATH)
                    .map_err(|e| e.to_string())?;
                let path = raw.to_string().map_err(|e| e.to_string());
                CoTaskMemFree(Some(raw.0 as *const c_void));
                Ok(Some(PathBuf::from(path?)))
            }
        })
    }
}

#[cfg(not(windows))]
mod imp {
    use std::path::PathBuf;

    const UNSUPPORTED: &str = "only supported on Windows";

    pub fn recycle(_paths: Vec<PathBuf>, _owner: Option<isize>) -> Result<bool, String> {
        Err(UNSUPPORTED.into())
    }

    pub fn copy_files(_list: Vec<u8>, _owner: Option<isize>) -> Result<(), String> {
        Err(UNSUPPORTED.into())
    }

    pub fn save_as(_owner: Option<isize>, _name: String) -> Result<Option<PathBuf>, String> {
        Err(UNSUPPORTED.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_clipboard_file_list_is_a_dropfiles_header_and_wide_paths() {
        let paths = [
            PathBuf::from(r"C:\lib\m1\clips\0001_kill.mp4"),
            PathBuf::from(r"D:\Clips\é.mp4"),
        ];
        let bytes = drop_files(&paths);
        let word = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
        assert_eq!(word(0), 20, "the list starts after the header");
        assert_eq!(
            (word(4), word(8), word(12)),
            (0, 0, 0),
            "no drop point, client area"
        );
        assert_eq!(word(16), 1, "wide (UTF-16) paths");
        let units: Vec<u16> = bytes[20..]
            .chunks(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        let text = String::from_utf16(&units).unwrap();
        assert_eq!(
            text, "C:\\lib\\m1\\clips\\0001_kill.mp4\0D:\\Clips\\é.mp4\0\0",
            "each path ends with a NUL, the list with one more"
        );
    }
}
