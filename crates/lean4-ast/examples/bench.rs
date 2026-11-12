//! What a caller pays to get a Lean file's syntax tree, and to read it.
//!
//! ```text
//! cargo run --release --example bench -- [rounds] [--write PATH]
//! ```
//!
//! Prints, for one generated file (deterministic, so runs are comparable):
//!
//! * how long `parse` takes, round after round;
//! * how many nodes the tree has and what a full traversal costs;
//! * a digest of the whole tree, which is how a run proves it saw the same tree as
//!   another run (the pre-change pipeline, in `BENCHMARK.md`, prints the same one);
//! * how long materialising it into owned `Node`s takes, for callers that want
//!   that instead of the borrowed views;
//! * resident memory before and after, since a tree that gets copied is paid for in
//!   memory as well as in time.
//!
//! [`BENCHMARK.md`] records the same measurement against the JSON pipeline this
//! replaced, and how to rebuild that one.

use lean4_ast::{init, parse, Options, ParseRequest, Syntax};
use std::time::{Duration, Instant};

fn main() {
    let mut rounds = 5usize;
    let mut write: Option<String> = None;
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--write" => write = arguments.next(),
            other => rounds = other.parse().expect("rounds"),
        }
    }

    let source = generate();
    println!(
        "source: {} bytes, {} lines",
        source.len(),
        source.lines().count()
    );
    if let Some(path) = &write {
        std::fs::write(path, &source).expect("write");
        println!("wrote {path}");
    }

    let sysroot = std::env::var("LEAN4_AST_SYSROOT").unwrap_or_default();
    let start = Instant::now();
    let info = init(&Options::new(sysroot)).expect("init");
    println!("init: {:?} (lean {})", start.elapsed(), info.version);

    let rss_before = rss();
    let mut parse_times = Vec::new();
    let mut walk_times = Vec::new();
    let mut materialise_times = Vec::new();
    for round in 0..rounds {
        let start = Instant::now();
        let tree = parse(ParseRequest {
            path: "Bench.lean",
            source: &source,
            imports: &[],
        })
        .expect("parse");
        parse_times.push(start.elapsed());

        let after_parse = rss();
        let start = Instant::now();
        let walked = walk(&tree);
        walk_times.push(start.elapsed());

        let start = Instant::now();
        let owned = tree.to_file_ast();
        materialise_times.push(start.elapsed());
        assert_eq!(owned.nodes.len(), tree.nodes().len());

        println!(
            "  round {round}: parse {:?}, walk {:?}, materialise {:?}",
            parse_times[round], walk_times[round], materialise_times[round]
        );
        if round == 0 {
            println!(
                "top-level nodes: {}, syntax errors: {}, nodes: {}, atoms: {}, idents: {}, text bytes: {}",
                tree.nodes().len(),
                tree.syntax_errors(),
                walked.nodes,
                walked.atoms,
                walked.idents,
                walked.text_bytes
            );
            println!(
                "rss: +{} MiB while parsing (peak {} MiB), {} MiB with the tree + owned copy held",
                after_parse.saturating_sub(rss_before) / MIB,
                peak_rss() / MIB,
                rss() / MIB
            );
        }
        println!("  digest {:#018x}", walked.digest);
    }
    println!("parse:      {}", summary(&parse_times));
    println!("walk:       {}", summary(&walk_times));
    println!("materialise:{}", summary(&materialise_times));
    println!("rss after drop: {} MiB", rss() / MIB);
}

const MIB: usize = 1024 * 1024;

/// min / median / mean.
fn summary(times: &[Duration]) -> String {
    let mut sorted = times.to_vec();
    sorted.sort();
    let mean = times.iter().sum::<Duration>() / times.len() as u32;
    format!(
        "{:>12?} (min {:?}, median {:?}, mean {:?})",
        sorted[sorted.len() / 2],
        sorted[0],
        sorted[sorted.len() / 2],
        mean
    )
}

#[derive(Default)]
struct Walked {
    nodes: usize,
    atoms: usize,
    idents: usize,
    text_bytes: u64,
    digest: u64,
}

/// A full traversal: every node's kind, span, text and missing flag, hashed.
///
/// `BENCHMARK.md`'s pre-change harness computes the same digest over the same
/// file, so equal digests mean both pipelines saw the same tree.
fn walk(tree: &lean4_ast::ParseTree) -> Walked {
    let mut walked = Walked {
        digest: 0xcbf29ce484222325,
        ..Walked::default()
    };
    let mut pending: Vec<Syntax<'_>> = tree.nodes().iter().rev().collect();
    while let Some(node) = pending.pop() {
        walked.nodes += 1;
        match node {
            Syntax::Atom(_) => walked.atoms += 1,
            Syntax::Ident(_) => walked.idents += 1,
            _ => {}
        }
        fnv(&mut walked.digest, node.short_kind().as_bytes());
        if let Some(span) = node.span() {
            fnv(&mut walked.digest, &span.start.to_le_bytes());
            fnv(&mut walked.digest, &span.end.to_le_bytes());
        }
        if let Some(text) = node.text() {
            walked.text_bytes += text.len() as u64;
            fnv(&mut walked.digest, text.as_bytes());
        }
        fnv(&mut walked.digest, &[node.missing() as u8]);
        for child in node.children().iter().rev() {
            pending.push(child);
        }
    }
    walked
}

fn fnv(hash: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *hash ^= *byte as u64;
        *hash = hash.wrapping_mul(0x100000001b3);
    }
}

fn rss() -> usize {
    let status = std::fs::read_to_string("/proc/self/statm").unwrap_or_default();
    let pages: usize = status
        .split_whitespace()
        .nth(1)
        .and_then(|field| field.parse().ok())
        .unwrap_or(0);
    pages * 4096
}

fn peak_rss() -> usize {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            if let Some(kib) = rest.split_whitespace().next().and_then(|v| v.parse::<usize>().ok()) {
                return kib * 1024;
            }
        }
    }
    0
}

/// A large Lean file: `blocks` copies of a realistic mix of declarations.
///
/// Deterministic, so the same file is measured before and after the change.
fn generate() -> String {
    // `BENCH_BLOCKS` scales the file, so the cost can be attributed: fixed
    // per-parse cost versus per-byte cost.
    let blocks: usize = std::env::var("BENCH_BLOCKS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(400);
    let mut out = String::with_capacity(blocks * 2_000);
    out.push_str("prelude\n\nnamespace Bench\n\n");
    out.push_str(
        "structure Point where\n  x : Nat\n  y : Nat\n  deriving Repr\n\n\
         inductive Colour where\n  | red\n  | green\n  | blue\n\n\
         class Pretty (α : Type) where\n  pretty : α → String\n\n",
    );
    for block in 0..blocks {
        out.push_str(&format!(
            "def add{block} (a b : Nat) : Nat := a + b\n\n\
             theorem add_zero{block} (n : Nat) : add{block} 0 n = n := by\n  simp [add{block}]\n\n\
             def term{block} (p : Point) : Nat :=\n  let q := {{ p with x := p.x + {block} }}\n  \
             match q.x with\n  | 0 => p.y\n  | n + 1 => n * 2 + p.y\n\n\
             def checked{block} (xs : List Nat) : Option Nat := do\n  let ys ← xs.mapM (fun x => pure (x + {block}))\n  \
             return ys.foldl (· + ·) 0\n\n\
             def show{block} (c : Colour) : String :=\n  match c with\n  | .red => \"r\"\n  | .green => \"g\"\n  \
             | .blue => \"b\"\n\n\
             def calc{block} (a b c : Nat) (h₁ : a = b) (h₂ : b = c) : a = c := by\n  calc a = b := h₁\n    \
             _ = c := h₂\n\n\
             def anon{block} : Nat :=\n  let f := fun x => x * 2\n  let g := fun x => x + 1\n  f (g {block})\n\n\
             syntax \"prove\" term : tactic\n\n\
             macro \"prove\" t:term : tactic => `(tactic| exact $t)\n\n\
             @[simp] def tagged{block} : Nat := {block}\n\n"
        ));
    }
    out.push_str("end Bench\n");
    out
}
