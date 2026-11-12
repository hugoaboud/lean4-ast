//! What the wrapper covers: one minimal test per language feature.
//!
//! These are the documentation for `lean4-ast`. Each test parses a snippet small
//! enough to read and asserts on the *syntax kinds* Lean produced, so a future
//! Lean change that renames or restructures a construct shows up here first.
//!
//! The tests read the tree through the borrowed views ([`Syntax`]) that sit
//! directly on Lean's heap; [`materialised_trees_match_the_views`] checks that the
//! owned [`FileAst`] carries exactly the same thing.
//!
//! Only the parser runs: no elaboration, no imports beyond `Init`/`Lean`, so a
//! snippet never needs to type check. Run them with
//!
//! ```text
//! python3 lean4-ast/build.py            # builds the library, then runs these
//! LEAN4_AST_SYSROOT=… cargo test -p lean4-ast
//! ```

use lean4_ast::{init, parse, FileAst, Node, Options, ParseRequest, ParseTree, Syntax};
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
fn tree(source: &str) -> ParseTree {
    setup();
    let tree = parse(ParseRequest {
        path: "Test.lean",
        source,
        imports: &[],
    })
    .expect("parsing");
    assert_eq!(
        tree.syntax_errors(),
        0,
        "snippet did not parse cleanly:\n{source}"
    );
    assert!(
        tree.is_clean(),
        "snippet produced a missing token:\n{source}"
    );
    tree
}

/// Parses a snippet and returns its owned, materialised tree.
fn ast(source: &str) -> FileAst {
    tree(source).to_file_ast()
}

/// Short kinds of the top-level nodes.
fn top(source: &str) -> Vec<String> {
    let tree = tree(source);
    commands(&tree)
}

fn commands(tree: &ParseTree) -> Vec<String> {
    tree.nodes()
        .iter()
        .map(|node| node.short_kind().into_owned())
        .collect()
}

/// The command inside the first `declaration` wrapper, which is what a caller
/// usually looks at: `def`, `theorem`, `structure`, …
fn declared(tree: &ParseTree) -> Syntax<'_> {
    let declaration = tree
        .nodes()
        .iter()
        .find(|node| node.is_of_kind("declaration"))
        .expect("a declaration");
    declaration
        .children()
        .iter()
        .find(|node| !node.is_of_kind("declModifiers"))
        .expect("the command inside the declaration")
}

// ------------------------------------------------------------------ commands

#[test]
fn definition() {
    let tree = tree("def foo : Nat := 1\n");
    let command = declared(&tree);
    assert_eq!(command.short_kind(), "definition");
    assert_eq!(
        command.first_ident().map(|id| id.name().to_string()),
        Some("foo".to_string())
    );
}

#[test]
fn theorem_with_tactic_proof() {
    let tree = tree("theorem t : 1 = 1 := by rfl\n");
    let command = declared(&tree);
    assert_eq!(command.short_kind(), "theorem");
    assert_eq!(
        command.first_ident().map(|id| id.name().to_string()),
        Some("t".to_string())
    );
    assert!(command.find("byTactic").is_some(), "the `by` block is in the tree");
}

#[test]
fn syntax_from_imported_modules_needs_those_imports() {
    // `lemma` is Mathlib syntax, not core: with only `Init`/`Lean` loaded it is a
    // syntax error. A caller gets project syntax by passing the file's imports.
    setup();
    let tree = parse(ParseRequest {
        path: "Test.lean",
        source: "lemma l : 1 = 1 := rfl\n",
        imports: &[],
    })
    .expect("the parser still answers");
    assert!(tree.syntax_errors() > 0, "`lemma` is not core Lean syntax");
}

#[test]
fn axiom() {
    assert_eq!(declared(&tree("axiom ax : False\n")).short_kind(), "axiom");
}

#[test]
fn example() {
    assert_eq!(
        declared(&tree("example : True := trivial\n")).short_kind(),
        "example"
    );
}

#[test]
fn abbrev_and_opaque() {
    assert_eq!(declared(&tree("abbrev a := 1\n")).short_kind(), "abbrev");
    assert_eq!(
        declared(&tree("opaque o : Nat := 2\n")).short_kind(),
        "opaque"
    );
}

#[test]
fn structure_with_fields() {
    let tree = tree("structure P where\n  x : Nat\n  y : Nat\n");
    let command = declared(&tree);
    assert_eq!(command.short_kind(), "structure");
    let mut fields = Vec::new();
    command.find_all("structSimpleBinder", &mut fields);
    assert_eq!(fields.len(), 2, "both fields are in the tree");
}

#[test]
fn inductive_with_constructors() {
    let tree = tree("inductive T where\n  | a : T\n  | b : T\n");
    let command = declared(&tree);
    assert_eq!(command.short_kind(), "inductive");
    let mut constructors = Vec::new();
    command.find_all("ctor", &mut constructors);
    assert_eq!(constructors.len(), 2, "both constructors are in the tree");
}

#[test]
fn class_and_instance() {
    // A `class` is parsed as a structure with the `class` keyword.
    let class = tree("class C where\n  v : Nat\n");
    assert_eq!(declared(&class).short_kind(), "structure");
    let instance = tree("instance : C := ⟨0⟩\n");
    assert_eq!(declared(&instance).short_kind(), "instance");
}

#[test]
fn namespace_section_and_mutual() {
    assert_eq!(top("namespace N\n#check Nat\nend N\n"), vec!["namespace", "check", "end"]);
    assert_eq!(top("section\n#check Nat\nend\n"), vec!["section", "check", "end"]);
    let mutual = tree("mutual\n  def a : Nat := 1\n  def b : Nat := 2\nend\n");
    assert_eq!(mutual.nodes().get(0).unwrap().short_kind(), "mutual");
    let mut declarations = Vec::new();
    mutual
        .nodes()
        .get(0)
        .unwrap()
        .find_all("declaration", &mut declarations);
    assert_eq!(declarations.len(), 2, "both definitions are inside");
}

#[test]
fn where_block() {
    let tree = tree("def g : Nat := h\nwhere\n  h : Nat := 1\n");
    let command = declared(&tree);
    assert_eq!(command.short_kind(), "definition");
    assert!(command.find("whereDecls").is_some());
}

#[test]
fn declarations_with_modifiers() {
    let tagged = tree("@[simp] def tagged : Nat := 3\n");
    let declaration = tagged.nodes().get(0).expect("a command");
    assert_eq!(declaration.short_kind(), "declaration");
    let modifiers = declaration.find("declModifiers").expect("modifiers");
    assert!(modifiers.find("simp").is_some(), "the attribute is in the tree");
    assert_eq!(
        declared(&tree("private def hidden : Nat := 4\n")).short_kind(),
        "definition"
    );
    assert_eq!(
        declared(&tree("noncomputable def c : Nat := Classical.choice ⟨1⟩\n")).short_kind(),
        "definition"
    );
}

#[test]
fn doc_comment_belongs_to_the_declaration() {
    let tree = tree("/-- Doc for foo. -/\ndef foo : Nat := 1\n");
    let declaration = tree.nodes().get(0).expect("a command");
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
    let tree = tree("syntax \"twice\" term : term\nmacro_rules\n  | `(twice $x) => `($x + $x)\n");
    assert_eq!(commands(&tree), vec!["syntax", "macro_rules"]);
    let rules = tree.nodes().get(1).expect("the macro_rules command");
    // The quotations inside `macro_rules` parse through Lean's quoter.
    let mut quotations = Vec::new();
    rules.find_all("quot", &mut quotations);
    assert_eq!(quotations.len(), 2, "pattern and body quotients");
    assert!(rules.find("antiquot").is_some(), "the `$x` splice");
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
    let tree = parse(ParseRequest {
        path: "Test.lean",
        source: "import Init\n\n#check Nat\n",
        imports: &[],
    })
    .expect("parsing");
    let kinds = commands(&tree);
    assert!(kinds.contains(&"import".to_string()), "the import is a node of its own: {kinds:?}");
    assert!(kinds.contains(&"check".to_string()));
}

// --------------------------------------------------------------------- terms

#[test]
fn lambda() {
    let tree = tree("def f := fun x : Nat => x\n");
    let command = declared(&tree);
    assert!(command.find("fun").is_some(), "the binder is in the tree");
    assert!(command.find("ident").is_some());
}

#[test]
fn match_expression() {
    let tree = tree("def m (n : Nat) : Nat := match n with\n  | 0 => 1\n  | k + 1 => k\n");
    let command = declared(&tree);
    assert!(command.find("match").is_some());
    let mut alternatives = Vec::new();
    command.find_all("matchAlt", &mut alternatives);
    assert_eq!(alternatives.len(), 2, "both alternatives are in the tree");
}

#[test]
fn do_notation() {
    let tree = tree("def d : IO Unit := do\n  let x ← pure 1\n  IO.println x\n");
    let command = declared(&tree);
    assert!(command.find("do").is_some());
    assert!(command.find("letDecl").is_some() || command.find("let").is_some());
}

#[test]
fn let_in_and_if() {
    let tree = tree("def f (n : Nat) : Nat := let m := n + 1; if m > 0 then m else 0\n");
    let command = declared(&tree);
    assert!(command.find("let").is_some() || command.find("letDecl").is_some());
    assert!(command.find("if").is_some());
}

#[test]
fn calc_block() {
    let tree = tree("def c : Nat := calc 1 = 1 := rfl\n");
    assert!(declared(&tree).find("calc").is_some());
}

#[test]
fn literals() {
    let text = tree("def l : String := \"text\"\n");
    assert!(declared(&text).find("str").is_some());
    let number = tree("def n : Nat := 42\n");
    assert!(declared(&number).find("num").is_some());
    let character = tree("def ch : Char := 'a'\n");
    assert!(declared(&character).find("char").is_some());
}

#[test]
fn unicode_identifiers() {
    let tree = tree("def αβ : Nat := 1\n");
    let ident = declared(&tree).first_ident().expect("the declared name");
    assert_eq!(ident.name().to_string(), "αβ");
    assert_eq!(ident.raw().as_str(), "αβ");
}

#[test]
fn comments_are_not_commands() {
    let tree = tree("-- a line comment\n/- a block comment -/\ndef foo : Nat := 1\n");
    assert_eq!(tree.nodes().len(), 1, "comments do not become commands");
    assert_eq!(tree.nodes().get(0).unwrap().short_kind(), "declaration");
}

// ---------------------------------------------------------------- positions

#[test]
fn spans_cut_the_source() {
    let source = "def foo : Nat := 1\n\ndef bar : Nat := 2\n";
    let tree = tree(source);
    let second = tree.nodes().get(1).expect("the second command");
    let span = second.span().expect("a span");
    assert_eq!(
        &source[span.start as usize..span.end as usize],
        "def bar : Nat := 2"
    );
    // The wrapper reports the declaration without its modifiers as `definition`.
    let command = second
        .children()
        .iter()
        .find(|node| !node.is_of_kind("declModifiers"))
        .expect("the command inside the declaration");
    assert_eq!(command.slice(source), "def bar : Nat := 2");
}

#[test]
fn syntax_errors_become_missing_tokens() {
    setup();
    let tree = parse(ParseRequest {
        path: "Broken.lean",
        source: "def broken : Nat := (1 +\n",
        imports: &[],
    })
    .expect("the parser still answers");
    assert!(tree.syntax_errors() > 0, "the error count is reported");
    assert!(!tree.is_clean());
}

#[test]
fn parsing_is_pure_and_repeatable() {
    let first = tree("def a : Nat := 1\n");
    let second = tree("def a : Nat := 1\n");
    assert_eq!(first.nodes().len(), second.nodes().len());
    assert_eq!(
        first.nodes().get(0).unwrap().short_kind(),
        second.nodes().get(0).unwrap().short_kind()
    );
}

// ------------------------------------------------------------ Lean's objects

#[test]
fn kinds_and_text_are_borrowed_from_leans_heap() {
    let source = "def foo : Nat := 1\n";
    let tree = tree(source);
    let command = declared(&tree);

    // The short kind is a slice of Lean's own kind name — no allocation, and no
    // rendering on the way.
    match command.short_kind() {
        std::borrow::Cow::Borrowed(_) => {}
        std::borrow::Cow::Owned(_) => panic!("the kind should be borrowed"),
    }
    assert_eq!(command.kind_string(), "Lean.Parser.Command.definition");

    // Atom text is the `String` Lean allocated, and an identifier's raw text is a
    // slice of the very source we passed in.
    let atom = command.find("def").expect("the `def` keyword");
    match atom.text().expect("atom text") {
        std::borrow::Cow::Borrowed(text) => assert_eq!(text, "def"),
        std::borrow::Cow::Owned(_) => panic!("atom text should be borrowed"),
    }
    let ident = command.first_ident().expect("the declared name");
    let raw = ident.raw();
    assert_eq!(raw.as_str(), "foo");
    // `raw` points into the one `String` Lean parsed the whole file from: no
    // per-identifier copy was made anywhere.
    assert_eq!(raw.text.len(), source.len());
    assert_eq!(raw.stop - raw.start, 3);
}

#[test]
fn trees_move_between_threads() {
    let tree = tree("def moved : Nat := 1\n");
    let handle = std::thread::spawn(move || {
        // Reading and releasing the tree is fine on another thread: the objects
        // are immutable and their reference counts were made atomic for exactly
        // this hand-off.
        commands(&tree)
    });
    assert_eq!(handle.join().expect("the worker"), vec!["declaration"]);
}

#[test]
fn materialised_trees_are_plain_data() {
    // `ast` drops the `ParseTree` it read the tree from; the owned copy needs no
    // Lean afterwards: it can be walked, searched and kept.
    let file = ast("def f (n : Nat) : Nat := n + 1\n");
    let mut identifiers = Vec::new();
    for node in &file.nodes {
        node.find_all("ident", &mut identifiers);
    }
    assert!(
        identifiers.iter().any(|id| id.text.as_deref() == Some("f")),
        "the declared name is in the owned tree"
    );
    assert!(file.is_clean());
}

#[test]
fn materialised_trees_match_the_views() {
    for source in [
        "def foo : Nat := 1\n",
        "structure P where\n  x : Nat\n",
        "theorem t : 1 = 1 := by rfl\n",
        "syntax \"twice\" term : term\nmacro_rules\n  | `(twice $x) => `($x + $x)\n",
        "def broken : Nat := (1 +\n",
        "mutual\n  def a : Nat := 1\n  def b : Nat := 2\nend\n",
    ] {
        let parsed = parse(ParseRequest {
            path: "Test.lean",
            source,
            imports: &[],
        })
        .expect("parsing");
        let owned = parsed.to_file_ast();
        assert_eq!(owned.syntax_errors, parsed.syntax_errors());
        assert_eq!(owned.nodes.len(), parsed.nodes().len());
        for index in 0..owned.nodes.len() {
            same_node(&parsed.nodes().get(index).expect("a node"), &owned.nodes[index]);
        }
    }
}

/// Asserts that a borrowed view and its materialised copy say the same thing, all
/// the way down.
fn same_node(view: &Syntax<'_>, node: &Node) {
    assert_eq!(view.short_kind().as_ref(), node.kind);
    assert_eq!(view.span().map(|span| [span.start, span.end]), node.span);
    assert_eq!(view.text().as_deref(), node.text.as_deref());
    assert_eq!(view.missing(), node.missing);
    assert_eq!(view.children().len(), node.children.len());
    for (index, child) in node.children.iter().enumerate() {
        let child_view = view.children().get(index).expect("a child");
        same_node(&child_view, child);
    }
}
