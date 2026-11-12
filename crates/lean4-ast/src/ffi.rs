//! The boundary: three C entry points, and the marshalling around them.
//!
//! Calls are serialised behind one mutex, which is also what makes the one-time
//! layout check safe: the Lean runtime is not re-entrant, and a `Syntax` tree is
//! read (and released) only from this side.

use crate::raw::{self, Header, LAYOUT_FIELDS};
use crate::{Error, Info, ParseTree};
use std::ffi::CString;
use std::os::raw::{c_char, c_int};
use std::sync::{Mutex, MutexGuard, OnceLock};

extern "C" {
    /// Starts the Lean runtime; `0` on success, `1` when it cannot start (the
    /// reason has been printed to stderr).
    pub fn lean4_ast_runtime_start() -> c_int;
    /// `Except String (String × Array String)`; see the wrapper.
    pub fn lean4_ast_init(
        sysroot: *const c_char,
        paths: *const *const c_char,
        n: usize,
    ) -> *mut Header;
    /// `Except String (Array Syntax × Nat)`; see the wrapper.
    pub fn lean4_ast_parse(
        path: *const c_char,
        source: *const c_char,
        source_len: usize,
        imports: *const *const c_char,
        n_imports: usize,
    ) -> *mut Header;
    /// `Array Syntax` with one value per `Syntax` constructor.
    pub fn lean4_ast_probe() -> *mut Header;
    /// Releases an object this library handed out.
    pub fn lean4_ast_free(object: *mut Header);
    /// What the C side thinks the runtime layout is, for [`raw::check`].
    pub fn lean4_ast_layout(out: *mut u64);
}

/// `Except.error` / `Except.ok`, in declaration order.
const EXCEPT_ERROR: u8 = 0;
const EXCEPT_OK: u8 = 1;

/// Serialises every call: the Lean runtime is single-threaded.
pub(crate) fn lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let mutex = LOCK.get_or_init(|| Mutex::new(()));
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Starts the runtime once, and checks that the heap layout this crate reads is
/// the one the linked library produces.
///
/// The check runs on the first call and its answer is kept: if the static library
/// and the crate disagree there is nothing a later call could do differently.
pub(crate) fn ensure_ready() -> Result<(), Error> {
    static READY: OnceLock<Result<(), Error>> = OnceLock::new();
    READY.get_or_init(ready).clone()
}

fn ready() -> Result<(), Error> {
    let mut layout = [0u64; LAYOUT_FIELDS];
    unsafe { lean4_ast_layout(layout.as_mut_ptr()) };
    raw::check(&layout).map_err(Error::Format)?;

    if unsafe { lean4_ast_runtime_start() } != 0 {
        return Err(Error::Runtime);
    }

    // One value per `Syntax` constructor: Lean's own answer to "which tag is
    // `node`, and how many fields does it carry".
    let probe = unsafe { lean4_ast_probe() };
    if probe.is_null() {
        return Err(Error::Runtime);
    }
    let result = check_probe(probe);
    unsafe { lean4_ast_free(probe) };
    result
}

/// The four `Syntax` constructors, in declaration order, with the number of fields
/// each carries: `missing` (a scalar, tag 0), `node` (3), `atom` (2), `ident` (4).
fn check_probe(probe: *mut Header) -> Result<(), Error> {
    let value = unsafe { raw::Obj::from_raw(probe) };
    let array = value
        .as_array()
        .ok_or_else(|| Error::Format("the constructor probe did not answer an array".to_string()))?;
    for (index, tag, fields) in [(0usize, 0usize, 0usize), (1, 1, 3), (2, 2, 2), (3, 3, 4)] {
        let case = array.get(index).ok_or_else(|| {
            Error::Format(format!("the constructor probe is missing case {index}"))
        })?;
        let (actual_tag, actual_fields) = match case.tag() {
            Some(found) => (found as usize, case.field_count()),
            None => (case.unbox().unwrap_or(usize::MAX), 0),
        };
        if actual_tag != tag || actual_fields != fields {
            return Err(Error::Format(format!(
                "this Lean encodes `Syntax` constructor {index} as tag {actual_tag} with \
                 {actual_fields} fields, the crate expects tag {tag} with {fields}: the static \
                 library was built against another Lean, rebuild it with `build.py`"
            )));
        }
    }
    Ok(())
}

/// Points Lean at `sysroot` and `paths`, and reads back what it answered.
pub(crate) fn init(sysroot: &str, paths: &[String]) -> Result<Info, Error> {
    let sysroot = CString::new(sysroot)
        .map_err(|_| Error::Format("the sysroot holds a NUL byte".to_string()))?;
    let paths = pointers(&strings(paths)?);
    let raw_object = unsafe { lean4_ast_init(sysroot.as_ptr(), paths.as_ptr(), paths.len()) };
    let object = except_value(raw_object)?;

    let value = unsafe { raw::Obj::from_raw(object) };
    // `Except.ok (version, searchPath)`: the answer's payload is the pair.
    let answer = value.field(0);
    let info = Info {
        version: answer.field(0).as_str().unwrap_or_default().to_string(),
        search_path: answer
            .field(1)
            .as_array()
            .map(|array| {
                array
                    .iter()
                    .filter_map(|entry| entry.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
    };
    unsafe { lean4_ast_free(object) };
    Ok(info)
}

/// Parses one file, taking ownership of the tree Lean hands back.
pub(crate) fn parse(
    path: &str,
    source: &str,
    imports: &[&str],
) -> Result<ParseTree, Error> {
    let path = CString::new(path).map_err(|_| Error::Format("the path holds a NUL byte".to_string()))?;
    let imports = pointers(&strings(imports)?);
    let raw_object = unsafe {
        lean4_ast_parse(
            path.as_ptr(),
            source.as_ptr().cast::<c_char>(),
            source.len(),
            imports.as_ptr(),
            imports.len(),
        )
    };
    let object = except_value(raw_object)?;
    Ok(unsafe { ParseTree::from_raw(object) })
}

/// Splits an `Except String α` handed over by Lean.
///
/// On `Except.error` the message is copied out and the object released; on
/// `Except.ok` the *whole* object is returned, so that the value inside keeps its
/// reference count without this side ever touching it.
fn except_value(object: *mut Header) -> Result<*mut Header, Error> {
    if object.is_null() {
        return Err(Error::Runtime);
    }
    let value = unsafe { raw::Obj::from_raw(object) };
    match value.tag() {
        Some(EXCEPT_OK) => Ok(object),
        Some(EXCEPT_ERROR) => {
            let message = value
                .field(0)
                .as_str()
                .unwrap_or("no reason given")
                .to_string();
            unsafe { lean4_ast_free(object) };
            Err(Error::Lean(message))
        }
        _ => {
            unsafe { lean4_ast_free(object) };
            Err(Error::Format(
                "Lean answered something that is not an `Except`".to_string(),
            ))
        }
    }
}

/// The pointers of prepared C strings, for a `*const *const c_char` argument.
fn pointers(strings: &[CString]) -> Vec<*const c_char> {
    strings.iter().map(|text| text.as_ptr()).collect()
}

/// `&[&str]` → NUL-terminated C strings, which the C side copies into Lean.
fn strings(items: &[impl AsRef<str>]) -> Result<Vec<CString>, Error> {
    items
        .iter()
        .map(|item| {
            CString::new(item.as_ref())
                .map_err(|_| Error::Format(format!("{:?} holds a NUL byte", item.as_ref())))
        })
        .collect()
}
