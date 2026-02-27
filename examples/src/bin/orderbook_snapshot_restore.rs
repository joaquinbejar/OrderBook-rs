use orderbook_rs::DefaultOrderBook;
use orderbook_rs::orderbook::{ORDERBOOK_SNAPSHOT_FORMAT_VERSION, OrderBookSnapshotPackage};
use pricelevel::{Id, Side, TimeInForce, setup_logger};
use std::error::Error;
use tracing::info;

fn main() -> Result<(), Box<dyn Error>> {
    setup_logger();
    // Initialize an order book for a demo symbol
    let book = DefaultOrderBook::new("SNAPSHOT/RESTORE");

    // Populate the book with bids and asks across multiple price levels
    add_limit_order(&book, 1001, 10_000, 5, Side::Buy)?;
    add_limit_order(&book, 1002, 9_950, 8, Side::Buy)?;
    add_limit_order(&book, 1003, 9_900, 15, Side::Buy)?;

    add_limit_order(&book, 2001, 10_050, 6, Side::Sell)?;
    add_limit_order(&book, 2002, 10_100, 9, Side::Sell)?;
    add_limit_order(&book, 2003, 10_150, 4, Side::Sell)?;

    info!("Created order book snapshot demo with depth across multiple price levels.\n");
    info!("Current best bid: {:?}", book.best_bid());
    info!("Current best ask: {:?}\n", book.best_ask());

    // Capture a checksum-protected snapshot package of the top levels
    let package = book
        .create_snapshot_package(10)
        .expect("snapshot creation should succeed");

    info!(
        "Snapshot captured using format version {} with checksum {}",
        ORDERBOOK_SNAPSHOT_FORMAT_VERSION, package.checksum
    );
    info!(
        "Recorded {} bid levels and {} ask levels\n",
        package.snapshot.bids.len(),
        package.snapshot.asks.len()
    );

    // Serialize the package to JSON for persistence or transfer
    let json_payload = package
        .to_json()
        .expect("snapshot serialization should succeed");
    info!(
        "Serialized snapshot package to JSON (truncated): {}...\n",
        &json_payload[..json_payload.len().min(120)]
    );

    // Reconstruct the package to simulate reading from storage
    let restored_package = OrderBookSnapshotPackage::from_json(&json_payload)
        .expect("restoring snapshot package from JSON should succeed");
    restored_package
        .validate()
        .expect("checksum validation should succeed");

    // Restore a fresh order book from the snapshot package
    let restored = DefaultOrderBook::new("SNAPSHOT/RESTORE");
    restored
        .restore_from_snapshot_package(restored_package)
        .expect("restoring order book from snapshot should succeed");

    info!("Restored order book state:");
    info!("  Best bid: {:?}", restored.best_bid());
    info!("  Best ask: {:?}", restored.best_ask());
    info!("  Mid price: {:?}", restored.mid_price());

    info!("\nSnapshot and restore demo completed successfully.");
    Ok(())
}

fn add_limit_order(
    book: &DefaultOrderBook,
    id: u64,
    price: u128,
    quantity: u64,
    side: Side,
) -> Result<(), Box<dyn Error>> {
    book.add_limit_order(
        Id::from_u64(id),
        price,
        quantity,
        side,
        TimeInForce::Gtc,
        None,
    )
    .map(|_| ())
    .map_err(|err| err.into())
}
