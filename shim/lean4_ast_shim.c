/*
  lean4-ast C entry points

  The Rust crate talks to Lean through these functions only, and what crosses the
  boundary is a `lean_object*`: the parser's own `Syntax` tree, exactly as Lean
  built it. Nothing is serialised and nothing is copied — the caller reads Lean's
  heap directly (see `crates/lean4-ast/src/raw.rs`), which is why this file only
  marshals the arguments and starts the runtime.

  They start the Lean runtime the way Lean's own generated `main` does:

    1. the wrapper module's initializer, which initializes the runtime module and
       (through its imports) `Init`, `Lean` and the parser extensions;
    2. `lean_io_mark_end_initialization()`;
    3. `lean_init_task_manager()`.

  `build.py` passes the wrapper's initializer name with `-DLEAN4_AST_INIT_FN=…`,
  because Lean mangles it after the module it was compiled as; nothing else here
  is path- or version-dependent.

  Ownership: `lean4_ast_parse`, `lean4_ast_init` and `lean4_ast_probe` return an
  owned object (reference count 1) that the caller must release with
  `lean4_ast_free`. A `NULL` return means the Lean runtime could not start; the
  reason has been printed to stderr.
*/

#include <lean/lean.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#ifndef LEAN4_AST_INIT_FN
#define LEAN4_AST_INIT_FN initialize_Lean4Ast
#endif

/* The wrapper module's initializer, emitted by Lean's C backend. */
lean_object* LEAN4_AST_INIT_FN(uint8_t builtin);

/* The exported wrapper functions (see `@[export]` in wrapper/Lean4Ast.lean). */
lean_object* lean4_ast_lean_init(lean_object* sysroot, lean_object* paths);
lean_object* lean4_ast_lean_parse(lean_object* path, lean_object* source, lean_object* imports);
lean_object* lean4_ast_lean_probe(lean_object* unit);

static int runtime_ready = 0;

static void report(const char* what, const char* detail) {
  fprintf(stderr, "lean4-ast: %s failed: %s\n", what, detail ? detail : "(no reason given)");
}

int lean4_ast_runtime_start(void) {
  if (runtime_ready) return 0;
  lean_object* result = LEAN4_AST_INIT_FN(1 /* builtin */);
  if (lean_io_result_is_error(result)) {
    report("module initializers", NULL);
    lean_io_result_show_error(result);
    lean_dec(result);
    return 1;
  }
  lean_dec(result);
  lean_io_mark_end_initialization();
  lean_init_task_manager();
  runtime_ready = 1;
  return 0;
}

/* Takes the value out of an `IO` result, keeping one owned reference to it.
   Returns `NULL` when the `IO` itself failed, which only happens on a Lean
   panic: ordinary failures travel back as `Except.error`. */
static lean_object* io_value(lean_object* result, const char* what) {
  if (lean_io_result_is_error(result)) {
    if (getenv("LEAN4_AST_DEBUG") != NULL) {
      report(what, NULL);
      lean_io_result_show_error(result);
    }
    lean_dec(result);
    return NULL;
  }
  lean_object* value = lean_io_result_get_value(result);
  lean_inc(value);
  lean_dec(result);
  /* The caller holds this tree across threads and releases it when it is done;
     promoting it to a multi-threaded object makes the reference count atomic, so
     that hand-off is safe (the tree itself is immutable and never handed back to
     Lean). `lean_mark_mt` walks the object graph once, without allocating. */
  lean_mark_mt(value);
  return value;
}

/* An `Array String` from C strings, all copied into the Lean heap. */
static lean_object* mk_string_array(const char* const* items, size_t n) {
  lean_object* array = lean_alloc_array(n, n);
  for (size_t i = 0; i < n; i++) {
    lean_array_set_core(array, i, lean_mk_string_from_bytes(items[i], strlen(items[i])));
  }
  return array;
}

/* `lean4_ast_init(sysroot, paths, n_paths)`.

   Answers `Except String (String × Array String)`: the Lean version and the
   resolved module search path, or the reason it could not be set. */
lean_object* lean4_ast_init(const char* sysroot, const char* const* paths, size_t n_paths) {
  if (lean4_ast_runtime_start() != 0) return NULL;
  lean_object* args = mk_string_array(paths, n_paths);
  lean_object* result = lean4_ast_lean_init(
      lean_mk_string_from_bytes(sysroot ? sysroot : "", sysroot ? strlen(sysroot) : 0), args);
  return io_value(result, "init");
}

/* `lean4_ast_parse(path, source, source_len, imports, n_imports)`.

   `source` is passed with its length so that it never has to be NUL-terminated:
   the buffer is copied into a Lean `String` here, and that copy is the only one
   the whole pipeline makes.

   Answers `Except String (Array Syntax × Nat)`: the file's top-level syntax nodes
   and its syntax error count, or the reason parsing could not start. */
lean_object* lean4_ast_parse(const char* path, const char* source, size_t source_len,
                             const char* const* imports, size_t n_imports) {
  if (lean4_ast_runtime_start() != 0) return NULL;
  lean_object* args = mk_string_array(imports, n_imports);
  lean_object* result = lean4_ast_lean_parse(
      lean_mk_string_from_bytes(path ? path : "", path ? strlen(path) : 0),
      lean_mk_string_from_bytes(source ? source : "", source ? source_len : 0), args);
  return io_value(result, "parse");
}

/* `lean4_ast_probe()`: `Array Syntax` with one value per `Syntax` constructor,
   used by the Rust side to check the tags and field counts it assumes. */
lean_object* lean4_ast_probe(void) {
  if (lean4_ast_runtime_start() != 0) return NULL;
  lean_object* result = lean4_ast_lean_probe(lean_box(0));
  return io_value(result, "probe");
}

/* Releases a tree (or any other object) this library handed out. */
void lean4_ast_free(lean_object* object) {
  if (object != NULL) lean_dec(object);
}

/* What this build of Lean thinks its object layout is, for the Rust side to
   compare with the layout it compiled in. Reading another language's heap is only
   sound if both agree, and this is how they find out:

     out[0]      sizeof(lean_object)
     out[1..9]   the bytes of a header with every field set to a known value
     out[9]      offset of lean_array_object.m_size
     out[10]     offset of lean_array_object.m_data
     out[11]     offset of lean_string_object.m_data
     out[12]     offset of lean_string_object.m_size
     out[13]     offset of lean_ctor_object.m_objs
     out[14]     sizeof(void*)
     out[15]     LeanMaxCtorTag */
void lean4_ast_layout(uint64_t* out) {
  lean_object probe;
  memset(&probe, 0, sizeof(probe));
  probe.m_rc = 0x01020304;
  probe.m_cs_sz = 0x0506;
  probe.m_other = 0x07;
  probe.m_tag = 0x08;

  out[0] = sizeof(lean_object);
  for (unsigned i = 0; i < sizeof(lean_object); i++) {
    out[1 + i] = ((const uint8_t*)&probe)[i];
  }
  out[9] = offsetof(lean_array_object, m_size);
  out[10] = offsetof(lean_array_object, m_data);
  out[11] = offsetof(lean_string_object, m_data);
  out[12] = offsetof(lean_string_object, m_size);
  out[13] = offsetof(lean_ctor_object, m_objs);
  out[14] = sizeof(void*);
  out[15] = LeanMaxCtorTag;
}
