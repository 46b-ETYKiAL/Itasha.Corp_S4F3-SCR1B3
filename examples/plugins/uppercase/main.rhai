// SCR1B3 example mod: uppercase the whole buffer.
// Drop this folder into your plugins directory — no build step required.

fn cmd_uppercase() {
    set_buffer_text(buffer_text().to_upper());
    notify("buffer uppercased");
}

// Declare a palette command: (id, label, handler-fn-name)
register_command("uppercase", "Uppercase Buffer", "cmd_uppercase");
