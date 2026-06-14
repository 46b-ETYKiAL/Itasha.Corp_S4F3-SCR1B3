//! Isolated Win32 window-chrome fixups.
//!
//! This is the ONLY crate in the SCR1B3 tree permitted `unsafe` — mirroring
//! scribe-core's single mmap exception. It quarantines the audited Win32 FFI
//! that removes the "doubled caption buttons" the OS draws over our custom
//! titlebar on a frameless + transparent window.
//!
//! ## Why the previous approaches failed
//!
//! winit leaves `WS_SYSMENU | WS_MINIMIZEBOX | WS_MAXIMIZEBOX` on undecorated
//! TOP-LEVEL windows (it only strips caption bits on `WS_CHILD`; winit #2754).
//! Two earlier fixes were the WRONG mechanism:
//!
//!   1. **Stripping those `WS_*` style bits.** DWM draws the caption buttons
//!      from the window's NON-CLIENT (NC) frame GEOMETRY, not purely from the
//!      style bits — so clearing the bits did not remove the buttons.
//!   2. **`DWMWA_NCRENDERING_POLICY = DWMNCRP_DISABLED`.** winit implements
//!      Windows transparency via `DwmEnableBlurBehindWindow`, and per Microsoft
//!      that path is *mutually exclusive* with disabling NC rendering — so the
//!      per-frame call fought winit's own transparency and never removed the
//!      buttons. (It only LOOKED inert when opaque because the solid panel fill
//!      covered the always-present NC strip; lowering the alpha unmasked it —
//!      the "doubled min/max/close with transparency on" report.)
//!
//! ## The fix (production-proven: MS DWM custom-frame sample, BorderlessWindow,
//! Tao)
//!
//! Subclass the HWND and return **0** from `WM_NCCALCSIZE` when `wParam == TRUE`.
//! Per Microsoft's "Custom Window Frame Using DWM": returning 0 makes the entire
//! window the client area, "removing the standard frame … this includes the
//! region where the caption buttons are drawn." With no NC strip, DWM has
//! nowhere to draw the system min/max/close — in opaque AND transparent modes.
//!
//! This is safe in SCR1B3 specifically because resize and drag are already
//! egui-owned (a `ViewportCommand::BeginResize` edge overlay — winit #4186 — and
//! `ViewportCommand::StartDrag`), so the NC area is unused for them. The one
//! thing the NC calc still owes us is a MAXIMIZED window that respects the
//! monitor work area (and an auto-hide taskbar), which the maximized branch
//! handles by clamping the client rect to `rcWork`.

/// Ensure THIS process's main top-level window draws no system caption buttons
/// over the custom titlebar, by installing a one-time `WM_NCCALCSIZE` subclass
/// that turns the whole window into client area. Windows-only; a no-op
/// everywhere else.
///
/// Safe + cheap to call every frame: the HWND is discovered once and cached, and
/// the subclass is installed exactly once (subsequent calls are a single relaxed
/// atomic load).
#[cfg(windows)]
pub fn ensure_caption_stripped() {
    imp::ensure_borderless();
}

/// No-op on non-Windows platforms (the bug is Windows-DWM-specific).
#[cfg(not(windows))]
pub fn ensure_caption_stripped() {}

#[cfg(windows)]
mod imp {
    use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

    use windows_sys::Win32::Foundation::{BOOL, HWND, LPARAM, LRESULT, RECT, WPARAM};
    use windows_sys::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;
    use windows_sys::Win32::UI::Shell::{
        DefSubclassProc, SHAppBarMessage, SetWindowSubclass, ABM_GETSTATE, ABS_AUTOHIDE, APPBARDATA,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowLongPtrW, GetWindowThreadProcessId, IsWindowVisible, SetWindowPos,
        GWL_STYLE, NCCALCSIZE_PARAMS, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
        SWP_NOZORDER, WM_NCCALCSIZE, WS_MAXIMIZE,
    };

    /// Cached main-window HWND (0 = not yet found). One window per process.
    static CACHED_HWND: AtomicIsize = AtomicIsize::new(0);
    /// Set once the NC subclass is successfully installed (install is one-shot).
    static SUBCLASSED: AtomicBool = AtomicBool::new(false);

    /// A stable, arbitrary subclass id for our single subclass entry.
    const SUBCLASS_ID: usize = 0x5C_1B_3E;

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

    /// Whether the window is currently maximized (its `WS_MAXIMIZE` style is set).
    fn is_maximized(hwnd: HWND) -> bool {
        // SAFETY: `hwnd` is an OS window owned by this process; GWL_STYLE read.
        let style = unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) } as u32;
        style & WS_MAXIMIZE != 0
    }

    /// The work area (screen minus taskbar) of the monitor the window is on.
    fn monitor_work_area(hwnd: HWND) -> Option<RECT> {
        // SAFETY: canonical monitor-info query; `mi.cbSize` set before the call.
        unsafe {
            let mon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
            if mon.is_null() {
                return None;
            }
            let mut mi: MONITORINFO = std::mem::zeroed();
            mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
            if GetMonitorInfoW(mon, &mut mi) != 0 {
                Some(mi.rcWork)
            } else {
                None
            }
        }
    }

    /// Whether any taskbar is in auto-hide mode. A borderless window that
    /// maximally covers an auto-hide taskbar's edge prevents it from popping up;
    /// the caller insets that edge by 1px to keep it reachable.
    fn taskbar_is_autohide() -> bool {
        // SAFETY: canonical app-bar state query; `cbSize` set before the call.
        unsafe {
            let mut abd: APPBARDATA = std::mem::zeroed();
            abd.cbSize = std::mem::size_of::<APPBARDATA>() as u32;
            let state = SHAppBarMessage(ABM_GETSTATE, &mut abd) as u32;
            state & ABS_AUTOHIDE != 0
        }
    }

    /// The `WM_NCCALCSIZE` subclass: turn the whole window into client area so
    /// the OS reserves no non-client strip (hence draws no caption buttons),
    /// while keeping a maximized window inside the monitor work area.
    unsafe extern "system" fn nc_subclass_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        _id: usize,
        _ref: usize,
    ) -> LRESULT {
        if msg == WM_NCCALCSIZE && wparam != 0 {
            // wParam == TRUE: `lparam` is `*mut NCCALCSIZE_PARAMS`. Returning 0
            // with `rgrc[0]` (the proposed client rect) left as the full window
            // rect makes the entire window client area → no NC caption strip →
            // no system min/max/close, opaque or transparent.
            if is_maximized(hwnd) {
                // A borderless maximize would otherwise cover the taskbar. Clamp
                // the client rect to the monitor work area.
                if let Some(mut work) = monitor_work_area(hwnd) {
                    if taskbar_is_autohide() {
                        // Leave a 1px sliver on the bottom so an auto-hide
                        // taskbar (the common edge) can still pop up.
                        work.bottom -= 1;
                    }
                    let params = lparam as *mut NCCALCSIZE_PARAMS;
                    (*params).rgrc[0] = work;
                }
            }
            return 0;
        }
        DefSubclassProc(hwnd, msg, wparam, lparam)
    }

    /// Install the NC subclass on `hwnd` and force a frame recalculation so the
    /// new (zero) non-client area takes effect immediately. Returns whether the
    /// subclass was installed.
    fn install_nc_subclass(hwnd: isize) -> bool {
        // SAFETY: `hwnd` is an OS window owned by this process; this is the
        // canonical comctl32 subclass install + a frame-changed re-layout.
        unsafe {
            let h = hwnd as HWND;
            let ok = SetWindowSubclass(h, Some(nc_subclass_proc), SUBCLASS_ID, 0) != 0;
            if ok {
                // "The new client area is not visible until the client region
                // needs to be resized" — trigger it once.
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
            ok
        }
    }

    pub fn ensure_borderless() {
        let mut hwnd = CACHED_HWND.load(Ordering::Relaxed);
        if hwnd == 0 {
            hwnd = find_main_window();
            CACHED_HWND.store(hwnd, Ordering::Relaxed);
        }
        // Install the subclass exactly once (early frames may run before the
        // window exists; we retry each frame until it does, then stop).
        if hwnd != 0 && !SUBCLASSED.load(Ordering::Relaxed) && install_nc_subclass(hwnd) {
            SUBCLASSED.store(true, Ordering::Relaxed);
        }
    }
}
