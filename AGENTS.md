# lean4-ast

Lean 4 syntax trees, parsed by Lean's own parser, callable from Rust without a
subprocess — and read where Lean left them.

## What it is

Three thin layers, and no parser of our own:

```
wrapper/Lean4Ast.lean   ~150 lines: calls Lean.Parser.parseHeader + parseCommand
                        and hands the `Syntax` tree back as it is. Nothing more.
shim/…_shim.c           starts the Lean runtime, passes the tree's objects across.
crates/lean4-ast/       Rust: links the static library and reads Lean's heap
                        directly (kind, spans, children, text, missing).
```

There is no intermediate representation: the tree Lean's parser built is the tree
the caller reads. `crates/lean4-ast/src/raw.rs` states Lean's object layout in
Rust, `src/syntax.rs` is the borrowed view over it, and
`crates/lean4-ast/src/materialize.rs` copies it into owned `Node`s for callers
that want it. Nothing is serialised on the way, which is what
[`BENCHMARK.md`](BENCHMARK.md) measures.

`build.py` puts the three together into `build/liblean4_ast.a` plus
`build/link.json`, and the crate's `build.rs` reads that file so no Lean path or
flag is ever hard-coded in Rust.

## Building

```bash
python3 lean4-ast/build.py                 # clone, extract, build, test
python3 lean4-ast/build.py --tag v4.36.0-rc2
python3 lean4-ast/build.py --stable        # newest tag without an -rc
python3 lean4-ast/build.py --verify-only   # just check the extraction
python3 lean4-ast/build.py --install-toolchain
```

What it does:

1. resolves the tag — the newest in `leanprover/lean4` unless `--tag` says otherwise;
2. clones that tag into `.cache/lean4-<tag>` and copies the parse-relevant files
   into `build/extracted-<tag>/` (see [`manifest.json`](manifest.json));
3. finds the elan toolchain for the tag (`leanprover/lean4:<tag>`), installing it
   only if `--install-toolchain` is given;
4. builds the wrapper as a Lake library, compiles the generated C plus the shim,
   archives them into `build/liblean4_ast.a`, and writes `build/link.json`;
5. builds `crates/lean4-ast` and runs its tests.

### Why a static library

Lean's parser needs the whole Lean library — the `Syntax` type, the macro
expander, the attribute tables that hold `syntax` extensions. Two things make
that embeddable:

- **Static linking.** `leanc --print-ldflags` links Lean's own archives
  (`libInit.a`, `libLean.a`, `libleancpp.a`, `libleanrt.a`, …) rather than
  `libleanshared.so`. `build.py` records those flags in `link.json`, and the
  consumers pass them on verbatim.
- **Lean's own startup order.** The generated module initializer, then
  `lean_io_mark_end_initialization()`, then `lean_init_task_manager()`, exactly as
  a generated `main` does — plus `-rdynamic`, without which Lean's interpreter
  cannot resolve the compiled symbols it needs while loading modules.
- **An executable, never a shared library.** Lean's runtime is built with
  initial-exec TLS, so the linker rejects it inside a `-shared` object
  (`R_X86_64_TPOFF32 … cannot be used with -shared`). A consumer therefore links
  the binary and must not list `cdylib` in its `crate-type`; `link.json` is meant
  for executable targets only.

### Reading Lean's heap

A `Syntax` tree handed to Rust is a graph of Lean objects: a header, then a tag
(`missing` 0, `node` 1, `atom` 2, `ident` 3), then the constructor's fields.
Reading it from another language is only sound if both sides agree on that
layout, so the crate checks rather than assumes:

- `lean4_ast_layout` has the C side report `sizeof`/`offsetof` and the bytes of a
  header with known field values; `raw::check` compares them with what Rust
  compiled in;
- the wrapper exports one `Syntax` per constructor (`lean4_ast_lean_probe`), so
  the tags and field counts are checked against the tree Lean really produces;
- the shim promotes each tree with `lean_mark_mt` before handing it over, which
  makes its reference counts atomic — a `ParseTree` can then be moved to another
  thread and dropped there, and the tree is never handed back to Lean.

Both checks run once, on the first call, and fail loudly. A mismatch means the
static library was built against another Lean; rebuild it with `build.py`.

## What the parser covers

Every test in `crates/lean4-ast/tests/features.rs` is one minimal snippet per
language feature — definitions, theorems with tactic proofs, structures with
fields, inductives with constructors, classes and instances, namespaces,
`sections` and `mutual` blocks, `where` declarations, doc comments, notation and
mixfix, `syntax`/`macro_rules` quotations, `open`/`export`/`set_option`/`attribute`
/`variable`/`universe`, `#check`/`#eval`, imports and preludes, lambdas, `match`,
`do`, `let`/`if`, `calc`, literals, Unicode identifiers, and comments. Together
they are the documentation of what is supported: run them to see what a future
Lean changed.

Deliberately **out of scope**: elaboration, type checking, diagnostics, goals.
The library answers one question — *what did Lean's parser make of this text* —
and syntax errors show up as `missing` nodes, which is what the parser produced.

## Version pinning

The static library is built against one Lean version and can only read that
version's `.olean` files (their headers carry the version). The crate therefore
exposes the version it was built with:

```rust
lean4_ast::LEAN_VERSION      // e.g. "4.36.0-rc2"
lean4_ast::BUILD_TOOLCHAIN   // the toolchain directory build.py used
```

Callers that own a toolchain (LEAN4-AST does) must run **that** Lean, or re-run
`build.py` against theirs. LEAN4-AST's `lean/toolchain.rs` implements exactly this
preference order and reports a clear error when no matching toolchain exists.

## Staying adaptable

Upstream Lean moves. Everything that depends on its layout is in one of two
places, both of which fail loudly instead of silently degrading:

| What moves | Where to fix it |
| --- | --- |
| file paths, renamed modules | `manifest.json` → `parse_sources` (globs are verified) |
| syntax kinds we map to rows | `crates/lean4-ast/tests/features.rs` |
| syntax changes in the wrapper | `wrapper/Lean4Ast.lean` (thin: parse, hand over) |
| the `Syntax` inductive's shape | the probe in `wrapper/Lean4Ast.lean` fails the first call |
| the runtime's object layout | `crates/lean4-ast/src/raw.rs` fails the first call |
| link flags, library names | nothing — `leanc --print-ldflags` is asked at build time |
| the pinned version | `python3 lean4-ast/build.py --tag <new tag>` |

`build.py` fails with the pattern that matched nothing and the reason it is
listed, so a rename upstream is a one-line change in `manifest.json`.
