# SCR1B3 Bug Root-Cause Analysis — Round 3

## Bug 1: WORD WRAP Can't Be Toggled (Appears Always On)

**Location:** crates/scribe-app/src/app.rs:5201–5224

**Root Cause:**
The word_wrap config value exists and is read at line 4995, then passed to a conditional at line 5201 to set ScrollArea direction + TextEdit's desired_width. However, the TextEdit is constructed with desired_width(dw) where dw = if word_wrap { ui.available_width() } else { f32::INFINITY }. 

The issue: egui 0.34's TextEdit::desired_width() is a hint, not a constraint. When content exceeds viewport width, egui always wraps internally regardless of the hint. The toggle only affects whether the editor can scroll horizontally — not the visual wrapping users see.

**Fix:** Replace the ScrollArea + desired_width pattern with egui's TextEdit::wrap() builder method (line 5216–5223):
- Use wrap(TextWrapping { break_anywhere: word_wrap, break_on_hyphen: word_wrap })

---

## Bug 2: WINDOW RESIZE Breaks After First Attempt

**Location:** crates/scribe-app/src/app.rs:5377–5379, 5789–5811

**Root Cause:**
ViewportCommand::BeginResize(dir) is stateless and does NOT track whether a resize is already in progress. Sending BeginResize twice in a row does not restart the resize; the first call begins the OS-level drag, and the second is ignored.

ctx.input(|i| i.pointer.primary_pressed()) returns true for only one frame per press. If the pointer is off an edge during that frame, resize does not start. By frame N+1, the OS resize is active and the pointer may no longer report primary_pressed.

**Fix:** Track in_resize: bool state on the app struct. Only call BeginResize when in_resize is false AND edge conditions are met. Clear the flag when the primary button is released. Alternatively, use primary_down() (held all frames) instead of primary_pressed().

---

## Bug 3: TRANSPARENCY/Glass Applies to Settings Window, Not Main App

**Location:** crates/scribe-app/src/app.rs:5983–5997

**Root Cause:**
paint_tint_overlay paints to Order::Foreground (the topmost egui layer). When Settings window is open, it renders in Order::Middle (default), so the tint paints over the Settings window interior because Foreground is the topmost layer.

**Fix:** Change the layer order from Order::Foreground to Order::Background so the tint paints behind windows, not over them.

---

## Bug 4: DUPLICATE Min/Max/Close Titlebar Buttons When Transparency ON

**Location:** crates/scribe-app/src/app.rs:668–670, 3987–4040

**Root Cause:**
When transparency is enabled, apply_window_effect() calls platform APIs (window_vibrancy) which request a transparent surface. On Windows, this causes the OS to re-add decorations (title bar, buttons) that were hidden by frameless=true. egui's frameless-rendering code still paints a second set on top.

The two states get out of sync: eframe's decorations flag vs. the OS's transparent-surface mode.

**Fix:** Sync frameless and effective_translucent() at init: pass with_decorations(!config.window.effective_translucent()) to eframe NativeOptions. Or disable custom frameless titlebar when transparency is on.

---

## Bug 5: SPELLCHECK Doesn't Underline Misspelled Words

**Location:** crates/scribe-app/src/app.rs:1393–1425 (spell_count), no underline painter

**Root Cause:**
spell_count() computes misspelling counts for the status bar, but the Vec<Misspelling> data is discarded. There is NO code that paints red squiggles or underlines on misspelled words in the editor.

**Fix:** 
1. Add field: last_misspellings: Vec<spell::Misspelling>
2. Store result at line 1423: self.last_misspellings = spell::check_text(...)
3. After TextEdit render, paint underlines from the vector using a Foreground painter

---

## Bug 6: SETTINGS Window Grows Too Large on Toolbar Page

**Location:** crates/scribe-app/src/settings.rs:1438–1456

**Root Cause:**
horizontal_wrapped() does NOT constrain desired width. It accumulates ~1200px from 15 toolbar buttons before wrapping. The Settings window expands to fit because it respects desired_width requests from children.

**Fix:** Constrain palette width with set_max_width() or use ScrollArea with explicit max_width.

---

## Bug 7: TOOLBAR Settings Page — 3 Buttons Show Empty Boxes (Tofu)

**Location:** crates/scribe-app/src/app.rs:3270–3302

**Root Cause:**
Three Phosphor Thin glyphs render as tofu:
1. "palette" → ph::COMMAND (⌘): May not exist in Phosphor Thin; it's a platform-specific symbol
2. "openfolder" → ph::FOLDER_OPEN: May not exist in the font subset
3. "lsp" → ph::PLAY: May not exist if font loading is incomplete

Root: egui_phosphor::thin may not include all codepoints, or the bundled font subset is incomplete.

**Fix:** Replace missing glyphs with alternatives known to exist in Phosphor Thin:
- "palette": Use ph::GEAR or fallback text "⌘" 
- "openfolder": Use ph::FOLDER or text "folder"
- "lsp": Use ph::LIGHTNING_CHARGE or text "lsp"

Alternatively, verify font coverage at startup; alert user or fall back to text labels if glyphs missing.

