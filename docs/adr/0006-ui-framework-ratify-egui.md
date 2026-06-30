# ADR-0006: Ratify egui as the SCR1B3 UI framework (NOT React/Tauri/shadcn)

**Date**: 2026-05-28
**Status**: accepted
**Supersedes**: none — first explicit framework-ratification ADR

## Context

The SCR1B3 v1 implementation is already built on **egui** (via `eframe` =
`winit` + `wgpu` + `egui` + `egui-winit` + `egui-wgpu`) per ADR-0001 and
the original D2 stack decision (2026-05-26). The 2026-05-27 enhancement
layer surfaced the explicit *conflicting design choices* question:
*should SCR1B3 migrate to a Web-stack (React + Tauri + shadcn / Vite)
or stay native-GPU egui?* The decision was deferred to this ADR; this
records it.

Sources consulted: the UI-framework-and-look research dossier
(2026-05-27), the competitive-notepad research dossier (2026-05-27),
the egui 0.34 release notes (docs.rs), the Lapce / Zed / Helix native-
GPU editor cohort, and the prior sibling terminal product (C0PL4ND —
also egui-native).

## Decision

**Stay egui-native. Do NOT migrate to React/Tauri/shadcn or any WebView-
core stack.** Bump egui 0.29 → 0.34 (T16.2) + add `egui-phosphor` 0.12
for the icon font (T16.3). `egui_tiles` will follow at 0.15 lockstep
when Phase 18 adds the multi-note grid.

## Rationale

1. **NFR fit.** SCR1B3's load-bearing non-functional requirements are
   *fast / lightweight / resource-efficient / not bloated / privacy-
   respecting / telemetry-free / single binary*. A WebView core (Tauri
   or Electron) contradicts every one of these directly:
   - System WebView memory + startup cost is multiples of egui's
     (Notepad++/Sublime/Zed band: tens of MB idle; webview-backed
     editors: hundreds of MB).
   - Webview attack-surface is the entire browser engine; egui-wgpu's
     is a Rust UI library + a GPU driver.
   - "Single binary" is structurally easier without a webview bundle.

2. **Rewrite cost vs. delivered value.** v1 already implements 14 of
   the 22 program base phases on egui (tabs, splits, file-tree,
   command palette, find/replace, minimap, settings UI, frameless
   titlebar). A WebView rewrite would discard this without delivering
   any user-visible improvement — and SCR1B3 explicitly defers the
   plugin marketplace / collab / cloud-sync features that justify a
   web stack elsewhere.

3. **The CRT post-process is a real fragment shader.** The Itasha.Corp
   brand identity (DECISION-2026-005) calls for a wgpu CRT post-pass
   (scanlines, phosphor, vignette, optional curvature) — implementable
   on `wgpu` because egui already owns the surface; not faked as CSS
   filters. C0PL4ND ships the same approach.

4. **Sibling precedent.** C0PL4ND (the sibling terminal product) is on
   the same egui + wgpu stack. Sharing the framework keeps the visual-
   system family coherent and the maintenance footprint single-source.

5. **Active 2025-2026 ecosystem.** egui 0.34 is the current stable
   (Dec 2025 / Q1 2026 era); `egui-phosphor` 0.12 tracks it; the
   ecosystem (egui_tiles, egui_extras, egui_kittest E2E testing) is
   alive and matched. The 2025 Rust GUI survey ranked egui as
   production-ready with 13M+ downloads (the largest in the cohort).

6. **Native-GPU editor precedent.** Zed (GPUI) and Lapce (Floem +
   wgpu) prove that native-GPU is the right axis for editor
   performance; their absence of a WebView is a feature, not a
   limitation.

## Falsification / when this should be revisited

- If `egui` were unable to host a custom rope-backed viewport-culled
  text widget (Phase 15 KEYSTONE) at the multi-GB / 1M-line target
  (cold-start < 300ms, p95 frame < 16ms, idle RAM in the Sublime/Zed
  tens-of-MB band), the `conflicting_design_choices` fork would
  revert to consider **Floem** (Lapce's path) — NOT Tauri/Electron.
  Slint is the secondary alt if AccessKit a11y becomes a v1 hard
  requirement.
- If egui 0.34 → 0.35+ introduces a breaking change so disruptive it
  exceeds the rewrite-cost of switching, revisit. (egui 0.35 released
  2026-06-25; SCR1B3 stays pinned at 0.34, upstream-blocked by
  `egui-phosphor` 0.12 which requires egui `^0.34`. The 0.35 bump
  follows once `egui-phosphor` tracks it — no disruptive breaking
  change forces an earlier move.)

## Consequences

- **Positive**: the v1 native-GPU implementation is preserved and
  evolved. The Phase-16 dep bump unlocks Phase 18's `egui_tiles` 0.15
  multi-note grid + Phase 17's modernised `Visuals`/`WidgetVisuals`
  surface (rounding, hover/active, frame elevation).
- **Negative**: egui's a11y story (AccessKit integration) is younger
  than Slint's — accessibility is addressed via the WCAG 2.2 AA theme
  variants + keyboard reachability rather than a polished screen-
  reader experience. The accessibility quality axis tracks this. (If
  a v1 hard a11y requirement lands later, see falsification pt 2.)
- **Lock-in**: SCR1B3 is now coupled to egui's release cadence + API
  stability. Bumps every 6-12 months are expected; the lockstep
  constraint with `egui_tiles` / `egui-phosphor` is documented in the
  `egui_major_bump` decision-contract fork.

## Compliance

- `external-dependency-governance.md`: egui + eframe + egui-phosphor are
  in-house-preferred OSS (Apache-2.0/MIT dual-licensed), not paid /
  cloud / proprietary services. No vendor SDK.
- DECISION-2026-005 brand canon: wired-noir / CRT post-pass / Phosphor
  icons — all implementable on the egui + wgpu stack.

## References

- The UI-framework-and-look research dossier (2026-05-27).
- The internal *egui major bump* decision-contract fork.
- ADR-0001 SCR1B3 stack-and-architecture (the initial egui choice).
- The C0PL4ND terminal sibling product precedent (egui-native).
