# Benchmark Results

Measured on Apple M4 Max, Rust 1.85 stable, `--release` profile.

Run benchmarks locally:

```sh
cargo bench --all-features
```

## Snapshot & Restore

| Operation | 100 orders | 1,000 orders | 10,000 orders |
|---|---|---|---|
| `create_snapshot` | 127 µs | 147 µs | 329 µs |
| `restore_from_snapshot` | 189 µs | 308 µs | 1.43 ms |
| `enriched_snapshot (ALL)` | 129 µs | 148 µs | 334 µs |
| `enriched_snapshot (MID_PRICE)` | 128 µs | 148 µs | 333 µs |
| `snapshot_json_roundtrip` | 202 µs | 1.76 ms | — |

## Journal & Replay

| Operation | 100 events | 1,000 events | 10,000 events |
|---|---|---|---|
| `journal_append` | 3.7 µs | 22.6 µs | 562 µs |
| `replay_from_journal` | 84 µs | 722 µs | 7.88 ms |
| `replay_verify` | — | — | — |

## Order Operations (existing)

| Operation | Time |
|---|---|
| Add limit order (single) | ~2.1 µs |
| Match market order (deep book) | ~34.6 ns |
| Mixed operations (1,000) | ~172 µs |

## Serialization

| Format | Serialize Trade | Deserialize Trade |
|---|---|---|
| JSON | ~80 µs | ~84 µs |
| Bincode | ~89 µs | ~98 µs |

## Observations

- **Snapshot creation scales sub-linearly**: 10,000 orders only ~2.6x
  slower than 100 orders due to efficient SkipMap iteration.
- **Enriched snapshots add negligible overhead**: metric calculation is
  fast compared to snapshot creation itself.
- **Journal append is very fast**: ~3.7 µs for 100 events (37 ns/event)
  thanks to the in-memory journal implementation.
- **Replay throughput**: ~1.27M events/sec at 10,000 events, suitable for
  fast disaster recovery.
- **Matching is sub-microsecond**: market order matching on a deep book
  takes ~35 ns — well within HFT latency requirements.
