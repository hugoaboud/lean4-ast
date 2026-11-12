#!/usr/bin/env python3
"""Builds lean4-ast: a static library that parses Lean 4 into a syntax tree.

What it does, in order:

  1. resolves the lean4 tag to build against (`--tag`, else the newest tag in the
     repository, release candidates included);
  2. clones that tag into `.cache/lean4-<tag>` and extracts the files that decide
     what the parser can parse — `manifest.json` lists them as globs, so a rename
     upstream is a one-line fix there;
  3. makes sure an elan toolchain for the tag exists (`lean-toolchain` in the
     checkout says which; installing one is opt-in with `--install-toolchain`);
  4. builds `wrapper/KleanAst.lean` as a Lake library, compiles the generated C
     and the C shim, and archives both into `build/libklean_ast.a`;
  5. writes `build/link.json` for the Rust crate — archive, include and library
     directories, `leanc`'s own flags, and the mangled initializer name;
  6. builds and tests `crates/klean-ast`, whose tests document the language
     features the wrapper covers.

Every step prints what it did; `--force` repeats steps that are already done, and
`--verify-only` checks the extraction and the manifest without building.

Usage:
    python3 lean4-ast/build.py                 # full build + tests
    python3 lean4-ast/build.py --tag v4.36.0-rc2
    python3 lean4-ast/build.py --skip-tests
    python3 lean4-ast/build.py --verify-only
"""

from __future__ import annotations

import argparse
import glob
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
CACHE = HERE / ".cache"
BUILD = HERE / "build"
LEAN_DIR = HERE / "lean"          # the Lake package holding the wrapper
CRATE = HERE / "crates" / "klean-ast"
MANIFEST = HERE / "manifest.json"


def log(message: str) -> None:
    print(f"\033[1m[lean4-ast]\033[0m {message}", flush=True)


def fail(message: str) -> "NoReturn":  # noqa: F821 - forward reference for clarity
    print(f"\033[31m[lean4-ast] {message}\033[0m", file=sys.stderr, flush=True)
    sys.exit(1)


def run(command: list[str], cwd: Path | None = None, capture: bool = False) -> str:
    result = subprocess.run(
        command,
        cwd=str(cwd) if cwd else None,
        capture_output=capture,
        text=True,
    )
    if capture:
        if result.returncode != 0:
            fail(f"command failed: {' '.join(command)}\n{result.stdout}{result.stderr}")
        return result.stdout
    if result.returncode != 0:
        fail(f"command failed: {' '.join(command)}")
    return ""


# --------------------------------------------------------------------- version


def version_key(tag: str) -> tuple:
    """Sort key for tags: v4.36.0-rc2 sorts above v4.35.0, below v4.36.0."""
    match = re.fullmatch(r"v(\d+)\.(\d+)\.(\d+)(?:-rc(\d+))?", tag)
    if not match:
        return (0, 0, 0, 0)
    major, minor, patch, rc = match.groups()
    # A release candidate sorts *after* nothing but *before* the release itself:
    # rc2 < rc10 < release, encoded by giving the release a high rc number.
    return (int(major), int(minor), int(patch), int(rc) if rc else 10**6)


def available_tags(repo: str) -> list[str]:
    output = run(["git", "ls-remote", "--tags", "--refs", repo], capture=True)
    tags = []
    for line in output.splitlines():
        ref = line.split("\t")[-1].strip()
        if ref.startswith("refs/tags/"):
            tags.append(ref.removeprefix("refs/tags/"))
    if not tags:
        fail(f"no tags found at {repo}")
    return sorted(tags, key=version_key)


def resolve_tag(repo: str, requested: str | None, stable_only: bool) -> str:
    tags = available_tags(repo)
    if requested:
        if requested not in tags:
            fail(f"tag {requested} does not exist (latest: {tags[-1]})")
        return requested
    candidates = [tag for tag in tags if version_key(tag) != (0, 0, 0, 0)]
    if stable_only:
        candidates = [tag for tag in candidates if "-rc" not in tag]
    if not candidates:
        fail("no version tags found")
    return candidates[-1]


# ------------------------------------------------------------------- checkout


def ensure_checkout(repo: str, tag: str, force: bool) -> Path:
    checkout = CACHE / f"lean4-{tag}"
    if force and checkout.exists():
        shutil.rmtree(checkout)
    if checkout.exists():
        log(f"checkout {checkout.relative_to(HERE)} already there")
        return checkout
    CACHE.mkdir(parents=True, exist_ok=True)
    log(f"cloning {repo} at {tag}")
    run(["git", "clone", "--depth", "1", "--branch", tag, repo, str(checkout)])
    return checkout


def extract(checkout: Path, tag: str, force: bool) -> dict:
    """Copies the parse-relevant files out of the checkout.

    Nothing is compiled from these files: the toolchain already has them as
    oleans, which is what the library actually parses with. Copying them out is
    how the build records *what* it depends on, verifies the layout has not moved
    under us, and gives the tests something to point at.
    """
    manifest = json.loads(MANIFEST.read_text())
    target = BUILD / f"extracted-{tag}"
    if force and target.exists():
        shutil.rmtree(target)
    target.mkdir(parents=True, exist_ok=True)

    extracted: list[dict] = []
    problems: list[str] = []
    for entry in manifest["parse_sources"]:
        pattern = entry["pattern"]
        matches = sorted(
            path for path in checkout.glob(pattern) if path.is_file()
        )
        if not matches:
            problems.append(
                f"pattern matched nothing: {pattern}\n"
                f"    ({entry['why']})\n"
                f"    → update `parse_sources` in {MANIFEST.name}"
            )
            continue
        for path in matches:
            relative = path.relative_to(checkout)
            destination = target / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(path, destination)
            extracted.append(
                {
                    "path": str(relative),
                    "bytes": path.stat().st_size,
                    "sha256": hashlib.sha256(path.read_bytes()).hexdigest()[:16],
                }
            )

    inventory = {}
    for relative in manifest["syntax_inventory"]["files"]:
        source = checkout / relative
        if not source.is_file():
            problems.append(
                f"syntax inventory file missing: {relative}\n"
                f"    → update `syntax_inventory` in {MANIFEST.name}"
            )
            continue
        text = source.read_text(errors="replace")
        inventory[relative] = {
            "syntax_declarations": len(re.findall(r"^\s*syntax\b", text, re.MULTILINE)),
            "macro_declarations": len(re.findall(r"^\s*macro\b", text, re.MULTILINE)),
            "notation_declarations": len(
                re.findall(r"^\s*(?:notation|infix|prefix|postfix)\b", text, re.MULTILINE)
            ),
        }

    if problems:
        fail("\n".join(["the lean4 layout changed:"] + problems))

    (target / "inventory.json").write_text(
        json.dumps(
            {"tag": tag, "files": len(extracted), "syntax": inventory},
            indent=2,
        )
    )
    log(f"extracted {len(extracted)} files to {target.relative_to(HERE)}")
    return {"files": len(extracted), "dir": str(target), "inventory": inventory}


# ------------------------------------------------------------------ toolchain


def toolchain_name(checkout: Path, tag: str) -> str:
    """The published toolchain for the tag, e.g. `leanprover/lean4:v4.36.0-rc2`.

    The checkout's own `lean-toolchain` names the *stage-1* toolchain Lean uses to
    build itself (`build/release/stage1`), which is not something elan can fetch.
    A released tag always has `leanprover/lean4:<tag>` on the release page, so
    that is the channel the library is built and run with.
    """
    path = checkout / "lean-toolchain"
    if path.is_file():
        declared = path.read_text().strip()
        if declared.startswith("leanprover/"):
            return declared
    return f"leanprover/lean4:{tag}"


def elan_home() -> Path:
    return Path(os.environ.get("ELAN_HOME", Path.home() / ".elan"))


def installed_toolchains() -> list[str]:
    directory = elan_home() / "toolchains"
    if not directory.is_dir():
        return []
    return sorted(path.name for path in directory.iterdir() if path.is_dir())


def toolchain_for(name: str) -> Path | None:
    """Maps `leanprover/lean4:v4.36.0-rc2` to its directory under elan."""
    version = name.split(":", 1)[-1]
    sanitized = "leanprover--lean4---" + re.sub(r"[/:]", "-", version)
    directory = elan_home() / "toolchains" / sanitized
    return directory if (directory / "bin" / "lean").is_file() else None


def ensure_toolchain(name: str, install: bool) -> Path:
    directory = toolchain_for(name)
    if directory:
        log(f"toolchain {name} → {directory}")
        return directory
    if install:
        log(f"installing toolchain {name} (this downloads ~200 MB)")
        run(["elan", "toolchain", "install", name])
        directory = toolchain_for(name)
        if directory:
            return directory
        fail(f"elan installed {name} but it cannot be found")
    available = ", ".join(installed_toolchains()) or "none"
    fail(
        f"toolchain {name} is not installed (installed: {available}).\n"
        f"    → `elan toolchain install {name}`\n"
        f"    → or rerun with --install-toolchain"
    )
    raise AssertionError("unreachable")


# ---------------------------------------------------------------------- build


def leanc(toolchain: Path, *args: str) -> str:
    return run([str(toolchain / "bin" / "leanc"), *args], capture=True)


def build_library(toolchain: Path, tag: str, force: bool) -> dict:
    BUILD.mkdir(parents=True, exist_ok=True)
    # The Lake package is what compiles the wrapper: a module of a package gets a
    # library-style initializer, which is what a host process needs.
    LEAN_DIR.mkdir(parents=True, exist_ok=True)
    shutil.copy2(HERE / "wrapper" / "KleanAst.lean", LEAN_DIR / "KleanAst.lean")
    (LEAN_DIR / "lean-toolchain").write_text(toolchain.name + "\n")
    (LEAN_DIR / "lakefile.toml").write_text(
        'name = "klean-ast"\nversion = "0.1.0"\n\n[[lean_lib]]\nname = "KleanAst"\n'
    )

    log("building the wrapper with lake")
    run([str(toolchain / "bin" / "lake"), "build", "KleanAst"], cwd=LEAN_DIR)

    generated_c = LEAN_DIR / ".lake" / "build" / "ir" / "KleanAst.c"
    if not generated_c.is_file():
        fail(f"lake did not produce {generated_c}")
    initializer = re.search(
        r"LEAN_EXPORT lean_object\* (initialize_[A-Za-z0-9_]+)", generated_c.read_text()
    )
    if not initializer:
        fail("cannot find the wrapper's module initializer in the generated C")
    init_fn = initializer.group(1)

    cflags = leanc(toolchain, "--print-cflags").split()
    # `leanc` adds the toolchain's own `lib` directory implicitly (it holds
    # libc++, libuv and libgmp); a plain linker command has to be told.
    ldflags = ["-L", str(toolchain / "lib"), *leanc(toolchain, "--print-ldflags").split()]

    objects = []
    for source, name in (
        (generated_c, "KleanAst.o"),
        (HERE / "shim" / "klean_ast_shim.c", "shim.o"),
    ):
        obj = BUILD / name
        log(f"compiling {source.name}")
        extra = [f"-DKLEAN_AST_INIT_FN={init_fn}"] if name == "shim.o" else []
        run(["cc", "-c", str(source), "-o", str(obj), *cflags, *extra, "-O2"])

        objects.append(obj)

    archive = BUILD / "libklean_ast.a"
    if archive.exists():
        archive.unlink()
    log(f"archiving {archive.name}")
    run(["ar", "rcs", str(archive), *[str(obj) for obj in objects]])

    link = {
        "tag": tag,
        "lean_version": toolchain.name,
        "lean_semver": tag.removeprefix("v"),
        "toolchain": str(toolchain),
        "static_lib": str(archive),
        "init_fn": init_fn,
        "include_dirs": [str(toolchain / "include")],
        "lib_dirs": [str(toolchain / "lib" / "lean"), str(toolchain / "lib")],
        "cflags": cflags,
        "ldflags": ldflags,
    }
    (BUILD / "link.json").write_text(json.dumps(link, indent=2))
    log(f"wrote {BUILD.name}/link.json")
    return link


def build_crate(link: dict, skip_tests: bool) -> None:
    environment = dict(os.environ)
    environment["KLEAN_AST_LINK"] = str(BUILD / "link.json")
    environment["KLEAN_AST_SYSROOT"] = link["toolchain"]
    log("building the Rust crate")
    subprocess.run(["cargo", "build", "--quiet"], cwd=CRATE, env=environment, check=True)
    if skip_tests:
        return
    log("running the crate's tests")
    subprocess.run(
        ["cargo", "test", "--quiet", "--", "--test-threads=1"],
        cwd=CRATE,
        env=environment,
        check=True,
    )


# ----------------------------------------------------------------------- main


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tag", help="lean4 tag to build against (default: the newest)")
    parser.add_argument("--stable", action="store_true", help="newest tag without a release candidate")
    parser.add_argument("--force", action="store_true", help="redo the clone, extraction and build")
    parser.add_argument("--install-toolchain", action="store_true", help="let elan install the toolchain")
    parser.add_argument("--skip-tests", action="store_true", help="do not run the crate's tests")
    parser.add_argument("--verify-only", action="store_true", help="check the extraction and exit")
    options = parser.parse_args()

    manifest = json.loads(MANIFEST.read_text())
    repo = manifest["repo"]
    tag = resolve_tag(repo, options.tag, options.stable)
    log(f"latest tag: {tag}")

    checkout = ensure_checkout(repo, tag, options.force)
    extracted = extract(checkout, tag, options.force)
    toolchain = ensure_toolchain(toolchain_name(checkout, tag), options.install_toolchain)
    log(f"extracted files: {extracted['files']}")

    if options.verify_only:
        log("verify only: no build")
        return

    link = build_library(toolchain, tag, options.force)
    build_crate(link, options.skip_tests)
    log(
        f"done · tag {tag} · Lean {link['lean_version']} · "
        f"{extracted['files']} extracted files · {Path(link['static_lib']).name}"
    )


if __name__ == "__main__":
    main()
