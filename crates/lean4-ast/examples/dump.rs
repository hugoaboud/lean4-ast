//! Prints the syntax tree of a file, for looking at what Lean actually produces.
//!
//! Everything printed here is read straight out of Lean's heap — no tree is
//! materialised on the way.

use lean4_ast::{init, parse, Options, ParseRequest, Syntax};

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let sysroot = std::env::var("LEAN4_AST_SYSROOT").unwrap_or_default();
    let info = init(&Options::new(sysroot)).expect("init");
    eprintln!("lean {}", info.version);

    let path = arguments.first().cloned().unwrap_or_else(|| "Demo.lean".to_string());
    let source = std::fs::read_to_string(&path).unwrap_or_else(|_| path.clone());
    let tree = parse(ParseRequest {
        path: &path,
        source: &source,
        imports: &[],
    })
    .expect("parse");
    println!("syntax errors: {}", tree.syntax_errors());
    for node in tree.nodes() {
        print_node(&node, 0, &source);
    }
}

fn print_node(node: &Syntax<'_>, depth: usize, source: &str) {
    let span = node
        .span()
        .map(|span| format!("{}..{}", span.start, span.end))
        .unwrap_or_default();
    let text = node.text().unwrap_or_default();
    let slice = if node.children().is_empty() && text.is_empty() {
        node.slice(source).lines().next().unwrap_or("").to_string()
    } else {
        String::new()
    };
    println!(
        "{}{} [{}] {}{}{}",
        "  ".repeat(depth),
        node.short_kind(),
        span,
        text,
        if slice.is_empty() { String::new() } else { format!("`{}`", slice) },
        if node.missing() { " MISSING" } else { "" }
    );
    if depth < 6 {
        for child in node.children() {
            print_node(&child, depth + 1, source);
        }
    }
}
