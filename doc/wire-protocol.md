# Binary Wire Protocol (feature `wire`)

> Status: MVP / additive. JSON and bincode paths are unchanged. Enable
> with `--features wire`.

The binary wire protocol is a small, fixed-layout, little-endian framing
used by gateways to talk to the engine without going through
`serde_json`. It is intentionally lean — the MVP covers four inbound
order-entry messages and three outbound execution / market-data
messages. A full TCP gateway is out of scope.

## Framing

Every frame on the wire has the layout:

```
+-------------------+--------+--------------------------+
| len (u32 LE)      | kind   | payload                  |
| 4 B               | 1 B    | len - 1 B                |
+-------------------+--------+--------------------------+
```

- `len` is the byte length of `kind + payload`. **It does NOT include
  the 4-byte `len` prefix itself.** The minimum legal `len` is `1`
  (kind byte present, zero-byte payload).
- All multi-byte integers on the wire are **little-endian**.
- Frames have no separator and no trailer — the next frame begins
  immediately after the previous one. Decoders should advance their
  read cursor by the `bytes_consumed` value returned from
  [`decode_frame`](../src/wire/framing.rs).

## `MessageKind` discriminants

Wire codes are stable across `0.7.x` patch releases. Inbound messages
occupy the low half of the byte (`0x01..=0x7F`); outbound messages
occupy the high half (`0x80..=0xFF`). Code `0x00` is reserved as a
"no-message" sentinel.

| Code   | Direction | Message         | Fixed payload size |
|--------|-----------|-----------------|-------------------:|
| `0x01` | inbound   | `NewOrder`      | 48 B               |
| `0x02` | inbound   | `CancelOrder`   | 24 B               |
| `0x03` | inbound   | `CancelReplace` | 40 B               |
| `0x04` | inbound   | `MassCancel`    | 24 B               |
| `0x81` | outbound  | `ExecReport`    | 44 B               |
| `0x82` | outbound  | `TradePrint`    | 48 B               |
| `0x83` | outbound  | `BookUpdate`    | 32 B               |

## Inbound layouts

Inbound messages are `#[repr(C, packed)]` and derive
`zerocopy::{FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout}`,
so the gateway can validate-and-cast `&[u8]` into a typed reference
without copying. Decoding is safe — `zerocopy` performs the layout
validation, no `unsafe` is required at any wire call site.

### `NewOrder` (`0x01`) — 48 B

| Offset | Size | Field           | Type | Notes                                |
|-------:|-----:|-----------------|------|--------------------------------------|
|      0 |    8 | `client_ts`     | u64  | client-side timestamp (ms)           |
|      8 |    8 | `order_id`      | u64  | unique order id                      |
|     16 |    8 | `account_id`    | u64  | numeric account id                   |
|     24 |    8 | `price`         | i64  | tick-scaled limit price              |
|     32 |    8 | `qty`           | u64  | quantity                             |
|     40 |    1 | `side`          | u8   | `0` Buy, `1` Sell                    |
|     41 |    1 | `time_in_force` | u8   | `0` GTC, `1` IOC, `2` FOK, `3` DAY   |
|     42 |    1 | `order_type`    | u8   | `0` Standard (only one in MVP)       |
|     43 |    5 | `_pad`          | u8×5 | reserved, must be zero               |
| **48** |      | **total**       |      |                                      |

`TryFrom<&NewOrderWire> for OrderType<()>` performs the wire → domain
conversion. `account_id` is encoded into the low 8 bytes of the
domain `Hash32` `user_id` so the field round-trips across the
boundary; gateways performing STP must use a non-zero `account_id`.

### `CancelOrder` (`0x02`) — 24 B

| Offset | Size | Field        | Type | Notes                      |
|-------:|-----:|--------------|------|----------------------------|
|      0 |    8 | `client_ts`  | u64  | client-side timestamp (ms) |
|      8 |    8 | `order_id`   | u64  | order id to cancel         |
|     16 |    8 | `account_id` | u64  | numeric account id         |
| **24** |      | **total**    |      |                            |

### `CancelReplace` (`0x03`) — 40 B

| Offset | Size | Field        | Type | Notes                       |
|-------:|-----:|--------------|------|-----------------------------|
|      0 |    8 | `client_ts`  | u64  | client-side timestamp (ms)  |
|      8 |    8 | `order_id`   | u64  | original order id           |
|     16 |    8 | `account_id` | u64  | numeric account id          |
|     24 |    8 | `new_price`  | i64  | replacement limit price     |
|     32 |    8 | `new_qty`    | u64  | replacement quantity        |
| **40** |      | **total**    |      |                             |

### `MassCancel` (`0x04`) — 24 B

| Offset | Size | Field        | Type | Notes                                |
|-------:|-----:|--------------|------|--------------------------------------|
|      0 |    8 | `client_ts`  | u64  | client-side timestamp (ms)           |
|      8 |    8 | `account_id` | u64  | numeric account id                   |
|     16 |    1 | `scope`      | u8   | `0` All, `1` ByAccount, `2` BySide   |
|     17 |    7 | `_pad`       | u8×7 | for `BySide`, `_pad[0] & 1` = side   |
| **24** |      | **total**    |      |                                      |

For `scope == BySide`, the low bit of `_pad[0]` encodes the side
(`0` = Buy, `1` = Sell). Other padding bits must be zero.

## Outbound layouts

Outbound messages use byte-cursor encoders rather than packed structs.
Outbound is I/O-dominated, so the cost of a few dozen bytes of explicit
field-by-field copying into a `Vec<u8>` is dwarfed by socket overhead,
and the layout stays free to evolve without exposing a packed type to
callers.

### `ExecReport` (`0x81`) — 44 B

| Offset | Size | Field            | Type | Notes                            |
|-------:|-----:|------------------|------|----------------------------------|
|      0 |    8 | `engine_seq`     | u64  | global engine sequence           |
|      8 |    8 | `order_id`       | u64  | order id                         |
|     16 |    1 | `status`         | u8   | see `STATUS_*` constants below   |
|     17 |    8 | `filled_qty`     | u64  | cumulative filled quantity       |
|     25 |    8 | `remaining_qty`  | u64  | quantity still resting           |
|     33 |    8 | `price`          | i64  | tick-scaled price                |
|     41 |    2 | `reject_reason`  | u16  | reject code, `0` if not rejected |
|     43 |    1 | `_pad`           | u8   | reserved, must be zero           |
| **44** |      | **total**        |      |                                  |

`status` discriminants (mirror of `OrderStatus`):

| Code | `OrderStatus`       |
|-----:|---------------------|
|    0 | `Open`              |
|    1 | `PartiallyFilled`   |
|    2 | `Filled`            |
|    3 | `Cancelled`         |
|    4 | `Rejected`          |

The `reject_reason` field carries the `RejectReason` numeric code
(stable across `0.7.x`); see `src/orderbook/reject_reason.rs`.

### `TradePrint` (`0x82`) — 48 B

| Offset | Size | Field         | Type | Notes                        |
|-------:|-----:|---------------|------|------------------------------|
|      0 |    8 | `engine_seq`  | u64  | global engine sequence       |
|      8 |    8 | `maker_id`    | u64  | maker order id (resting)     |
|     16 |    8 | `taker_id`    | u64  | taker order id (incoming)    |
|     24 |    8 | `price`       | i64  | tick-scaled fill price       |
|     32 |    8 | `qty`         | u64  | matched quantity             |
|     40 |    8 | `ts`          | u64  | engine timestamp (ms)        |
| **48** |      | **total**     |      |                              |

### `BookUpdate` (`0x83`) — 32 B

| Offset | Size | Field        | Type | Notes                              |
|-------:|-----:|--------------|------|------------------------------------|
|      0 |    8 | `engine_seq` | u64  | global engine sequence             |
|      8 |    1 | `side`       | u8   | `0` Buy, `1` Sell                  |
|      9 |    8 | `price`      | i64  | tick-scaled level price            |
|     17 |    8 | `qty`        | u64  | new total quantity at level (`0` = wiped) |
|     25 |    7 | `_pad`       | u8×7 | reserved, must be zero             |
| **32** |      | **total**    |      | (rounded to 32 B; trailing pad)    |

The trailing 7-byte pad rounds the message to a comfortable 32 B block
and leaves room for forward-compatible field additions without bumping
the wire code.

## Endianness

All multi-byte integers are little-endian. The packed inbound structs
use native-endian primitives, so their memory layout matches the
on-wire byte order only on little-endian targets — accordingly,
`feature = "wire"` is currently restricted to little-endian platforms
via a `compile_error!` in `src/wire/inbound/mod.rs`. Big-endian
support would require switching the packed inbound fields to
endian-aware types (e.g. `zerocopy::little_endian::*`) and is not
implemented in `0.7.x`.

## Round-trip tests

Every inbound and outbound message has a `proptest` round-trip test
that builds a representative shape, encodes through the framer, and
decodes back. See:

- `src/wire/inbound/new_order.rs`
- `src/wire/inbound/cancel.rs`
- `src/wire/inbound/cancel_replace.rs`
- `src/wire/inbound/mass_cancel.rs`
- `src/wire/outbound/exec_report.rs`
- `src/wire/outbound/trade_print.rs`
- `src/wire/outbound/book_update.rs`

A runnable end-to-end demo lives in
`examples/src/bin/wire_roundtrip.rs` (gated on
`required-features = ["wire"]`).
