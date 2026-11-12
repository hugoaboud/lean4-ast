//! What the wrapper covers: one minimal test per language feature.
//!
//! These are the documentation for `lean4-ast`. Each test parses a snippet small
//! enough to read and asserts on the *syntax kinds* Lean produced, so a future
//! Lean change that renames or restructures a construct shows up here first.
//!
//! Only the parser runs: no elaboration, no imports beyond `Init`/`Lean`, so a
//! snippet never needs to type check. Run them with
//!
//! ```text
//! python3 lean4-ast/build.py            # builds the library, then runs these
//! KLEAN_AST_SYSROOT=… cargo test -p klean-ast
//! ```

use klean_ast::{init, parse, FileAst, Node, Options, ParseRequest};
use std::sync::Once;

/// Starts Lean once for the whole test binary.
fn setup() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let options = Options::from_env();
        let info = init(&options).expect("initializing the Lean runtime");
        assert!(!info.version.is_empty(), "Lean reported no version");
    });
}

/// Parses a snippet, asserting it produced no syntax errors.
fn ast(source: &str) -> FileAst {
    setup();
    let file = parse(ParseRequest {
        path: "Test.lean",
        source,
        imports: &[],
    })
    .expect("parsing");
    assert_eq!(
        file.syntax_errors, 0,
        "snippet did not parse cleanly:\n{source}"
    );
    assert!(
        !file.nodes.iter().any(|node| node.missing),
        "snippet produced a missing token:\n{source}"
    );
    file
}

/// Short kinds of the top-level nodes.
fn top(source: &str) -> Vec<String> {
    ast(source)
        .nodes
        .iter()
        .map(|node| node.short_kind().to_string())
        .collect()
}

/// The command inside the first `declaration` wrapper, which is what a caller
/// usually looks at: `def`, `theorem`, `structure`, …
fn declared(source: &str) -> Node {
    let file = ast(source);
    let declaration = file
        .nodes
        .iter()
        .find(|node| node.short_kind() == "declaration")
        .expect("a declaration");
    declaration
        .children
        .iter()
        .find(|node| node.short_kind() != "declModifiers")
        .cloned()
        .expect("the command inside the declaration")
}

// ------------------------------------------------------------------ commands

#[test]
fn definition() {
    let command = declared("def foo : Nat := 1\n");
    assert_eq!(command.short_kind(), "definition");
    assert_eq!(command.first_ident().and_then(|id| id.text.clone()).as_deref(), Some("foo"));
}

#[test]
fn theorem_with_tactic_proof() {
    let command = declared("theorem t : 1 = 1 := by rfl\n");
    assert_eq!(command.short_kind(), "theorem");
    assert_eq!(command.first_ident().and_then(|id| id.text.clone()).as_deref(), Some("t"));
    assert!(command.find("byTactic").is_some(), "the `by` block is in the tree");
}

#[test]
fn syntax_from_imported_modules_needs_those_imports() {
    // `lemma` is Mathlib syntax, not core: with only `Init`/`Lean` loaded it is a
    // syntax error. A caller gets project syntax by passing the file's imports.
    setup();
    let file = parse(ParseRequest {
        path: "Test.lean",
        source: "lemma l : 1 = 1 := rfl\n",
        imports: &[],
    })
    .expect("the parser still answers");
    assert!(file.syntax_errors > 0, "`lemma` is not core Lean syntax");
}

#[test]
fn axiom() {
    let command = declared("axiom ax : False\n");
    assert_eq!(command.short_kind(), "axiom");
}

#[test]
fn example() {
    let command = declared("example : True := trivial\n");
    assert_eq!(command.short_kind(), "example");
}

#[test]
fn abbrev_and_opaque() {
    assert_eq!(declared("abbrev a := 1\n").short_kind(), "abbrev");
    assert_eq!(declared("opaque o : Nat := 2\n").short_kind(), "opaque");
}

#[test]
fn structure_with_fields() {
    let command = declared("structure P where\n  x : Nat\n  y : Nat\n");
    assert_eq!(command.short_kind(), "structure");
    let mut fields = Vec::new();
    command.find_all("structSimpleBinder", &mut fields);
    assert_eq!(fields.len(), 2, "both fields are in the tree");
}

#[test]
fn inductive_with_constructors() {
    let command = declared("inductive T where\n  | a : T\n  | b : T\n");
    assert_eq!(command.short_kind(), "inductive");
    let mut constructors = Vec::new();
    command.find_all("ctor", &mut constructors);
    assert_eq!(constructors.len(), 2, "both constructors are in the tree");
}

#[test]
fn class_and_instance() {
    // A `class` is parsed as a structure with the `class` keyword.
    let command = declared("class C where\n  v : Nat\n");
    assert_eq!(command.short_kind(), "structure");
    assert_eq!(declared("instance : C := ⟨0⟩\n").short_kind(), "instance");
}

#[test]
fn namespace_section_and_mutual() {
    assert_eq!(top("namespace N\n#check Nat\nend N\n"), vec!["namespace", "check", "end"]);
    assert_eq!(top("section\n#check Nat\nend\n"), vec!["section", "check", "end"]);
    let mutual = ast("mutual\n  def a : Nat := 1\n  def b : Nat := 2\nend\n");
    assert_eq!(mutual.nodes[0].short_kind(), "mutual");
    let mut declarations = Vec::new();
    mutual.nodes[0].find_all("declaration", &mut declarations);
    assert_eq!(declarations.len(), 2, "both definitions are inside");
}

#[test]
fn where_block() {
    let command = declared("def g : Nat := h\nwhere\n  h : Nat := 1\n");
    assert_eq!(command.short_kind(), "definition");
    assert!(command.find("whereDecls").is_some());
}

#[test]
fn declarations_with_modifiers() {
    let file = ast("@[simp] def tagged : Nat := 3\n");
    let declaration = &file.nodes[0];
    assert_eq!(declaration.short_kind(), "declaration");
    let modifiers = declaration.find("declModifiers").expect("modifiers");
    assert!(modifiers.find("simp").is_some(), "the attribute is in the tree");
    assert_eq!(declared("private def hidden : Nat := 4\n").short_kind(), "definition");
    assert_eq!(
        declared("noncomputable def c : Nat := Classical.choice ⟨1⟩\n").short_kind(),
        "definition"
    );
}

#[test]
fn doc_comment_belongs_to_the_declaration() {
    let file = ast("/-- Doc for foo. -/\ndef foo : Nat := 1\n");
    let declaration = &file.nodes[0];
    let doc = declaration.find("docComment").expect("doc comment");
    assert!(doc.tokens().contains("Doc for foo."));
}

#[test]
fn notation_and_mixfix() {
    assert_eq!(top("notation \"!!\" => 1\n"), vec!["notation"]);
    assert_eq!(top("infixl:65 \"+!\" => Nat.add\n"), vec!["mixfix"]);
}

#[test]
fn syntax_and_macro_rules() {
    let file = ast("syntax \"twice\" term : term\nmacro_rules\n  | `(twice $x) => `($x + $x)\n");
    let kinds: Vec<&str> = file.nodes.iter().map(|node| node.short_kind()).collect();
    assert_eq!(kinds, vec!["syntax", "macro_rules"]);
    // The quotations inside `macro_rules` parse through Lean's quoter.
    let mut quotations = Vec::new();
    file.nodes[1].find_all("quot", &mut quotations);
    assert_eq!(quotations.len(), 2, "pattern and body quotients");
    assert!(file.nodes[1].find("antiquot").is_some(), "the `$x` splice");
}

#[test]
fn open_export_and_set_option() {
    assert_eq!(top("open Nat\n"), vec!["open"]);
    assert_eq!(top("export Nat (add)\n"), vec!["export"]);
    assert_eq!(top("set_option maxHeartbeats 400000\n"), vec!["set_option"]);
    assert_eq!(top("attribute [simp] Nat.add\n"), vec!["attribute"]);
    assert_eq!(top("variable (n : Nat)\n"), vec!["variable"]);
    assert_eq!(top("universe u\n"), vec!["universe"]);
}

#[test]
fn hash_commands() {
    assert_eq!(top("#check Nat\n"), vec!["check"]);
    assert_eq!(top("#eval 1 + 1\n"), vec!["eval"]);
}

#[test]
fn import_and_prelude_header() {
    setup();
    let file = parse(ParseRequest {
        path: "Test.lean",
        source: "import Init\n\n#check Nat\n",
        imports: &[],
    })
    .expect("parsing");
    let kinds: Vec<&str> = file.nodes.iter().map(|node| node.short_kind()).collect();
    assert!(kinds.contains(&"import"), "the import is a node of its own: {kinds:?}");
    assert!(kinds.contains(&"check"));
}

// --------------------------------------------------------------------- terms

#[test]
fn lambda() {
    let command = declared("def f := fun x : Nat => x\n");
    assert!(command.find("fun").is_some(), "the binder is in the tree");
    assert!(command.find("ident").is_some());
}

#[test]
fn match_expression() {
    let command = declared("def m (n : Nat) : Nat := match n with\n  | 0 => 1\n  | k + 1 => k\n");
    assert!(command.find("match").is_some());
    let mut alternatives = Vec::new();
    command.find_all("matchAlt", &mut alternatives);
    assert_eq!(alternatives.len(), 2, "both alternatives are in the tree");
}

#[test]
fn do_notation() {
    let command = declared("def d : IO Unit := do\n  let x ← pure 1\n  IO.println x\n");
    assert!(command.find("do").is_some());
    assert!(command.find("letDecl").is_some() || command.find("let").is_some());
}

#[test]
fn let_in_and_if() {
    let command = declared("def f (n : Nat) : Nat := let m := n + 1; if m > 0 then m else 0\n");
    assert!(command.find("let").is_some() || command.find("letDecl").is_some());
    assert!(command.find("if").is_some());
}

#[test]
fn calc_block() {
    let command = declared("def c : Nat := calc 1 = 1 := rfl\n");
    assert!(command.find("calc").is_some());
}

#[test]
fn literals() {
    let command = declared("def l : String := \"text\"\n");
    assert!(command.find("str").is_some());
    let number = declared("def n : Nat := 42\n");
    assert!(number.find("num").is_some());
    let character = declared("def ch : Char := 'a'\n");
    assert!(character.find("char").is_some());
}

#[test]
fn unicode_identifiers() {
    let command = declared("def αβ : Nat := 1\n");
    assert_eq!(command.first_ident().and_then(|id| id.text.clone()).as_deref(), Some("αβ"));
}

#[test]
fn comments_are_not_commands() {
    let file = ast("-- a line comment\n/- a block comment -/\ndef foo : Nat := 1\n");
    assert_eq!(file.nodes.len(), 1, "comments do not become commands");
    assert_eq!(file.nodes[0].short_kind(), "declaration");
}

// ---------------------------------------------------------------- positions

#[test]
fn spans_cut_the_source() {
    let source = "def foo : Nat := 1\n\ndef bar : Nat := 2\n";
    let file = ast(source);
    let second = &file.nodes[1];
    let span = second.span().expect("a span");
    assert_eq!(&source[span.start as usize..span.end as usize], "def bar : Nat := 2");
    // The wrapper reports the declaration without its modifiers as `definition`.
    let command = second.children.iter().find(|n| n.short_kind() != "declModifiers").unwrap();
    assert_eq!(command.slice(source), "def bar : Nat := 2");
}

#[test]
fn syntax_errors_become_missing_tokens() {
    setup();
    let file = parse(ParseRequest {
        path: "Broken.lean",
        source: "def broken : Nat := (1 +\n",
        imports: &[],
    })
    .expect("the parser still answers");
    assert!(file.syntax_errors > 0, "the error count is reported");
    assert!(!file.is_clean());
}

#[test]
fn parsing_is_pure_and_repeatable() {
    let first = ast("def a : Nat := 1\n");
    let second = ast("def a : Nat := 1\n");
    assert_eq!(first.nodes.len(), second.nodes.len());
    assert_eq!(first.nodes[0].short_kind(), second.nodes[0].short_kind());
}
