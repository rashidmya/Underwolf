//! Offline schema dump: `dump_schema <client.dll> > schema-<ClientVersion>.json`.
//! Prints class -> field -> offset (sorted) as JSON on stdout; diagnostics go to stderr.

use std::fmt::Write;

use uw_spike_dlcollector::module::{Module, file_image};
use uw_spike_dlcollector::schema::SchemaFields;

fn json_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: dump_schema <client.dll>");
        std::process::exit(2);
    };
    let bytes = std::fs::read(&path).unwrap_or_else(|e| {
        eprintln!("{path}: {e}");
        std::process::exit(1);
    });
    let fail = |what: &str, e: &dyn std::fmt::Display| -> ! {
        eprintln!("{what}: {e}");
        std::process::exit(1)
    };
    let (mem, base) = file_image(&bytes).unwrap_or_else(|e| fail("image", &e));
    drop(bytes);
    let client =
        Module::from_headers(&mem, "client.dll", base).unwrap_or_else(|e| fail("headers", &e));
    let schema = SchemaFields::load(&mem, &client).unwrap_or_else(|e| fail("schema", &e));

    let mut out = String::from("{\n");
    let mut classes = schema.classes().iter().peekable();
    while let Some((class, fields)) = classes.next() {
        out.push_str("  ");
        json_string(&mut out, class);
        out.push_str(": {");
        let mut it = fields.iter().peekable();
        while let Some((name, offset)) = it.next() {
            json_string(&mut out, name);
            let _ = write!(out, ": {offset}");
            if it.peek().is_some() {
                out.push_str(", ");
            }
        }
        out.push('}');
        out.push_str(if classes.peek().is_some() {
            ",\n"
        } else {
            "\n"
        });
    }
    out.push_str("}\n");
    print!("{out}");

    let fields: usize = schema.classes().values().map(|f| f.len()).sum();
    eprintln!(
        "layout {}: {} classes, {fields} fields, {} ambiguous, {} unreadable",
        schema.layout_name(),
        schema.classes().len(),
        schema.ambiguous().len(),
        schema.unreadable().len()
    );
    for r in schema.ambiguous() {
        eprintln!("ambiguous: {r}");
    }
    for c in schema.unreadable() {
        eprintln!("unreadable: {c}");
    }
}
