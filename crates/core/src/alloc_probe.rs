//! Test-only allocation counter: lets a test prove that a step made no buffer
//! of a given size (an image was moved rather than copied), and that a buffer
//! was freed before the next step ran.
//!
//! Only buffers made by the calling thread are counted, so tests that run in
//! parallel do not see each other's buffers. A watch covers the buffers of at
//! least `min_size` bytes: `made` counts every one allocated while watching,
//! and `live` counts the ones not freed yet.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    static WATCHED: Cell<Option<usize>> = const { Cell::new(None) };
    static MADE: Cell<usize> = const { Cell::new(0) };
    static LIVE: Cell<isize> = const { Cell::new(0) };
}

struct Counting;

#[global_allocator]
static ALLOCATOR: Counting = Counting;

// SAFETY: every call is forwarded unchanged to the system allocator. The
// thread-local bookkeeping neither allocates nor panics.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if watched(layout) {
            let _ = MADE.try_with(|c| c.set(c.get() + 1));
            let _ = LIVE.try_with(|c| c.set(c.get() + 1));
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if watched(layout) {
            let _ = LIVE.try_with(|c| c.set(c.get() - 1));
        }
        unsafe { System.dealloc(ptr, layout) }
    }
}

fn watched(layout: Layout) -> bool {
    WATCHED
        .try_with(|w| w.get().is_some_and(|min| layout.size() >= min))
        .unwrap_or(false)
}

/// Starts counting the buffers of at least `min_size` bytes from zero.
pub(crate) fn watch(min_size: usize) {
    MADE.with(|c| c.set(0));
    LIVE.with(|c| c.set(0));
    WATCHED.with(|w| w.set(Some(min_size)));
}

/// Stops counting and returns how many watched buffers were made.
pub(crate) fn unwatch() -> usize {
    WATCHED.with(|w| w.set(None));
    MADE.with(|c| c.get())
}

/// Watched buffers that are allocated and not freed yet.
pub(crate) fn live() -> usize {
    LIVE.with(|c| c.get().max(0)) as usize
}

/// Runs `f` and returns its result with the number of watched buffers that
/// were made while it ran.
pub(crate) fn large_allocations<T>(min_size: usize, f: impl FnOnce() -> T) -> (T, usize) {
    watch(min_size);
    let out = f();
    (out, unwatch())
}
