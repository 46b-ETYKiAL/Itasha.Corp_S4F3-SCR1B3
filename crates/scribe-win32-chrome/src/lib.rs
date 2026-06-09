//! Isolated Win32 window-chrome fixups.
//!
//! This is the ONLY crate in the SCR1B3 tree permitted `unsafe` — mirroring
//! scribe-core's single mmap exception. It quarantines three audited Win32 calls
//! that fix the "doubled caption buttons" bug:
//!
//! winit keeps `WS_SYSMENU | WS_MINIMIZEBOX | WS_MAXIMIZEBOX` on undecorated
//! **top-level** windows (the only branch that strips caption bits is gated on
//! `WS_CHILD`; see winit #2754). On Windows 11, DWM paints the three native
//! caption buttons from those residual style bits — in the composited non-client
//! band, OVER the app's custom titlebar. Removing the DWM backdrop did nothing
//! because the backdrop was never the cause; the per-frame
//! `ViewportCommand::Decorations(false)` re-assert was also a no-op (it only
//! re-clears winit's decorations marker, never the style bits). The fix is to
//! clear those bits on the HWND.

/// Strip the residual native caption buttons from THIS process's main top-level
/// window so Windows stops painting them over the custom titlebar. Windows-only;
/// a no-op everywhere else.
///
/// Safe + cheap to call every frame: the HWND is discovered once and cached, and
/// the style change is only issued when the bits are actually present (e.g. after
/// a maximize re-applies winit's window styles), so the steady-state cost is a
/// single `GetWindowLongPtrW` read.
#[cfg(windows)]
pub fn ensure_caption_stripped() {
    imp::ensure_caption_stripped();
}

/// No-op on non-Windows platforms (the bug is Windows-DWM-specific).
#[cfg(not(windows))]
pub fn ensure_caption_stripped() {}

#[cfg(windows)]
mod imp {
    use std::sync::atomic::{AtomicIsize, Ordering};
    use windows_sys::Win32::Foundation::{BOOL, HWND, LPARAM};
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowLongPtrW, GetWindowThreadProcessId, IsWindowVisible,
        SetWindowLongPtrW, SetWindowPos, GWL_STYLE, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE,
        SWP_NOSIZE, SWP_NOZORDER, WS_MAXIMIZEBOX, WS_MINIMIZEBOX, WS_SYSMENU,
    };

    /// Cached main-window HWND (0 = not yet found). One window per process.
    static CACHED_HWND: AtomicIsize = AtomicIsize::new(0);

    /// `EnumWindows` callback: record the first visible top-level window owned by
    /// this process into the `*mut isize` passed via `lparam`, then stop.
    unsafe extern "system" fn enum_cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, &mut pid);
        if pid == GetCurrentProcessId() && IsWindowVisible(hwnd) != 0 {
            *(lparam as *mut isize) = hwnd as isize;
            return 0; // FALSE → stop enumerating
        }
        1 // TRUE → keep going
    }

    fn find_main_window() -> isize {
        let mut found: isize = 0;
        // SAFETY: `enum_cb` only writes the `isize` behind `lparam` (a stack local
        // that outlives the synchronous EnumWindows call) and reads OS-owned HWNDs.
        unsafe {
            EnumWindows(Some(enum_cb), (&mut found as *mut isize) as LPARAM);
        }
        found
    }

    fn strip(hwnd: isize) {
        let bits = (WS_SYSMENU | WS_MINIMIZEBOX | WS_MAXIMIZEBOX) as isize;
        // SAFETY: `hwnd` is an OS window handle owned by this process; these are
        // the canonical style-query / style-set / frame-recalc calls.
        unsafe {
            let h = hwnd as HWND;
            let style = GetWindowLongPtrW(h, GWL_STYLE);
            if style & bits != 0 {
                SetWindowLongPtrW(h, GWL_STYLE, style & !bits);
                SetWindowPos(
                    h,
                    std::ptr::null_mut(),
                    0,
                    0,
                    0,
                    0,
                    SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
        }
    }

    pub fn ensure_caption_stripped() {
        let mut hwnd = CACHED_HWND.load(Ordering::Relaxed);
        if hwnd == 0 {
            hwnd = find_main_window();
            CACHED_HWND.store(hwnd, Ordering::Relaxed);
        }
        if hwnd != 0 {
            strip(hwnd);
        }
    }
}
