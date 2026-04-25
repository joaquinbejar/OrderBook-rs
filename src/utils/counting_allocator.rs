//! Process-global counting allocator for hot-path allocation budgeting.
//!
//! Behind the `alloc-counters` feature flag. Wraps an inner
//! [`GlobalAlloc`] implementation (`std::alloc::System` by default) and
//! tracks four `AtomicU64` counters: total allocations, total
//! deallocations, total bytes allocated, total bytes deallocated.
//!
//! ## Usage
//!
//! Bench / test binaries opt in by installing the allocator at the
//! crate root:
//!
//! ```ignore
//! use orderbook_rs::utils::CountingAllocator;
//! use std::alloc::System;
//!
//! #[global_allocator]
//! static A: CountingAllocator<System> = CountingAllocator::new(System);
//! ```
//!
//! and read the counters via [`CountingAllocator::allocs`] etc.
//!
//! The library's `rlib` itself does **not** install the allocator —
//! consumers pick their own (`jemalloc`, `mimalloc`, system, …). The
//! wrapper exists to give bench and budget-test binaries a measurement
//! hook without forcing a global choice on the library.
//!
//! ## Why `unsafe`
//!
//! Implementing [`GlobalAlloc`] requires `unsafe impl` per Rust's
//! allocator protocol. The crate's top-level `#![deny(unsafe_code)]`
//! attribute would otherwise reject this module; `#[allow(unsafe_code)]`
//! is applied here as the documented exception. The `unsafe` blocks
//! exist only at the `GlobalAlloc` trait boundary (`alloc`, `dealloc`,
//! `alloc_zeroed`, `realloc`); every block delegates immediately to
//! the inner allocator after updating the counters.

#![allow(unsafe_code)]

use std::alloc::{GlobalAlloc, Layout};
use std::sync::atomic::{AtomicU64, Ordering};

/// Snapshot of the counters at a point in time.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AllocSnapshot {
    /// Total `alloc` / `alloc_zeroed` calls observed since process
    /// start.
    pub allocs: u64,
    /// Total `dealloc` calls observed since process start.
    pub deallocs: u64,
    /// Sum of `Layout::size()` across every observed allocation.
    pub bytes_allocated: u64,
    /// Sum of `Layout::size()` across every observed deallocation.
    pub bytes_deallocated: u64,
}

impl AllocSnapshot {
    /// Return the per-event delta from `earlier` to `self` (e.g.
    /// "allocs after warmup → allocs at end of measurement window").
    #[inline]
    #[must_use]
    pub fn since(self, earlier: Self) -> Self {
        Self {
            allocs: self.allocs.saturating_sub(earlier.allocs),
            deallocs: self.deallocs.saturating_sub(earlier.deallocs),
            bytes_allocated: self.bytes_allocated.saturating_sub(earlier.bytes_allocated),
            bytes_deallocated: self
                .bytes_deallocated
                .saturating_sub(earlier.bytes_deallocated),
        }
    }
}

/// Wrapping allocator that increments per-call counters before
/// delegating to the inner allocator.
///
/// `Inner` is typically `std::alloc::System`. `CountingAllocator` is a
/// generic wrapper so callers can layer it on top of any custom
/// allocator they already use.
pub struct CountingAllocator<Inner: GlobalAlloc> {
    inner: Inner,
    allocs: AtomicU64,
    deallocs: AtomicU64,
    bytes_allocated: AtomicU64,
    bytes_deallocated: AtomicU64,
}

impl<Inner: GlobalAlloc> CountingAllocator<Inner> {
    /// Construct a new counting allocator wrapping `inner`. `const fn`
    /// so it works as the initialiser of a `static` `#[global_allocator]`.
    pub const fn new(inner: Inner) -> Self {
        Self {
            inner,
            allocs: AtomicU64::new(0),
            deallocs: AtomicU64::new(0),
            bytes_allocated: AtomicU64::new(0),
            bytes_deallocated: AtomicU64::new(0),
        }
    }

    /// Total number of allocations observed since process start.
    #[inline]
    pub fn allocs(&self) -> u64 {
        self.allocs.load(Ordering::Relaxed)
    }

    /// Total number of deallocations observed since process start.
    #[inline]
    pub fn deallocs(&self) -> u64 {
        self.deallocs.load(Ordering::Relaxed)
    }

    /// Total bytes allocated since process start.
    #[inline]
    pub fn bytes_allocated(&self) -> u64 {
        self.bytes_allocated.load(Ordering::Relaxed)
    }

    /// Total bytes deallocated since process start.
    #[inline]
    pub fn bytes_deallocated(&self) -> u64 {
        self.bytes_deallocated.load(Ordering::Relaxed)
    }

    /// Capture the four counters into a single struct.
    #[inline]
    pub fn snapshot(&self) -> AllocSnapshot {
        AllocSnapshot {
            allocs: self.allocs(),
            deallocs: self.deallocs(),
            bytes_allocated: self.bytes_allocated(),
            bytes_deallocated: self.bytes_deallocated(),
        }
    }
}

// SAFETY: `GlobalAlloc` is an unsafe trait. Each method below is
// implemented as: increment a counter with `Ordering::Relaxed`, then
// delegate to the inner allocator. The inner allocator's safety
// requirements are forwarded verbatim — every `unsafe` block here only
// calls into the inner allocator's `alloc` / `dealloc` / `realloc` /
// `alloc_zeroed` with the same `layout` / `ptr` the caller passed to
// us. The atomic counter writes are safe (no `unsafe` needed for
// `fetch_add`).
unsafe impl<Inner: GlobalAlloc> GlobalAlloc for CountingAllocator<Inner> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.allocs.fetch_add(1, Ordering::Relaxed);
        self.bytes_allocated
            .fetch_add(layout.size() as u64, Ordering::Relaxed);
        // SAFETY: forwarded `layout` is whatever the caller supplied to
        // `<CountingAllocator as GlobalAlloc>::alloc`; the inner
        // allocator's safety contract is the same.
        unsafe { self.inner.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        self.deallocs.fetch_add(1, Ordering::Relaxed);
        self.bytes_deallocated
            .fetch_add(layout.size() as u64, Ordering::Relaxed);
        // SAFETY: the caller of `<CountingAllocator as GlobalAlloc>::dealloc`
        // already promised `ptr` was returned by a prior `alloc` /
        // `alloc_zeroed` / `realloc` on the same allocator instance.
        unsafe { self.inner.dealloc(ptr, layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        self.allocs.fetch_add(1, Ordering::Relaxed);
        self.bytes_allocated
            .fetch_add(layout.size() as u64, Ordering::Relaxed);
        // SAFETY: same as `alloc`.
        unsafe { self.inner.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // Realloc counts as one alloc + one dealloc with size deltas.
        self.allocs.fetch_add(1, Ordering::Relaxed);
        self.deallocs.fetch_add(1, Ordering::Relaxed);
        self.bytes_allocated
            .fetch_add(new_size as u64, Ordering::Relaxed);
        self.bytes_deallocated
            .fetch_add(layout.size() as u64, Ordering::Relaxed);
        // SAFETY: forwarded `ptr` / `layout` / `new_size` are caller's
        // — the inner allocator's contract is the same.
        unsafe { self.inner.realloc(ptr, layout, new_size) }
    }
}
