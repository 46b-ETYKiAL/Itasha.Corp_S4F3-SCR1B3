//! The single wgpu-adapter probe for the GPU-gated test lanes.
//!
//! `visual_qa` and `visual_regression` each had their own byte-identical copy of
//! this (the latter's doc comment even said "mirrors `visual_qa`"). Two copies
//! of one gate drift independently, and this gate decides whether a whole test
//! lane runs at all — so it lives here once.
//!
//! WHY `SCR1B3_REQUIRE_GPU` EXISTS. The skip is honest on a dev laptop with no
//! adapter: nobody is relying on that run. It is NOT honest in CI. Every caller
//! of `render_scene` discards its `Option`, so with no adapter the 31 visual-QA
//! tests do not skip loudly — they PASS having rendered nothing. A CI job that
//! ran them without a working adapter would report green over a lane that never
//! executed, which is worse than no job at all: it manufactures confidence.
//!
//! So the CI GPU lane sets `SCR1B3_REQUIRE_GPU=1` and an absent adapter becomes
//! a hard failure. If mesa/lavapipe breaks or the package moves, that job goes
//! RED instead of quietly green.
//!
//! WHY THE DECISION IS A SEPARATE, PURE FUNCTION. The obvious way to test the
//! guard is to remove the adapter and watch it fire — but there is no reliable
//! way to un-GPU a host from inside its own test (`WGPU_BACKEND=noop` does not
//! do it; the adapter still resolved and the "proof" passed for the wrong
//! reason). A guard whose failing branch cannot be reached is a guard nobody has
//! ever seen work. So [`enforce`] takes the probe result as an ARGUMENT: the
//! panic path is then directly reachable and is asserted below.

/// True if a usable wgpu adapter resolves on this host.
///
/// Impure: this is the actual environment probe. The decision about what to DO
/// with the answer lives in [`enforce`], which is testable.
fn adapter_resolves() -> bool {
    let instance = wgpu::Instance::default();
    pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::LowPower,
        compatible_surface: None,
        force_fallback_adapter: false,
    }))
    .is_ok()
}

/// Whether this run declared the GPU lane mandatory. Only the literal `1` arms
/// it, so a typo'd value cannot silently disarm the CI lane.
fn gpu_required() -> bool {
    env_arms_requirement(std::env::var_os("SCR1B3_REQUIRE_GPU").as_deref())
}

/// The `SCR1B3_REQUIRE_GPU` parsing contract, as a pure function.
fn env_arms_requirement(v: Option<&std::ffi::OsStr>) -> bool {
    v.is_some_and(|v| v == "1")
}

/// Decide what an adapter probe result means for this run.
///
/// Returns whether the GPU lane may run. A missing adapter is a clean skip
/// UNLESS the run declared the lane mandatory, in which case it is a failure —
/// silence there would report green over a lane that never executed.
///
/// # Panics
/// When `required` is true and `resolved` is false.
fn enforce(resolved: bool, required: bool) -> bool {
    assert!(
        resolved || !required,
        "SCR1B3_REQUIRE_GPU=1 but no wgpu adapter resolved. This run declared the \
         GPU lane mandatory, and a silent skip here would report GREEN while \
         rendering nothing. Install a software adapter (mesa-vulkan-drivers / \
         lavapipe) or unset SCR1B3_REQUIRE_GPU."
    );
    resolved
}

/// True if the GPU lane may run on this host.
///
/// Avoids the panic `Harness::wgpu()` raises when no adapter exists, so a
/// GPU-less host skips cleanly — unless `SCR1B3_REQUIRE_GPU=1`.
///
/// # Panics
/// When `SCR1B3_REQUIRE_GPU=1` and no wgpu adapter can be resolved.
pub(super) fn gpu_available() -> bool {
    enforce(adapter_resolves(), gpu_required())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    // ---- the decision (the part that must not be trusted untested) ----

    #[test]
    #[should_panic(expected = "no wgpu adapter resolved")]
    fn a_mandatory_lane_with_no_adapter_fails_instead_of_skipping() {
        // THE point of this module. Without this branch, a CI job with a broken
        // lavapipe install turns 31 render tests into 31 no-ops and still
        // reports green.
        enforce(false, true);
    }

    #[test]
    fn a_missing_adapter_is_a_clean_skip_when_the_lane_is_not_mandatory() {
        assert!(
            !enforce(false, false),
            "a dev host with no GPU must skip, not fail — nobody is relying on it"
        );
    }

    #[test]
    fn a_resolved_adapter_runs_the_lane_either_way() {
        assert!(enforce(true, true), "adapter present + required => run");
        assert!(enforce(true, false), "adapter present + optional => run");
    }

    // ---- the env contract ----

    #[test]
    fn only_the_exact_value_1_arms_the_requirement() {
        assert!(env_arms_requirement(Some(OsStr::new("1"))), "1 arms it");
        assert!(!env_arms_requirement(Some(OsStr::new("0"))), "0 must not");
        assert!(
            !env_arms_requirement(Some(OsStr::new("true"))),
            "only the literal 1 arms it"
        );
        assert!(
            !env_arms_requirement(Some(OsStr::new(""))),
            "empty must not"
        );
        assert!(!env_arms_requirement(None), "unset must not");
    }

    // ---- the probe ----

    #[test]
    fn the_probe_is_stable_within_a_run() {
        // A flapping probe would make the lane's skip/run decision
        // nondeterministic. This asserts agreement, not a particular answer:
        // the answer legitimately differs between a GPU host and a bare runner.
        assert_eq!(
            adapter_resolves(),
            adapter_resolves(),
            "the adapter probe must not flap between calls"
        );
    }
}
