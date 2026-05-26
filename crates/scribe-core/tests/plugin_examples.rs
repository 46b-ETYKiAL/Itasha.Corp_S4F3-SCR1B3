//! Integration test: the example plugins shipped in `examples/plugins/` are
//! valid and behave as documented. This guards the authoring guide (PLUGINS.md)
//! against drift.

use scribe_core::plugin::{discover, HookEvent, PluginContext, PluginHost};
use std::path::PathBuf;

fn examples_dir() -> PathBuf {
    // crates/scribe-core -> ../../examples/plugins
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/plugins")
}

#[test]
fn example_plugins_discover_and_load() {
    let dir = examples_dir();
    let (found, errors) = discover(&dir);
    assert!(errors.is_empty(), "discovery errors: {errors:?}");
    assert!(
        found.len() >= 2,
        "expected >=2 example plugins, found {}",
        found.len()
    );

    let mut host = PluginHost::new();
    for p in &found {
        let src = std::fs::read_to_string(p.entry_path()).unwrap();
        host.load_script(&p.manifest.id, &src)
            .unwrap_or_else(|e| panic!("load {}: {e}", p.manifest.id));
    }
    assert_eq!(host.plugin_count(), found.len());

    // The 'uppercase' command must transform the buffer.
    let ids: Vec<String> = host.commands().iter().map(|c| c.id.clone()).collect();
    assert!(ids.contains(&"uppercase".to_string()), "commands: {ids:?}");
    let mut ctx = PluginContext::new("hello world");
    host.run_command("uppercase", &mut ctx).unwrap();
    assert_eq!(ctx.text, "HELLO WORLD");

    // The 'wordcount' plugin hooks save and reports a count.
    let mut ctx = PluginContext::new("one two three");
    host.fire_event(HookEvent::Save, &mut ctx).unwrap();
    assert!(
        ctx.notifications.iter().any(|n| n.contains("words: 3")),
        "notes: {:?}",
        ctx.notifications
    );
}
