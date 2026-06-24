//! Integration test: the full plugin-host LIFECYCLE through the PUBLIC API —
//! load → register → run commands → fire hooks → multi-plugin coexistence →
//! sandbox boundary holds end-to-end.
//!
//! This is the POSITIVE lifecycle complement to two existing sibling suites:
//!   * `plugin_examples.rs` exercises the on-disk `examples/plugins/` fixtures
//!     and the authoring guide;
//!   * `plugin_sandbox_isolation.rs` drives the ADVERSARIAL capability matrix
//!     (no net/process/env host fns, deadline re-arm, allocation bombs).
//!
//! Neither walks the *lifecycle of a hand-authored plugin* end-to-end: compile,
//! discover the registered command surface, run a buffer-transform command,
//! drive each lifecycle hook (open/save/change), confirm two plugins coexist
//! without cross-talk, and confirm a transform genuinely round-trips the buffer
//! the way the app's command palette + save pipeline would call it. We also
//! re-assert the sandbox boundary HOLDS while a legitimate plugin is mid-flight
//! (the security contract must survive the happy path, not only adversarial
//! input). Public-API only (`scribe_core::plugin::*`).

use scribe_core::plugin::{discover, CommandInfo, HookEvent, PluginContext, PluginHost};
use tempfile::tempdir;

/// A small but complete plugin that registers a command + hooks all three
/// lifecycle events, so one load exercises the whole surface.
const FULL_LIFECYCLE_PLUGIN: &str = r#"
    fn cmd_reverse() {
        // Reverse the buffer text grapheme-naively (chars) — a real transform.
        let chars = buffer_text().split("");
        chars.reverse();
        let out = "";
        for c in chars { out += c; }
        set_buffer_text(out);
        notify("reversed");
    }
    fn on_open()   { notify("opened:" + buffer_text()); }
    fn on_save()   { notify("saved:" + buffer_text()); }
    fn on_change() { notify("changed:" + buffer_text()); }

    register_command("reverse", "Reverse Buffer", "cmd_reverse");
    on_event("open", "on_open");
    on_event("save", "on_save");
    on_event("change", "on_change");
"#;

#[test]
fn load_registers_command_surface() {
    let mut host = PluginHost::new();
    assert_eq!(host.plugin_count(), 0);
    host.load_script("lifecycle", FULL_LIFECYCLE_PLUGIN)
        .unwrap();
    assert_eq!(host.plugin_count(), 1);

    let cmds = host.commands();
    assert_eq!(cmds.len(), 1);
    assert_eq!(
        cmds[0],
        CommandInfo {
            plugin_id: "lifecycle".into(),
            id: "reverse".into(),
            label: "Reverse Buffer".into(),
        }
    );
}

#[test]
fn run_command_transforms_the_buffer_and_notifies() {
    let mut host = PluginHost::new();
    host.load_script("lifecycle", FULL_LIFECYCLE_PLUGIN)
        .unwrap();

    let mut ctx = PluginContext::new("abc");
    host.run_command("reverse", &mut ctx).unwrap();
    assert_eq!(ctx.text, "cba", "the command transformed the buffer");
    assert_eq!(ctx.notifications, vec!["reversed"]);

    // The command is idempotent under re-invocation: reversing twice restores.
    host.run_command("reverse", &mut ctx).unwrap();
    assert_eq!(ctx.text, "abc");
}

#[test]
fn each_lifecycle_hook_fires_independently() {
    let mut host = PluginHost::new();
    host.load_script("lifecycle", FULL_LIFECYCLE_PLUGIN)
        .unwrap();

    for (event, prefix) in [
        (HookEvent::Open, "opened:"),
        (HookEvent::Save, "saved:"),
        (HookEvent::Change, "changed:"),
    ] {
        let mut ctx = PluginContext::new("doc");
        host.fire_event(event, &mut ctx).unwrap();
        assert_eq!(
            ctx.notifications,
            vec![format!("{prefix}doc")],
            "hook for {event:?} fired with the right buffer"
        );
    }
}

#[test]
fn notifications_do_not_leak_across_invocations() {
    // Each invocation clears the notification sink; a fresh ctx starts empty and
    // never inherits the prior call's notifications (a real cross-call leak would
    // double-report to the UI).
    let mut host = PluginHost::new();
    host.load_script("lifecycle", FULL_LIFECYCLE_PLUGIN)
        .unwrap();

    let mut ctx_a = PluginContext::new("one");
    host.fire_event(HookEvent::Save, &mut ctx_a).unwrap();
    assert_eq!(ctx_a.notifications, vec!["saved:one"]);

    let mut ctx_b = PluginContext::new("two");
    host.fire_event(HookEvent::Save, &mut ctx_b).unwrap();
    assert_eq!(ctx_b.notifications, vec!["saved:two"]);
}

#[test]
fn two_plugins_coexist_without_cross_talk() {
    // Both register a distinct command and both hook `save`. Running one
    // command must not invoke the other; firing `save` invokes BOTH in load
    // order. This is the multi-plugin coexistence contract.
    let mut host = PluginHost::new();
    host.load_script(
        "up",
        r#"
        fn cmd_up() { set_buffer_text(buffer_text().to_upper()); notify("up"); }
        fn save_up() { notify("up-save"); }
        register_command("up", "Up", "cmd_up");
        on_event("save", "save_up");
        "#,
    )
    .unwrap();
    host.load_script(
        "down",
        r#"
        fn cmd_down() { set_buffer_text(buffer_text().to_lower()); notify("down"); }
        fn save_down() { notify("down-save"); }
        register_command("down", "Down", "cmd_down");
        on_event("save", "save_down");
        "#,
    )
    .unwrap();

    assert_eq!(host.plugin_count(), 2);
    assert_eq!(host.commands().len(), 2);

    // One command runs only its own handler.
    let mut ctx = PluginContext::new("MixedCase");
    host.run_command("down", &mut ctx).unwrap();
    assert_eq!(ctx.text, "mixedcase");
    assert_eq!(ctx.notifications, vec!["down"], "only `down` ran");

    // Firing `save` invokes BOTH hooks, in load order.
    let mut ctx = PluginContext::new("doc");
    host.fire_event(HookEvent::Save, &mut ctx).unwrap();
    assert_eq!(ctx.notifications, vec!["up-save", "down-save"]);
}

#[test]
fn discover_then_load_then_run_is_the_real_app_path() {
    // Mirror the app's startup: discover plugins on disk, load each entry
    // script, then drive its command — the whole disk→host→command pipeline.
    let dir = tempdir().unwrap();
    let pdir = dir.path().join("trim");
    std::fs::create_dir_all(&pdir).unwrap();
    std::fs::write(
        pdir.join("plugin.toml"),
        "id='trim'\nname='Trim'\napi_version=1\nentry='main.rhai'\ncapabilities=['buffer']\n",
    )
    .unwrap();
    // `to_upper()` returns a fresh string (the in-place mutators like `trim()`
    // return `()` in Rhai, which the host fn would reject) — a real transform.
    std::fs::write(
        pdir.join("main.rhai"),
        r#"
        fn cmd_shout() { set_buffer_text(buffer_text().to_upper()); notify("shouted"); }
        register_command("shout", "Shout", "cmd_shout");
        "#,
    )
    .unwrap();

    let (found, errors) = discover(dir.path());
    assert!(errors.is_empty(), "discovery errors: {errors:?}");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].manifest.id, "trim");
    assert!(found[0].manifest.privileged().is_empty(), "only buffer cap");

    let mut host = PluginHost::new();
    for p in &found {
        let src = std::fs::read_to_string(p.entry_path()).unwrap();
        host.load_script(&p.manifest.id, &src).unwrap();
    }

    let mut ctx = PluginContext::new("padded text");
    host.run_command("shout", &mut ctx).unwrap();
    assert_eq!(ctx.text, "PADDED TEXT");
    assert_eq!(ctx.notifications, vec!["shouted"]);
}

#[test]
fn sandbox_boundary_holds_while_a_legitimate_plugin_runs() {
    // A normal, well-behaved plugin runs fine; an attempt to reach the
    // filesystem from WITHIN a registered command fails at load (no such host
    // fn), proving the boundary is not relaxed for "trusted" registered code.
    let mut host = PluginHost::new();
    host.load_script("good", FULL_LIFECYCLE_PLUGIN).unwrap();
    let mut ctx = PluginContext::new("ok");
    host.run_command("reverse", &mut ctx).unwrap(); // legit path works

    // Same host, a malicious second plugin that tries filesystem access at load
    // time must be rejected — the boundary is per-construction, not
    // per-plugin-trust. The top-level call runs during `load_script`, so the
    // absent `read_file` host fn surfaces immediately as a load error.
    let err = host.load_script(
        "evil",
        r#"let leaked = read_file("/etc/passwd"); notify(leaked);"#,
    );
    assert!(
        err.is_err(),
        "no `read_file` host fn exists — must fail to load"
    );

    // Even deferred filesystem access (inside a command body) cannot succeed:
    // running the command surfaces the missing host fn as an error, never a read.
    host.load_script(
        "evil2",
        r#"fn cmd_x() { let s = read_file("/etc/passwd"); set_buffer_text(s); } register_command("x","X","cmd_x");"#,
    )
    .unwrap();
    let mut bad = PluginContext::new("safe");
    let r = host.run_command("x", &mut bad);
    assert!(
        r.is_err(),
        "deferred filesystem access must error at run time"
    );
    assert_eq!(
        bad.text, "safe",
        "the buffer was never overwritten by a file read"
    );

    // The good plugin is unaffected and still runnable.
    let mut ctx2 = PluginContext::new("xy");
    host.run_command("reverse", &mut ctx2).unwrap();
    assert_eq!(ctx2.text, "yx");
}

#[test]
fn running_an_unregistered_command_is_a_clean_error() {
    let mut host = PluginHost::new();
    host.load_script("lifecycle", FULL_LIFECYCLE_PLUGIN)
        .unwrap();
    let mut ctx = PluginContext::new("x");
    let r = host.run_command("does-not-exist", &mut ctx);
    assert!(r.is_err(), "unknown command id must Err, not panic");
    // The buffer is untouched by a failed lookup.
    assert_eq!(ctx.text, "x");
}
