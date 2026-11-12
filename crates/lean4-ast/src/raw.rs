//! Lean's runtime object layout, mirrored in Rust.
//!
//! Lean hands this crate its own heap objects: a `Syntax` tree is a graph of
//! [`Header`]-prefixed allocations, exactly as Lean's runtime built it. Reading
//! it from Rust therefore needs the layout, and this module is the one place that
//! states it. Everything here is a straight transcription of `lean.h`:
//!
//! ```c
//! typedef struct { int m_rc; unsigned m_cs_sz:16; unsigned m_other:8; unsigned m_tag:8; } lean_object;
//! typedef struct { lean_object m_header; size_t m_size; size_t m_capacity; lean_object *m_data[]; } lean_array_object;
//! typedef struct { lean_object m_header; size_t m_size; size_t m_capacity; size_t m_length; char m_data[]; } lean_string_object;
//! typedef struct { lean_object m_header; lean_object *m_objs[]; } lean_ctor_object;
//! ```
//!
//! Two things keep the transcription honest rather than hopeful:
//!
//! * [`check`] asks the C side for `sizeof`/`offsetof` and for the bytes of a
//!   header with known field values, and refuses to run when they disagree with
//!   what Rust compiled in;
//! * the Lean wrapper exports one `Syntax` per constructor, so the tags and field
//!   counts this module assumes are checked against the tree Lean really produces.
//!
//! A Lean `Syntax` value uses at most [`MAX_CTOR_TAG`], so anything above it is a
//! runtime object kind ([`ARRAY`], [`STRING`], …); everything at or below it is a
//! constructor (`m_tag` = constructor index, `m_other` = number of object fields).
//! Values that fit in a pointer are *scalars*: `lean_box(n)` is `n << 1 | 1`, which
//! is how `Nat`s, `Bool`s and nullary constructors are stored.

use std::marker::PhantomData;
use std::mem::{offset_of, size_of};

/// The header every Lean heap object starts with (`lean_object`).
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct Header {
    /// Reference count. Positive: single-threaded, negative: multi-threaded,
    /// zero: not counted (persistent or copied into a compact region).
    pub rc: i32,
    /// Size of a small object; the runtime reuses it for its deletion list.
    pub cs_sz: u16,
    /// Number of object fields of a constructor object.
    pub other: u8,
    /// Constructor index, or the runtime object kind of a big object.
    pub tag: u8,
}

/// Highest tag that denotes a constructor; above it come the runtime's own kinds.
pub const MAX_CTOR_TAG: u8 = 243;
/// `Array α`.
pub const ARRAY: u8 = 246;
/// `String`.
pub const STRING: u8 = 249;
/// A `Nat` too large to be a scalar.
pub const MPZ: u8 = 250;

/// `lean_array_object`, without its trailing `m_data` array.
#[repr(C)]
struct ArrayObject {
    header: Header,
    size: usize,
    capacity: usize,
}

/// `lean_string_object`, without its trailing `m_data` bytes.
#[repr(C)]
struct StringObject {
    header: Header,
    size: usize,
    capacity: usize,
    length: usize,
}

/// `lean_ctor_object`, without its trailing fields.
#[repr(C)]
struct CtorObject {
    header: Header,
}

/// Where an array's elements start.
const ARRAY_DATA: usize = size_of::<ArrayObject>();
/// Where a string's bytes start.
const STRING_DATA: usize = size_of::<StringObject>();
/// Where a constructor's fields start.
const CTOR_FIELDS: usize = size_of::<CtorObject>();

/// A borrowed Lean value: either a scalar, or a pointer to a heap object.
///
/// `'a` is the lifetime of whoever keeps the object alive — for a parsed file,
/// the [`ParseTree`](crate::ParseTree) it came from. Reading is all this type
/// does; it never mutates or frees.
#[derive(Clone, Copy)]
pub struct Obj<'a> {
    raw: *mut Header,
    borrow: PhantomData<&'a Header>,
}

impl<'a> Obj<'a> {
    /// # Safety
    ///
    /// `raw` must be a live Lean value — a scalar, or a heap object that stays
    /// alive and untouched for `'a`.
    pub unsafe fn from_raw(raw: *mut Header) -> Obj<'a> {
        Obj {
            raw,
            borrow: PhantomData,
        }
    }

    /// The underlying pointer, for a caller that wants to talk to C directly.
    pub fn as_raw(&self) -> *mut Header {
        self.raw
    }

    /// True for values that fit in a pointer (`Nat`, `Bool`, nullary constructors).
    pub fn is_scalar(&self) -> bool {
        self.raw as usize & 1 == 1
    }

    /// The runtime tag, or `None` for a scalar.
    pub fn tag(&self) -> Option<u8> {
        if self.is_scalar() {
            None
        } else {
            Some(unsafe { (*self.raw).tag })
        }
    }

    /// The value of a scalar (a `Nat`, a `Bool`, a nullary constructor index).
    pub fn unbox(&self) -> Option<usize> {
        if self.is_scalar() {
            Some(self.raw as usize >> 1)
        } else {
            None
        }
    }

    /// True when this is the constructor numbered `tag`.
    pub fn is_constructor(&self, tag: u8) -> bool {
        self.tag() == Some(tag)
    }

    /// Number of object fields of a constructor object (0 for scalars).
    pub fn field_count(&self) -> usize {
        match self.tag() {
            Some(tag) if tag <= MAX_CTOR_TAG => unsafe { (*self.raw).other as usize },
            _ => 0,
        }
    }

    /// A scalar field of a constructor object — a `Bool`, `UInt8`, … — which Lean
    /// stores after the object fields rather than among them.
    pub fn scalar_u8(&self, index: usize) -> u8 {
        let count = self.field_count();
        unsafe {
            let scalars =
                (self.raw as *const u8).add(CTOR_FIELDS + size_of::<usize>() * count);
            *scalars.add(index)
        }
    }

    /// Field `index` of a constructor object.
    ///
    /// Panics if this is not a constructor object or the index is out of range.
    pub fn field(&self, index: usize) -> Obj<'a> {
        let count = self.field_count();
        assert!(
            index < count,
            "lean4-ast: field {index} of an object with {count} fields"
        );
        unsafe {
            let fields = (self.raw as *const u8).add(CTOR_FIELDS) as *const *mut Header;
            Obj::from_raw(*fields.add(index))
        }
    }

    /// The `Array` behind this value, when it is one.
    pub fn as_array(&self) -> Option<Array<'a>> {
        if self.tag() != Some(ARRAY) {
            return None;
        }
        unsafe {
            let object = self.raw as *const ArrayObject;
            Some(Array {
                data: (self.raw as *const u8).add(ARRAY_DATA) as *const *mut Header,
                len: (*object).size,
                borrow: PhantomData,
            })
        }
    }

    /// The bytes of a `String`, without its NUL terminator.
    pub fn as_bytes(&self) -> Option<&'a [u8]> {
        if self.tag() != Some(STRING) {
            return None;
        }
        unsafe {
            let object = self.raw as *const StringObject;
            let len = (*object).size.saturating_sub(1);
            Some(std::slice::from_raw_parts(
                (self.raw as *const u8).add(STRING_DATA),
                len,
            ))
        }
    }

    /// The text of a `String`. Lean guarantees strings hold valid UTF-8, and this
    /// crate only ever sees strings the parser cut out of a `&str` it was given.
    pub fn as_str(&self) -> Option<&'a str> {
        let bytes = self.as_bytes()?;
        debug_assert!(std::str::from_utf8(bytes).is_ok(), "Lean string is not UTF-8");
        Some(unsafe { std::str::from_utf8_unchecked(bytes) })
    }

    /// The value of a `Nat` small enough to be a scalar.
    pub fn as_usize(&self) -> Option<usize> {
        self.unbox()
    }

    /// The value of a `Bool`.
    pub fn as_bool(&self) -> Option<bool> {
        self.unbox().map(|value| value != 0)
    }
}

impl std::fmt::Debug for Obj<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.tag() {
            None => write!(f, "scalar({})", self.unbox().unwrap_or(0)),
            Some(tag) => f
                .debug_struct("Obj")
                .field("tag", &tag)
                .field("fields", &self.field_count())
                .finish(),
        }
    }
}

/// A borrowed Lean `Array α`.
#[derive(Clone, Copy)]
pub struct Array<'a> {
    data: *const *mut Header,
    len: usize,
    borrow: PhantomData<&'a Header>,
}

impl<'a> Array<'a> {
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Element `index`, or `None` when out of bounds.
    pub fn get(&self, index: usize) -> Option<Obj<'a>> {
        if index >= self.len {
            return None;
        }
        unsafe { Some(Obj::from_raw(*self.data.add(index))) }
    }

    /// Every element, in order. Takes the array by value, so the iterator does
    /// not borrow the object it came from.
    pub fn iter(self) -> impl DoubleEndedIterator<Item = Obj<'a>> + 'a {
        (0..self.len).filter_map(move |index| self.get(index))
    }

    /// The elements as a slice of raw pointers, for callers that want the array
    /// without a per-element wrapper.
    pub fn as_slice(&self) -> &'a [*mut Header] {
        unsafe { std::slice::from_raw_parts(self.data, self.len) }
    }
}

/// What the C side reports about its own idea of the layout.
///
/// Must match `LAYOUT_FIELDS` in `shim/lean4_ast_shim.c`.
pub const LAYOUT_FIELDS: usize = 16;

/// Checks that the layout this module compiled in is the one the Lean runtime
/// actually uses.
///
/// Returns the mismatch in words a user can act on: the only fix is to rebuild
/// the static library with `build.py`, against the toolchain the crate expects.
pub fn check(actual: &[u64; LAYOUT_FIELDS]) -> Result<(), String> {
    let header = Header {
        rc: 0x01020304,
        cs_sz: 0x0506,
        other: 0x07,
        tag: 0x08,
    };
    let expected_size = size_of::<Header>();
    if actual[0] as usize != expected_size {
        return Err(format!(
            "lean_object is {} bytes, this crate was built for {expected_size}",
            actual[0]
        ));
    }
    let bytes =
        unsafe { std::slice::from_raw_parts((&header as *const Header).cast::<u8>(), expected_size) };
    for (index, byte) in bytes.iter().enumerate() {
        if actual[1 + index] != *byte as u64 {
            return Err(format!(
                "lean_object field layout differs at byte {index}: runtime {:#04x}, crate {byte:#04x}",
                actual[1 + index]
            ));
        }
    }

    let expected = [
        (9, offset_of!(ArrayObject, size), "array size"),
        (10, ARRAY_DATA, "array data"),
        (11, STRING_DATA, "string data"),
        (12, offset_of!(StringObject, size), "string size"),
        (13, CTOR_FIELDS, "constructor fields"),
        (14, size_of::<usize>(), "pointer size"),
        (15, MAX_CTOR_TAG as usize, "max constructor tag"),
    ];
    for (slot, want, what) in expected {
        if actual[slot] as usize != want {
            return Err(format!(
                "{what} is at {} in the runtime, {want} in this crate",
                actual[slot]
            ));
        }
    }
    Ok(())
}
