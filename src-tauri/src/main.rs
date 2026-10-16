// Never a console window, debug builds included: RiftReel lives in the tray.
#![windows_subsystem = "windows"]

fn main() {
    #[cfg(all(windows, debug_assertions))]
    attach_parent_console();
    riftreel_lib::run();
}

/// A debug build started from a terminal prints its logs there; started any other way
/// (Explorer, a shortcut, another program) it has no console at all.
#[cfg(all(windows, debug_assertions))]
fn attach_parent_console() {
    const ATTACH_PARENT_PROCESS: u32 = u32::MAX;
    #[link(name = "kernel32")]
    extern "system" {
        fn AttachConsole(process_id: u32) -> i32;
    }
    // SAFETY: no pointers are passed; failing (no parent console) is expected and fine.
    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}
