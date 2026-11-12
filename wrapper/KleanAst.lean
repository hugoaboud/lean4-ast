/-
  KLEAN · lean4-ast wrapper

  A thin standalone wrapper around Lean's own parser. Nothing here re-implements
  parsing: it calls `Lean.Parser.parseHeader` and `Lean.Parser.parseCommand` — the
  same functions the Lean frontend uses — and serialises the resulting `Syntax`
  tree to JSON. There is no elaboration, no type checking and no diagnostics;
  syntax errors show up as `missing` nodes, exactly as the parser produced them.

  Two functions are exported to C (see `shim/klean_ast_shim.c`):

    `klean_ast_lean_init`  — sets the module search path, reports the Lean version
    `klean_ast_lean_parse` — takes `{"path":…,"source":…,"imports":[…]}` and answers
                             `{"ok":true,"nodes":[…],"syntaxErrors":n}`

  Environments are cached per import list, so parsing the next file of a project
  does not re-load the modules it shares with the previous one.
-/
import Lean
import Lean.Elab.Frontend

open Lean

namespace KleanAst

/-- A parsed command, or the header that opened the file. -/
private structure Ctx where
  /-- Depth allowed in the JSON, so a pathological file cannot explode. -/
  maxDepth : Nat := 96
  /-- Nodes allowed in the JSON. -/
  maxNodes : Nat := 400_000

/-- Short, stable kind name: `Lean.Parser.Command.definition` → `definition`. -/
private def shortKind (kind : SyntaxNodeKind) : String :=
  match (kind.toString.splitOn ".").reverse with
  | head :: _ => head
  | [] => kind.toString

private def spanJson (stx : Syntax) : Json :=
  match stx.getPos? (canonicalOnly := true), stx.getTailPos? (canonicalOnly := true) with
  | some start, some stop => Json.arr #[toJson start.byteIdx, toJson stop.byteIdx]
  | _, _ => Json.null

/-- One syntax node as JSON: kind, byte span, and — for tokens and identifiers —
the text, plus the children. `missing` marks a token the parser had to invent
because of a syntax error. -/
private partial def nodeJson (ctx : Ctx) (budget : IO.Ref Nat) (depth : Nat) (stx : Syntax) :
    IO Json := do
  let left ← budget.modifyGet fun n => (n, n - 1)
  let text : Json ←
    match stx with
    | .atom _ value => pure (toJson value)
    | .ident _ _ name _ => pure (toJson name.toString)
    | _ => pure Json.null
  let children : Json ←
    if depth >= ctx.maxDepth || left <= 0 then
      pure (Json.arr #[])
    else do
      let mut out : Array Json := #[]
      for child in stx.getArgs do
        out := out.push (← nodeJson ctx budget (depth + 1) child)
      pure (Json.arr out)
  return Json.mkObj [
    ("kind", toJson (shortKind stx.getKind)),
    ("span", spanJson stx),
    ("text", text),
    ("missing", toJson stx.isMissing),
    ("children", children),
  ]

/-- The commands of a header, with Lean's structural `null` wrappers peeled off so
that `import` and `prelude` come out as nodes of their own, in file order. -/
private partial def headerCommands (header : Syntax) (out : Array Syntax := #[]) : Array Syntax :=
  header.getArgs.foldl (init := out) fun acc child =>
    if child.isOfKind nullKind then headerCommands child acc else acc.push child

/-- Parses a whole file: the header, then one command at a time until `eoi`.

This is `Lean.Parser` used on its own: no elaboration runs, so nothing here needs
the file to type check, and a file that does not type check still parses. -/
private def parseFile (env : Environment) (ctx : Ctx) (path source : String) :
    IO (Array Json × Nat) := do
  let inputCtx := Parser.mkInputContext source path
  let (header, mps, messages) ← Parser.parseHeader inputCtx
  let pmctx : Parser.ParserModuleContext := { env, options := {} }
  let budget ← IO.mkRef ctx.maxNodes
  let mut nodes : Array Json := #[]
  for command in headerCommands header.raw do
    nodes := nodes.push (← nodeJson ctx budget 0 command)
  let mut mps := mps
  let mut errors : Nat := messages.toList.length
  let mut keepGoing := true
  while keepGoing && (← budget.get) > 0 do
    let (stx, mps', messages) := Parser.parseCommand inputCtx pmctx mps {}
    mps := mps'
    errors := errors + messages.toList.length
    if stx.isOfKind `Lean.Parser.Command.eoi then
      keepGoing := false
    else
      nodes := nodes.push (← nodeJson ctx budget 0 stx)
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

private def jsonString (value : Json) : String := Json.compress value

private def ok (fields : List (String × Json)) : String :=
  jsonString (Json.mkObj (("ok", toJson true) :: fields))

private def fail (message : String) : String :=
  jsonString (Json.mkObj [("ok", toJson false), ("error", toJson message)])

/-- Reads one string field of a request, `""` when absent. -/
private def field (request : Json) (name : String) : String :=
  match request.getObjValD name with
  | .str value => value
  | _ => ""

private def fieldArray (request : Json) (name : String) : Array String :=
  match request.getObjValD name with
  | .arr values => values.map fun value => match value with
      | .str text => text
      | _ => ""
  | _ => #[]

/-- Parses one file. Request: `{"path","source","imports"}`, where `imports` names
the modules whose syntax extensions the file may use (`["Init","Lean"]` is enough
for a file that imports nothing). -/
private unsafe def parseRequest (requestJson : String) : IO String := do
  match Json.parse requestJson with
  | .error error => return fail s!"bad request: {error}"
  | .ok request =>
    let path := field request "path"
    let source := field request "source"
    let imports := fieldArray request "imports"
    if source.isEmpty then
      return fail "the request carried no source"
    let names := if imports.isEmpty then #[`Init, `Lean] else imports.map String.toName
    let env ← envFor names
    let (nodes, errors) ← parseFile env {} path source
    return ok [("nodes", Json.arr nodes), ("syntaxErrors", toJson errors)]

/-- Sets the module search path so that importing a project's modules works:
`sysroot` is the Lean toolchain (`…/toolchains/leanprover--lean4---v4.36.0-rc2`),
`paths` are extra module directories (a project's `.lake/build/lib/lean`).

`Lean.findSysroot` is deliberately not used: it looks for a `lean` executable
next to the host process, which is not this library's business. -/
private unsafe def initRequest (requestJson : String) : IO String := do
  match Json.parse requestJson with
  | .error error => return fail s!"bad request: {error}"
  | .ok request =>
    let sysroot := field request "sysroot"
    let extra := (fieldArray request "paths").map System.FilePath.mk
    if sysroot.isEmpty then
      return fail "the request carried no sysroot: pass the Lean toolchain directory"
    Lean.initSearchPath (System.FilePath.mk sysroot) (sp := extra.toList)
    let path ← Lean.searchPathRef.get
    return ok [
      ("version", toJson Lean.versionString),
      ("searchPath", Json.arr (path.map (fun entry => toJson entry.toString) |>.toArray)),
    ]

end KleanAst

/-- C entry point: `klean_ast_init(search_paths)` → JSON. -/
@[export klean_ast_lean_init]
unsafe def kleanAstInit (searchPaths : String) : IO String :=
  KleanAst.initRequest searchPaths

/-- C entry point: `klean_ast_parse(request_json)` → JSON. -/
@[export klean_ast_lean_parse]
unsafe def kleanAstParse (requestJson : String) : IO String :=
  KleanAst.parseRequest requestJson

/-- Temporary probe: walks the steps a first parse needs, printing each one. -/
@[export probe_steps]
unsafe def probeSteps (sysroot : String) : IO String := do
  IO.eprintln "step 1: searchPathRef.set"
  Lean.searchPathRef.set [System.FilePath.mk s!"{sysroot}/lib/lean"]
  IO.eprintln "step 2: import Init"
  Lean.enableInitializersExecution
  let env ← Lean.importModules #[{ module := `Init }] {} (loadExts := false)
  IO.eprintln s!"step 3: imported {env.header.moduleNames.toList.length} modules"
  return "ok"

/-- Temporary probe: read (not write) Lean's global search path. -/
@[export probe_get]
unsafe def probeGet (_ : String) : IO String := do
  let p ← Lean.searchPathRef.get
  return s!"search path has {p.length} entries"
