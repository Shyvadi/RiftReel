//! Windows shell actions on library files (doc 03 §8–9): send to the Recycle Bin (asking
//! first about a file that can't be recycled, for the window's buttons; never deleting
//! it, for the cleanup), put on the clipboard as a file, "Save as…", show in Explorer.
//! Each runs on a thread of its own with COM in a single-threaded apartment, owned by the
//! window that asked (`owner`, a raw window handle) if any, and returns when the shell is
//! done.

use std::path::{Path, PathBuf};

pub use crate::library::janitor::Recycled;

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

/// Sends one file to the Recycle Bin with no window and no question, for the cleanup,
/// which runs unattended. Unlike [`recycle`] nothing is ever asked: a file Windows can't
/// recycle is kept ([`Recycled::NotRecyclable`]), never deleted.
pub fn recycle_strict(path: &Path) -> Recycled {
    imp::recycle_strict(path.to_owned())
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
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    use windows::core::{implement, w, Ref, HRESULT, HSTRING, PCWSTR};
    use windows::Win32::Foundation::{GlobalFree, ERROR_CANCELLED, E_ABORT, HANDLE, HGLOBAL, HWND};
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
        FileOperation, FileSaveDialog, IFileOperation, IFileOperationProgressSink,
        IFileOperationProgressSink_Impl, IFileSaveDialog, IShellItem, SHCreateItemFromParsingName,
        FOFX_EARLYFAILURE, FOFX_RECYCLEONDELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NO_UI,
        FOF_SILENT, FOF_WANTNUKEWARNING, FOS_FORCEFILESYSTEM, FOS_OVERWRITEPROMPT,
        FOS_PATHMUSTEXIST, SIGDN_FILESYSPATH, TSF_DELETE_RECYCLE_IF_POSSIBLE,
    };

    use super::Recycled;

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

    /// What the shell told [`RecycleOnly`] about its one item.
    #[derive(Default)]
    struct DeleteSeen {
        /// The shell was about to delete it for good, and was stopped.
        refused: AtomicBool,
        /// It is in the Recycle Bin.
        recycled: AtomicBool,
    }

    /// Stops any delete that wouldn't go to the Recycle Bin: the shell leaves
    /// `TSF_DELETE_RECYCLE_IF_POSSIBLE` out of the flags of an item it is about to destroy
    /// (too big for the bin, a drive without one), and an error from `PreDeleteItem`
    /// cancels it.
    #[implement(IFileOperationProgressSink)]
    struct RecycleOnly(Arc<DeleteSeen>);

    impl IFileOperationProgressSink_Impl for RecycleOnly_Impl {
        fn PreDeleteItem(&self, flags: u32, _item: Ref<IShellItem>) -> windows::core::Result<()> {
            if flags & TSF_DELETE_RECYCLE_IF_POSSIBLE.0 as u32 == 0 {
                self.0.refused.store(true, Ordering::SeqCst);
                return Err(E_ABORT.into());
            }
            Ok(())
        }

        fn PostDeleteItem(
            &self,
            _flags: u32,
            _item: Ref<IShellItem>,
            result: HRESULT,
            in_bin: Ref<IShellItem>,
        ) -> windows::core::Result<()> {
            // The new item is the file's place in the Recycle Bin.
            if result.is_ok() && !in_bin.is_null() {
                self.0.recycled.store(true, Ordering::SeqCst);
            }
            Ok(())
        }

        fn StartOperations(&self) -> windows::core::Result<()> {
            Ok(())
        }
        fn FinishOperations(&self, _result: HRESULT) -> windows::core::Result<()> {
            Ok(())
        }
        fn PreRenameItem(
            &self,
            _flags: u32,
            _item: Ref<IShellItem>,
            _name: &PCWSTR,
        ) -> windows::core::Result<()> {
            Ok(())
        }
        fn PostRenameItem(
            &self,
            _flags: u32,
            _item: Ref<IShellItem>,
            _name: &PCWSTR,
            _result: HRESULT,
            _created: Ref<IShellItem>,
        ) -> windows::core::Result<()> {
            Ok(())
        }
        fn PreMoveItem(
            &self,
            _flags: u32,
            _item: Ref<IShellItem>,
            _folder: Ref<IShellItem>,
            _name: &PCWSTR,
        ) -> windows::core::Result<()> {
            Ok(())
        }
        fn PostMoveItem(
            &self,
            _flags: u32,
            _item: Ref<IShellItem>,
            _folder: Ref<IShellItem>,
            _name: &PCWSTR,
            _result: HRESULT,
            _created: Ref<IShellItem>,
        ) -> windows::core::Result<()> {
            Ok(())
        }
        fn PreCopyItem(
            &self,
            _flags: u32,
            _item: Ref<IShellItem>,
            _folder: Ref<IShellItem>,
            _name: &PCWSTR,
        ) -> windows::core::Result<()> {
            Ok(())
        }
        fn PostCopyItem(
            &self,
            _flags: u32,
            _item: Ref<IShellItem>,
            _folder: Ref<IShellItem>,
            _name: &PCWSTR,
            _result: HRESULT,
            _created: Ref<IShellItem>,
        ) -> windows::core::Result<()> {
            Ok(())
        }
        fn PreNewItem(
            &self,
            _flags: u32,
            _folder: Ref<IShellItem>,
            _name: &PCWSTR,
        ) -> windows::core::Result<()> {
            Ok(())
        }
        fn PostNewItem(
            &self,
            _flags: u32,
            _folder: Ref<IShellItem>,
            _name: &PCWSTR,
            _template: &PCWSTR,
            _attributes: u32,
            _result: HRESULT,
            _item: Ref<IShellItem>,
        ) -> windows::core::Result<()> {
            Ok(())
        }
        fn UpdateProgress(&self, _total: u32, _done: u32) -> windows::core::Result<()> {
            Ok(())
        }
        fn ResetTimer(&self) -> windows::core::Result<()> {
            Ok(())
        }
        fn PauseTimer(&self) -> windows::core::Result<()> {
            Ok(())
        }
        fn ResumeTimer(&self) -> windows::core::Result<()> {
            Ok(())
        }
    }

    pub fn recycle_strict(path: PathBuf) -> Recycled {
        let seen = Arc::new(DeleteSeen::default());
        let watched = seen.clone();
        let target = path.clone();
        let performed = on_sta(move || {
            // SAFETY: COM is initialised on this thread; every pointer outlives its call.
            unsafe {
                let operation: IFileOperation =
                    CoCreateInstance(&FileOperation, None, CLSCTX_INPROC_SERVER)
                        .map_err(|e| e.to_string())?;
                // No window, no question, no error box; stop at the first failure.
                operation
                    .SetOperationFlags(
                        FOF_ALLOWUNDO | FOFX_RECYCLEONDELETE | FOF_NO_UI | FOFX_EARLYFAILURE,
                    )
                    .map_err(|e| e.to_string())?;
                let item: IShellItem =
                    SHCreateItemFromParsingName(&HSTRING::from(target.as_os_str()), None)
                        .map_err(|e| e.to_string())?;
                let sink: IFileOperationProgressSink = RecycleOnly(watched).into();
                operation
                    .DeleteItem(&item, &sink)
                    .map_err(|e| e.to_string())?;
                let result = operation.PerformOperations();
                let aborted = operation
                    .GetAnyOperationsAborted()
                    .map_or(true, |aborted| aborted.as_bool());
                match result {
                    Ok(()) if !aborted => Ok(()),
                    Ok(()) => Err("the Recycle Bin operation was stopped".to_owned()),
                    Err(e) => Err(e.to_string()),
                }
            }
        });
        if seen.refused.load(Ordering::SeqCst) {
            return Recycled::NotRecyclable;
        }
        match performed {
            Err(error) => Recycled::Failed(error),
            Ok(()) if path.exists() => Recycled::Failed("the file is still there".into()),
            Ok(()) => {
                if !seen.recycled.load(Ordering::SeqCst) {
                    tracing::error!("the shell reported no Recycle Bin item for a removed file");
                }
                Recycled::Done
            }
        }
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

    use super::Recycled;

    const UNSUPPORTED: &str = "only supported on Windows";

    pub fn recycle(_paths: Vec<PathBuf>, _owner: Option<isize>) -> Result<bool, String> {
        Err(UNSUPPORTED.into())
    }

    pub fn recycle_strict(_path: PathBuf) -> Recycled {
        Recycled::Failed(UNSUPPORTED.into())
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
