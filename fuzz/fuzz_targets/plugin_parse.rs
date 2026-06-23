#![no_main]
//! Fuzz the plugin trust boundary's parse surfaces:
//!   1. `PluginManifest::from_toml_str` — an arbitrary `plugin.toml` must never
//!      panic (malformed TOML returns `Err`; the host skips the plugin).
//!   2. `PluginHost::load_script` — compiling an arbitrary Rhai script must
//!      never panic. The sandbox bounds (max_expr_depths, disabled `eval`/
//!      `import`, deadline guard) mean a hostile script returns a compile/run
//!      `Err`, never a crash. This is the highest-value security fuzz surface:
//!      the plugin host runs semi-trusted third-party code.
//!
//! Split the raw input on the first NUL so one corpus entry can carry both a
//! manifest blob and a script blob; either half alone is also valid.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Partition: [manifest_toml] \0 [rhai_script]. A NUL-free input drives the
    // whole thing through both parsers (manifest first, then script).
    let (manifest_bytes, script_bytes) = match data.iter().position(|&b| b == 0) {
        Some(i) => (&data[..i], &data[i + 1..]),
        None => (data, data),
    };

    // 1. Manifest TOML parse — must be total.
    if let Ok(s) = std::str::from_utf8(manifest_bytes) {
        if let Ok(manifest) = scribe_core::plugin::PluginManifest::from_toml_str(s) {
            // Drive the compatibility checks the host runs on a parsed manifest.
            let _ = manifest.is_compatible();
        }
    }

    // 2. Rhai script compile — must be total (sandboxed compile never panics).
    if let Ok(src) = std::str::from_utf8(script_bytes) {
        // Bound the input so a pathological multi-MB script doesn't dominate the
        // fuzzer's time budget; the parser-recursion / size caps are unit-tested.
        if src.len() <= 64 * 1024 {
            let mut host = scribe_core::plugin::PluginHost::new();
            let _ = host.load_script("fuzz", src);
        }
    }
});
