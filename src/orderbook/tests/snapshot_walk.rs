//! `create_snapshot` / `enriched_snapshot` price-ordered walk (#313).

#[cfg(test)]
mod tests {
    use crate::orderbook::OrderBook;
    use pricelevel::{Id, PriceLevel, Side, TimeInForce};
    use std::sync::Arc;

    fn book() -> OrderBook<()> {
        let book = OrderBook::<()>::new("TEST");
        for (id, price) in [(1u64, 1000u128), (2, 990), (3, 980), (4, 970)] {
            book.add_limit_order(
                Id::from_u64(id),
                price,
                10,
                Side::Buy,
                TimeInForce::Gtc,
                None,
            )
            .expect("bid");
        }
        for (id, price) in [(5u64, 1010u128), (6, 1020), (7, 1030), (8, 1040)] {
            book.add_limit_order(
                Id::from_u64(id),
                price,
                10,
                Side::Sell,
                TimeInForce::Gtc,
                None,
            )
            .expect("ask");
        }
        book
    }

    fn prices(levels: &[pricelevel::PriceLevelSnapshot]) -> Vec<u128> {
        levels.iter().map(|l| l.price().as_u128()).collect()
    }

    /// Plants a level with no orders, the state a concurrent cancel or
    /// sweep leaves between removing the last order and removing the level.
    fn plant_empty_levels(book: &OrderBook<()>) {
        book.bids.insert(1050, Arc::new(PriceLevel::new(1050)));
        book.bids.insert(985, Arc::new(PriceLevel::new(985)));
        book.asks.insert(950, Arc::new(PriceLevel::new(950)));
        book.asks.insert(1025, Arc::new(PriceLevel::new(1025)));
    }

    #[test]
    fn bids_descend_and_asks_ascend() {
        let snap = book().create_snapshot(usize::MAX).expect("snapshot");
        assert_eq!(prices(&snap.bids), vec![1000, 990, 980, 970]);
        assert_eq!(prices(&snap.asks), vec![1010, 1020, 1030, 1040]);
    }

    #[test]
    fn depth_is_respected() {
        let book = book();
        let snap = book.create_snapshot(2).expect("snapshot");
        assert_eq!(prices(&snap.bids), vec![1000, 990]);
        assert_eq!(prices(&snap.asks), vec![1010, 1020]);
        let snap = book.create_snapshot(0).expect("snapshot");
        assert!(snap.bids.is_empty() && snap.asks.is_empty());
    }

    #[test]
    fn depth_usize_max_returns_every_level() {
        let snap = book().create_snapshot(usize::MAX).expect("snapshot");
        assert_eq!(snap.bids.len(), 4);
        assert_eq!(snap.asks.len(), 4);
    }

    #[test]
    fn emptied_level_is_skipped_and_leaves_no_gap() {
        let book = book();
        plant_empty_levels(&book);
        let snap = book.create_snapshot(1).expect("snapshot");
        assert_eq!(prices(&snap.bids), vec![1000]);
        assert_eq!(prices(&snap.asks), vec![1010]);
        let snap = book.create_snapshot(3).expect("snapshot");
        assert_eq!(prices(&snap.bids), vec![1000, 990, 980]);
        assert_eq!(prices(&snap.asks), vec![1010, 1020, 1030]);
        let best_ask = snap.best_ask().expect("best ask");
        assert_eq!(best_ask.0, 1010);
        assert!(best_ask.1 > 0);
    }

    #[test]
    fn enriched_snapshot_skips_emptied_levels() {
        let book = book();
        plant_empty_levels(&book);
        let snap = book.enriched_snapshot(2).expect("enriched");
        assert_eq!(prices(&snap.bids), vec![1000, 990]);
        assert_eq!(prices(&snap.asks), vec![1010, 1020]);
    }

    #[test]
    fn hidden_only_level_is_kept() {
        let book = OrderBook::<()>::new("TEST");
        book.add_iceberg_order(
            Id::from_u64(1),
            101,
            0,
            10,
            Side::Sell,
            TimeInForce::Gtc,
            None,
        )
        .expect("hidden-only ask");
        book.add_limit_order(Id::from_u64(2), 102, 5, Side::Sell, TimeInForce::Gtc, None)
            .expect("ask");
        let snap = book.create_snapshot(1).expect("snapshot");
        assert_eq!(prices(&snap.asks), vec![101]);
        let level = snap.asks.first().expect("level");
        assert_eq!(level.visible_quantity().as_u64(), 0);
        assert_eq!(level.order_count(), 1);
    }
}
