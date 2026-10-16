//! Which monitor to record: the one League's game window is on (doc 03 §5), as the
//! DXGI adapter + output pair `ddagrab` needs. Desktop Duplication has to run on the
//! adapter that drives the monitor, and this machine has two GPUs.
//!
//! The game window is found by its owner, `League of Legends.exe`: window enumeration
//! reads only window ownership, never the (Vanguard-protected) game process itself.

use serde::Serialize;

use crate::league::client::GAME_PROCESS;

/// One monitor, as DXGI enumerates it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Display {
    /// DXGI adapter index (ffmpeg's `d3d11va` device).
    pub adapter: u32,
    /// Output index on that adapter (ddagrab's `output_idx`).
    pub output: u32,
    pub adapter_name: String,
    /// `\\.\DISPLAY1` and so on.
    pub device_name: String,
    pub width: u32,
    pub height: u32,
}

/// The monitor League's game window is on, if the game window exists.
pub fn game_display() -> Option<Display> {
    let pids = game_pids();
    if pids.is_empty() {
        return None;
    }
    imp::display_of_window_owned_by(&pids)
}

/// Process IDs of running `League of Legends.exe` (names only, like the tracker's scan).
fn game_pids() -> Vec<u32> {
    use std::ffi::OsStr;
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};

    let mut system = System::new();
    system.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing());
    system
        .processes()
        .values()
        .filter(|p| p.name().eq_ignore_ascii_case(OsStr::new(GAME_PROCESS)))
        .map(|p| p.pid().as_u32())
        .collect()
}

/// The primary monitor (fallback when the game window never shows up).
pub fn primary_display() -> Option<Display> {
    imp::primary_display()
}

/// Every monitor attached to the desktop.
pub fn displays() -> Vec<Display> {
    imp::displays()
        .into_iter()
        .map(|(display, _)| display)
        .collect()
}

#[cfg(windows)]
mod imp {
    use windows::core::BOOL;
    use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT};
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1};
    use windows::Win32::Graphics::Gdi::{
        MonitorFromPoint, MonitorFromWindow, HMONITOR, MONITOR_DEFAULTTONEAREST,
        MONITOR_DEFAULTTOPRIMARY,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowRect, GetWindowThreadProcessId, IsWindowVisible,
    };

    use super::Display;

    fn wide(text: &[u16]) -> String {
        let end = text.iter().position(|&c| c == 0).unwrap_or(text.len());
        String::from_utf16_lossy(&text[..end]).trim().to_owned()
    }

    pub fn displays() -> Vec<(Display, HMONITOR)> {
        let mut found = Vec::new();
        // SAFETY: COM calls on interfaces DXGI hands out; every out-value is checked.
        unsafe {
            let Ok(factory) = CreateDXGIFactory1::<IDXGIFactory1>() else {
                return found;
            };
            for adapter_index in 0.. {
                let Ok(adapter) = factory.EnumAdapters1(adapter_index) else {
                    break;
                };
                let adapter_name = adapter
                    .GetDesc1()
                    .map(|d| wide(&d.Description))
                    .unwrap_or_default();
                for output_index in 0.. {
                    let Ok(output) = adapter.EnumOutputs(output_index) else {
                        break;
                    };
                    let Ok(desc) = output.GetDesc() else {
                        continue;
                    };
                    if !desc.AttachedToDesktop.as_bool() {
                        continue;
                    }
                    let rect = desc.DesktopCoordinates;
                    found.push((
                        Display {
                            adapter: adapter_index,
                            output: output_index,
                            adapter_name: adapter_name.clone(),
                            device_name: wide(&desc.DeviceName),
                            width: rect.right.abs_diff(rect.left),
                            height: rect.bottom.abs_diff(rect.top),
                        },
                        desc.Monitor,
                    ));
                }
            }
        }
        found
    }

    fn on_monitor(monitor: HMONITOR) -> Option<Display> {
        if monitor.is_invalid() {
            return None;
        }
        displays()
            .into_iter()
            .find(|(_, m)| *m == monitor)
            .map(|(display, _)| display)
    }

    /// The largest visible top-level window of these processes.
    struct Search<'a> {
        pids: &'a [u32],
        best: Option<(HWND, i64)>,
    }

    unsafe extern "system" fn visit(window: HWND, context: LPARAM) -> BOOL {
        // SAFETY: `context` is the `Search` that `display_of_window_owned_by` passes and
        // outlives the enumeration.
        let search = unsafe { &mut *(context.0 as *mut Search) };
        let mut pid = 0;
        // SAFETY: window queries on a handle EnumWindows just gave us.
        unsafe {
            GetWindowThreadProcessId(window, Some(std::ptr::from_mut(&mut pid)));
            if search.pids.contains(&pid) && IsWindowVisible(window).as_bool() {
                let mut rect = RECT::default();
                if GetWindowRect(window, &mut rect).is_ok() {
                    let area =
                        i64::from(rect.right - rect.left) * i64::from(rect.bottom - rect.top);
                    if search.best.is_none_or(|(_, best)| area > best) {
                        search.best = Some((window, area));
                    }
                }
            }
        }
        BOOL(1)
    }

    pub fn display_of_window_owned_by(pids: &[u32]) -> Option<Display> {
        let mut search = Search { pids, best: None };
        // SAFETY: the callback only uses `search`, which lives across the call.
        let monitor = unsafe {
            EnumWindows(Some(visit), LPARAM(&mut search as *mut Search as isize)).ok()?;
            MonitorFromWindow(search.best?.0, MONITOR_DEFAULTTONEAREST)
        };
        on_monitor(monitor)
    }

    pub fn primary_display() -> Option<Display> {
        // SAFETY: no pointers involved.
        let monitor = unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) };
        on_monitor(monitor)
    }
}

#[cfg(not(windows))]
mod imp {
    use super::Display;

    pub fn displays() -> Vec<(Display, ())> {
        Vec::new()
    }

    pub fn display_of_window_owned_by(_pids: &[u32]) -> Option<Display> {
        None
    }

    pub fn primary_display() -> Option<Display> {
        None
    }
}

#[cfg(all(test, windows))]
mod tests {
    use std::ffi::OsStr;

    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};

    use super::*;

    #[test]
    fn a_processs_window_maps_to_a_dxgi_monitor() {
        // Explorer owns the taskbar: a visible window on a monitor, as League's would be.
        let mut system = System::new();
        system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing(),
        );
        let explorer: Vec<u32> = system
            .processes()
            .values()
            .filter(|p| p.name().eq_ignore_ascii_case(OsStr::new("explorer.exe")))
            .map(|p| p.pid().as_u32())
            .collect();
        let monitors = displays();
        if explorer.is_empty() || monitors.is_empty() {
            return; // no desktop session (e.g. a service account)
        }
        let found = imp::display_of_window_owned_by(&explorer).expect("the taskbar's monitor");
        assert!(monitors.contains(&found));
        assert!(found.width > 0 && found.height > 0);
        assert!(imp::display_of_window_owned_by(&[u32::MAX]).is_none());
    }
}
