//! Prints the syntax tree of a file, for looking at what Lean actually produces.

use klean_ast::{init, parse, Options, ParseRequest};

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let sysroot = std::env::var("KLEAN_AST_SYSROOT").unwrap_or_default();
    let info = init(&Options::new(sysroot)).expect("init");
    eprintln!("lean {}", info.version);

    let path = arguments.first().cloned().unwrap_or_else(|| "Demo.lean".to_string());
    let source = std::fs::read_to_string(&path).unwrap_or_else(|_| path.clone());
    let file = parse(ParseRequest { path: &path, source: &source, imports: &[] }).expect("parse");
    println!("syntax errors: {}", file.syntax_errors);
    for node in &file.nodes {
        print_node(node, 0, &source);
    }
}

fn print_node(node: &klean_ast::Node, depth: usize, source: &str) {
    let span = node.span().map(|s| format!("{}..{}", s.start, s.end)).unwrap_or_default();
    let text = node.text.as_deref().unwrap_or("");
    let slice = if node.children.is_empty() && text.is_empty() {
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
        if node.missing { " MISSING" } else { "" }
    );
    if depth < 6 {
        for child in &node.children {
            print_node(child, depth + 1, source);
        }
    }
}
