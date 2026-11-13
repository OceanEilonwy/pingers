// SPDX-License-Identifier: AGPL-3.0-only
//! An allocator that zeroes memory as it is freed (feature `wipe-freed`), as in fob-ffi.
//!
//! Secrets and locations pass through memory nobody controls the lifetime of: the buffers
//! UniFFI copies records into, rusqlite's and minicbor's temporaries, a `String` that grew and
//! left its old copy behind. Wiping every block on free covers all of them at once, where
//! `Zeroizing` covers only the values someone remembered to wrap. It is the process's allocator
//! because this crate is the library the app loads.
//!
//! **The workspace's one `unsafe` exception** (style guide §11; architecture §3, pingers-ffi).
//! `GlobalAlloc` is an unsafe trait and every call into `System` is unsafe, so this module
//! expects `unsafe_code`; the rest of the crate keeps it denied. Each block has a `SAFETY:`
//! comment.
#![expect(
    unsafe_code,
    reason = "a GlobalAlloc must be unsafe; the listed exception in style guide §11"
)]

use std::alloc::{GlobalAlloc, Layout, System};

/// The system allocator, zeroing each block before handing it back.
pub struct Wiping;

/// Zeroes `len` bytes at `ptr` in a way the compiler must keep.
///
/// # Safety
///
/// `ptr` must be valid for writes of `len` bytes.
#[inline]
unsafe fn wipe(ptr: *mut u8, len: usize) {
    // SAFETY: the caller promises `ptr..ptr + len` is writable.
    unsafe { std::ptr::write_bytes(ptr, 0, len) };
    // Zeroing memory that is about to be freed is a dead store the optimiser may delete. An
    // empty asm block that is given the pointer and is not marked `nomem` may read that
    // memory, so the zeroes have to be there first.
    // SAFETY: the asm does nothing; it only stands for a read.
    unsafe { std::arch::asm!("/* {0} */", in(reg) ptr, options(nostack, preserves_flags)) };
}

// SAFETY: every block comes from `System` with the layout it was asked for and goes back to
// `System` with that same layout, so `System`'s guarantees carry over; the only addition is
// writing zeroes into a block while it is still ours, before it is freed.
unsafe impl GlobalAlloc for Wiping {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: passed on with the caller's guarantees.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: passed on with the caller's guarantees.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` is a live block of `layout.size()` bytes from this allocator, so it is
        // writable until `System` takes it back.
        unsafe {
            wipe(ptr, layout.size());
            System.dealloc(ptr, layout);
        }
    }

    // `System`'s own realloc may move the block and free the old one unwiped, so growing or
    // shrinking is a new block, a copy, and the old one wiped and freed, as GlobalAlloc's
    // default does but for the wipe.
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: the caller promises `new_size` is non-zero and, rounded up to
        // `layout.align()`, does not overflow `isize`: a valid layout.
        let new_layout = unsafe { Layout::from_size_align_unchecked(new_size, layout.align()) };
        // SAFETY: a valid, non-zero-sized layout.
        let new = unsafe { self.alloc(new_layout) };
        if !new.is_null() {
            // SAFETY: both blocks are live and distinct, and each is at least the smaller size
            // long; the old block is then ours to free.
            unsafe {
                std::ptr::copy_nonoverlapping(ptr, new, layout.size().min(new_size));
                self.dealloc(ptr, layout);
            }
        }
        new
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn growing_keeps_the_contents() {
        // The test binary runs on this allocator too (lib.rs), so this goes through realloc.
        let mut bytes: Vec<u8> = (0..=255).collect();
        bytes.reserve(4096);
        bytes.extend(0..=255);
        assert_eq!(bytes.len(), 512);
        assert!(bytes.iter().take(256).copied().eq(0..=255));
        assert!(bytes.iter().skip(256).copied().eq(0..=255));
    }
}
