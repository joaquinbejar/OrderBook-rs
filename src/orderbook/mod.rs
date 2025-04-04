//! OrderBook implementation for managing multiple price levels and order matching.

mod book;
mod error;
mod operations;
mod snapshot;

mod modifications;
mod private;
mod tests;

pub use book::OrderBook;
pub use error::OrderBookError;
pub use snapshot::OrderBookSnapshot;
