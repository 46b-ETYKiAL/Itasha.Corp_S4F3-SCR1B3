# Visual-regression baselines

Committed known-good PNG frames for the `visual_regression` render → diff →
assert → view gate (`crates/scribe-app/src/app/visual_regression.rs`,
SCR1B3 testing-program task #31b).

## Why this directory may be empty in a headless checkout

The baseline PNGs are produced by the real `ScribeApp` frame rendered through
egui_kittest's **wgpu** backend, which needs a usable GPU adapter. CI/dev hosts
without a GPU **honest-skip** the scene tests (they print that they skipped —
they never pass falsely and never fail for lack of a GPU). The perceptual-diff
math and the `prefers-reduced-motion` resting-frame assertions are pure and run
on every host.

## Generating / refreshing the baselines (GPU host, run once)

```bash
SCR1B3_UPDATE_VISUAL_BASELINE=1 \
  cargo test -p scribe-app visual_regression -- --ignored --nocapture
```

Each scene writes its frame to `<scene>.png` here. On a normal (non-update)
GPU run, a missing baseline is recorded once and the assert is skipped that run
(bootstrap), exactly like the SVG `svg_render_qa` tool. Once a baseline exists,
subsequent GPU runs diff against it and FAIL on a perceptual regression
(layout shift, colour change, size change).

Scenes: `default_editor`, `settings_open`, `find_bar`.

## Discipline (the project's visual-QA discipline)

An agent cannot "see" a frame from source — render it, diff it, assert it, then
**Read** the rendered PNG (written to `%TEMP%/scr1b3-visual-regression/`) to
self-check against the design spec before claiming a frame correct.
