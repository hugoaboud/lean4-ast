# lean4-ast

A Rust wrapper for the official Lean4 parser.

> This repository contains a Python script which clones the [lean4](https://github.com/leanprover/lean4) repository, and extracts all code relative to AST parsing, without elaboration, type checking, diagnostics, goals and other aspects of the lean Kernel.
> Then, it builds a simple Rust crate which allows parsing a lean file into an AST which follows Leans terms.

## Building

The `build.py` script at the root of the repo is what builds the library, following the steps below:
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
lean4-ast = { path = ".../lean4-ast" }
```
