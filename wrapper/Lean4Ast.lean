/-
  lean4-ast wrapper

  A thin standalone wrapper around Lean's own parser. Nothing here re-implements
  parsing, and nothing is serialised: the functions below call
  `Lean.Parser.parseHeader` and `Lean.Parser.parseCommand` — the same functions the
  Lean frontend uses — and hand the resulting `Syntax` values back to the caller
  as they are. The Rust crate reads those objects straight out of Lean's heap (see
  `crates/lean4-ast/src/raw.rs`), so a file is never copied into an intermediate
  representation. There is no elaboration, no type checking and no diagnostics;
  syntax errors show up as `missing` nodes, exactly as the parser produced them.

  Three functions are exported to C (see `shim/lean4_ast_shim.c`):

    `lean4_ast_lean_init`  — sets the module search path, reports the Lean version
    `lean4_ast_lean_parse` — parses a file: path, source and import list in,
                             `Array Syntax` plus the syntax error count out
    `lean4_ast_lean_probe` — one `Syntax` per constructor, which is how the Rust
                             side verifies the object tags and field counts it reads

  Every answer is an `Except`: `.error` carries the reason as a `String`, so a
  failure needs no side channel.

  Environments are cached per import list, so parsing the next file of a project
  does not re-load the modules it shares with the previous one.
-/
import Lean
import Lean.Elab.Frontend

open Lean

namespace Lean4Ast

/-- The commands of a header, with Lean's structural `null` wrappers peeled off so
that `import` and `prelude` come out as nodes of their own, in file order. -/
private partial def headerCommands (header : Syntax) (out : Array Syntax := #[]) : Array Syntax :=
  header.getArgs.foldl (init := out) fun acc child =>
    if child.isOfKind nullKind then headerCommands child acc else acc.push child

/-- Parses a whole file: the header, then one command at a time until `eoi`.
Returns the top-level syntax nodes and how many syntax errors Lean reported.

This is `Lean.Parser` used on its own: no elaboration runs, so nothing here needs
the file to type check, and a file that does not type check still parses. -/
private def parseFile (env : Environment) (path source : String) : IO (Array Syntax × Nat) := do
  let inputCtx := Parser.mkInputContext source path
  let (header, mps, messages) ← Parser.parseHeader inputCtx
  let pmctx : Parser.ParserModuleContext := { env, options := {} }
  let mut nodes : Array Syntax := #[]
  for command in headerCommands header.raw do
    nodes := nodes.push command
  let mut mps := mps
  let mut errors : Nat := messages.toList.length
  let mut keepGoing := true
  while keepGoing do
    let (stx, mps', messages) := Parser.parseCommand inputCtx pmctx mps {}
    mps := mps'
    errors := errors + messages.toList.length
    if stx.isOfKind `Lean.Parser.Command.eoi then
      keepGoing := false
    else
      nodes := nodes.push stx
  return (nodes, errors)

/-- Environments by import list: the expensive part of parsing is loading the
modules that define the syntax extensions, so it is done once per list.

An environment for a large project (Mathlib-scale imports) holds hundreds of
megabytes, so only a few are kept: loading one is paid again rather than letting
a session grow without bound. Keeping the count at hand is what keeps a project
open from consuming the whole machine. -/
initialize envCache : IO.Ref (Std.HashMap String Environment) ← IO.mkRef {}
initialize envOrder : IO.Ref (Array String) ← IO.mkRef #[]

/-- How many import-list environments are remembered at a time. -/
private def envCacheSize : Nat := 2

private def importKey (imports : Array Name) : String :=
  String.intercalate "," (imports.map (fun name => name.toString)).toList

private unsafe def envFor (imports : Array Name) : IO Environment := do
  let key := importKey imports
  if let some env := (← envCache.get)[key]? then
    return env
  -- Initializers run once per process, so they are re-armed for every list.
  Lean.enableInitializersExecution
  let env ← Lean.importModules (imports.map (fun name => { module := name : Import })) {}
    (loadExts := true)
  -- Remember this one, and drop the oldest when the budget is full. `HashMap`
  -- has no eviction order of its own, so the insertion order is kept beside it.
  let mut order ← envOrder.get
  order := order.push key
  let mut cache ← envCache.get
  cache := cache.insert key env
  while order.size > envCacheSize do
    match order[0]? with
    | none => break
    | some oldest =>
      cache := cache.erase oldest
      order := order.drop 1
  envCache.set cache
  envOrder.set order
  return env

end Lean4Ast

open Lean4Ast

/-- C entry point: `lean4_ast_lean_init(sysroot, paths)`.

Sets the module search path so that importing a project's modules works:
`sysroot` is the Lean toolchain (`…/toolchains/leanprover--lean4---v4.36.0-rc2`),
`paths` are extra module directories (a project's `.lake/build/lib/lean`).
Answers `Except.error` (nothing to carry on with), or the Lean version paired with
the resolved search path.

`Lean.findSysroot` is deliberately not used: it looks for a `lean` executable
next to the host process, which is not this library's business. -/
@[export lean4_ast_lean_init]
unsafe def leanAstInit (sysroot : String) (paths : Array String) :
    IO (Except String (String × Array String)) := do
  if sysroot.isEmpty then
    return .error "no sysroot: pass the Lean toolchain directory"
  Lean.initSearchPath (System.FilePath.mk sysroot) (sp := paths.toList.map System.FilePath.mk)
  let path ← Lean.searchPathRef.get
  return .ok (Lean.versionString, (path.map (fun entry => entry.toString)).toArray)

/-- C entry point: `lean4_ast_lean_parse(path, source, imports)`.

`imports` names the modules whose syntax extensions the file may use (`Init` and
`Lean` are assumed when it is empty, which is enough for a file that imports
nothing). Answers `Except.error`, or the file's top-level syntax nodes together
with the number of syntax errors the parser reported. -/
@[export lean4_ast_lean_parse]
unsafe def leanAstParse (path : String) (source : String) (imports : Array String) :
    IO (Except String (Array Syntax × Nat)) := do
  if source.isEmpty then
    return .error "the request carried no source"
  let names := if imports.isEmpty then #[`Init, `Lean] else imports.map String.toName
  let env ← envFor names
  let (nodes, errors) ← parseFile env path source
  return .ok (nodes, errors)

/-- C entry point: `lean4_ast_lean_probe()`.

One `Syntax` value per constructor, in declaration order. The Rust side matches
them against the object tags it assumes (`missing`, `node`, `atom`, `ident`) and
against the number of fields each carries, so a Lean version that reorders or
reshapes the constructors is caught at startup instead of read as garbage. -/
@[export lean4_ast_lean_probe]
unsafe def leanAstProbe (_ : Unit) : IO (Array Syntax) :=
  return #[Syntax.missing, Syntax.node .none `probe #[], Syntax.atom .none "",
    Syntax.ident .none (default : Substring.Raw) `probe []]
