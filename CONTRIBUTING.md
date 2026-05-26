# Contributing to SCR1B3

Thanks for your interest in contributing. SCR1B3 is a fast, telemetry-free, cross-platform editor — contributions that keep it that way (small, native, privacy-respecting, not bloated) are very welcome.

## Project layout

SCR1B3 is a Cargo workspace:

```
scr1b3/
├── crates/
│   ├── scribe-core    # engine: rope buffer, file I/O + mmap, encoding/EOL,
│   │                  #         config, theme, syntect highlighting, search,
│   │                  #         update logic. No UI dependency.
│   ├── scribe-render  # maps the engine Theme onto egui Visuals; CRT params.
│   └── scribe-app     # the binary: egui/eframe shell, tabs, find bar,
│                      #             status bar, frameless titlebar.
├── assets/            # SVG identity, bundled themes, fonts, media
├── docs/adr/          # architecture decision records
└── packaging/         # per-OS install recipes
```

The **core / render / app** seam is deliberate: `scribe-core` is the replaceable engine with a clean public API and no UI dependency. Keep UI-specific code out of `scribe-core`.

## Prerequisites

- A [Rust toolchain](https://rustup.rs/). The exact version is pinned in `rust-toolchain.toml` and will be installed automatically by `rustup`.
- Recommended dev tools:
  ```bash
  cargo install cargo-nextest   # fast test runner
  cargo install cargo-audit     # advisory scanning
  cargo install cargo-deny      # license + advisory gate
  ```

## Build

```bash
cargo build              # debug
cargo build --release    # optimized, stripped binary
cargo run -- path/to/file.txt   # run, optionally opening a file
```

## Test

The test runner is [`cargo-nextest`](https://nexte.st/):

```bash
cargo nextest run        # whole workspace
```

Plain `cargo test` also works if you don't have nextest installed. Every new behavior should ship with a test. The engine crates favor pure, offline-testable logic (for example, update version-comparison is tested without touching the network).

## Format & lint

These must pass before a PR is merged:

```bash
cargo fmt --check          # formatting
cargo clippy -- -D warnings   # lint; warnings are errors
```

Do not silence clippy with `#[allow(...)]` without a one-line justification comment explaining why.

## Security & dependencies

```bash
cargo audit     # known-vulnerability scan
cargo deny check   # license + advisory policy
```

- All dependencies are pinned via `Cargo.lock`; commit lock changes alongside `Cargo.toml` changes.
- Prefer the standard library and small, well-maintained crates. SCR1B3 has no embedded webview and no paid/cloud dependencies, and that is a design constraint, not an accident.
- The only permitted network surface is the telemetry-free update check. Do not add code that transmits file contents, usage data, or PII. See [SECURITY.md](SECURITY.md).

## Pull request conventions

1. **Branch** from the default branch; use a short descriptive name (`feat/...`, `fix/...`, `docs/...`).
2. **Commits** follow [Conventional Commits](https://www.conventionalcommits.org/): `feat:`, `fix:`, `docs:`, `refactor:`, `test:`, `chore:`.
3. **Keep PRs focused** — one logical change per PR. Files stay small and single-purpose where practical.
4. **Tests + green checks** — `cargo nextest run`, `cargo fmt --check`, `cargo clippy -D warnings`, `cargo audit`, and `cargo deny check` all pass.
5. **Document decisions** — significant architectural changes get an ADR under `docs/adr/` (see the existing records for the format).
6. **No telemetry, no bloat** — changes that add tracking, a system webview, or a heavy runtime will be declined.

## Architecture decisions

Read the [ADRs](docs/adr/) before proposing structural changes — they explain why the stack is Rust + egui/wgpu, why the config is TOML, why syntect is the v1 syntax engine (with tree-sitter as a planned structural enhancement), and how the telemetry-free auto-update is designed.

## Code of conduct

Be respectful, assume good faith, and keep discussion technical. Harassment or discrimination is not tolerated. By participating you agree to uphold a welcoming, inclusive environment for everyone. Report conduct concerns to the maintainers via a private channel (security/abuse contact in [SECURITY.md](SECURITY.md)).

## License

By contributing, you agree that your contributions are dual-licensed under **MIT OR Apache-2.0**, matching the project license, with no additional terms.
