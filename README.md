# lean4-ast
`v4.36.0-rc2`

A Rust wrapper for the official Lean4 parser.

<!-- TOC -->

- [lean4-ast](#lean4-ast)
  - [How it Works](#how-it-works)
  - [Building](#building)
  - [Installing](#installing)
  - [Usage](#usage)
    - [Borrow](#borrow)
    - [Copy](#copy)
    - [Example](#example)
    - [Benchmark](#benchmark)

<!-- /TOC -->

## How it Works

This repository contains a Python script which clones the [lean4](https://github.com/leanprover/lean4) repository and extracts all code relative to AST parsing, without elaboration, type checking, diagnostics, goals and other aspects of the lean Kernel.

Then, it builds a simple Rust crate able to parse a lean file into the syntax tree produced by Lean's parser, read straight out of Lean's heap.

## Building

The `build.py` script at the root of the repo is what builds the crate, following the steps below:

1. Resolve the newest or specified tag from `leanprover/lean4`
2. Clone tag and extract relevant .lean files
3. Find and/or install the lean toolchain that matches the tag
4. Build a Lake library, then compile it along with a shim as a C library
5. Build a Rust crate from the C library, and run it's tests

```bash
cd lean4-ast/
python3 build.py                      # newest tag
python3 build.py --tag v4.36.0-rc2
python3 build.py --stable             # newest tag without an -rc
python3 build.py --verify-only        # just check the extraction
python3 build.py --install-toolchain  # also installs toolchain if missing
```

## Installing

On your project's `Cargo.toml`:
```toml
[dependencies]
lean4-ast = { path = ".../lean4-ast/crates/lean4-ast" }
```


## Usage

The library must be initialized with a folder containing the `.olean` files of the version for which it was built.
These files contain fundamental pre-compiled axioms, which are needed to correctly parse any lean file.

```rust
use lean4_ast::{init, parse, Options, ParseRequest};

init(&Options::new("/home/me/.elan/toolchains/leanprover--lean4---v4.36.0-rc2"))?;

let tree = parse(ParseRequest {
  path: "Demo.lean",
  source: "def two : Nat := 2\n",
  imports: &[],
})?;
```

### Borrow

An AST view can be borrowed directly from Lean's heap:

```rust
// Borrowed views onto Lean's heap.
let command = tree.nodes().get(0).expect("a command");
assert_eq!(command.short_kind(), "declaration");

for node in command.children() {
    println!("{} {:?}", node.short_kind(), node.span());
}
```

### Copy

An AST view can be copied to the application memory:

```rust
// Or an owned copy, if you would rather not hold on to Lean's tree.
let owned = tree.to_file_ast();
assert_eq!(owned.nodes[0].short_kind(), "declaration");
```

### Example

You can run the example with:
```sh
cd crates/lean4-ast

LEAN4_AST_SYSROOT=~/.elan/toolchains/leanprover--lean4---v4.36.0-rc2 \
  cargo run --release --example dump -- /path/to/File.lean
```

### Benchmark

You can run a benchmark:

```bash
cd crates/lean4-ast
LEAN4_AST_SYSROOT=… cargo run --release --example bench -- 5
```