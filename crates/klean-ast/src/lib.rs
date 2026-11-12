//! Lean 4 syntax trees, from Lean's own parser, in process.
//!
//! This crate is a thin Rust face over a static library that `lean4-ast/build.py`
//! compiles out of a small Lean wrapper (`wrapper/KleanAst.lean`). The wrapper
//! calls `Lean.Parser.parseHeader` and `Lean.Parser.parseCommand` — the parser the
//! Lean frontend itself uses — and serialises the resulting `Syntax` tree. There
//! is no elaboration, no type checking and no diagnostics here: a file that does
//! not type check still parses, and a syntax error shows up as a `missing` node.
//!
//! ```no_run
//! use klean_ast::{init, parse, Options, ParseRequest};
//!
//! # fn main() -> Result<(), klean_ast::Error> {
//! init(&Options::new("/home/me/.elan/toolchains/leanprover--lean4---v4.36.0-rc2"))?;
//! let file = parse(ParseRequest {
//!     path: "Demo.lean",
//!     source: "def two : Nat := 2\n",
//!     imports: &[],
//! })?;
//! assert_eq!(file.nodes[0].short_kind(), "declaration");
//! # Ok(())
//! # }
//! ```
//!
//! Reading the tree:
//!
//! * every node has a [`Node::kind`] — Lean's own syntax kind name, with
//!   [`Node::short_kind`] giving the last dotted component (`definition`,
//!   `theorem`, `declaration`, …);
//! * [`Node::span`] is a byte range into the source, so a caller can slice a
//!   command out of a file and write it back without disturbing anything else;
//! * [`Node::text`] is set for tokens and identifiers;
//! * [`Node::missing`] marks a token Lean had to invent because of a syntax error.
//!
//! The Lean runtime is not re-entrant, so every call is serialised behind one
//! mutex; parsing a file is fast (microseconds to a few milliseconds), and the
//! expensive part — loading the modules that define syntax extensions — happens
//! once per imported-module list.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

pub use serde_json::Value;

/// The Lean version this library was built against.
///
/// The parser is statically linked, so it reads oleans only from a toolchain of
/// exactly this version — see [`Options`].
pub const LEAN_VERSION: &str = env!("KLEAN_AST_LEAN_VERSION");

/// The toolchain directory the library was built with, on the machine that built
/// it. Empty when `build.py` did not record one.
pub const BUILD_TOOLCHAIN: &str = env!("KLEAN_AST_TOOLCHAIN");

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
        &source[start..end]
    }
}

/// One node of Lean's syntax tree.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct Node {
    /// Lean's syntax kind, e.g. `Lean.Parser.Command.definition`.
    pub kind: String,
    /// Byte range in the source; `None` for synthetic (whitespace) nodes.
    pub span: Option<[u32; 2]>,
    /// Token text, for atoms and identifiers.
    #[serde(default)]
    pub text: Option<String>,
    /// True when the parser invented this token after a syntax error.
    #[serde(default)]
    pub missing: bool,
    #[serde(default)]
    pub children: Vec<Node>,
}

impl Node {
    /// Last component of the syntax kind: `definition` for
    /// `Lean.Parser.Command.definition`.
    pub fn short_kind(&self) -> &str {
        self.kind.rsplit('.').next().unwrap_or(&self.kind)
    }

    pub fn span(&self) -> Option<Span> {
        self.span.map(|[start, end]| Span { start, end })
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
        for child in &self.children {
            if let Some(found) = child.find(short_kind) {
                return Some(found);
            }
        }
        None
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
        for child in &self.children {
            if let Some(found) = child.first_ident() {
                return Some(found);
            }
        }
        None
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

/// One parsed file.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct FileAst {
    /// Header commands (`prelude`, `import`) followed by the file's commands.
    pub nodes: Vec<Node>,
    /// How many syntax errors the parser reported. `0` means the file parsed;
    /// the errors themselves are the caller's business (see the LSP for those).
    #[serde(default, rename = "syntaxErrors")]
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

/// What Lean the library was built against, and where it looks for modules.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct Info {
    pub version: String,
    #[serde(default, rename = "searchPath")]
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
        let sysroot = std::env::var("KLEAN_AST_SYSROOT").unwrap_or_else(|_| {
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

#[derive(Debug)]
pub enum Error {
    /// The Lean runtime could not start (see stderr for Lean's own message).
    Runtime,
    /// Lean refused the request: bad arguments or a failed module import.
    Lean(String),
    /// The wrapper answered something this version cannot read.
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

mod ffi {
    use std::os::raw::c_char;

    extern "C" {
        pub fn klean_ast_init(request_json: *const c_char) -> *mut c_char;
        pub fn klean_ast_parse(request_json: *const c_char) -> *mut c_char;
        pub fn klean_ast_string_free(text: *mut c_char);
    }
}

/// Serialises every call: the Lean runtime is single-threaded.
fn lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let mutex = LOCK.get_or_init(|| Mutex::new(()));
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The options the runtime was last started with, so re-initialising for another
/// project only happens when something actually changed.
fn current() -> &'static Mutex<Option<Info>> {
    static INFO: OnceLock<Mutex<Option<Info>>> = OnceLock::new();
    INFO.get_or_init(|| Mutex::new(None))
}

/// Sends one request to the wrapper and reads back its JSON answer.
fn call(
    function: unsafe extern "C" fn(*const std::os::raw::c_char) -> *mut std::os::raw::c_char,
    request: &serde_json::Value,
) -> Result<serde_json::Value, Error> {
    let text = request.to_string();
    let answer = match std::ffi::CString::new(text) {
        Ok(text) => unsafe { function(text.as_ptr()) },
        Err(_) => return Err(Error::Format("the request held a NUL byte".to_string())),
    };
    if answer.is_null() {
        return Err(Error::Runtime);
    }
    let owned = unsafe { std::ffi::CStr::from_ptr(answer) };
    let body = owned.to_string_lossy().to_string();
    unsafe { ffi::klean_ast_string_free(answer) };
    let parsed: serde_json::Value = serde_json::from_str(&body)
        .map_err(|error| Error::Format(format!("{error}: {body}")))?;
    match parsed.get("ok").and_then(Value::as_bool) {
        Some(true) => Ok(parsed),
        _ => {
            let message = parsed
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("no reason given")
                .to_string();
            Err(Error::Lean(message))
        }
    }
}

/// Starts Lean and points it at `options`.
///
/// Called once per project: the module search path is what makes a file's
/// `import` lines resolvable at parse time, so switching projects re-runs it.
pub fn init(options: &Options) -> Result<Info, Error> {
    let _guard = lock();
    let request = serde_json::json!({
        "sysroot": options.sysroot.to_string_lossy(),
        "paths": options
            .search_path
            .iter()
            .map(|path| path.to_string_lossy().to_string())
            .collect::<Vec<_>>(),
    });
    let answer = call(ffi::klean_ast_init, &request)?;
    let info: Info = serde_json::from_value(answer)
        .map_err(|error| Error::Format(format!("init: {error}")))?;
    *current().lock().expect("info lock") = Some(info.clone());
    Ok(info)
}

/// What Lean the library is talking to, if `init` has run.
pub fn info() -> Option<Info> {
    current().lock().ok().and_then(|info| info.clone())
}

/// Parses one file into Lean's syntax tree.
pub fn parse(request: ParseRequest<'_>) -> Result<FileAst, Error> {
    let _guard = lock();
    let payload = serde_json::json!({
        "path": request.path,
        "source": request.source,
        "imports": request.imports,
    });
    let answer = call(ffi::klean_ast_parse, &payload)?;
    serde_json::from_value(answer).map_err(|error| Error::Format(format!("parse: {error}")))
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
