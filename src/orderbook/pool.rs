use pricelevel::Id;
use std::cell::RefCell;
use std::sync::Arc;

/// A memory pool for reusing vectors to reduce allocations in hot paths.
#[derive(Debug)]
pub struct MatchingPool {
    /// Reusable buffers of `(fully_consumed_maker_id, filled_quantity)` collected
    /// during a match so terminal `Filled` events carry the true fill (#104).
    filled_orders_pool: RefCell<Vec<Vec<(Id, u64)>>>,
    price_vec_pool: RefCell<Vec<Vec<u128>>>,
    /// Reusable buffers for the per-level STP scan snapshot. Each match fills
    /// one of these via `PriceLevel::snapshot_by_seq_into` instead of allocating
    /// a fresh `Vec<Arc<OrderType<()>>>` per conflicting level (#107).
    order_snapshot_pool: RefCell<Vec<Vec<Arc<pricelevel::OrderType<()>>>>>,
}

impl MatchingPool {
    /// Creates a new, empty matching pool.
    pub fn new() -> Self {
        MatchingPool {
            filled_orders_pool: RefCell::new(Vec::with_capacity(4)),
            price_vec_pool: RefCell::new(Vec::with_capacity(4)),
            order_snapshot_pool: RefCell::new(Vec::new()),
        }
    }

    /// Retrieves a vector for filled orders (with their filled quantities) from
    /// the pool.
    pub fn get_filled_orders_vec(&self) -> Vec<(Id, u64)> {
        self.filled_orders_pool
            .borrow_mut()
            .pop()
            .unwrap_or_else(|| Vec::with_capacity(16))
    }

    /// Returns a filled orders vector to the pool for reuse.
    pub fn return_filled_orders_vec(&self, mut vec: Vec<(Id, u64)>) {
        vec.clear();
        self.filled_orders_pool.borrow_mut().push(vec);
    }

    /// Retrieves a vector for the per-level STP scan snapshot from the pool.
    ///
    /// Mirrors [`Self::get_filled_orders_vec`]: pops a reusable buffer or
    /// allocates a fresh one. The caller fills it with
    /// `PriceLevel::snapshot_by_seq_into` and returns it via
    /// [`Self::return_order_snapshot_vec`] (#107).
    pub fn get_order_snapshot_vec(&self) -> Vec<Arc<pricelevel::OrderType<()>>> {
        self.order_snapshot_pool
            .borrow_mut()
            .pop()
            .unwrap_or_else(|| Vec::with_capacity(16))
    }

    /// Returns a STP scan snapshot vector to the pool for reuse.
    ///
    /// Clears the buffer **first** so the `Arc<OrderType<()>>` clones are
    /// dropped immediately — leaving them in place would pin resting orders
    /// alive across reuse. Mirrors [`Self::return_filled_orders_vec`] (#107).
    pub fn return_order_snapshot_vec(&self, mut vec: Vec<Arc<pricelevel::OrderType<()>>>) {
        vec.clear();
        self.order_snapshot_pool.borrow_mut().push(vec);
    }

    /// Retrieves a vector for prices from the pool.
    pub fn get_price_vec(&self) -> Vec<u128> {
        self.price_vec_pool
            .borrow_mut()
            .pop()
            .unwrap_or_else(|| Vec::with_capacity(32))
    }

    /// Returns a price vector to the pool for reuse.
    pub fn return_price_vec(&self, mut vec: Vec<u128>) {
        vec.clear();
        self.price_vec_pool.borrow_mut().push(vec);
    }
}

impl Default for MatchingPool {
    fn default() -> Self {
        Self::new()
    }
}
