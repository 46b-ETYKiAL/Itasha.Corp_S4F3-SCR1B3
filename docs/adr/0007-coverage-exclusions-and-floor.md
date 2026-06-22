# ADR 0007 — Coverage Exclusions and the CI Coverage Floor

**Status:** Accepted

## Context

The SCR1B3 coverage initiative (work-units WU-1 .. WU-7) raised workspace line
coverage from ~73.9% to **82.77%** by testing the genuinely-testable surface
(engine logic, e2e/kittest GUI drive-throughs, payload assembly, parsers,
state reducers). After those WUs landed, the remaining uncovered surface is no
longer "we have not written the test yet" — a material fraction of it is
**structurally uncoverable in a headless CI runner** and will never be reached
by any honest test that runs without a GPU adapter, a real OS window, a live
subprocess, or a multi-gigabyte file.

A coverage gate that counts those lines against the percentage punishes the
project for code that *cannot* be exercised in CI, and it pressures authors
toward the worst possible response: writing fake tests that call into
GPU/FFI/boot code purely to mark the line "executed" without asserting
anything. That is a soft-completion leak (a test that exists to move a number,
not to catch a bug).

This ADR records (a) the measured testable-coverage ceiling, (b) each excluded
file/region and *why* it is structurally uncoverable headless, (c) the rule
that these surfaces get **exclusions, never faked tests**, and (d) the CI
floor that locks the achieved level in.

Source assessment: the per-file coverage gap map produced at the start of the
coverage initiative (the WU planning artifact), measured with cargo-llvm-cov +
nextest under `--test-threads=1`.

## Decision

### 1. Whole-file exclusions (purely uncoverable headless)

Four files are excluded from the coverage denominator via cargo-llvm-cov's
`--ignore-filename-regex` in `.github/workflows/coverage.yml`. Each is
**0%-or-near-0% coverable in CI by construction** — not because tests are
missing:

| Excluded file | CI line cov | Why structurally uncoverable headless |
|---|---|---|
| `scribe-app/src/app/visual_qa.rs` | 0.00% | The GPU render-QA harness itself. Every scene is `#[ignore]`-gated and only runs when a real **wgpu adapter** resolves. CI has no GPU; the scenes never execute. This is the visual-QA discipline working as designed — the harness is the thing that needs a GPU, so it cannot self-cover headless. |
| `scribe-app/src/app/effects.rs` | 0.00% | Pure `ctx.layer_painter().rect_filled(...)` CRT/motion overlays (tint, scanline, flicker, VHS, wired-mesh, caret-trail, boot-glitch). Their entire output is **GPU pixels**; there is no return value or observable state to assert. The only non-painter logic (`Rgba::parse_hex` clamp guards) is already covered where it lives in `scribe-render`. |
| `scribe-app/src/main.rs` | 0.00% | The `eframe::run_native` process/window **entry point**. `main()` owns the OS event loop and never returns under a test harness; the env-filter init and `ExitCode` plumbing run only in a real launched process. A test cannot enter `main()` without launching the actual windowed app. |
| `scribe-win32-chrome/src/lib.rs` | 31.96% | Live Win32 FFI — `SetWindowLongPtrW` / `SetWindowPos` / `EnumWindows` / the `nc_subclass_proc` window-procedure — needs a **real HWND + DWM**. The FFI bodies are `#[cfg(windows)]`-gated, so on the Linux CI runner they are **not even compiled**, and on a Windows runner they would still need a live composited window. The pure style-math helpers (`caption_button_styles_present`, `style_without_caption_buttons`, inset/clamp math) ARE tested; whole-file exclusion is chosen because cargo-llvm-cov cannot region-exclude the FFI bodies cleanly and the file's coverable surface is dominated by `cfg(windows)` dead code on the CI target. |

The exclusion regex (cross-platform — `[/\\]` separator classes so it matches
both the Windows backslash path form and the Linux forward-slash form CI uses):

```
scribe-app[/\\]src[/\\]app[/\\](visual_qa|effects)\.rs$|scribe-app[/\\]src[/\\]main\.rs$|scribe-win32-chrome[/\\]src[/\\]lib\.rs$
```

### 2. Region-level uncoverable arms (inside otherwise-covered files)

These files are **kept in the denominator** (their testable surface is large
and well-covered), but they each carry a residue of headless-uncoverable
arms. cargo-llvm-cov 0.8.7 has **no stable inline-comment region-exclusion**
mechanism (that is a grcov-only feature), so per the WU-0 contract these arms
are **left in place and accounted for here — never faked**:

| File | CI line cov | Uncoverable region + why |
|---|---|---|
| `scribe-app/src/app/mod.rs` | 58.50% | `rfd::FileDialog` blocking open/save dialogs (~9 call sites) spawn a **native OS file picker** — no headless return; plus interspersed `ctx`-painter arms (GPU pixels). The *bulk* of the 2,684 missed lines here is testable GUI-state logic that the WU-1 god-file decomposition did not finish reaching — that residue is ordinary test backlog, NOT an exclusion, and is deliberately left to a future testing WU rather than excluded or faked. |
| `scribe-app/src/issue_intake.rs` | 94.18% | The OS-launch glue — `webbrowser::open(...)` / `mailto:` `launch()` — hands off to the **OS default browser / mail client**. The URL-assembly + 414-fallback logic around it is covered; the actual `launch()` syscall is not. |
| `scribe-app/src/updater.rs` | 73.13% | Thread-spawn + `mpsc` + `ctx.request_repaint` orchestration. The pure state-transition reducer is covered; the **spawned background thread** that does the real fetch/apply is not entered in a single-threaded instrumented test. |
| `scribe-core/src/lsp/mod.rs` | 69.19% | The live **language-server subprocess** surface (`spawn` / `did_open` / `shutdown` / `Drop`). Driving these needs a real LSP child process over stdio; the protocol-framing logic (`lsp/protocol.rs`, 96.20%) is covered, the process lifecycle is not. |
| `scribe-core/src/update/net.rs` | 82.16% | `ureq` HTTP error/redirect arms that require a **live (or mock) network endpoint**. The happy-path + parse logic is covered; the transport error branches are not (no test server is wired). |
| `scribe-core/src/document.rs` | 91.94% | The `LARGE_FILE_THRESHOLD = 256 * 1024 * 1024` mmap browse path. The 256 MiB threshold is a **non-injectable `const`**; exercising the mmap arm would require materialising a ≥256 MiB file in CI, which is not a reasonable test fixture. |
| `scribe-app/src/app/chrome.rs` | 64.06% | `#[cfg(windows)]` titlebar/caption-layout arms that do not compile on the Linux CI target. |

### 3. The rule

**Structurally-uncoverable-headless surfaces get exclusions or are accounted
for in this ADR — they are NEVER given fake tests.** A test that enters
GPU/FFI/boot/subprocess/network/large-file code solely to mark a line
"executed" without a meaningful assertion is forbidden (it is a
soft-completion leak — `thorough-completion.md` / `no-future-work.md`). The
correct CI path for these surfaces is:

- GPU render-QA → the `#[ignore]`'d `visual_qa.rs` harness, run on demand with
  a real wgpu adapter (optional local lane), not in the headless gate.
- FFI / OS-process / OS-dialog / OS-launch → exercised by the developer on the
  real OS, not asserted in the headless gate.
- eframe boot → validated by actually launching the app, not by a unit test.

### 4. Verification correction (gap-map claim refuted)

The gap-map hypothesised that *"the tree-sitter-rust native grammar fails to
build under the llvm-cov instrumentation profile, so `syntax.rs`'s tree-sitter
arms fall back to syntect and are engine-uncoverable in CI."* **This was
measured to be false.** Under the instrumented profile, `scribe-core/src/syntax.rs`
measures **92.09% line coverage**, and the tree-sitter assertions
(`tree_sitter_rust_grammar_is_wired` asserting `tree_sitter_language_count() == 1`,
and `rust_uses_tree_sitter_and_colors_keywords`) **pass under coverage**. The
grammar DOES build and execute under instrumentation. Therefore `syntax.rs` is
**not excluded** — its residual ~63 missed lines are ordinary
less-common-token-rule backlog, not a structural exclusion.

## Consequences

- **Measured testable line coverage rises from 82.77% → 84.61%** once the four
  whole-file structural exclusions are applied (the 674 excluded lines, of which
  604 were uncovered, no longer drag the denominator).
- **The CI floor (`--fail-under-lines`) is raised from 74 → 83**, locking in the
  84.61% achieved level with a ~1.6-point safety margin for normal fluctuation.
  RAISE this floor as future testing WUs reach the `app/mod.rs` backlog; never
  lower it to make a red build pass.
- The reported percentage now reflects the **testable** surface. The excluded
  surface (GPU / FFI / OS-process / boot) is documented here, not faked.
- The exclusion list is narrow and full-path-anchored, so it cannot silently
  swallow a future newly-added file.

## References

- The coverage initiative's per-file gap-map assessment (WU planning artifact).
- `.github/workflows/coverage.yml` — the gate (exclusion regex + floor).
- ADR 0006 (`0006-syntax-engine.md`) — the syntect-first / tree-sitter engine
  decision referenced by the verification correction above.
