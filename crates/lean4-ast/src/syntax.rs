//! Lean's `Syntax` tree, read where Lean left it.
//!
//! A [`Syntax`] is a borrowed view of one node of the tree a [`ParseTree`] owns;
//! nothing is copied, allocated or decoded. `Syntax` mirrors Lean's own inductive:
//!
//! ```lean
//! inductive Syntax where
//!   | missing
//!   | node  (info : SourceInfo) (kind : SyntaxNodeKind) (args : Array Syntax)
//!   | atom  (info : SourceInfo) (val : String)
//!   | ident (info : SourceInfo) (rawVal : Substring.Raw) (val : Name) (preresolved : List …)
//! ```
//!
//! so a caller can read the tree at whatever level of detail it needs: the kind of
//! a node costs a pointer chase, `short_kind` costs nothing at all, and the text of
//! an atom is the `String` Lean allocated for it — not a copy of it.
//!
//! [`ParseTree`]: crate::ParseTree

use crate::raw::Obj;
use crate::Span;
use std::borrow::Cow;
use std::fmt;

/// Constructor indices of Lean's `Syntax`, in declaration order.
const SYNTAX_MISSING: u8 = 0;
const SYNTAX_NODE: u8 = 1;
const SYNTAX_ATOM: u8 = 2;
const SYNTAX_IDENT: u8 = 3;

/// Constructor indices of `Lean.SourceInfo`.
const INFO_ORIGINAL: u8 = 0;
const INFO_SYNTHETIC: u8 = 1;

/// Constructor indices of `Lean.Name`.
const NAME_ANONYMOUS: u8 = 0;
const NAME_STR: u8 = 1;
const NAME_NUM: u8 = 2;

/// One node of Lean's syntax tree.
///
/// A `Syntax` is a borrowed handle: copying it is free, and it stays valid for as
/// long as the [`ParseTree`](crate::ParseTree) it was read from.
#[derive(Clone, Copy)]
pub enum Syntax<'a> {
    /// A token Lean had to invent because of a syntax error.
    Missing,
    /// A composite node: a kind plus its children.
    Node(SyntaxNode<'a>),
    /// A token: keyword, literal, punctuation.
    Atom(SyntaxAtom<'a>),
    /// An identifier, with both its source text and its parsed name.
    Ident(SyntaxIdent<'a>),
}

impl<'a> Syntax<'a> {
    /// Reads one `Syntax` value out of Lean's heap.
    ///
    /// # Panics
    ///
    /// If the object is not a `Syntax` this crate knows about. [`crate::init`]
    /// checks the four constructors against the library at startup, so reaching
    /// this panic means the static library and the crate disagree.
    pub fn from_obj(object: Obj<'a>) -> Syntax<'a> {
        match object.tag() {
            None => {
                assert_eq!(
                    object.unbox(),
                    Some(SYNTAX_MISSING as usize),
                    "lean4-ast: not a Syntax value"
                );
                Syntax::Missing
            }
            Some(SYNTAX_NODE) => Syntax::Node(SyntaxNode { object }),
            Some(SYNTAX_ATOM) => Syntax::Atom(SyntaxAtom { object }),
            Some(SYNTAX_IDENT) => Syntax::Ident(SyntaxIdent { object }),
            Some(tag) => panic!(
                "lean4-ast: Syntax constructor {tag} is unknown to this crate; \
                 the static library was built against another Lean"
            ),
        }
    }

    /// The underlying Lean value, for a caller that wants to read it directly.
    pub fn object(&self) -> Obj<'a> {
        match self {
            // `Syntax.missing` *is* the scalar `lean_box(0)`: a tagged null, so its
            // representation is the address 1 — never dereferenced, only read as a
            // scalar.
            Syntax::Missing => unsafe { Obj::from_raw(std::ptr::without_provenance_mut(1)) },
            Syntax::Node(node) => node.object,
            Syntax::Atom(atom) => atom.object,
            Syntax::Ident(ident) => ident.object,
        }
    }

    /// Where this node came from in the source.
    pub fn source_info(&self) -> SourceInfo<'a> {
        SourceInfo::from_obj(match self {
            Syntax::Missing => return SourceInfo::None,
            Syntax::Node(node) => node.info(),
            Syntax::Atom(atom) => atom.info(),
            Syntax::Ident(ident) => ident.info(),
        })
    }

    /// The node's kind, for a `node`; `None` for the other constructors (Lean
    /// calls their kinds pseudo-kinds, see [`Syntax::short_kind`]).
    pub fn node_kind(&self) -> Option<Name<'a>> {
        match self {
            Syntax::Node(node) => Some(node.kind()),
            _ => None,
        }
    }

    /// Lean's full kind name, e.g. `Lean.Parser.Command.definition`.
    ///
    /// For the non-`node` constructors this is the pseudo-kind Lean assigns:
    /// `missing`, `ident`, or the atom's own text.
    pub fn kind_string(&self) -> String {
        match self {
            Syntax::Missing => "missing".to_string(),
            Syntax::Node(node) => node.kind().to_string(),
            Syntax::Atom(atom) => atom.text().to_string(),
            Syntax::Ident(_) => "ident".to_string(),
        }
    }

    /// Last component of the kind: `definition` for
    /// `Lean.Parser.Command.definition`.
    ///
    /// This is the name every caller matches on, and it is a borrow into Lean's
    /// heap: no allocation, no scan. Atoms use their text, as Lean's
    /// `Syntax.getKind` does.
    pub fn short_kind(&self) -> Cow<'a, str> {
        match self {
            Syntax::Missing => Cow::Borrowed("missing"),
            Syntax::Node(node) => node.kind().last().into_cow(),
            Syntax::Atom(atom) => Cow::Borrowed(last_dotted(atom.text())),
            Syntax::Ident(_) => Cow::Borrowed("ident"),
        }
    }

    /// True when this node is a token Lean had to invent after a syntax error.
    pub fn missing(&self) -> bool {
        matches!(self, Syntax::Missing)
    }

    /// True when [`Syntax::short_kind`] is `short_kind`.
    pub fn is_of_kind(&self, short_kind: &str) -> bool {
        match &self.short_kind() {
            Cow::Borrowed(text) => *text == short_kind,
            Cow::Owned(text) => text == short_kind,
        }
    }

    /// The children of a `node`; empty for every other constructor.
    pub fn children(&self) -> SyntaxList<'a> {
        match self {
            Syntax::Node(node) => node.args(),
            _ => SyntaxList::empty(),
        }
    }

    /// The node's own text: the value of an atom, the parsed name of an
    /// identifier. `None` for nodes, whose text is the concatenation of their
    /// children's (see [`Syntax::tokens`]).
    pub fn text(&self) -> Option<Cow<'a, str>> {
        match self {
            Syntax::Atom(atom) => Some(Cow::Borrowed(atom.text())),
            Syntax::Ident(ident) => Some(Cow::Owned(ident.name().to_string())),
            _ => None,
        }
    }

    /// Byte range in the source, when the node carries a canonical position.
    ///
    /// This is the same range the parser would report: the head position of the
    /// node's own source information, and the tail of the last token below it.
    pub fn span(&self) -> Option<Span> {
        let start = self.head_pos()?;
        let end = self.tail_pos()?;
        Some(Span {
            start: start as u32,
            end: end as u32,
        })
    }

    /// The source text this node covers, or `""` when it carries no span.
    pub fn slice(&self, source: &'a str) -> &'a str {
        match self.span() {
            Some(span) => span.slice(source),
            None => "",
        }
    }

    /// `Syntax.getHeadInfo?`: the start of the left-most token below this node.
    ///
    /// A node the parser built carries `SourceInfo.none` itself — the positions
    /// live on the tokens inside it — so this descends until it finds one.
    fn head_pos(&self) -> Option<usize> {
        match self {
            Syntax::Missing => None,
            Syntax::Atom(_) | Syntax::Ident(_) => self.source_info().canonical_pos(),
            Syntax::Node(node) => match SourceInfo::from_obj(node.info()) {
                SourceInfo::None => node
                    .args()
                    .iter()
                    .find_map(|child| child.head_pos()),
                info => info.canonical_pos(),
            },
        }
    }

    fn tail_pos(&self) -> Option<usize> {
        match self {
            Syntax::Missing => None,
            Syntax::Atom(_) | Syntax::Ident(_) => self.source_info().canonical_tail_pos(),
            Syntax::Node(node) => {
                if let Some(end) = SourceInfo::from_obj(node.info()).canonical_tail_pos() {
                    return Some(end);
                }
                // `Syntax.getTailPos?`: a node without a usable end position
                // takes the last end position among its children.
                node.args()
                    .iter()
                    .rev()
                    .find_map(|child| child.tail_pos())
            }
        }
    }

    /// Depth-first walk over this node and everything below it.
    pub fn walk(&self, visit: &mut impl FnMut(Syntax<'a>)) {
        visit(*self);
        for child in self.children() {
            child.walk(&mut *visit);
        }
    }

    /// First descendant (inclusive) with the given short kind.
    pub fn find(&self, short_kind: &str) -> Option<Syntax<'a>> {
        if self.is_of_kind(short_kind) {
            return Some(*self);
        }
        self.children()
            .iter()
            .find_map(|child| child.find(short_kind))
    }

    /// Every descendant (inclusive) with the given short kind.
    pub fn find_all(&self, short_kind: &str, out: &mut Vec<Syntax<'a>>) {
        if self.is_of_kind(short_kind) {
            out.push(*self);
        }
        for child in self.children() {
            child.find_all(short_kind, out);
        }
    }

    /// First identifier below this node, which for a command is its declared name.
    pub fn first_ident(&self) -> Option<SyntaxIdent<'a>> {
        if let Syntax::Ident(ident) = self {
            return Some(*ident);
        }
        self.children().iter().find_map(|child| child.first_ident())
    }

    /// Concatenated text of every token below this node.
    pub fn tokens(&self) -> String {
        let mut out = String::new();
        self.walk(&mut |node| {
            if let Some(text) = node.text() {
                out.push_str(&text);
                out.push(' ');
            }
        });
        out
    }
}

impl fmt::Debug for Syntax<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", self.short_kind(), if self.missing() { " (missing)" } else { "" })
    }
}

/// A `Syntax.node`: a kind and its children.
#[derive(Clone, Copy)]
pub struct SyntaxNode<'a> {
    object: Obj<'a>,
}

impl<'a> SyntaxNode<'a> {
    pub fn info(&self) -> Obj<'a> {
        self.object.field(0)
    }

    /// The node's kind, e.g. `Lean.Parser.Command.definition`.
    pub fn kind(&self) -> Name<'a> {
        Name {
            object: self.object.field(1),
        }
    }

    /// The node's children, in source order.
    pub fn args(&self) -> SyntaxList<'a> {
        match self.object.field(2).as_array() {
            Some(array) => SyntaxList::from_array(array),
            None => SyntaxList::empty(),
        }
    }

    pub fn object(&self) -> Obj<'a> {
        self.object
    }
}

/// A `Syntax.atom`: a keyword, literal or punctuation mark.
#[derive(Clone, Copy)]
pub struct SyntaxAtom<'a> {
    object: Obj<'a>,
}

impl<'a> SyntaxAtom<'a> {
    pub fn info(&self) -> Obj<'a> {
        self.object.field(0)
    }

    /// The token as it appears in the source.
    pub fn text(&self) -> &'a str {
        self.object.field(1).as_str().unwrap_or("")
    }

    pub fn object(&self) -> Obj<'a> {
        self.object
    }
}

/// A `Syntax.ident`: an identifier, with its source text and parsed `Name`.
#[derive(Clone, Copy)]
pub struct SyntaxIdent<'a> {
    object: Obj<'a>,
}

impl<'a> SyntaxIdent<'a> {
    pub fn info(&self) -> Obj<'a> {
        self.object.field(0)
    }

    /// The literal text of the identifier, as a slice of the source Lean read.
    pub fn raw(&self) -> Substring<'a> {
        Substring::from_obj(self.object.field(1))
    }

    /// The parsed name, macro scopes included.
    pub fn name(&self) -> Name<'a> {
        Name {
            object: self.object.field(2),
        }
    }

    pub fn object(&self) -> Obj<'a> {
        self.object
    }
}

/// An `Array Syntax`: the children of a node, or a whole file's top-level nodes.
#[derive(Clone, Copy)]
pub struct SyntaxList<'a> {
    array: Option<crate::raw::Array<'a>>,
}

impl<'a> SyntaxList<'a> {
    /// The empty list, which is what a non-`node` node has.
    pub(crate) fn empty() -> SyntaxList<'a> {
        SyntaxList { array: None }
    }

    /// Wraps a Lean `Array Syntax`, which is what a node's children are.
    pub(crate) fn from_array(array: crate::raw::Array<'a>) -> SyntaxList<'a> {
        SyntaxList { array: Some(array) }
    }

    pub fn len(&self) -> usize {
        self.array.map_or(0, |array| array.len())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The node at `index`, or `None` when out of bounds.
    pub fn get(&self, index: usize) -> Option<Syntax<'a>> {
        self.array
            .and_then(|array| array.get(index))
            .map(Syntax::from_obj)
    }

    /// Every node, in order.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = Syntax<'a>> + 'a {
        self.array
            .into_iter()
            .flat_map(|array| array.iter())
            .map(Syntax::from_obj)
    }

    pub fn first(&self) -> Option<Syntax<'a>> {
        self.get(0)
    }

    pub fn last(&self) -> Option<Syntax<'a>> {
        self.len().checked_sub(1).and_then(|last| self.get(last))
    }
}

impl<'a> IntoIterator for SyntaxList<'a> {
    type Item = Syntax<'a>;
    type IntoIter = Box<dyn Iterator<Item = Syntax<'a>> + 'a>;

    fn into_iter(self) -> Self::IntoIter {
        Box::new(self.iter())
    }
}

impl fmt::Debug for SyntaxList<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

/// A `Lean.Name`: a dotted name, linked through its parent.
#[derive(Clone, Copy)]
pub struct Name<'a> {
    object: Obj<'a>,
}

impl<'a> Name<'a> {
    pub fn is_anonymous(&self) -> bool {
        self.object.is_scalar() && self.object.unbox() == Some(NAME_ANONYMOUS as usize)
    }

    /// The last component of the name, which for a syntax kind is the part
    /// callers match on.
    pub fn last(&self) -> NamePart<'a> {
        match self.object.tag() {
            Some(NAME_STR) => NamePart::Str(self.object.field(1).as_str().unwrap_or("")),
            Some(NAME_NUM) => NamePart::Num(self.object.field(1).as_usize().unwrap_or(0)),
            _ => NamePart::Anonymous,
        }
    }

    pub fn object(&self) -> Obj<'a> {
        self.object
    }
}

impl fmt::Display for Name<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.object.tag() {
            Some(NAME_STR) => {
                let parent = Name {
                    object: self.object.field(0),
                };
                let component = self.object.field(1).as_str().unwrap_or("");
                if parent.is_anonymous() {
                    f.write_str(component)
                } else {
                    write!(f, "{parent}.{component}")
                }
            }
            Some(NAME_NUM) => {
                let parent = Name {
                    object: self.object.field(0),
                };
                let component = self.object.field(1).as_usize().unwrap_or(0);
                if parent.is_anonymous() {
                    write!(f, "{component}")
                } else {
                    write!(f, "{parent}.{component}")
                }
            }
            _ => f.write_str("[anonymous]"),
        }
    }
}

/// One component of a [`Name`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NamePart<'a> {
    Anonymous,
    Str(&'a str),
    Num(usize),
}

impl<'a> NamePart<'a> {
    /// The component as text, allocating only for the numeric case.
    pub fn into_cow(self) -> Cow<'a, str> {
        match self {
            NamePart::Anonymous => Cow::Borrowed("[anonymous]"),
            NamePart::Str(text) => Cow::Borrowed(text),
            NamePart::Num(value) => Cow::Owned(value.to_string()),
        }
    }
}

/// Where a node's text sits in the file: a slice of the string Lean parsed.
#[derive(Clone, Copy, Debug)]
pub struct Substring<'a> {
    /// The string the slice belongs to — usually the whole file.
    pub text: &'a str,
    /// Byte offset of the first byte.
    pub start: usize,
    /// Byte offset one past the last byte.
    pub stop: usize,
}

impl<'a> Substring<'a> {
    fn from_obj(object: Obj<'a>) -> Substring<'a> {
        Substring {
            text: object.field(0).as_str().unwrap_or(""),
            start: position(object.field(1)),
            stop: position(object.field(2)),
        }
    }

    /// The slice itself.
    pub fn as_str(&self) -> &'a str {
        self.text.get(self.start..self.stop).unwrap_or("")
    }
}

/// `Lean.SourceInfo`: what the parser recorded about where a node came from.
#[derive(Clone, Copy, Debug)]
pub enum SourceInfo<'a> {
    /// A token from the input, with the whitespace around it.
    Original {
        leading: Substring<'a>,
        pos: usize,
        trailing: Substring<'a>,
        end_pos: usize,
    },
    /// Syntax Lean made up, optionally marked as standing in for real input.
    Synthetic {
        pos: usize,
        end_pos: usize,
        canonical: bool,
    },
    /// No position information at all.
    None,
}

impl<'a> SourceInfo<'a> {
    fn from_obj(object: Obj<'a>) -> SourceInfo<'a> {
        match object.tag() {
            Some(INFO_ORIGINAL) => SourceInfo::Original {
                leading: Substring::from_obj(object.field(0)),
                pos: position(object.field(1)),
                trailing: Substring::from_obj(object.field(2)),
                end_pos: position(object.field(3)),
            },
            Some(INFO_SYNTHETIC) => SourceInfo::Synthetic {
                pos: position(object.field(0)),
                end_pos: position(object.field(1)),
                canonical: object.scalar_u8(0) != 0,
            },
            _ => SourceInfo::None,
        }
    }

    /// The position this information starts at, if any.
    pub fn pos(&self) -> Option<usize> {
        match self {
            SourceInfo::Original { pos, .. } | SourceInfo::Synthetic { pos, .. } => Some(*pos),
            SourceInfo::None => None,
        }
    }

    /// The position this information ends at, if any.
    pub fn tail_pos(&self) -> Option<usize> {
        match self {
            SourceInfo::Original { end_pos, .. } | SourceInfo::Synthetic { end_pos, .. } => {
                Some(*end_pos)
            }
            SourceInfo::None => None,
        }
    }

    /// Like [`SourceInfo::pos`], but a synthetic node that Lean did not mark as
    /// canonical counts as having no position (`canonicalOnly := true`).
    pub fn canonical_pos(&self) -> Option<usize> {
        match self {
            SourceInfo::Original { pos, .. } => Some(*pos),
            SourceInfo::Synthetic {
                pos,
                canonical: true,
                ..
            } => Some(*pos),
            _ => None,
        }
    }

    /// Like [`SourceInfo::tail_pos`], with the same canonical-only rule.
    pub fn canonical_tail_pos(&self) -> Option<usize> {
        match self {
            SourceInfo::Original { end_pos, .. } => Some(*end_pos),
            SourceInfo::Synthetic {
                end_pos,
                canonical: true,
                ..
            } => Some(*end_pos),
            _ => None,
        }
    }
}

/// The value of a `String.Pos.Raw`, which is a `Nat` in a one-field structure.
fn position(object: Obj<'_>) -> usize {
    if object.is_scalar() {
        object.unbox().unwrap_or(0)
    } else {
        object.field(0).as_usize().unwrap_or(0)
    }
}

/// The part of `text` after its last `.`, which is what Lean's kind names are
/// split on.
fn last_dotted(text: &str) -> &str {
    match text.rfind('.') {
        Some(index) => &text[index + 1..],
        None => text,
    }
}
