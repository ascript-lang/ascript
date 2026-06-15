//! NANB Phase 2 (Task 2.1) — `ThinStr`: a single-allocation, header-length, `!Send`
//! thin string. The ONLY new `unsafe` in Candidate B (spec §3.1.1).
//!
//! TDD note: this file is written test-first. The `#[test]`s below are the contract.
//!
//! # Layout (the single allocation)
//!
//! One `std::alloc` allocation holds, contiguously:
//!
//! ```text
//!   [ StrHeader { strong, len } | utf8 byte 0 | utf8 byte 1 | ... | utf8 byte len-1 ]
//!   ^ ptr (NonNull<StrHeader>)   ^ ptr + size_of::<StrHeader>() (the bytes start)
//! ```
//!
//! `StrHeader` is `#[repr(C)]` so its field order/offsets are fixed and the trailing
//! UTF-8 bytes begin at exactly `size_of::<StrHeader>()` past `ptr`. The handle is a
//! single thin word (`NonNull<StrHeader>`), so `Option<ThinStr>` is also one word via the
//! `NonNull` niche. `len` lives beside `strong` in the SAME allocation/cache line as the
//! first bytes — one dependent load to reach text (the §3.1.1 win over `Rc<Box<str>>`).
//!
//! # The allocation `Layout` (load-bearing — alloc and dealloc MUST agree)
//!
//! The layout is `Layout::new::<StrHeader>().extend(Layout::array::<u8>(len))`, then
//! `.pad_to_align()`. `extend` returns `(combined_layout, offset_of_bytes)` where
//! `offset_of_bytes == size_of::<StrHeader>()` (StrHeader is `usize`-aligned and the byte
//! array needs alignment 1, so no padding is inserted between them — but we still use the
//! returned offset, never a hand-computed one). The padded-to-align layout is stored
//! implicitly by being recomputed identically in `Drop` from the (immutable) `len`, so the
//! `dealloc` `Layout` is bit-identical to the `alloc` one.
//!
//! # Refcount discipline (`Rc`-style, non-atomic)
//!
//! `strong` is a non-atomic `Cell<usize>` (single-threaded `Rc` discipline; `ThinStr` is
//! `!Send`/`!Sync` via `PhantomData<Rc<()>>`). `Clone` increments with the `isize::MAX`
//! abort guard `Rc` uses (a count exceeding `isize::MAX` aborts the process — a refcount
//! that large can only mean `mem::forget` abuse). `Drop` decrements and deallocates the
//! single allocation when the count reaches zero.
//!
//! # UTF-8 invariant
//!
//! Every constructor takes already-valid UTF-8 (`&str`/`String`/`Rc<str>`), so the byte
//! region is valid UTF-8 by construction. `Deref` therefore uses `str::from_utf8_unchecked`
//! over the `len` bytes at `ptr + size_of::<StrHeader>()`.

use std::alloc::{self, Layout};
use std::cell::Cell;
use std::cmp::Ordering;
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;
use std::ops::Deref;
use std::ptr::NonNull;
use std::rc::Rc;

/// The in-allocation header: the refcount and the byte length, immediately followed
/// (in the SAME allocation) by `len` UTF-8 bytes. `#[repr(C)]` fixes the field layout so
/// the bytes always begin at `size_of::<StrHeader>()` past the header pointer.
#[repr(C)]
struct StrHeader {
    /// Non-atomic strong refcount (`Rc` discipline; abort-on-overflow like `Rc`).
    strong: Cell<usize>,
    /// Length in BYTES of the trailing UTF-8 region.
    len: usize,
}

/// A single-word, header-length, reference-counted, `!Send` UTF-8 string — a drop-in for
/// `Rc<str>` by content (`Eq`/`Ord`/`Hash`/`Display`/`Debug` all agree with `&str`), but
/// stored as ONE allocation (`[ StrHeader | bytes ]`) reached by ONE thin pointer. See the
/// module docs for the layout, the alloc/dealloc `Layout` pairing, and the safety model.
pub struct ThinStr {
    ptr: NonNull<StrHeader>,
    /// `!Send` + `!Sync`, exactly like `Rc<str>` (single-threaded refcount discipline).
    _not_send: PhantomData<Rc<()>>,
}

impl ThinStr {
    /// Compute the single-allocation `Layout` for a header plus `len` trailing bytes, and
    /// the byte offset at which the UTF-8 region begins. This is the ONE place the layout
    /// is computed; both `alloc` (in `from_bytes`) and `dealloc` (in `Drop`) call it so the
    /// two `Layout`s are bit-identical by construction.
    #[inline]
    fn layout_for(len: usize) -> (Layout, usize) {
        let header = Layout::new::<StrHeader>();
        // `Layout::array::<u8>(len)` is align 1, size `len` — can only fail if `len`
        // overflows `isize::MAX`, which means the source `&str` could not exist.
        let bytes = Layout::array::<u8>(len).expect("byte length overflows isize::MAX");
        let (combined, offset) = header
            .extend(bytes)
            .expect("ThinStr layout overflow (len overflows isize::MAX)");
        (combined.pad_to_align(), offset)
    }

    /// Build a `ThinStr` from a valid-UTF-8 byte slice (the single shared constructor —
    /// all public `From` impls funnel here). The caller guarantees `bytes` is valid UTF-8
    /// (every caller passes the bytes of a `&str`/`String`/`Rc<str>`).
    fn from_bytes(bytes: &[u8]) -> ThinStr {
        let len = bytes.len();
        let (layout, offset) = Self::layout_for(len);

        // SAFETY: `layout` has non-zero size (`StrHeader` is non-empty, so the combined
        // layout's size is always >= size_of::<StrHeader>() > 0), satisfying `alloc`'s
        // requirement that the layout has non-zero size. We immediately null-check.
        let raw = unsafe { alloc::alloc(layout) };
        let header_ptr = match NonNull::new(raw as *mut StrHeader) {
            Some(p) => p,
            None => alloc::handle_alloc_error(layout),
        };

        // SAFETY: `header_ptr` points at the freshly allocated, properly aligned, owned-but-
        // uninitialized header (the allocation is sized/aligned for `StrHeader` followed by
        // `len` bytes per `layout_for`). We initialize the WHOLE header before any read:
        //   - `write` (not `=`) because the destination is uninitialized.
        //   - `strong` starts at 1 (this `ThinStr` is the first owner).
        //   - `len` records the byte count for `Deref`/`Drop`.
        // Then we copy the `len` UTF-8 bytes into the region starting at `offset` (==
        // size_of::<StrHeader>()); that region is in-bounds of the same allocation and is
        // non-overlapping with `bytes` (a freshly-allocated, distinct allocation), so the
        // `copy_nonoverlapping` is sound. After this block the whole allocation is init.
        unsafe {
            header_ptr.as_ptr().write(StrHeader {
                strong: Cell::new(1),
                len,
            });
            let dst = raw.add(offset);
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), dst, len);
        }

        ThinStr {
            ptr: header_ptr,
            _not_send: PhantomData,
        }
    }

    /// Borrow the header. SAFETY obligation discharged at the single call cluster below.
    #[inline]
    fn header(&self) -> &StrHeader {
        // SAFETY: `self.ptr` was produced by `from_bytes`, which initialized the header
        // before constructing any `ThinStr`, and the allocation stays live as long as any
        // `ThinStr` (including `self`) holds it (refcount > 0 — `Drop` only deallocates at
        // zero, which removes the last handle). So a shared `&StrHeader` is valid here. The
        // only interior mutation is `strong` (a `Cell`), which is sound through `&`.
        unsafe { self.ptr.as_ref() }
    }

    /// The byte length of the string (no allocation, reads the in-header `len`).
    #[inline]
    pub fn len(&self) -> usize {
        self.header().len
    }

    /// Whether the string is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The current strong refcount (for tests + parity assertions).
    #[inline]
    pub fn strong_count(&self) -> usize {
        self.header().strong.get()
    }

    /// Borrow the UTF-8 bytes living immediately after the header in the same allocation.
    #[inline]
    fn as_bytes(&self) -> &[u8] {
        let len = self.len();
        // SAFETY: The bytes begin at `ptr + size_of::<StrHeader>()` (the `#[repr(C)]`
        // offset `from_bytes` copied into, == `layout_for`'s `offset`). `from_bytes`
        // initialized exactly `len` bytes there, all within the single allocation, and the
        // allocation is live (refcount > 0 while `self` exists). `len` is immutable after
        // construction, so the `[ptr_bytes, ptr_bytes + len)` range is fully initialized and
        // in-bounds — `slice::from_raw_parts` is sound. The returned slice borrows `self`,
        // so it cannot outlive the allocation. For `len == 0` the pointer is still within
        // the allocation (a valid one-past-the-header address) and a zero-length slice never
        // dereferences it, which `from_raw_parts` explicitly permits.
        unsafe {
            let bytes_ptr = (self.ptr.as_ptr() as *const u8).add(std::mem::size_of::<StrHeader>());
            std::slice::from_raw_parts(bytes_ptr, len)
        }
    }

    /// Borrow the string content. The bytes are valid UTF-8 by construction (every
    /// constructor takes a `&str`/`String`/`Rc<str>`), so the unchecked conversion is sound.
    #[inline]
    pub fn as_str(&self) -> &str {
        let bytes = self.as_bytes();
        // SAFETY: `bytes` is exactly the UTF-8 region copied verbatim from a `&str` in
        // `from_bytes`; UTF-8 validity is preserved by a byte copy, so the unchecked
        // conversion is sound. (There is no constructor that can introduce non-UTF-8 bytes.)
        unsafe { std::str::from_utf8_unchecked(bytes) }
    }
}

impl Deref for ThinStr {
    type Target = str;
    #[inline]
    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl Clone for ThinStr {
    #[inline]
    fn clone(&self) -> ThinStr {
        let header = self.header();
        let old = header.strong.get();
        // `Rc`'s abort-on-overflow guard: a strong count exceeding `isize::MAX` can only be
        // reached via `mem::forget` abuse, and continuing would risk a refcount wrap → a
        // use-after-free. Aborting (not panicking) matches `Rc::clone` exactly, and is the
        // safe choice — there is no sound way to recover an over-incremented count.
        if old > (isize::MAX as usize) {
            std::process::abort();
        }
        header.strong.set(old + 1);
        ThinStr {
            ptr: self.ptr,
            _not_send: PhantomData,
        }
    }
}

impl Drop for ThinStr {
    fn drop(&mut self) {
        let header = self.header();
        let old = header.strong.get();
        header.strong.set(old - 1);
        if old != 1 {
            // Other handles remain; nothing to free.
            return;
        }
        // Last handle: deallocate the single allocation. Recompute the EXACT `Layout` used
        // to allocate from the immutable `len` (so alloc/dealloc layouts are bit-identical).
        let (layout, _) = Self::layout_for(header.len);
        // SAFETY: We are the last owner (`old == 1`), so no other `&`/`ThinStr` references
        // this allocation — `header` is the final borrow and is not used after this point.
        // `self.ptr` came from `alloc::alloc(layout)` in `from_bytes` with this same
        // `layout` (recomputed identically here), satisfying `dealloc`'s contract that the
        // pointer and layout match the original allocation. The header holds no `Drop`-glue
        // fields (`Cell<usize>`/`usize` are trivially droppable) and the bytes are plain
        // `u8`, so freeing the raw memory is the complete teardown.
        unsafe {
            alloc::dealloc(self.ptr.as_ptr() as *mut u8, layout);
        }
    }
}

impl From<&str> for ThinStr {
    #[inline]
    fn from(s: &str) -> ThinStr {
        ThinStr::from_bytes(s.as_bytes())
    }
}

impl From<String> for ThinStr {
    #[inline]
    fn from(s: String) -> ThinStr {
        ThinStr::from_bytes(s.as_bytes())
    }
}

impl From<&String> for ThinStr {
    #[inline]
    fn from(s: &String) -> ThinStr {
        ThinStr::from_bytes(s.as_bytes())
    }
}

impl From<Rc<str>> for ThinStr {
    #[inline]
    fn from(s: Rc<str>) -> ThinStr {
        ThinStr::from_bytes(s.as_bytes())
    }
}

impl From<&ThinStr> for ThinStr {
    #[inline]
    fn from(s: &ThinStr) -> ThinStr {
        s.clone()
    }
}

// ── Content-based Eq/Ord/Hash (the `Rc<str>` parity contract) ────────────────

impl PartialEq for ThinStr {
    #[inline]
    fn eq(&self, other: &ThinStr) -> bool {
        // Fast path: identical allocation is trivially equal (cheap pointer compare).
        self.ptr == other.ptr || **self == **other
    }
}
impl Eq for ThinStr {}

impl PartialEq<str> for ThinStr {
    #[inline]
    fn eq(&self, other: &str) -> bool {
        &**self == other
    }
}

impl PartialOrd for ThinStr {
    #[inline]
    fn partial_cmp(&self, other: &ThinStr) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ThinStr {
    #[inline]
    fn cmp(&self, other: &ThinStr) -> Ordering {
        (**self).cmp(&**other)
    }
}

impl Hash for ThinStr {
    #[inline]
    fn hash<H: Hasher>(&self, state: &mut H) {
        // Hash by content so a `ThinStr` keys identically to its `&str`/`Rc<str>` content
        // (`str`'s `Hash` impl is what `Rc<str>`/`String` defer to as well).
        (**self).hash(state);
    }
}

impl std::borrow::Borrow<str> for ThinStr {
    #[inline]
    fn borrow(&self) -> &str {
        self
    }
}

impl AsRef<str> for ThinStr {
    #[inline]
    fn as_ref(&self) -> &str {
        self
    }
}

impl std::fmt::Display for ThinStr {
    #[inline]
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&**self, f)
    }
}

impl std::fmt::Debug for ThinStr {
    #[inline]
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Debug as the &str (quoted/escaped), matching `Rc<str>`/`String` Debug output.
        std::fmt::Debug::fmt(&**self, f)
    }
}

// ── Tests (the contract — written FIRST, per the TDD plan Task 2.1 Step 1) ───
#[cfg(test)]
mod tests {
    use super::*;
    use std::hash::{Hash, Hasher};
    use std::rc::Rc;

    fn hash_of<T: Hash>(v: &T) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        v.hash(&mut h);
        h.finish()
    }

    #[test]
    fn thin_str_is_one_word_and_not_send() {
        assert_eq!(std::mem::size_of::<ThinStr>(), 8);
        assert_eq!(std::mem::size_of::<Option<ThinStr>>(), 8); // NonNull niche
        static_assertions::assert_not_impl_any!(ThinStr: Send, Sync);
    }

    #[test]
    fn thin_str_round_trip_clone_drop_balance() {
        for s in ["", "a", "hé", "x".repeat(4096).as_str()] {
            let t = ThinStr::from(s);
            assert_eq!(&*t, s);
            assert_eq!(t.len(), s.len());
            let c = t.clone();
            assert_eq!(t.strong_count(), 2);
            drop(c);
            assert_eq!(t.strong_count(), 1);
            // Hash/Eq/Ord agree with str content (the Rc<str> parity contract).
            assert_eq!(hash_of(&t), hash_of(&s));
        }
    }

    #[test]
    fn thin_str_empty_derefs_to_empty() {
        let t = ThinStr::from("");
        assert_eq!(&*t, "");
        assert!(t.is_empty());
        assert_eq!(t.len(), 0);
    }

    #[test]
    fn thin_str_from_string_and_rc() {
        let t1 = ThinStr::from(String::from("hello"));
        let t2 = ThinStr::from(Rc::<str>::from("hello"));
        assert_eq!(&*t1, "hello");
        assert_eq!(t1, t2);
        assert_eq!(hash_of(&t1), hash_of(&t2));
    }

    #[test]
    fn thin_str_eq_ord_by_content() {
        let a = ThinStr::from("apple");
        let b = ThinStr::from("banana");
        let a2 = ThinStr::from("apple");
        assert_eq!(a, a2);
        assert_ne!(a, b);
        assert!(a < b);
        assert_eq!(a.cmp(&a2), std::cmp::Ordering::Equal);
        assert_eq!(a.cmp(&b), std::cmp::Ordering::Less);
    }

    #[test]
    fn thin_str_display_debug_match_str() {
        let s = "wörld\n\"q\"";
        let t = ThinStr::from(s);
        assert_eq!(format!("{t}"), format!("{s}"));
        assert_eq!(format!("{t:?}"), format!("{s:?}"));
    }

    #[test]
    fn thin_str_clone_shares_allocation_count() {
        let t = ThinStr::from("shared");
        assert_eq!(t.strong_count(), 1);
        let a = t.clone();
        let b = t.clone();
        assert_eq!(t.strong_count(), 3);
        // All views see the same content.
        assert_eq!(&*a, "shared");
        assert_eq!(&*b, "shared");
        drop(a);
        assert_eq!(t.strong_count(), 2);
        drop(b);
        assert_eq!(t.strong_count(), 1);
    }

    #[test]
    fn thin_str_large_round_trip() {
        let s = "λ".repeat(100_000); // multi-byte, large
        let t = ThinStr::from(s.as_str());
        assert_eq!(&*t, s.as_str());
        assert_eq!(t.len(), s.len());
    }
}
