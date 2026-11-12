# lean4-ast

Lean 4 syntax trees, parsed by Lean's own parser, callable from Rust without a
subprocess.

```rust
use klean_ast::{init, parse, Options, ParseRequest};

init(&Options::new("/home/me/.elan/toolchains/leanprover--lean4---v4.36.0-rc2"))?;
let file = parse(ParseRequest { path: "Demo.lean", source: "def two : Nat := 2\n", imports: &[] })?;
assert_eq!(file.nodes[0].short_kind(), "declaration");
# Ok::<(), klean_ast::Error>(())
```

## What it is

Three thin layers, and no parser of our own:

```
wrapper/KleanAst.lean   ~250 lines: calls Lean.Parser.parseHeader + parseCommand,
                        serialises the Syntax tree to JSON. Nothing more.
shim/…_shim.c           starts the Lean runtime, hands back JSON strings.
crates/klean-ast/       Rust: links the static library, parses the JSON, exposes
                        a small API (kind, spans, children, text, missing).
```

`build.py` puts the three together into `build/libklean_ast.a` plus
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
   archives them into `build/libklean_ast.a`, and writes `build/link.json`;
5. builds `crates/klean-ast` and runs its tests.

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

## What the parser covers

Every test in `crates/klean-ast/tests/features.rs` is one minimal snippet per
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
klean_ast::LEAN_VERSION      // e.g. "4.36.0-rc2"
klean_ast::BUILD_TOOLCHAIN   // the toolchain directory build.py used
```

Callers that own a toolchain (KLEAN does) must run **that** Lean, or re-run
`build.py` against theirs. KLEAN's `lean/toolchain.rs` implements exactly this
preference order and reports a clear error when no matching toolchain exists.

## Staying adaptable

Upstream Lean moves. Everything that depends on its layout is in one of two
places, both of which fail loudly instead of silently degrading:

| What moves | Where to fix it |
| --- | --- |
| file paths, renamed modules | `manifest.json` → `parse_sources` (globs are verified) |
| syntax kinds we map to rows | `crates/klean-ast/tests/features.rs` |
| syntax changes in the wrapper | `wrapper/KleanAst.lean` (thin: parse + JSON only) |
| link flags, library names | nothing — `leanc --print-ldflags` is asked at build time |
| the pinned version | `python3 lean4-ast/build.py --tag <new tag>` |

`build.py` fails with the pattern that matched nothing and the reason it is
listed, so a rename upstream is a one-line change in `manifest.json`.
