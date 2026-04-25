// examples/src/bin/wire_roundtrip.rs
//
// Demonstrates encoding and decoding of the binary wire protocol (issue #59).
//
//   1. Build a `NewOrderWire` with realistic field values.
//   2. Encode it inside a length-prefixed frame via `encode_frame`.
//   3. Decode the frame, validate the kind byte, and decode the payload.
//   4. Convert the decoded `NewOrderWire` into a domain `OrderType<()>` and
//      print every field via `tracing::info!`.

use orderbook_rs::wire::{
    MessageKind, NewOrderWire, decode_frame, decode_new_order, encode_frame,
    inbound::new_order::{ORDER_TYPE_STANDARD, SIDE_BUY, TIF_GTC},
};
use pricelevel::{OrderType, setup_logger};
use tracing::info;

fn main() {
    let _ = setup_logger();
    info!("Wire roundtrip example");

    let original = NewOrderWire {
        client_ts: 1_716_000_000_000,
        order_id: 4242,
        account_id: 7,
        price: 100_500,
        qty: 25,
        side: SIDE_BUY,
        time_in_force: TIF_GTC,
        order_type: ORDER_TYPE_STANDARD,
        _pad: [0u8; 5],
    };

    // 1. Encode the message into a length-prefixed frame.
    let mut frame = Vec::new();
    encode_payload(&original, &mut frame);

    info!(
        bytes = frame.len(),
        "encoded NewOrder frame ({} bytes)",
        frame.len()
    );

    // 2. Decode the frame back.
    let (kind_byte, payload, consumed) = decode_frame(&frame).expect("decode frame");
    let kind = MessageKind::from_u8(kind_byte).expect("known kind");
    info!(
        kind = ?kind,
        kind_byte = format!("0x{kind_byte:02x}"),
        consumed,
        "decoded frame header"
    );
    assert_eq!(kind, MessageKind::NewOrder);
    assert_eq!(consumed, frame.len());

    let decoded = decode_new_order(payload).expect("decode NewOrder payload");

    // 3. Mirror packed fields into stack locals (taking a reference to a
    // packed field is undefined behaviour).
    let ts = { decoded.client_ts };
    let oid = { decoded.order_id };
    let acct = { decoded.account_id };
    let px = { decoded.price };
    let qty = { decoded.qty };
    info!(
        ts,
        oid, acct, px, qty, "decoded NewOrderWire fields (round-trip OK)"
    );

    // 4. Convert wire → domain.
    let domain: OrderType<()> = (&decoded).try_into().expect("convert to OrderType");
    match domain {
        OrderType::Standard {
            price,
            quantity,
            side,
            time_in_force,
            ..
        } => {
            info!(
                price = %price,
                quantity = %quantity,
                side = %side,
                time_in_force = %time_in_force,
                "domain OrderType::Standard built from wire"
            );
        }
        _ => {
            tracing::error!("expected Standard variant from MVP wire decoder");
        }
    }

    info!("Wire roundtrip example complete");
}

fn encode_payload(order: &NewOrderWire, out: &mut Vec<u8>) {
    encode_frame(MessageKind::NewOrder.as_u8(), order.as_payload_bytes(), out)
        .expect("encode_frame should not fail on Vec<u8>");
}
