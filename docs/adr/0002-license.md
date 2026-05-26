# ADR 0002 — License

**Status:** Accepted

## Context

SCR1B3 is intended for the widest possible adoption as an open-source desktop editor. The license must be permissive, compatible with the Rust crate ecosystem it depends on, and impose no copyleft obligations on users or downstream integrators.

## Decision

SCR1B3 is **dual-licensed under MIT OR Apache-2.0**, at the user's option. This is the de-facto standard for the Rust ecosystem (the toolchain and the overwhelming majority of crates use exactly this dual license), which guarantees license compatibility with our dependency graph.

- **MIT** provides maximal simplicity and permissiveness.
- **Apache-2.0** adds an explicit patent grant, valued by larger organizations.
- "OR" lets each user pick whichever they prefer.

Contributions are accepted under the same dual license; the CONTRIBUTING guide states this explicitly so no separate CLA is required.

## Consequences

- Both `LICENSE-MIT` and `LICENSE-APACHE` ship at the repository root, and the README states the dual-license choice.
- Dependencies remain license-compatible by construction; `cargo deny` enforces the policy in CI.
- Downstream users and forks face no copyleft obligations and gain a patent grant via the Apache option.
