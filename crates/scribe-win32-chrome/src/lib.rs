//! Isolated Win32 window-chrome fixups.
//!
//! This is the ONLY crate in the SCR1B3 tree permitted `unsafe` — mirroring
//! scribe-core's single mmap exception. It quarantines the audited Win32 FFI
//! that removes the "doubled caption buttons" the OS draws over our custom
//! titlebar on a frameless + transparent window.
//!
//! ## Root cause (deep-researched, primary-sourced)
//!
//! winit leaves `WS_SYSMENU | WS_MINIMIZEBOX | WS_MAXIMIZEBOX` set on an
//! UNDECORATED top-level window — it strips only `WS_CAPTION`/`WS_SIZEBOX`
//! (winit #2754). On Win11 DWM draws the native min/max/close caption buttons
//! GATED ON THOSE STYLE BITS. winit's transparency is `DwmEnableBlurBehindWindow`
//! (a DWM-COMPOSITED frame), so it does not CREATE the buttons — it UNMASKS the
//! DWM-drawn buttons the opaque panel was hiding ("doubled buttons with
//! transparency on"; opaque pixels hid them before).
//!
//! ## Why the earlier `WM_NCCALCSIZE` fix could never work
//!
//! Returning 0 from `WM_NCCALCSIZE` removes the STANDARD non-client frame, but
//! Microsoft documents that it "does not affect frames that are extended into
//! the client area" / DWM-composited content. The caption buttons are
//! DWM-composited, so `WM_NCCALCSIZE` is STRUCTURALLY incapable of removing
//! them. (`DWMWA_NCRENDERING_POLICY = DWMNCRP_DISABLED` is also wrong — it fights
//! winit's `DwmEnableBlurBehindWindow` transparency.)
//!
//! ## The fix (canonical: melak47/BorderlessWindow, MS DWM sample, Tao/Tauri,
//! Electron)
//!
//! CLEAR the caption-button style bits with `SetWindowLongPtrW(GWL_STYLE, …)` +
//! `SetWindowPos(SWP_FRAMECHANGED)` — see [`imp::CAPTION_BUTTON_STYLES`]. With
//! the bits gone, DWM draws no native buttons, in opaque OR transparent mode,
//! and winit's transparency is left untouched. The strip is re-applied every
//! frame because winit re-derives styles from its `WindowFlags` on some
//! resize/restore paths (cheap: it only writes when a bit is actually present).
//!
//! The `WM_NCCALCSIZE` subclass is RETAINED, but ONLY for its other job: clamping
//! a borderless MAXIMIZE to the monitor work area (and an auto-hide taskbar) so
//! it doesn't cover the taskbar. Resize and drag are egui-owned
//! (`ViewportCommand::BeginResize` — winit #4186 — and `StartDrag`), so the NC
//! area is otherwise unused.
//!
//! Trade-off of clearing `WS_SYSMENU`: Alt+Space and the taskbar right-click
//! system menu go away; the custom titlebar already provides min/max/close.

/// Ensure THIS process's main top-level window draws no system caption buttons
/// over the custom titlebar, by clearing the caption-button window styles
/// (`WS_SYSMENU|WS_MINIMIZEBOX|WS_MAXIMIZEBOX`) winit leaves on the undecorated
/// window, and installing a one-time `WM_NCCALCSIZE` subclass that clamps a
/// borderless maximize to the monitor work area. Windows-only; a no-op
/// everywhere else.
///
/// Safe + cheap to call every frame: the HWND is cached, the subclass installs
/// once, and the style-strip only writes when a caption-button bit is actually
/// present (so it self-heals if winit re-asserts the styles, at near-zero cost
/// otherwise).
#[cfg(windows)]
pub fn ensure_caption_stripped() {
    imp::ensure_borderless();
}

/// No-op on non-Windows platforms (the bug is Windows-DWM-specific).
#[cfg(not(windows))]
pub fn ensure_caption_stripped() {}

/// Prime the crate with the REAL native window handle (from eframe's
/// `Frame`/`HasWindowHandle`). This is the authoritative HWND of the app's
/// window; passing it here means [`ensure_caption_stripped`] subclasses the
/// CORRECT window instead of falling back to an `EnumWindows` guess (which could
/// latch onto the wrong top-level window — the likely reason earlier caption
/// fixes had no effect). Idempotent; call every frame. Windows-only.
#[cfg(windows)]
pub fn set_main_hwnd(hwnd: isize) {
    imp::set_main_hwnd(hwnd);
}

/// No-op on non-Windows platforms.
#[cfg(not(windows))]
pub fn set_main_hwnd(_hwnd: isize) {}

/// Hand THIS process's foreground right to any process about to be spawned, so a
/// just-launched child (the self-updater's relaunched binary, or the elevated
/// `setup.exe` started via PowerShell) is permitted to call
/// `SetForegroundWindow` on its own window and come to the FRONT instead of
/// flashing in the taskbar behind us.
///
/// ## Root cause (deep-researched, primary-sourced)
///
/// Windows enforces a foreground lock (`SPI_GETFOREGROUNDLOCKTIMEOUT`): a process
/// may set the foreground window only if it currently OWNS the foreground, or a
/// foreground-owning process called `AllowSetForegroundWindow` to delegate that
/// right to a target. When SCR1B3 spawns the installer/new binary it IS the
/// foreground process (the `ViewportCommand::Close` is queued, drained on the
/// next egui frame), but it never delegates the right — so the child's
/// `SetForegroundWindow` is silently demoted to a taskbar flash and the window
/// lands BEHIND. (MS docs: "SetForegroundWindow", "AllowSetForegroundWindow".)
///
/// Pass `ASFW_ANY` (`u32::MAX`), not a specific PID: on the elevated path the
/// real installer is a GRANDCHILD (PowerShell → UAC `consent.exe` → `setup.exe`),
/// so the PID SCR1B3 gets back from spawning `powershell` is not the installer's.
/// `ASFW_ANY` is the only grant that reaches it.
///
/// MUST be called BEFORE the spawn, while SCR1B3 still owns the foreground (the
/// API no-ops once the caller is no longer foreground). Windows-only; a no-op
/// everywhere else.
#[cfg(windows)]
pub fn allow_foreground_handoff() {
    imp::allow_foreground_handoff();
}

/// No-op on non-Windows platforms (the foreground-lock is Windows-specific).
#[cfg(not(windows))]
pub fn allow_foreground_handoff() {}

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
        AllowSetForegroundWindow, EnumWindows, GetClientRect, GetWindowLongPtrW, GetWindowRect,
        GetWindowThreadProcessId, IsWindowVisible, SetWindowLongPtrW, SetWindowPos, GWL_STYLE,
        NCCALCSIZE_PARAMS, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
        WM_NCCALCSIZE, WS_CAPTION, WS_MAXIMIZE, WS_MAXIMIZEBOX, WS_MINIMIZEBOX, WS_SYSMENU,
    };

    /// The window-style bits that make DWM draw the native min/max/close caption
    /// buttons. winit leaves `WS_SYSMENU | WS_MINIMIZEBOX | WS_MAXIMIZEBOX` set on
    /// an UNDECORATED window — it only strips `WS_CAPTION`/`WS_SIZEBOX` (winit
    /// #2754). On a transparent (DWM blur-behind) window those buttons are DWM-
    /// composited and show THROUGH as a doubled set over our custom titlebar.
    /// Clearing these bits is the canonical fix (melak47/BorderlessWindow, the MS
    /// DWM custom-frame sample, Tao/Tauri, Electron); `WM_NCCALCSIZE` cannot
    /// remove them because they are composited by DWM, not part of the standard
    /// non-client frame (MS WM_NCCALCSIZE docs). `WS_CAPTION` is included for
    /// completeness — clearing an already-absent bit is a no-op.
    const CAPTION_BUTTON_STYLES: u32 = WS_SYSMENU | WS_MINIMIZEBOX | WS_MAXIMIZEBOX | WS_CAPTION;

    /// Whether any caption-button style bit is currently set on `style`.
    fn caption_button_styles_present(style: u32) -> bool {
        style & CAPTION_BUTTON_STYLES != 0
    }

    /// `style` with every caption-button bit cleared; all other bits preserved.
    fn style_without_caption_buttons(style: u32) -> u32 {
        style & !CAPTION_BUTTON_STYLES
    }

    /// Cached main-window HWND (0 = not yet found). One window per process.
    /// Primed by [`set_main_hwnd`] with the real eframe handle when available;
    /// falls back to the `EnumWindows` guess only if never primed.
    static CACHED_HWND: AtomicIsize = AtomicIsize::new(0);
    /// Set once the NC subclass is successfully installed (install is one-shot).
    static SUBCLASSED: AtomicBool = AtomicBool::new(false);
    /// Set once the one-shot diagnostic file has been written.
    static DIAG_WRITTEN: AtomicBool = AtomicBool::new(false);

    /// A stable, arbitrary subclass id for our single subclass entry.
    const SUBCLASS_ID: usize = 0x5C_1B_3E;

    /// Prime the cached HWND with the authoritative handle (see the public
    /// wrapper). Stores only a non-zero value; idempotent.
    pub fn set_main_hwnd(hwnd: isize) {
        if hwnd != 0 {
            CACHED_HWND.store(hwnd, Ordering::Relaxed);
        }
    }

    /// `ASFW_ANY`: grant the foreground-set right to ANY process. Required
    /// because the elevated installer is a grandchild (PowerShell → UAC →
    /// setup.exe), so a PID-specific grant would target the wrong process.
    const ASFW_ANY: u32 = 0xFFFF_FFFF;

    /// Delegate this (currently-foreground) process's right to set the foreground
    /// window to any soon-to-be-spawned process. See the public wrapper.
    pub fn allow_foreground_handoff() {
        // SAFETY: a single Win32 call with a constant `u32` argument (no pointers,
        // no handles). No-ops harmlessly if this process is not the foreground.
        unsafe {
            AllowSetForegroundWindow(ASFW_ANY);
        }
    }

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

    /// PURE geometry decision for the maximized `WM_NCCALCSIZE` client rect.
    ///
    /// A borderless maximize would otherwise cover the taskbar, so a maximized
    /// window clamps its client rect to the monitor work area. When an auto-hide
    /// taskbar is present we leave a 1px sliver on the bottom edge (the common
    /// edge) so it can still pop up.
    ///
    /// `proposed` is the OS-proposed client rect (the full window rect); it is
    /// returned unchanged here — the caller only invokes this on the maximized
    /// path, where the work-area clamp wins. The non-maximized path returns the
    /// proposed rect unchanged and never calls this. Keeping `proposed` in the
    /// signature documents the contract and keeps the function self-describing.
    fn maximized_client_rect(proposed: RECT, work: RECT, taskbar_autohide: bool) -> RECT {
        let _ = proposed;
        let mut work = work;
        if taskbar_autohide {
            // Leave a 1px sliver on the bottom so an auto-hide taskbar (the
            // common edge) can still pop up.
            work.bottom -= 1;
        }
        work
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
                // the client rect to the monitor work area via the pure decision.
                if let Some(work) = monitor_work_area(hwnd) {
                    let params = lparam as *mut NCCALCSIZE_PARAMS;
                    let proposed = (*params).rgrc[0];
                    (*params).rgrc[0] =
                        maximized_client_rect(proposed, work, taskbar_is_autohide());
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

    /// Clear the caption-button window styles winit leaves on the undecorated
    /// window, then force a frame recalculation so DWM stops compositing the
    /// native min/max/close. Only acts when a bit is actually set, so it is cheap
    /// to call every frame AND self-heals if winit re-asserts the styles on a
    /// resize/restore (winit re-derives styles from its `WindowFlags`). Leaves
    /// winit's `DwmEnableBlurBehindWindow` transparency untouched — unlike the old
    /// `DWMNCRP_DISABLED` attempt, which fought it.
    fn strip_caption_styles(hwnd: isize) {
        // SAFETY: `hwnd` is an OS window owned by this process; GWL_STYLE
        // read/write + a frame-changed re-layout — the canonical borderless
        // technique (melak47/BorderlessWindow, MS DWM custom-frame sample).
        unsafe {
            let h = hwnd as HWND;
            let style = GetWindowLongPtrW(h, GWL_STYLE) as u32;
            if !caption_button_styles_present(style) {
                return; // already stripped — nothing to do this frame.
            }
            SetWindowLongPtrW(h, GWL_STYLE, style_without_caption_buttons(style) as isize);
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

    pub fn ensure_borderless() {
        let mut hwnd = CACHED_HWND.load(Ordering::Relaxed);
        if hwnd == 0 {
            // Not primed with the real eframe handle yet — fall back to the guess.
            hwnd = find_main_window();
            CACHED_HWND.store(hwnd, Ordering::Relaxed);
        }
        // Install the subclass exactly once (early frames may run before the
        // window exists; we retry each frame until it does, then stop). The
        // subclass now exists ONLY for the maximized→work-area clamp (so a
        // borderless maximize doesn't cover the taskbar); it is NOT what removes
        // the caption buttons.
        if hwnd != 0 && !SUBCLASSED.load(Ordering::Relaxed) && install_nc_subclass(hwnd) {
            SUBCLASSED.store(true, Ordering::Relaxed);
        }
        // THE caption-button fix: strip the residual `WS_SYSMENU|WS_MINIMIZEBOX|
        // WS_MAXIMIZEBOX` styles every frame (self-healing against winit
        // re-asserting them). This is what actually removes the doubled native
        // min/max/close that show through a transparent window — `WM_NCCALCSIZE`
        // structurally cannot (DWM-composited; see CAPTION_BUTTON_STYLES).
        if hwnd != 0 {
            strip_caption_styles(hwnd);
        }
        // One-shot diagnostic: after the subclass is installed, record whether the
        // non-client strip is actually gone (client rect == window rect). Written
        // to %TEMP%\scr1b3-caption-diag.txt so a STILL-failing fix can be debugged
        // from evidence rather than another blind guess.
        if hwnd != 0
            && SUBCLASSED.load(Ordering::Relaxed)
            && !DIAG_WRITTEN.swap(true, Ordering::Relaxed)
        {
            write_diag(hwnd);
        }
    }

    /// Build a one-line snapshot of the window's NC state. `nc_strip_gone` is the
    /// load-bearing signal: when the `WM_NCCALCSIZE` fix works, the client rect
    /// equals the full window rect (no reserved caption strip).
    fn nc_state_line(hwnd: isize) -> String {
        // SAFETY: `hwnd` is an OS window owned by this process; rect + style reads.
        unsafe {
            let h = hwnd as HWND;
            let mut wr: RECT = std::mem::zeroed();
            let mut cr: RECT = std::mem::zeroed();
            let gw = GetWindowRect(h, &mut wr);
            let gc = GetClientRect(h, &mut cr);
            let win = (wr.right - wr.left, wr.bottom - wr.top);
            let cli = (cr.right - cr.left, cr.bottom - cr.top);
            let nc_gone = gw != 0 && gc != 0 && win == cli;
            let style = GetWindowLongPtrW(h, GWL_STYLE) as u32;
            // The load-bearing signal post-fix: with the caption-button styles
            // cleared, DWM draws no native min/max/close. `nc_strip_gone` is now
            // secondary (it never governed the DWM-composited buttons).
            let caption_btn_styles = if caption_button_styles_present(style) {
                "present"
            } else {
                "stripped"
            };
            format!(
                "scr1b3 caption diag: hwnd=0x{hwnd:x} subclassed={} style=0x{style:08x} \
                 caption_btn_styles={caption_btn_styles} win={}x{} client={}x{} \
                 nc_strip_gone={nc_gone}",
                SUBCLASSED.load(Ordering::Relaxed),
                win.0,
                win.1,
                cli.0,
                cli.1
            )
        }
    }

    /// Write the diagnostic line to `%TEMP%\scr1b3-caption-diag.txt` (best-effort).
    fn write_diag(hwnd: isize) {
        use std::io::Write;
        let path = std::env::temp_dir().join("scr1b3-caption-diag.txt");
        if let Ok(mut f) = std::fs::File::create(&path) {
            let _ = writeln!(f, "{}", nc_state_line(hwnd));
        }
    }

    #[cfg(all(windows, test))]
    mod tests {
        use super::*;

        fn rect(left: i32, top: i32, right: i32, bottom: i32) -> RECT {
            RECT {
                left,
                top,
                right,
                bottom,
            }
        }

        #[test]
        fn maximized_clamps_to_work_area() {
            // Proposed = full monitor; work area reserves the bottom (taskbar).
            let proposed = rect(0, 0, 1920, 1080);
            let work = rect(0, 0, 1920, 1040);
            let out = maximized_client_rect(proposed, work, false);
            // No inset when the taskbar is not auto-hide: returns the work area.
            assert_eq!(out.left, work.left);
            assert_eq!(out.top, work.top);
            assert_eq!(out.right, work.right);
            assert_eq!(out.bottom, work.bottom);
        }

        #[test]
        fn autohide_taskbar_gets_one_px_inset() {
            let proposed = rect(0, 0, 1920, 1080);
            let work = rect(0, 0, 1920, 1080);
            let out = maximized_client_rect(proposed, work, true);
            // Bottom inset by exactly 1px; other edges identical to the work area.
            assert_eq!(out.bottom, work.bottom - 1);
            assert_eq!(out.left, work.left);
            assert_eq!(out.top, work.top);
            assert_eq!(out.right, work.right);
        }

        #[test]
        fn multi_monitor_offset_preserved() {
            // A second monitor placed left of / above the primary: negative origin.
            let proposed = rect(-1920, -120, 0, 960);
            let work = rect(-1920, -120, 0, 920);
            let out = maximized_client_rect(proposed, work, false);
            // Offset coordinates carried through exactly, no inset.
            assert_eq!(out.left, -1920);
            assert_eq!(out.top, -120);
            assert_eq!(out.right, 0);
            assert_eq!(out.bottom, 920);
        }

        #[test]
        fn autohide_inset_preserves_offset_edges() {
            // Auto-hide inset on an offset monitor still only touches `bottom`.
            let proposed = rect(-1920, -120, 0, 960);
            let work = rect(-1920, -120, 0, 920);
            let out = maximized_client_rect(proposed, work, true);
            assert_eq!(out.left, -1920);
            assert_eq!(out.top, -120);
            assert_eq!(out.right, 0);
            assert_eq!(out.bottom, 919);
        }

        #[test]
        fn caption_button_styles_detected_and_cleared() {
            // A typical winit undecorated style: the caption-button bits set,
            // plus an unrelated bit (WS_MAXIMIZE = the maximized STATE) that must
            // be preserved.
            let with = WS_CAPTION | WS_SYSMENU | WS_MAXIMIZEBOX | WS_MINIMIZEBOX | WS_MAXIMIZE;
            assert!(caption_button_styles_present(with));

            let stripped = style_without_caption_buttons(with);
            assert!(
                !caption_button_styles_present(stripped),
                "all caption-button bits must be cleared"
            );
            // The unrelated bit survives the strip.
            assert_eq!(stripped & WS_MAXIMIZE, WS_MAXIMIZE);
            // Idempotent: stripping an already-clean style changes nothing.
            assert_eq!(style_without_caption_buttons(stripped), stripped);
            // A style with none of the bits is reported clean (no needless work).
            assert!(!caption_button_styles_present(WS_MAXIMIZE));
        }
    }
}
