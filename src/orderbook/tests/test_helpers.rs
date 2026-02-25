/// Test helper to construct a PriceLevelSnapshot with pre-set aggregates.
///
/// In pricelevel v0.7 the fields are private, so we round-trip through JSON.
#[allow(dead_code)]
pub fn make_snapshot(
    price: u128,
    visible_quantity: u64,
    hidden_quantity: u64,
    order_count: usize,
) -> pricelevel::PriceLevelSnapshot {
    let json = serde_json::json!({
        "price": price,
        "visible_quantity": visible_quantity,
        "hidden_quantity": hidden_quantity,
        "order_count": order_count,
        "orders": []
    });
    serde_json::from_value(json).expect("valid snapshot JSON")
}
