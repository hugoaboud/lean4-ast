//! An owned copy of a syntax tree, for callers that would rather not think about
//! Lean's heap.
//!
//! [`Syntax`] views are borrowed and zero-copy, which is what a fast path wants;
//! [`FileAst`] is the opposite trade — every kind, every token and every span
//! copied into `String`s and `Vec`s, so the tree outlives the [`ParseTree`] and
//! can be sent anywhere, stored, or compared.
//!
//! [`ParseTree::to_file_ast`]: crate::ParseTree::to_file_ast

use crate::syntax::Syntax;

/// One node of Lean's syntax tree, materialised.
#[derive(Debug, Clone)]
pub struct Node {
    /// Lean's syntax kind, with the dotted prefix stripped: `definition` for
    /// `Lean.Parser.Command.definition`.
    pub kind: String,
    /// Byte range in the source; `None` for synthetic (whitespace) nodes.
    pub span: Option<[u32; 2]>,
    /// Token text, for atoms and identifiers.
    pub text: Option<String>,
    /// True when the parser invented this token after a syntax error.
    pub missing: bool,
    pub children: Vec<Node>,
}

impl Node {
    /// The node's kind (already the last component of Lean's kind name).
    pub fn short_kind(&self) -> &str {
        &self.kind
    }

    pub fn span(&self) -> Option<crate::Span> {
        self.span.map(|[start, end]| crate::Span { start, end })
    }

    /// The source text this node covers.
    pub fn slice<'a>(&self, source: &'a str) -> &'a str {
        match self.span() {
            Some(span) => span.slice(source),
            None => "",
        }
    }

    /// Depth-first walk over this node and everything below it.
    pub fn walk<'a>(&'a self, visit: &mut impl FnMut(&'a Node)) {
        visit(self);
        for child in &self.children {
            child.walk(visit);
        }
    }

    /// First descendant (inclusive) with the given short kind.
    pub fn find(&self, short_kind: &str) -> Option<&Node> {
        if self.short_kind() == short_kind {
            return Some(self);
        }
        self.children.iter().find_map(|child| child.find(short_kind))
    }

    /// Every descendant (inclusive) with the given short kind.
    pub fn find_all<'a>(&'a self, short_kind: &str, out: &mut Vec<&'a Node>) {
        if self.short_kind() == short_kind {
            out.push(self);
        }
        for child in &self.children {
            child.find_all(short_kind, out);
        }
    }

    /// First identifier below this node, which for a command is its declared name.
    pub fn first_ident(&self) -> Option<&Node> {
        if self.short_kind() == "ident" {
            return Some(self);
        }
        self.children.iter().find_map(|child| child.first_ident())
    }

    /// Concatenated text of all tokens below this node.
    pub fn tokens(&self) -> String {
        let mut out = String::new();
        self.walk(&mut |node| {
            if let Some(text) = &node.text {
                out.push_str(text);
                out.push(' ');
            }
        });
        out
    }
}

/// One parsed file, materialised.
#[derive(Debug, Clone)]
pub struct FileAst {
    /// Header commands (`prelude`, `import`) followed by the file's commands.
    pub nodes: Vec<Node>,
    /// How many syntax errors the parser reported. `0` means the file parsed;
    /// the errors themselves are the caller's business (see the LSP for those).
    pub syntax_errors: usize,
}

impl FileAst {
    /// The top-level commands, header included.
    pub fn commands(&self) -> &[Node] {
        &self.nodes
    }

    /// True when the file parsed without inventing tokens.
    pub fn is_clean(&self) -> bool {
        self.syntax_errors == 0 && !self.nodes.iter().any(|node| node.missing)
    }
}

/// Copies one [`Syntax`] view, and everything below it, into owned nodes.
pub(crate) fn node(syntax: &Syntax<'_>) -> Node {
    Node {
        kind: syntax.short_kind().into_owned(),
        span: syntax.span().map(|span| [span.start, span.end]),
        text: syntax.text().map(|text| text.into_owned()),
        missing: syntax.missing(),
        children: syntax.children().iter().map(|child| node(&child)).collect(),
    }
}
