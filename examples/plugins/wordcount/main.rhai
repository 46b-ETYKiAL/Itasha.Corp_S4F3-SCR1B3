// SCR1B3 example mod: word count on save + on demand.

fn report() {
    let n = buffer_text().split(" ").len();
    notify("words: " + n);
}

register_command("wordcount", "Count Words", "report");
on_event("save", "report");
