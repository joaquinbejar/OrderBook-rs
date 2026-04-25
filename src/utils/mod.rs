mod time;

mod tests;

pub use time::current_time_millis;

#[cfg(feature = "alloc-counters")]
pub mod counting_allocator;

#[cfg(feature = "alloc-counters")]
pub use counting_allocator::{AllocSnapshot, CountingAllocator};
