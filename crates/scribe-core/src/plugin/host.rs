//! Rhai scripting host for the plugin "easy mode".
//!
//! Plugins are `.rhai` files that, at load time, call host functions to declare
//! their contributions:
//!
//! ```rhai
//! fn cmd_uppercase() { set_buffer_text(buffer_text().to_upper()); notify("done"); }
//! register_command("uppercase", "Uppercase Buffer", "cmd_uppercase");
//! on_event("save", "cmd_uppercase");
//! ```
//!
//! The host exposes ONLY buffer-transform + notify + command/hook registration.
//! Rhai grants no ambient filesystem/network/process access, so a script mod is
//! sandboxed by construction.

use rhai::{Engine, ImmutableString, AST};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Wall-clock budget for a single plugin command/hook invocation. `max_operations`
/// alone does NOT bound time (a tight native-ish loop can still stall) — this
/// deadline force-terminates a runaway script via the engine's progress hook.
const PLUGIN_DEADLINE: Duration = Duration::from_millis(2000);

/// Lifecycle events a plugin can hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookEvent {
    Open,
    Save,
    Change,
}

impl HookEvent {
    fn parse(s: &str) -> Option<HookEvent> {
        match s {
            "open" => Some(HookEvent::Open),
            "save" => Some(HookEvent::Save),
            "change" => Some(HookEvent::Change),
            _ => None,
        }
    }
}

/// A command a plugin registered (surfaced in the command palette).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandInfo {
    pub plugin_id: String,
    pub id: String,
    pub label: String,
}

/// Context handed to a command/hook invocation: the buffer text in, the
/// (possibly transformed) text out, plus any notifications the plugin raised.
#[derive(Debug, Clone, Default)]
pub struct PluginContext {
    pub text: String,
    pub notifications: Vec<String>,
}

impl PluginContext {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            notifications: Vec::new(),
        }
    }
}

#[derive(Default)]
struct Shared {
    buffer_text: String,
    notifications: Vec<String>,
    current_plugin: Option<String>,
    commands: Vec<CommandInfo>,
    command_fns: HashMap<String, String>, // command_id -> handler fn name
    hooks: Vec<(String, HookEvent, String)>, // (plugin_id, event, fn_name)
}

struct Loaded {
    id: String,
    ast: AST,
}

pub struct PluginHost {
    engine: Engine,
    plugins: Vec<Loaded>,
    shared: Arc<Mutex<Shared>>,
    /// When set, the engine's progress hook aborts execution past this instant.
    deadline: Arc<Mutex<Option<Instant>>>,
}

impl Default for PluginHost {
    fn default() -> Self {
        Self::new()
    }
}

impl PluginHost {
    pub fn new() -> Self {
        let shared = Arc::new(Mutex::new(Shared::default()));
        let mut engine = Engine::new();
        // Sandbox hardening: bound script resource use so a buggy/hostile mod
        // can't hang or OOM the editor.
        engine.set_max_operations(5_000_000);
        engine.set_max_call_levels(64);
        engine.set_max_string_size(50 * 1024 * 1024);
        engine.set_max_array_size(1_000_000);

        // Wall-clock guard: abort if a script runs past its deadline.
        let deadline: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));
        let dl = Arc::clone(&deadline);
        engine.on_progress(move |_ops| match *dl.lock().unwrap() {
            Some(end) if Instant::now() > end => {
                Some(rhai::Dynamic::from("plugin exceeded time limit"))
            }
            _ => None,
        });

        register_host_fns(&mut engine, &shared);
        Self {
            engine,
            plugins: Vec::new(),
            shared,
            deadline,
        }
    }

    /// Arm the wall-clock deadline for the next script run.
    fn arm_deadline(&self) {
        *self.deadline.lock().unwrap() = Some(Instant::now() + PLUGIN_DEADLINE);
    }

    /// Compile + load a script plugin. Runs its top-level statements once (which
    /// register commands/hooks), then strips statements so later command calls
    /// re-run only the named function, not the registration block.
    pub fn load_script(&mut self, plugin_id: &str, source: &str) -> Result<(), String> {
        let mut ast = self
            .engine
            .compile(source)
            .map_err(|e| format!("{plugin_id}: parse: {e}"))?;
        {
            let mut s = self.shared.lock().unwrap();
            s.current_plugin = Some(plugin_id.to_string());
        }
        self.arm_deadline();
        self.engine
            .run_ast(&ast)
            .map_err(|e| format!("{plugin_id}: load: {e}"))?;
        {
            let mut s = self.shared.lock().unwrap();
            s.current_plugin = None;
        }
        ast.clear_statements(); // keep only fn defs for later invocation
        self.plugins.push(Loaded {
            id: plugin_id.to_string(),
            ast,
        });
        Ok(())
    }

    /// All registered commands (for the command palette).
    pub fn commands(&self) -> Vec<CommandInfo> {
        self.shared.lock().unwrap().commands.clone()
    }

    pub fn plugin_count(&self) -> usize {
        self.plugins.len()
    }

    /// Invoke a registered command against `ctx`. The plugin's function may read
    /// `buffer_text()` and call `set_buffer_text()` / `notify()`; the results are
    /// written back into `ctx`.
    pub fn run_command(&self, command_id: &str, ctx: &mut PluginContext) -> Result<(), String> {
        let (plugin_id, fn_name) = {
            let s = self.shared.lock().unwrap();
            let cmd = s
                .commands
                .iter()
                .find(|c| c.id == command_id)
                .ok_or_else(|| format!("no such command: {command_id}"))?;
            // fn_name is stored in hooks/commands map; commands store label, so
            // we look up the fn via a parallel scan of the registration.
            (cmd.plugin_id.clone(), command_fn_name(&s, command_id))
        };
        let fn_name = fn_name.ok_or_else(|| format!("command {command_id} has no handler"))?;
        self.invoke(&plugin_id, &fn_name, ctx)
    }

    /// Fire a lifecycle event to every plugin that hooked it.
    pub fn fire_event(&self, event: HookEvent, ctx: &mut PluginContext) -> Result<(), String> {
        let hooks: Vec<(String, String)> = {
            let s = self.shared.lock().unwrap();
            s.hooks
                .iter()
                .filter(|(_, e, _)| *e == event)
                .map(|(pid, _, f)| (pid.clone(), f.clone()))
                .collect()
        };
        for (pid, fname) in hooks {
            self.invoke(&pid, &fname, ctx)?;
        }
        Ok(())
    }

    fn invoke(
        &self,
        plugin_id: &str,
        fn_name: &str,
        ctx: &mut PluginContext,
    ) -> Result<(), String> {
        let plugin = self
            .plugins
            .iter()
            .find(|p| p.id == plugin_id)
            .ok_or_else(|| format!("plugin not loaded: {plugin_id}"))?;
        {
            let mut s = self.shared.lock().unwrap();
            s.buffer_text = ctx.text.clone();
            s.notifications.clear();
        }
        let mut scope = rhai::Scope::new();
        self.arm_deadline();
        self.engine
            .call_fn::<()>(&mut scope, &plugin.ast, fn_name, ())
            .map_err(|e| format!("{plugin_id}.{fn_name}: {e}"))?;
        let mut s = self.shared.lock().unwrap();
        ctx.text = s.buffer_text.clone();
        ctx.notifications.append(&mut s.notifications);
        Ok(())
    }
}

/// Find the handler fn name for a command id (stored alongside hooks at
/// registration; we keep commands' fn names in the hooks vec keyed by a
/// synthetic `cmd:<id>` event-less entry). Simpler: we stored it in a side map.
fn command_fn_name(s: &Shared, command_id: &str) -> Option<String> {
    s.command_fns.get(command_id).cloned()
}

fn register_host_fns(engine: &mut Engine, shared: &Arc<Mutex<Shared>>) {
    let sh = Arc::clone(shared);
    engine.register_fn("buffer_text", move || -> ImmutableString {
        sh.lock().unwrap().buffer_text.clone().into()
    });
    let sh = Arc::clone(shared);
    engine.register_fn("set_buffer_text", move |t: ImmutableString| {
        sh.lock().unwrap().buffer_text = t.to_string();
    });
    let sh = Arc::clone(shared);
    engine.register_fn("notify", move |m: ImmutableString| {
        sh.lock().unwrap().notifications.push(m.to_string());
    });
    engine.register_fn("log", move |m: ImmutableString| {
        tracing::info!(target: "scribe::plugin", "{m}");
    });
    let sh = Arc::clone(shared);
    engine.register_fn(
        "register_command",
        move |id: ImmutableString, label: ImmutableString, fn_name: ImmutableString| {
            let mut s = sh.lock().unwrap();
            if let Some(pid) = s.current_plugin.clone() {
                s.commands.push(CommandInfo {
                    plugin_id: pid,
                    id: id.to_string(),
                    label: label.to_string(),
                });
                s.command_fns.insert(id.to_string(), fn_name.to_string());
            }
        },
    );
    let sh = Arc::clone(shared);
    engine.register_fn(
        "on_event",
        move |event: ImmutableString, fn_name: ImmutableString| {
            let mut s = sh.lock().unwrap();
            if let (Some(pid), Some(ev)) = (s.current_plugin.clone(), HookEvent::parse(&event)) {
                s.hooks.push((pid, ev, fn_name.to_string()));
            }
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host_with(script: &str) -> PluginHost {
        let mut h = PluginHost::new();
        h.load_script("test", script).unwrap();
        h
    }

    #[test]
    fn register_and_run_command_transforms_buffer() {
        let h = host_with(
            r#"
            fn cmd_up() { set_buffer_text(buffer_text().to_upper()); notify("upped"); }
            register_command("up", "Uppercase", "cmd_up");
            "#,
        );
        assert_eq!(h.commands().len(), 1);
        assert_eq!(h.commands()[0].label, "Uppercase");
        let mut ctx = PluginContext::new("hello");
        h.run_command("up", &mut ctx).unwrap();
        assert_eq!(ctx.text, "HELLO");
        assert_eq!(ctx.notifications, vec!["upped"]);
    }

    #[test]
    fn event_hook_fires() {
        let h = host_with(
            r#"
            fn on_save() { notify("saved: " + buffer_text()); }
            on_event("save", "on_save");
            "#,
        );
        let mut ctx = PluginContext::new("doc");
        h.fire_event(HookEvent::Save, &mut ctx).unwrap();
        assert_eq!(ctx.notifications, vec!["saved: doc"]);
        // A different event does nothing.
        let mut ctx2 = PluginContext::new("doc");
        h.fire_event(HookEvent::Open, &mut ctx2).unwrap();
        assert!(ctx2.notifications.is_empty());
    }

    #[test]
    fn sandbox_has_no_file_access() {
        // Rhai has no `open`/`read_file`/`import fs` — such a script fails to
        // compile/run, proving no ambient filesystem capability.
        let mut h = PluginHost::new();
        let err = h.load_script("evil", r#"let x = open_file("/etc/passwd");"#);
        assert!(
            err.is_err(),
            "script with file access must fail (no such fn)"
        );
    }

    #[test]
    fn unknown_command_errors() {
        let h = host_with("fn noop() {}");
        let mut ctx = PluginContext::new("x");
        assert!(h.run_command("nope", &mut ctx).is_err());
    }

    #[test]
    fn parse_error_is_reported_not_panic() {
        let mut h = PluginHost::new();
        assert!(h.load_script("bad", "fn (((").is_err());
    }

    #[test]
    fn runaway_script_is_terminated_not_hung() {
        // An infinite loop must be force-terminated by the op-budget / wall-clock
        // guard — the editor never hangs on a hostile or buggy mod.
        let mut h = PluginHost::new();
        h.load_script(
            "evil",
            r#"fn spin() { let x = 0; loop { x += 1; } } register_command("spin", "Spin", "spin");"#,
        )
        .unwrap();
        let mut ctx = PluginContext::new("x");
        let r = h.run_command("spin", &mut ctx);
        assert!(r.is_err(), "runaway script must be terminated, got {r:?}");
    }
}
