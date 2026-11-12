// Scratch benchmark (deleted after use).
use lean4_ast::{init, parse, Options, ParseRequest};
use std::time::Instant;

fn main() {
    let path = std::env::args().nth(1).expect("file");
    let source = std::fs::read_to_string(&path).expect("read");
    let sysroot = std::env::var("LEAN4_AST_SYSROOT").unwrap_or_default();
    let t = Instant::now();
    init(&Options::new(sysroot)).expect("init");
    println!("init      {:>9.1?}", t.elapsed());
    for round in 0..4 {
        let t = Instant::now();
        let file = parse(ParseRequest { path: &path, source: &source, imports: &[] }).expect("parse");
        println!("parse {round}   {:>9.1?}  ({} nodes, {} errors)", t.elapsed(), file.nodes.len(), file.syntax_errors);
    }
}
