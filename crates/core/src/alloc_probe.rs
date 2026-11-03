//! Test-only allocation counter: lets a test prove that a step made no buffer
//! of a given size, for example that an image was moved rather than copied.
//!
//! Only allocations made by the calling thread are counted, so tests that run
//! in parallel do not see each other's buffers.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    static MIN_SIZE: Cell<Option<usize>> = const { Cell::new(None) };
    static COUNT: Cell<usize> = const { Cell::new(0) };
}

struct Counting;

#[global_allocator]
static ALLOCATOR: Counting = Counting;

// SAFETY: every call is forwarded unchanged to the system allocator. The
// thread-local bookkeeping neither allocates nor panics.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = MIN_SIZE.try_with(|min| {
            if min.get().is_some_and(|m| layout.size() >= m) {
                let _ = COUNT.try_with(|c| c.set(c.get() + 1));
            }
        });
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

/// Runs `f` and returns its result with the number of allocations of at
/// least `min_size` bytes that the current thread made while it ran.
pub(crate) fn large_allocations<T>(min_size: usize, f: impl FnOnce() -> T) -> (T, usize) {
    COUNT.with(|c| c.set(0));
    MIN_SIZE.with(|m| m.set(Some(min_size)));
    let out = f();
    MIN_SIZE.with(|m| m.set(None));
    (out, COUNT.with(|c| c.replace(0)))
}
