//! Anti-regression source-scan guard (#67).
//!
//! The whole class of "I clicked X and nothing happened" bugs in this app
//! traced to a frameless-resize overlay built from `egui::Area`s at
//! `Order::Foreground`: a Foreground `Area` is **interactable by default**,
//! so one that covers (part of) the window silently swallows every click in
//! its rect — tab switches, the settings ✕, panel-resize handles, all of it.
//! The fix removed that overlay (resize is now a pointer-gated per-frame edge
//! check with NO Area — see `handle_frameless_resize`).
//!
//! This guard scans the source so the dangerous pattern cannot creep back:
//! every `egui::Area` placed at `Order::Foreground` MUST either declare
//! `.interactable(false)` (paint-only / hint overlay — cannot eat clicks) or
//! be an allowlisted bounded popup that is positioned at a point (so it
//! covers a small region, not the window) and only shown on demand. A new
//! Foreground `Area` that is neither fails this test loudly, with a pointer
//! to this comment, before it can ship.

/// Foreground `Area`s that are intentionally interactable. Each is a small,
/// on-demand, point-anchored popup — NOT a window-spanning cover. Adding an
/// entry here is the explicit, reviewed way to introduce a new one.
const ALLOWED_INTERACTIVE_FOREGROUND_AREAS: &[&str] = &[
    // Code-completion list, anchored just below the cursor via `.fixed_pos`,
    // shown only while a completion is active. Rows must be clickable.
    "scr1b3-completion",
];

/// The app-shell source files this guard scans. After the A-01 wave-3
/// decomposition the click-eating surface that once lived entirely in `mod.rs`
/// is spread across the render loop (`frame_tick.rs`) and the leaf-helper module
/// that builds the completion popup (`render_support.rs`). All three are scanned
/// so the guard keeps covering exactly the code that moved — a new Foreground
/// `Area` in any of them is policed identically to the pre-split `mod.rs`.
const SCANNED_SOURCES: &[(&str, &str)] = &[
    ("mod.rs", include_str!("mod.rs")),
    ("frame_tick.rs", include_str!("frame_tick.rs")),
    ("render_support.rs", include_str!("render_support.rs")),
];

#[test]
fn no_ungated_interactable_foreground_area() {
    for (file, src) in SCANNED_SOURCES {
        let lines: Vec<&str> = src.lines().collect();

        for (i, line) in lines.iter().enumerate() {
            // Find the start of an Area construction that names its Id.
            let Some(rest) = line.split("egui::Area::new(egui::Id::new(\"").nth(1) else {
                continue;
            };
            let Some(id) = rest.split('"').next() else {
                continue;
            };

            // Collect the builder chain: this line plus the following lines up to
            // the `.show(` that ends the builder. That window is what we inspect
            // for `.order(...Foreground)` and the gating call.
            let mut chain = String::new();
            for l in lines.iter().skip(i).take(20) {
                chain.push_str(l);
                chain.push('\n');
                if l.contains(".show(") {
                    break;
                }
            }

            let is_foreground = chain.contains("Order::Foreground");
            if !is_foreground {
                continue;
            }

            // Paint-only / hint overlays opt out of input explicitly — safe.
            let non_interactable = chain.contains(".interactable(false)");
            if non_interactable {
                continue;
            }

            let allowlisted = ALLOWED_INTERACTIVE_FOREGROUND_AREAS.contains(&id);
            assert!(
                allowlisted,
                "{file}:{}: `egui::Area` id={id:?} is at Order::Foreground and \
                 interactable-by-default, which swallows clicks in its rect \
                 (the resize-overlay click-eating regression class — see the \
                 `foreground_area_guard` module doc). Either add \
                 `.interactable(false)` if it must not take input, or — if it is \
                 genuinely a small on-demand popup — add {id:?} to \
                 ALLOWED_INTERACTIVE_FOREGROUND_AREAS with a justifying comment.",
                i + 1
            );
        }
    }
}

#[test]
fn the_completion_popup_is_still_present_so_the_scan_is_not_vacuous() {
    // If the only allowlisted Area ever disappears, this guard would pass
    // trivially on an empty match set. Pin that the scan actually sees it in one
    // of the scanned sources (it lives in `render_support.rs` post-split).
    let found = SCANNED_SOURCES
        .iter()
        .any(|(_, src)| src.contains("egui::Id::new(\"scr1b3-completion\")"));
    assert!(
        found,
        "completion popup Area id not found in any scanned source — the \
             foreground-area scan would be vacuous; update \
             ALLOWED_INTERACTIVE_FOREGROUND_AREAS / SCANNED_SOURCES to match \
             reality"
    );
}
