/*
  KLEAN · lean4-ast C entry points

  The Rust crate talks to Lean through these functions only. They start the Lean
  runtime the way Lean's own generated `main` does:

    1. the wrapper module's initializer, which initializes the runtime module and
       (through its imports) `Init`, `Lean` and the parser extensions;
    2. `lean_io_mark_end_initialization()`;
    3. `lean_init_task_manager()`.

  `build.py` passes the wrapper's initializer name with `-DKLEAN_AST_INIT_FN=…`,
  because Lean mangles it after the module it was compiled as; nothing else here
  is path- or version-dependent.

  Every string handed back is `malloc`'d and must be released with
  `klean_ast_string_free`. A `NULL` return means the Lean runtime could not start;
  the reason has been printed to stderr.
*/

#include <lean/lean.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#ifndef KLEAN_AST_INIT_FN
#define KLEAN_AST_INIT_FN initialize_KleanAst
#endif

/* The wrapper module's initializer, emitted by Lean's C backend. */
lean_object* KLEAN_AST_INIT_FN(uint8_t builtin);

/* The exported wrapper functions (see `@[export]` in wrapper/KleanAst.lean). */
lean_object* klean_ast_lean_init(lean_object* requestJson);
lean_object* klean_ast_lean_parse(lean_object* requestJson);

static int runtime_ready = 0;

static int check(lean_object* result, const char* what) {
  if (lean_io_result_is_error(result)) {
    fprintf(stderr, "lean4-ast: %s failed\n", what);
    lean_io_result_show_error(result);
    lean_dec(result);
    return 1;
  }
  lean_dec(result);
  return 0;
}

/* Runs an exported `String → IO String` function and copies its result out. */
static char* run_string(lean_object* (*fn)(lean_object*), const char* argument,
                        const char* what) {
  lean_object* input = lean_mk_string(argument);
  lean_object* result = fn(input);
  if (lean_io_result_is_error(result)) {
    // The reason travels back as JSON; only print it when debugging.
    if (getenv("KLEAN_AST_DEBUG") != NULL) {
      fprintf(stderr, "lean4-ast: %s failed\n", what);
      lean_io_result_show_error(result);
    }
    lean_dec(result);
    return NULL;
  }
  lean_object* value = lean_io_result_get_value(result);
  char* out = strdup(lean_string_cstr(value));
  lean_dec(result);
  return out;
}

int klean_ast_runtime_start(void) {
  if (runtime_ready) return 0;
  if (check(KLEAN_AST_INIT_FN(1 /* builtin */), "module initializers")) return 1;
  lean_io_mark_end_initialization();
  lean_init_task_manager();
  runtime_ready = 1;
  return 0;
}

char* klean_ast_init(const char* request_json) {
  if (klean_ast_runtime_start() != 0) return NULL;
  return run_string(klean_ast_lean_init, request_json ? request_json : "{}", "init");
}

char* klean_ast_parse(const char* request_json) {
  if (klean_ast_runtime_start() != 0) return NULL;
  return run_string(klean_ast_lean_parse, request_json ? request_json : "{}", "parse");
}

void klean_ast_string_free(char* text) { free(text); }
