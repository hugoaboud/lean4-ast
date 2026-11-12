//! Lean 4 syntax trees, from Lean's own parser, in process.
//!
//! This crate is a thin Rust face over a static library that `lean4-ast/build.py`
//! compiles out of a small Lean wrapper (`wrapper/Lean4Ast.lean`). The wrapper
//! calls `Lean.Parser.parseHeader` and `Lean.Parser.parseCommand` — the parser the
//! Lean frontend itself uses — and hands the resulting `Syntax` tree back **as it
//! is**: a graph of Lean heap objects, which [`raw`] describes and [`syntax`]
//! reads. Nothing is serialised and nothing is copied, so a file costs what Lean
//! spent parsing it, plus whatever the caller asks to read.
//!
//! There is no elaboration, no type checking and no diagnostics here: a file that
//! does not type check still parses, and a syntax error shows up as a
//! [`Syntax::Missing`] node.
//!
//! ```no_run
//! use lean4_ast::{init, parse, Options, ParseRequest};
//!
//! # fn main() -> Result<(), lean4_ast::Error> {
//! init(&Options::new("/home/me/.elan/toolchains/leanprover--lean4---v4.36.0-rc2"))?;
//! let tree = parse(ParseRequest {
//!     path: "Demo.lean",
//!     source: "def two : Nat := 2\n",
//!     imports: &[],
//! })?;
//!
//! // Borrowed views onto Lean's heap: the kind is a slice of Lean's own string.
//! let command = tree.nodes().get(0).expect("a command");
//! assert_eq!(command.short_kind(), "declaration");
//!
//! // Or an owned copy of the whole tree, if that is easier to hold on to.
//! let owned = tree.to_file_ast();
//! assert_eq!(owned.nodes[0].short_kind(), "declaration");
//! # Ok(())
//! # }
//! ```
//!
//! Reading the tree:
//!
//! * every node has a kind — [`Syntax::short_kind`] gives the last dotted component
//!   (`definition`, `theorem`, `declaration`, …) without allocating;
//! * [`Syntax::span`] is a byte range into the source, so a caller can slice a
//!   command out of a file and write it back without disturbing anything else;
//! * [`Syntax::text`] is set for tokens and identifiers;
//! * [`Syntax::missing`] marks a token Lean had to invent because of a syntax error;
//! * [`raw`] is the escape hatch: the Lean object layout, if a caller wants to read
//!   something this crate does not surface.
//!
//! A [`ParseTree`] owns its tree and can be moved between threads; the views
//! borrowed from it cannot outlive it. Tree objects are released back to Lean when
//! the `ParseTree` is dropped, and Lean's runtime is not re-entrant, so every call
//! that enters it is serialised behind one mutex. The expensive part of parsing —
//! loading the modules that define syntax extensions — happens once per imported
//! module list, not once per file.

use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::{Mutex, OnceLock};

mod materialize;
pub mod raw;
pub mod syntax;

mod ffi;

pub use materialize::{FileAst, Node};
pub use syntax::{
    Name, NamePart, SourceInfo, Substring, Syntax, SyntaxAtom, SyntaxIdent, SyntaxList, SyntaxNode,
};

/// The Lean version this library was built against.
///
/// The parser is statically linked, so it reads oleans only from a toolchain of
/// exactly this version — see [`Options`].
pub const LEAN_VERSION: &str = env!("LEAN4_AST_LEAN_VERSION");

/// The toolchain directory the library was built with, on the machine that built
/// it. Empty when `build.py` did not record one.
pub const BUILD_TOOLCHAIN: &str = env!("LEAN4_AST_TOOLCHAIN");

/// Byte range inside the parsed source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub fn len(&self) -> u32 {
        self.end.saturating_sub(self.start)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The text this span covers in `source`.
    pub fn slice<'a>(&self, source: &'a str) -> &'a str {
        let start = (self.start as usize).min(source.len());
        let end = (self.end as usize).min(source.len());
        if start > end {
            return "";
        }
        source.get(start..end).unwrap_or("")
    }
}

/// What Lean the library was built against, and where it looks for modules.
#[derive(Debug, Clone)]
pub struct Info {
    pub version: String,
    pub search_path: Vec<String>,
}

/// Where to find Lean's modules.
#[derive(Debug, Clone)]
pub struct Options {
    /// The Lean toolchain (`…/toolchains/leanprover--lean4---v4.36.0-rc2`).
    pub sysroot: PathBuf,
    /// Extra module directories, e.g. a project's `.lake/build/lib/lean`.
    pub search_path: Vec<PathBuf>,
}

impl Options {
    pub fn new(sysroot: impl Into<PathBuf>) -> Options {
        Options {
            sysroot: sysroot.into(),
            search_path: Vec::new(),
        }
    }

    /// Adds module directories, such as the ones `lake` produces.
    pub fn with_search_path(mut self, paths: impl IntoIterator<Item = PathBuf>) -> Options {
        self.search_path.extend(paths);
        self
    }

    /// Reads the toolchain location from the environment, for tests.
    pub fn from_env() -> Options {
        let sysroot = std::env::var("LEAN4_AST_SYSROOT").unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_default();
            format!("{home}/.elan/toolchains/leanprover--lean4---v4.36.0-rc2")
        });
        Options::new(sysroot)
    }
}

/// One file to parse.
#[derive(Debug, Clone)]
pub struct ParseRequest<'a> {
    /// File name, used in the parser's positions.
    pub path: &'a str,
    pub source: &'a str,
    /// Modules whose syntax extensions the file may use. Empty means
    /// `["Init", "Lean"]`, which is enough for a file that imports nothing.
    pub imports: &'a [&'a str],
}

#[derive(Debug, Clone)]
pub enum Error {
    /// The Lean runtime could not start (see stderr for Lean's own message).
    Runtime,
    /// Lean refused the request: bad arguments or a failed module import.
    Lean(String),
    /// The wrapper, or the heap layout, is not something this version can read.
    Format(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Runtime => write!(f, "the Lean runtime could not start"),
            Error::Lean(message) => write!(f, "Lean: {message}"),
            Error::Format(message) => write!(f, "unexpected answer from Lean: {message}"),
        }
    }
}

impl std::error::Error for Error {}

/// A parsed file, owning Lean's syntax tree.
///
/// Dropping it releases the tree back to Lean's runtime. It may be moved between
/// threads; the views borrowed from it ([`Syntax`], [`SyntaxList`]) may not.
pub struct ParseTree {
    /// The `Except.ok` object Lean answered with, kept as the tree's owner: the
    /// nodes and the error count live inside it.
    root: NonNull<raw::Header>,
}

// The tree is marked multi-threaded by the C shim before it is handed over, which
// makes its reference count atomic, and nothing but this crate's read-only views
// touches it afterwards. Its objects are immutable, so sharing them is safe; the
// allocation itself is released through Lean's runtime, whose allocator is
// thread-safe.
unsafe impl Send for ParseTree {}
unsafe impl Sync for ParseTree {}

impl ParseTree {
    /// Takes ownership of the object Lean answered with.
    ///
    /// # Safety
    ///
    /// `root` must be an owned `Except String (Array Syntax × Nat)` from this
    /// library, which nothing else references.
    unsafe fn from_raw(root: *mut raw::Header) -> ParseTree {
        ParseTree {
            root: NonNull::new(root).expect("lean4-ast: Lean answered a null tree"),
        }
    }

    /// The `Array Syntax × Nat` inside the answer.
    fn pair(&self) -> raw::Obj<'_> {
        unsafe { raw::Obj::from_raw(self.root.as_ptr()) }.field(0)
    }

    /// The raw top-level nodes, for a caller that wants Lean's objects directly.
    pub fn raw(&self) -> raw::Obj<'_> {
        self.pair().field(0)
    }

    /// The file's top-level nodes: header commands (`prelude`, `import`) followed
    /// by the commands in source order.
    pub fn nodes(&self) -> SyntaxList<'_> {
        match self.raw().as_array() {
            Some(array) => SyntaxList::from_array(array),
            None => SyntaxList::empty(),
        }
    }

    /// How many syntax errors the parser reported.
    pub fn syntax_errors(&self) -> usize {
        self.pair().field(1).as_usize().unwrap_or(0)
    }

    /// True when the file parsed without inventing tokens.
    pub fn is_clean(&self) -> bool {
        self.syntax_errors() == 0 && !self.nodes().iter().any(|node| node.missing())
    }

    /// Copies the tree into owned nodes, for a caller that wants it to outlive
    /// this `ParseTree` — or to read it without Lean in the way.
    pub fn to_file_ast(&self) -> FileAst {
        FileAst {
            nodes: self
                .nodes()
                .iter()
                .map(|node| materialize::node(&node))
                .collect(),
            syntax_errors: self.syntax_errors(),
        }
    }
}

impl std::fmt::Debug for ParseTree {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ParseTree")
            .field("nodes", &self.nodes())
            .field("syntax_errors", &self.syntax_errors())
            .finish()
    }
}

impl Drop for ParseTree {
    fn drop(&mut self) {
        unsafe { ffi::lean4_ast_free(self.root.as_ptr()) };
    }
}

/// The options the runtime was last started with, so re-initialising for another
/// project only happens when something actually changed.
fn current() -> &'static Mutex<Option<Info>> {
    static INFO: OnceLock<Mutex<Option<Info>>> = OnceLock::new();
    INFO.get_or_init(|| Mutex::new(None))
}

/// Starts Lean and points it at `options`.
///
/// Called once per project: the module search path is what makes a file's
/// `import` lines resolvable at parse time, so switching projects re-runs it.
pub fn init(options: &Options) -> Result<Info, Error> {
    let _guard = ffi::lock();
    ffi::ensure_ready()?;
    let paths: Vec<String> = options
        .search_path
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect();
    let info = ffi::init(&options.sysroot.to_string_lossy(), &paths)?;
    *current().lock().expect("info lock") = Some(info.clone());
    Ok(info)
}

/// What Lean the library is talking to, if `init` has run.
pub fn info() -> Option<Info> {
    current().lock().ok().and_then(|info| info.clone())
}

/// Parses one file into Lean's syntax tree.
pub fn parse(request: ParseRequest<'_>) -> Result<ParseTree, Error> {
    let _guard = ffi::lock();
    ffi::ensure_ready()?;
    ffi::parse(request.path, request.source, request.imports)
}

/// True when `path` looks like a Lean toolchain directory this crate can use.
pub fn is_toolchain(path: &Path) -> bool {
    path.join("lib/lean").is_dir() && path.join("include/lean/lean.h").is_file()
}

/// The toolchain `readlink` of a `lean` binary points into, when it is an elan
/// proxy: `…/.elan/bin/lean` → its toolchain directory.
pub fn toolchain_of_lean_binary(lean: &Path) -> Option<PathBuf> {
    let target = std::fs::read_link(lean).ok()?;
    let mut directory = target.parent()?.to_path_buf();
    while let Some(parent) = directory.parent() {
        if is_toolchain(&directory) {
            return Some(directory);
        }
        directory = parent.to_path_buf();
    }
    if is_toolchain(&directory) {
        return Some(directory);
    }
    None
}
