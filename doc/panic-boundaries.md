# Panic boundaries

Issue #242. Companion to the Production Panic Policy in
`rules/global_rules.md`. Modelled on PriceLevel's `doc/panic-boundaries.md`
(that crate's issue #172/#173), adapted to this crate's dependency set and
public surface.

**Status: complete (post-audit, 0.14.0).** The per-file ratchet
introduced by #242 was burned down by #243-#259 and #265, the final
three-way audit (#260) found the residual gaps fixed in #294 and #295, and
the temporary ratchet ledgers were removed in #260: the gate is absolute
(see "Enforcement" at the end). This document enumerates what that gate
(`[lints.clippy]` in `Cargo.toml`, `scripts/check_panic_policy.py`) cannot
see: irreducible dependency panic surface, the documented `unsafe`
exceptions, caller-supplied code obligations, and the lock / partial
mutation / unwind inventory for each of them.

## Contract

The Production Panic Policy in `rules/global_rules.md` requires that
crate-owned code not initiate panics. For production code (everything
under `src/` outside `#[cfg(test)]` test modules) that is the enforced
state: no production file carries a panic-policy exception, and any new
panicking form, `assert!`-family macro, `saturating_*` / `wrapping_*` on
state, or `#[allow]` / `#[expect]` of a denied clippy lint fails
`make lint`.

This document covers what the gate cannot certify: panic surface the
crate does not own (dependency internals, `unsafe`) and caller-supplied
generic code.

- **What the library guarantees.** It never panics from crate-owned code
  on invalid input, a failed invariant, a dependency error, or an
  exceptional branch, in debug or release, for every feature combination.
  It installs no panic hook, uses no `catch_unwind` / `panic_any` /
  `resume_unwind`, and never aborts on purpose (`std::process::exit` /
  `abort` are denied, see `scripts/check_panic_policy.py`).
- **What it does not guarantee.** It does not recover from a caller panic
  (in the generic `T` on `OrderBook<T>`, or a listener/serializer/journal/
  clock implementation supplied by the caller) and does not certify
  third-party code. An allocator's OOM abort is a process-wide failure and
  is not reported as a typed error (`rules/global_rules.md`'s Production
  Panic Policy is explicit about this). What an unwinding caller panic
  leaves behind is bounded and listed per call site below.

## Irreducible dependency panic surface

Code this crate calls but does not own. Each row is either "no known panic
path reachable with valid crate-internal usage" (the crate's own
responsibility is to keep using it that way) or a specific documented
exception.

| Dependency | Surface used | Panic surface | Notes |
|---|---|---|---|
| `dashmap::DashMap` / `DashSet` | Order-location and user indices (`book.rs`), risk counters and entries (`risk.rs`), order-state entries (`order_state.rs`), special-order trackers (`repricing.rs`) | Internal `RandomState` hasher panics are not part of its public contract; growth (`RawTable` resize) aborts the process on allocator OOM, not a Rust panic | No known panic path from crate-internal usage (keys are `Id`/`String`, never attacker-controlled hash-flooding input in the trusted-input model this crate assumes) |
| `crossbeam-skiplist::SkipMap` | Price-level index (book side maps) | Node allocation aborts the process on allocator OOM, not a Rust panic | Same allocator-OOM caveat as `DashMap` |
| `crossbeam::queue::ArrayQueue` | Per-book pool of recycled listener event buffers (`EventOutbox::pool`, `src/orderbook/emission.rs`, #249) | `ArrayQueue::new` panics on a capacity of zero; `push` / `pop` never panic (a full queue returns the value) | Precondition satisfied by construction: the capacity is the non-zero constant `BUFFER_POOL`. A full pool drops the surplus buffer. No `SegQueue` is used |
| `crossbeam::channel` (`BookManagerStd`) | Unbounded trade-event channel, `bounded(1)` stop signal, `Select` in the processor thread (#255) | `Select::select` panics if no operation is registered; a `SelectedOperation` panics if it is dropped uncompleted or completed with a receiver it was not registered with | `run_std_processor` registers exactly two receivers once and completes every selected operation exhaustively (`if index == stop { recv(&stop) } else { recv(&events) }`), so neither panic is reachable. Sends use the `Result`-returning `send` / `try_send`; the stop channel only ever carries one message, so `try_send` never sees a full buffer |
| `std::collections::hash_map::RandomState` | Default hasher for the above and for every `HashMap`/`HashSet` (`DashMap::new`, `HashMap::new`) | Seeds its keys from OS entropy the first time a thread builds one; `std` panics if the platform entropy source is unavailable (no fallible constructor exists) | Irreducible dependency limit: the crate does not choose a hasher for these maps, and `std` exposes no fallible seeding. Crate-owned entropy is gone (#265: default trade-id namespaces no longer call `Uuid::new_v4()`), so this is the only remaining OS-entropy read |
| `tokio` (`BookManagerTokio`, NATS publishers) | `tokio::sync::{RwLock, Mutex, mpsc, broadcast, watch, oneshot}`, `Handle::spawn`, `tokio::time` | `tokio::spawn` / `Handle::current()` panic when called outside a runtime; `broadcast::Receiver::recv` can return `Lagged`; a `JoinHandle` can return `Err(JoinError)` for a cancelled or panicked task; polling a completed `oneshot::Receiver` again panics | No production call site uses the ambient `tokio::spawn` / `Handle::current()` any more (the remaining ones in `nats.rs` / `nats_common.rs` are inside `#[cfg(test)]` modules). `BookManagerTokio::start_trade_processor{,_with}` resolve the runtime with `Handle::try_current()` and return `ManagerError::NoRuntime` outside one; `start_trade_processor_on` and the NATS publishers take an explicit `Handle` and spawn on it (#255, #253). Spawning onto a runtime that already shut down is not a panic: the task is cancelled and `stop_trade_processor` / `shutdown` report it (`ManagerError::ProcessorCancelled`, `NatsPublisherError::TaskCancelled`). Every processor / publisher `JoinHandle` is awaited by its stop path with `JoinError` mapped to a typed error (panicked vs cancelled). The publishers' `shutdown()` is cancel-safe: the handle is taken out of its slot only for the await and a drop guard puts it back if the shutdown future is dropped first, so a cancelled `shutdown()` never detaches the task and a completed `JoinHandle` is never polled again (#295). `shutdown_with_deadline` wraps the join in `tokio::time::timeout` (which falls back to a far-future deadline instead of overflowing on `Duration::MAX`), then aborts and joins the task (`NatsPublisherError::ShutdownTimedOut`). The manager's stop `oneshot::Receiver` is dropped from the poll loop the first time it resolves, so it is never polled after completion |
| `tokio` time driver (NATS publishers, `nats` feature) | `tokio::time::timeout_at` / `tokio::time::sleep` inside the background task of `NatsTradePublisher` / `NatsBookChangePublisher` | Tokio panics when the runtime was built without the time driver (`enable_time` / `enable_all`); `Handle` exposes no way to check this up front | Documented runtime requirement on both publishers (`new`, `into_listener`, module docs). The runtime is passed explicitly to `new`, so no ambient-runtime lookup (`Handle::current()`) happens. A time-driver panic is confined to the spawned task and surfaced by `shutdown()` as `NatsPublisherError::TaskPanicked`; a runtime that already shut down surfaces as `NatsPublisherError::TaskCancelled`. Every `Instant` / `Duration` computation in the task is checked, builder inputs are clamped (`MAX_BATCH_WINDOW_MS`, `MAX_MIN_PUBLISH_INTERVAL_MS`, `MAX_BATCH_SIZE`, `MAX_CHANNEL_CAPACITY` = `Semaphore::MAX_PERMITS`) so `mpsc::channel` and the batch pre-allocation (`try_reserve`) cannot panic (#253). `with_max_retries` is clamped to `MAX_PUBLISH_RETRIES` (10) with a `WARN` (#295), bounding one publish to 11 attempts (at most about 10 s of backoff plus 11 JetStream ack timeouts) |
| `async-nats` (`nats` feature) | JetStream publish, connection management | Network/protocol errors are typed (`async_nats::Error`); no known panic path in the publish path this crate calls | Not a panic surface but a liveness one (#295): with the server unreachable every publish fails only after its retries. Once shutdown is requested (`shutdown()` sets shared state before signalling, so a normal flush already running sees it too), the first publish that exhausts its retries while the link is down latches a give-up flag: nothing more is published and the remaining buffered events are counted in `dropped_events`, so `shutdown()` is bounded by the publishes already in flight (one or two, each bounded by `MAX_PUBLISH_RETRIES`); `shutdown_with_deadline` gives a hard wall-clock bound |
| `serde_json` | JSON encoding (`EventSerializer` default, journal fallback) | No panic on well-formed input via the `Result`-returning API this crate uses (`to_string`/`from_str`, never the panicking `Value` index operators) | Crate code must never use `serde_json::Value`'s `Index`/`IndexMut` (panics on a missing key) — reviewed at each call site |
| `bincode` (`bincode` feature) | Zero-copy event serialization | No panic on the `Result`-returning encode/decode API this crate uses | — |
| `zerocopy` (`wire` feature) | `FromBytes::ref_from_bytes` (inbound decode), `IntoBytes::as_bytes` (inbound encode) on `#[repr(C, packed)]` types | `ref_from_bytes` returns `Err` on a size/alignment mismatch and never panics; `as_bytes` is infallible | Wire bytes are untrusted (issue #254): the crate's own decoders read through checked offsets and `slice::get`, never indexing or `copy_from_slice`; layout sizes are guarded at compile time by a type-level `[(); N]` equality, not a runtime `assert!`; outbound encoders reserve with `Vec::try_reserve` and return `WireError::CapacityOverflow`. `decode_frame` rejects a `len` above `MAX_FRAME_BODY` (4096) with `InvalidPayload` from the header alone, and the encoders refuse (`InvalidPayload`) the `status` / `_pad` / `side` values their decoders reject (#295) |
| Global allocator | Every collection growth | OOM aborts the process; this is not a Rust panic and is explicitly out of scope (`rules/global_rules.md`) | `try_reserve`/`try_reserve_exact` convert a growth failure into a typed error where the allocation is caller-sized (e.g. a decoded journal/wire length prefix); ordinary amortized `Vec`/`HashMap` growth is not wrapped, matching the OOM-is-not-a-panic carve-out |

### Concurrent-map write paths

Every write to a crate-owned concurrent map, the guard it holds, and what
an unwind in the middle leaves behind. `DashMap` shard locks are
non-reentrant and do **not** poison: an unwind drops the guard and the
next acquisition proceeds. `SkipMap` is lock-free and holds no guard at
all. `BookManagerStd` / `BookManagerTokio` keep their books in a plain
`HashMap` owned through `&mut self` (no shared-map write path), and
`PriceLevelCache` (`cache.rs`) is atomics only.

| Map | Write sites | Guard held across | Unwind effect |
|---|---|---|---|
| `bids` / `asks` (`SkipMap<u128, Arc<PriceLevel>>`) | `get_or_insert_with` in `rest_on_level` (`modifications.rs`; a join on an existing level holds the price's shared stripe, creating a level holds the exclusive stripe through linking, the #309 best-price generation bump and its first admission); `remove_level_if_empty` (exclusive stripe, re-reads emptiness); `restore_from_snapshot_package` commit (`insert`, exclusive gate) | No map guard. The stripe is held for one level operation (see the lock inventory) | A level created by an unwound rest is removed by `UnrestedClaim`; any other emptied level left behind is removed by the next `remove_level_if_empty` on that price, and readers treat an empty level as absent |
| `order_locations` (`DashMap<Id, (u128, Side)>`) | Claim with `entry` in `rest_on_level`; `remove` in the sweep drain (`matching.rs`), `finish_removal` and the zero-quantity `UpdateQuantity` (`modifications.rs`), `withdraw_unrested`; `insert` in the restore commit | One shard guard for the single `entry` / `insert` / `remove` call; no caller code, no log, no other lock | Single-step: the entry is present or absent. The ownership-token order (#288) and the `FilledMakerRelease` / `UnrestedClaim` guards (#294) make sure an unwound admission or sweep does not leave a location behind |
| `user_orders` (`DashMap<Hash32, Vec<Id>>`) | `track_user_order` (`entry().or_default().push`), `untrack_user_order` (`get_mut` + `retain`, then `remove_if` on emptiness), `purge_stale_user_ids` (mass cancel, reads `order_locations` under the guard: lock order `user_orders` then `order_locations`, never the reverse) | One shard guard per call; no caller code, no log | Single-step per call (a `Vec` push or retain). A push that fails to allocate aborts (OOM, out of scope) |
| Risk `orders` / `counters` (`risk.rs`) | `on_admission` (`orders` vacant slot, then `counters` entry: lock order orders then counters), `on_fill` (entry update and full-fill removal in one critical section, #288), `release` (read guard on counters), `release_reservation` / `on_cancel` (`remove_if` / `remove`), `evict_if_zeroed` (`remove_if`), `keep_booked_quantity`, restore (`insert`) | One or two shard guards for one reservation / release. Caller code under them: the `tracing` subscriber only, from the `WARN` of an accounting underflow (`note_release_underflow`, reachable only on a double release or an accounting bug) | The counters are atomics updated with checked read-modify-write, and the `WARN` runs after the update it reports. An unwind out of it can skip the rest of that one release (the open-count slot after a notional underflow) or leave a refused reservation's vacant `orders` slot unfilled; the unwind is under the submit gate, so the kill switch fences the book. No poison |
| Order-state `entries` (`DashMap<Id, TrackedOrder>`, `order_state.rs`) | `record` (the `Clock` is read **before** the entry lock), `withdraw_last_transition`, `evict_if_terminal` (`remove_if`) | One shard guard; no caller code (the listener runs from the deferred dispatcher, not under the entry) | Single-step: status and history change together |
| Special-order trackers (`DashSet<Id>`, `repricing.rs`, `special_orders`) | `register_*` / `unregister_*`, the repricer's conditional release (`remove_if` checking `order_locations` under the tracker shard: lock order tracker then location) | One shard guard; no caller code | A stale id is dropped by the next repricing pass (#291) |

## Documented `unsafe` exceptions

`#![deny(unsafe_code)]` is on `lib.rs`. Two modules are exempt with a
module- or site-level `#[allow(unsafe_code)]`, each behind a feature flag:

- **`memmap2`, feature `journal`, `src/orderbook/sequencer/file_journal.rs`.**
  `FileJournal` memory-maps append-only segment files
  (`memmap2::MmapMut`/`Mmap`). The crate's `unsafe` is three site-level
  `#[allow(unsafe_code)]` blocks (segment creation, reopen, read-only
  mapping), each a single call to `memmap2`'s `MmapMut::map_mut` /
  `Mmap::map` with a `SAFETY` comment (memory-mapping a file is inherently
  unsafe: the kernel can deliver `SIGBUS` on a truncated/concurrently-modified
  backing file, which `memmap2` cannot prevent). Every access to the mapped
  bytes afterwards is safe slice access; it mitigates by pre-allocating (not truncating)
  segment files and by not sharing the mapped file with another writer.
  Since #252 segments are created with `create_new` (rotation onto an
  existing file is `JournalError::SegmentExists`, never a truncation of a
  file a reader may have mapped), appends refuse non-increasing sequences
  before touching disk (`NonMonotonicSequence`), and the old segment is
  never shrunk after rotation. `SIGBUS` from a corrupted external
  truncation is a process-level fault, not a Rust panic, and is explicitly
  out of the Production Panic Policy's scope (irreducible OS-level risk of
  memory-mapped I/O). Journal bytes are untrusted: every length, offset and
  header field is decoded with checked access (`get`, `first_chunk`,
  `try_from`), a non-zero bad header is `JournalError::InvalidEntryHeader`
  (only a zero `entry_length` ends the data), CRC32 checksums detect a
  truncated/corrupted segment, and readers never read past the committed
  write position of the active segment. Reopen zeroes the torn tail with
  safe slice writes, and refuses (does not truncate) corruption followed by
  valid entries. The read/replay path returns `JournalError` /
  `ReplayError`, never panics, for anything short of the kernel-level
  fault above. Since #295 the reopen scan is bounded (cheap pre-checks,
  at most 64 CRC probes / 256 MiB hashed, then a typed refusal), a latest
  segment too small to hold an entry is grown with `set_len` before it is
  mapped (never shrunk, and nothing is mapped yet), a failed append flush
  re-zeroes its bytes, only canonical segment names are listed, and replay
  refuses a journal whose entries end before `last_sequence()`
  (`ReplayError::JournalTruncated`).
- **`CountingAllocator`, feature `alloc-counters`,
  `src/utils/counting_allocator.rs`.** A forwarding `GlobalAlloc` wrapper
  for bench and budget-test binaries (the library never installs it).
  `GlobalAlloc` is an `unsafe trait`, so the module carries
  `#![allow(unsafe_code)]`: the `unsafe impl` and one `unsafe` block per
  method (`alloc`, `dealloc`, `alloc_zeroed`, `realloc`), each of which
  bumps `AtomicU64` counters with `fetch_add` and then forwards the
  caller's `layout` / `ptr` / `new_size` verbatim to the inner allocator,
  whose safety contract is the caller's. Nothing inside allocates, logs,
  panics or loops: the diagnostic counters **wrap** at `u64::MAX`
  (documented; unreachable in practice, and `AllocSnapshot::since` returns
  `None` across a wrap) rather than use a `fetch_update` CAS loop on every
  allocation (#295).

**Macro-generated `unsafe` (informational).** Dependency build scripts
and proc macros run at compile time on the build host and add no runtime
`unsafe` of their own. The one macro that emits `unsafe` into this crate's
items is `zerocopy-derive` (feature `wire`): `#[derive(FromBytes,
IntoBytes, Unaligned, Immutable, KnownLayout)]` on the `#[repr(C, packed)]`
inbound wire types expands to `unsafe impl`s of those traits. rustc's
`unsafe_code` lint does not report expansions of an external macro, and
`zerocopy` validates the layout at derive time (a type that does not
qualify fails to compile), so these are not crate-owned `unsafe` blocks
and need no `#[allow(unsafe_code)]`. No other dependency macro used here
(`serde_derive`, `thiserror`, `tracing` attributes, `bitflags`) emits
`unsafe`.

## Caller-supplied code obligations

`rules/global_rules.md`'s Production Panic Policy: "Caller-supplied code
cannot be globally certified... Document their obligation not to panic,
audit where they execute relative to locks and mutations..., and state the
boundary's limits instead of promising to prevent every external panic."

| Caller-supplied surface | Where it runs | Obligation | Notes |
|---|---|---|---|
| Generic `T` on `OrderBook<T>` (`Clone`, `Default`, `Debug`, and any trait bound the caller's `T` carries) | Order storage, snapshot/clone paths, `Debug` formatting | Must not panic; `Debug` must not leak caller-identifying data if `T` carries user-identifying fields (`rules/global_rules.md`'s Security/Safety section) | No engine lock is held across a `T::clone()`/`T::fmt()` call on the matching hot path (matching operates on `pricelevel`'s `OrderType<()>` internally; `T` is only touched at the book's own boundary, not inside `pricelevel`'s matcher). `T::default()` does run inside gated entry points (order conversion at the boundary, and between a modify's cancel and its re-add); a panic there unwinds out of the entry point mid-mutation, and the unwinding submit-gate guard engages the kill switch on either side of the gate (#249, #294, see "Listener emission and submit-gate poisoning" below) |
| `TradeListener` (`Arc<dyn Fn(&TradeResult) + Send + Sync>`, `src/orderbook/trade.rs`) | Runs after commit, outside the submit gate, ordered by commit (#249): buffered during the mutation, stamped with `engine_seq` under the gate, delivered by the book's single active dispatcher after the gate is released | Must not panic; must return quickly (no blocking I/O) — push into a channel, do not do work inline. May re-enter the book | No book lock is held. An unwind does not corrupt book state and does not poison the gate; it releases the dispatcher role, drops the rest of the batch being delivered (`dropped_listener_events`, `listener_panics`, `ERROR` log) and propagates out of the book call that was dispatching. Queued batches are delivered by the next dispatch (or `flush_listener_events`) |
| Book-manager trade handler (`FnMut(TradeEvent) + Send + 'static`, `BookManagerStd` / `BookManagerTokio::start_trade_processor_with`, `BookManagerTokio::start_trade_processor_on`, `src/orderbook/manager.rs`) | Invoked once per trade event on the manager's processor: a dedicated OS thread (`BookManagerStd`) or a task on a Tokio worker (`BookManagerTokio`), never on the matching path | Must not panic. On Tokio it must also return quickly (move blocking work to `spawn_blocking`) | Runs after the book has committed the trade and its listener has queued the event; no book or manager lock is held. A panic ends the processor only: `stop_trade_processor` surfaces it as `ManagerError::ProcessorPanicked { message }`, and every later trade event is counted in `dropped_trade_events()` (and `orderbook_manager_trade_events_dropped_total` under `metrics`) instead of being processed. Book state is unaffected (#255) |
| `OrderStateListener` (`Arc<dyn Fn(Id, &OrderStatus, &OrderStatus) + Send + Sync>`, `src/orderbook/order_state.rs`) | On a book's tracker: the transition is recorded during the mutation, the listener runs after commit, outside the submit gate, ordered by commit, in the same stream as the trade listener (#249). On a standalone tracker, `OrderStateTracker::transition` calls it inline | Same as `TradeListener` | Same as `TradeListener` |
| `PriceLevelChangedListener` (`Arc<dyn Fn(PriceLevelChangedEvent) + Send + Sync>`, `src/orderbook/book_change_event.rs`) | Runs after commit, outside the submit gate, ordered by commit, in the same `engine_seq` stream as the trade listener (#249; feeds `NatsBookChangePublisher`) | Same as `TradeListener` | Same as `TradeListener` |
| `EventSerializer` impls (`src/orderbook/serialization.rs`) | `NatsTradePublisher` payload encoding only (`with_serializer`). The journal encodes with `serde_json` directly, and `NatsBookChangePublisher` always encodes its batches as JSON (#295), so neither runs a caller-supplied serializer | Must return a typed error rather than panicking on an unencodable value | Crate-provided JSON/Bincode impls follow this; a caller-supplied impl is not re-certified. It runs in the trade publisher's background task with no lock held; a panic there stops the task and is reported by `shutdown()` as `NatsPublisherError::TaskPanicked` |
| `Journal<T>` impls (`src/orderbook/sequencer/journal.rs`) | Append/read of sequencer events (`InMemoryJournal`, `FileJournal`, or a caller's own impl) | Must return `JournalError`/`ReplayError` rather than panicking; must not silently drop or reorder entries; must refuse a non-increasing `append` with `NonMonotonicSequence`, and report an unreadable `last_sequence` as `Err`, never `Ok(None)` (#252) | Crate-provided impls follow this end-to-end (poisoned locks are `MutexPoisoned`; `InMemoryJournal` clones `T` outside its lock on both append and read, #295); a caller-supplied `Journal<T>` is not re-certified |
| `Clock` impls (`src/orderbook/clock.rs`) | Timestamp generation for the book and sequencer. On a book it runs **under the submit gate, mid-mutation**: the book's clock once per sweep (`taker_ts` in `match_order`), the order-state tracker's clock on every recorded transition (`OrderStateTracker::record`, reached from `track_state`: a resting state before the level admits the order, each filled maker's `Filled` in the sweep drain before its indices are released, a taker's terminal state) | Must not panic; must be monotonic if used with `ReplayEngine`'s determinism guarantee | `MonotonicClock` is crate-provided and compliant; a caller-supplied `Clock` breaking monotonicity is a correctness bug in the caller, not a crate panic. An unwind from it leaves the mutation partial; the submit-gate guard then engages the kill switch and latches `submit_gate_poisoned` on either side (#294). Two drop guards bound the partial state (see "Core boundary gaps" below): the sweep drain releases the indices of every filled maker not yet released, and the rest path withdraws an order's claim (location, user index, risk reservation, resting state, an empty level it created) when it unwinds before the level admits the order. Neither path leaves a ghost location |
| Replay progress callbacks (`replay_from_with_progress`, `replay_from_with_clock_and_progress`, `src/orderbook/sequencer/replay.rs`) | Invoked per applied journal entry during replay | Must not panic; must return quickly | Runs after each entry is applied to the in-memory book, not while any lock is held |
| `metrics` recorder (feature `metrics`, `src/orderbook/metrics.rs`) | The process-installed global `metrics` recorder, invoked synchronously from `record_reject` / `record_depth` / `record_trades` / `record_reserve_hidden_discarded` / `record_risk_accounting_anomaly` / `record_match_abort` / `record_match_fold_failure` / `record_trade_ids_exhausted` / `record_manager_trade_event_dropped` on the calling thread (the book's ones under the submit gate). Some calls fire **mid-mutation**: `record_reject` from `track_state` of a `Rejected` state, `record_reserve_hidden_discarded` per strandable maker in the sweep drain, both before the sweep's index cleanup and the taker's resting or terminal bookkeeping finish | Must not panic on a recorded metric; must return quickly; owns its own counter overflow semantics for `increment(n)` | The crate never installs its own recorder (`rules/global_rules.md`'s Logging & Observability rule against installing a global subscriber applies by the same reasoning to a metrics recorder); with none installed the `metrics` crate's no-op recorder is used. The helpers do no integer arithmetic themselves (issue #254); the `u64` to `f64` gauge casts are exact below 2^53 and cannot panic. An unwind from it is handled like a `Clock` unwind (kill switch on either gate side, drain release guard, #294) |
| `tracing` subscriber | Every `tracing::{trace,debug,info,warn,error}!` call site, including mid-mutation ones under the submit gate (the sweep drain's strandable-maker `INFO`) and the `ERROR` the submit-gate guard logs while the thread is already unwinding | Must not panic; must not call back into the book | The crate never installs its own subscriber. A mid-mutation unwind is handled like a `Clock` unwind (#294). A subscriber that panics while the thread is already unwinding aborts the process (a Rust double panic); the guard logs once per book, at the first unwind |

### Per-call-site inventory

Where each caller-supplied surface runs, which crate lock is held across
the call, what is already committed when it runs, and what an unwind out
of it leaves behind. "Kill switch" means the submit-gate guard's policy
(#294): the unwind engages the kill switch and latches
`submit_gate_poisoned` before the gate is released.

| Surface | Call site | Crate locks held across the call | Committed before the call | Unwind effect |
|---|---|---|---|---|
| `T::default()` | Unit conversion of a resting order in `rest_on_level` (after the location claim, before the stripe); order conversion at the entry points (`book.rs`) before any mutation | Submit gate only (never a stripe, a map shard or the outbox) | Rest path: risk reservation, location, user entry, and possibly the resting state; entry points: nothing | Rest path: `UnrestedClaim` withdraws the claim; entry points: nothing to undo. Kill switch either way |
| `T::clone()` | Modify: the copy of the original read before the cancel, and the re-add rebuilt from the cancelled remainder when the order changed meanwhile (#247); snapshot and `get_order` reads | Submit gate (modify), none for reads | Modify: nothing for the first copy; the cancel of the original for the rebuilt re-add | Modify after the cancel: the original is gone and not re-added (the rollback / `ModifyOrderLost` resolution does not run on an unwind); kill switch. Reads: nothing |
| `TradeListener`, `OrderStateListener`, `PriceLevelChangedListener` | Book's deferred dispatcher (`emission.rs`) after the gate is released | None | The whole mutation and its `engine_seq` stamping | Dispatcher role released, undelivered remainder of the batch counted and dropped, queued batches kept; no kill switch (the book is consistent) |
| `OrderStateListener` on a standalone tracker | `OrderStateTracker::transition`, inline | None (the entry guard and the terminal queue are released first) | The transition and its eviction enqueue (#294) | Propagates to the caller; tracker consistent |
| Manager trade handler | `BookManagerStd` processor thread / `BookManagerTokio` task | None | The trade and its listener enqueue | Processor ends; `ManagerError::ProcessorPanicked` at stop; later events counted as dropped |
| `EventSerializer` | `NatsTradePublisher` background task | None | The event was dequeued from the publisher channel | Task ends; `NatsPublisherError::TaskPanicked` at `shutdown()`; buffered events lost |
| `Journal<T>` | Caller's `append`; replay's `last_sequence` / `read_from` and the entry iterator | None (replay takes the book's gate per applied event, never across a journal call) | Replay: every event already applied | Propagates out of replay; the partially replayed book is discarded by the caller |
| `Clock` (book) | `match_order`'s `taker_ts` once per sweep; tracker `record` (before its entry lock) | Submit gate | Sweep: anything the call did before the sweep; tracker: everything up to that transition | `FilledMakerRelease` / `UnrestedClaim` release or withdraw the affected maker / claim; kill switch |
| Replay progress callback | After each applied entry | None | That entry | Propagates out of replay |
| `metrics` recorder | `record_*` helpers, several under the submit gate and mid-mutation (see the row above) | Submit gate for the book's calls; none for the manager's drop counter | Up to the recorded event | As `Clock`; kill switch when under the gate |
| `tracing` subscriber | Every log call; under the submit gate, a level stripe, the outbox lock, the terminal-queue mutex, or a risk shard guard (accounting-underflow `WARN` only) at the sites named in the lock inventory and the concurrent-map table | Any of those | Up to the log call | Guards drop without poisoning a `DashMap`; `std` locks follow the lock inventory's poison policy; kill switch when under the gate. A subscriber must not call back into the book: the map shards and the exclusive gate are not reentrant |

## Default trade-id namespace (#265)

Constructors that are not given a namespace (`OrderBook::new`,
`with_clock`, `with_trade_listener`, `with_trade_and_price_level_listener`
and every constructor built on them) used `Uuid::new_v4()`, which reads OS
entropy through `getrandom` and panics when the RNG fails. They now derive
the namespace with `default_trade_id_namespace` (`src/orderbook/book.rs`):
a UUIDv5 under a per-symbol UUIDv5 namespace, over the process id, the
wall clock in nanoseconds since the UNIX epoch (`0` if the clock reads
before the epoch) and a process-wide `AtomicU64` construction counter.
Every input is read without a panicking path.

Uniqueness argument, as documented on the function:

- same process: the counter advances with `fetch_update` + `checked_add`,
  so every construction gets a distinct value regardless of thread, symbol
  or clock;
- concurrent processes: distinct pids;
- restarts: a later wall clock (and usually a different pid); a collision
  needs the clock stepped back to the same nanosecond with the same pid
  reissued and the same counter value (a pre-epoch clock contributes `0`,
  leaving restarts to the pid while the clock stays broken);
- SHA-1 over distinct names: a 122-bit collision, negligible for
  non-adversarial input.

Counter exhaustion (`u64::MAX` constructions in one process) pins the
counter, logs a `WARN` and leaves uniqueness to the nanosecond wall clock;
it never panics or wraps. pricelevel 0.10's fallible `Id::try_new_uuid`
was not used: it needs a caller-supplied `EntropySource`, and the crate
has no non-panicking OS entropy source without a new dependency. Replay
determinism is unaffected: replay injects the recorded namespace
(`set_trade_id_namespace`, `ReplayBookConfig`), so the default only needs
to be unique. `Uuid::new_v4()` remains only in tests and doc examples.

## Matching sweep failures (#240)

pricelevel 0.10 reports a failure inside `PriceLevel::match_order` through
`MatchResult::error()` while keeping the prefix the level committed. The
book stops the sweep at that level, never trades at a worse price, publishes
the committed prefix exactly like a partial fill (trade listener, price-level
listener, risk `on_fill`, maker states, location cleanup), never rests the
remainder, and returns `OrderBookError::MatchAborted` (taker state
`Cancelled { MatchAborted }`).

Each level's result is folded into the aggregate with pricelevel's
`MatchResult::try_absorb` (PriceLevel#219, pricelevel 0.10.1) when the level
was asked for exactly the aggregate's remaining quantity, and trade by trade
otherwise (quote-notional levels, self-trade prevention pre-matches). A fold
is all or nothing: a refused one leaves the aggregate unchanged, so the
published prefix is always a whole number of levels. Before a level is
matched, the pooled filled-maker buffer is reserved for
`min(resting makers, quantity asked of the level)`, and the aggregate's
trade and filled-id vectors (split reservations, PriceLevel#219) for the same
bound **unless** the aggregate is still empty and the fold will absorb: then
`try_absorb` adopts the level's own buffers, which cannot fail and allocates
nothing. In practice that is the first traded level of a non-FOK
base-quantity sweep (a FOK reserves its whole sweep up front, and a
quote-notional level or an STP pre-match asked for less than the remainder
takes the reserved copy path). A refused reservation aborts the sweep **before** the
level is touched, with the prefix of the earlier levels. A level that failed
mid-match hands its error to the aggregate when it is absorbed; the abort
path rebuilds the published prefix without it, so the committed
`TradeResult` keeps an empty error slot (if that rebuild is refused by the
allocator, the prefix is published with the slot set rather than dropped).

Fill-or-kill takers are preflighted before any mutation, under the exclusive
submit gate (PriceLevel#218, pricelevel 0.10.1). Each level the sweep will
reach is dry-run with `PriceLevel::match_requirements` for exactly the
quantity the sweep will ask of it, and must be matchable in full: not
poisoned (`PriceLevel::is_poisoned`), its counters with headroom
(`MatchRequirements::check` against `PriceLevel::counter_headroom`: the FIFO
queue sequence replenishments take, the topology and mutation epochs), and
no maker step the sweep would stop at (`MatchRequirements::stop_error`). A
self-trade prevention `CancelTaker` / `CancelBoth` pre-match counts the dry
run's fill for `min(quantity, safe_quantity)`, not the visible depth
`safe_quantity` counts ahead of the same-user maker. The
trade-id headroom is checked against the exact sum of
`MatchRequirements::trade_ids_required`, and the result buffers are reserved
for the exact trade count (at least the per-level reservation above), so no
level's fold can need more room once the first level mutated. A shortfall
rejects the taker untouched with `OrderBookError::PriceLevelError`
(`InvalidOperation` for a poisoned level, `CounterExhausted`,
`CapacityExceeded`). The per-level answers are only valid while nothing else
can mutate those levels until the last match returns. The exclusive submit
gate guarantees it: every `OrderBook` mutation path (submits, modifies,
cancels, mass cancels, expiry eviction, and the re-pricers, which go through
`update_order`) takes a side of that gate or `&mut self`, and the public API
hands out no level handles (#228). A fill-or-kill re-add of a modify is
refused before the cancel (#209, #247), so the preflight never runs under
the shared side.

Residuals, stated precisely:

- **Fold beyond the reserved bound.** On a level **after the first traded
  one**, a replenishing iceberg / reserve maker can trade again, and a maker
  admitted to the level by a concurrent submit on the shared submit gate
  while it is being swept can be consumed too. Those trades exceed
  `min(resting makers, quantity asked of the level)`, so their slots grow
  during the fold (unless `try_absorb` can adopt the level's buffer). Only if
  the allocator refuses that growth are the level's committed trades left
  out of the aggregate `MatchResult` (and so out of the `TradeResult` /
  journal), while the level, the makers' risk counters and order states
  reflect them. The sweep then aborts and the gap is logged at `ERROR`
  ("committed trades of a price level could not be folded into the taker's
  result"). Since PriceLevel#219 the first traded level cannot hit this (an
  absorb into an empty aggregate cannot fail), and a fill-or-kill sweep
  cannot hit it at any level it dry-ran exactly (see the next item). On deep
  levels after the first the bound still over-reserves; the capacity is
  amortized and carries over to later levels.
- **Fill-or-kill preflight.** What remains outside it:
  - an allocator refusal inside pricelevel while a level matches (its own
    result buffers; `match_requirements` does not cover allocation);
  - a level where self-trade prevention `CancelMaker` cancels same-user
    makers before the match. The dry run cannot see the post-cancel queue,
    so that level is checked only for poisoning and a closed epoch (cancels
    never reopen one); its trade ids count against the conservative bound
    (`min(makers, quantity)` without hidden depth, the quantity with it),
    its buffers against `min(makers, quantity)` (replenishment trades beyond
    grow the fold, as above), and a failing maker cancel aborts as in #247.

  A FOK stopped by either follows the abort rules above (a partial fill
  reported as `MatchAborted`, never rested).
- **Poisoned levels.** Closed by pricelevel 0.10.1 (PriceLevel#217): a
  poisoned level refuses every match with `InvalidOperation` in
  `MatchResult::error()` and no trades, so the sweep stops there with the
  earlier levels' prefix (`MatchAborted`) instead of walking on to a worse
  price. A post-only probe on a poisoned level is rejected untouched with
  that error, and a fill-or-kill taker is rejected untouched by its
  preflight. The level stays poisoned: rebuild the book from a snapshot.
- **Replay of recorded aborts.** Replay re-executes a journaled
  `SequencerResult::MatchAborted` and requires the same committed prefix.
  An abort caused by trade-id generator exhaustion or by an allocator
  refusal depends on state the journal does not carry (the generator's
  counter is not part of the snapshot package or `ReplayBookConfig`, and
  allocation outcomes are not deterministic), so it usually does not
  reproduce on a fresh replay book: the replayed submit fills further and
  replay stops with `ReplayError::OutcomeMismatch`. That is by design —
  loud, never a silent divergence. Such a journal is replayable at best
  from genesis onto a book whose generator state matches; it is never
  replayable from a mid-stream snapshot, because the snapshot package does
  not carry the trade-id generator. Likewise, a release that changes the
  fill-or-kill preflight's precision (for example #293, which admits a FOK
  that exactly fits the remaining trade ids and rejects one it previously
  let abort mid-sweep) makes cross-version replay of a journal recorded
  before it report `OutcomeMismatch` at that submit, by design.
- **Prefix reconciliation coverage.** Replay compares the committed prefix
  only for submits the sequencer recorded through the `*_with_committed`
  entry points and `SequencerResult::from_submit_failure`. An abort recorded
  through `From<&OrderBookError>` (`RejectedWithCode`, code 15), and every
  aborted `UpdateOrder`, is reconciled by reject code only.
- **Cross-stream disagreement.** The fold-beyond-the-bound case above is
  the only path on which the trade stream can disagree with the book, risk
  and order-state streams. It is counted by `OrderBook::match_fold_failures`
  and the `orderbook_match_fold_failures_total` metric.
- **Exhausted trade-id generator.** Once the generator is exhausted the
  book latches `OrderBook::trade_ids_exhausted` (logged once at `ERROR`,
  `orderbook_trade_ids_exhausted_total`) and rejects every crossing submit,
  crossing modify (before the original is cancelled) and publishing
  `match_*` call untouched with `CapacityExceeded`. The raw `match_order*`
  family still aborts with an empty prefix. A limit taker counts as
  crossing only when its price crosses the best opposite price. No kill
  switch is engaged automatically; replace the generator with
  `set_trade_id_namespace`. The check runs under the submit gate the sweep
  holds, before any mutation, and is exact under the exclusive gate or a
  single writer. Under the shared gate it is best-effort: pricelevel 0.10
  exposes no public atomic id reservation, so concurrent takers racing for
  the last ids can all pass it, and the losers abort inside the sweep with
  `MatchAborted` (their prefix published, possibly empty) instead of the
  untouched rejection.

## Level statistics under concurrent takers (#241)

pricelevel 0.10 (its `PriceLevelStatistics` "Writer contract", issue #153)
supports exactly one concurrent writer of a level's execution aggregates
(`orders_executed`, `quantity_executed`, `value_executed`,
`last_execution_time`, `sum_waiting_time`, and the `stats_degraded` flag the
recorder sets). Its sequence guard protects multi-field readers
(`PriceLevel::snapshot`, serde, `Display`); it is not a writer lock.

This crate does not provide that single writer. Every sweep that holds the
shared side of the submit gate can match at the same level as another one:
non-fill-or-kill takers and matching-capable modifies on an `STPMode::None`
book, and anonymous match-only sweeps (`match_order`) under any mode, as long
as no strandable maker rests. Fill-or-kill, STP-relevant submits and sweeps
in a book holding a strandable maker take the exclusive side and never
overlap. Decision D6: document the statistics as advisory rather than
serialize ordinary sweeps, so there is no gate change and no performance
cost.

Stated precisely:

- **Not affected:** trades, `MatchResult`, `TradeResult` (fees included),
  level queues, quantities, order counts and order vectors, the admission /
  removal counters (`orders_added`, `orders_removed`), and everything the book
  derives from prices and quantities (`depth_statistics`, imbalance,
  distribution, pressure, enriched-snapshot metrics, market impact).
- **Advisory while sweeps overlap:** a snapshot of a level (`create_snapshot`,
  `create_snapshot_package`, `snapshot_to_json`, `enriched_snapshot*`,
  `impl Serialize for OrderBook`) taken while two recorders overlap on it can
  capture a partial execution, for example `orders_executed` counting a fill
  whose `quantity_executed` / `value_executed` has not landed. A package
  captured then checksums and restores those values verbatim.
- **Exact once quiescent:** every counter update is an atomic checked
  read-modify-write and a rollback subtracts exactly what its own call added,
  so the next snapshot with no sweep in flight reads the true totals. Readers
  cannot hang: the sequence never stays odd once writers stop.
- **Replay is exact.** Replay is single-threaded, so the replayed book has
  one writer per level. `snapshots_match` keeps comparing the deterministic
  execution counters: against a live snapshot taken with no sweep in flight
  they are equal, and a live snapshot taken mid-sweep matches no journal
  prefix anyway. The one residual is counter exhaustion, where which
  execution is dropped can depend on the live interleaving.

For exact statistics, capture with no sweep in flight or drive the book from
one submitting thread (as a sequencer does).

## Fee and notional arithmetic (#244)

Fees and trade notionals are never clamped, saturated or dropped:
`FeeSchedule::calculate_fee`, `TradeResult::new` / `with_fees` /
`total_fees` and `TradeInfo::from_trade_result` return typed errors
(`FeeOverflow`, `TradeArithmeticError`). The trade path keeps those errors
unreachable for the trades it commits:

- **Pre-check scope.** Every taker's worst-case notional must fit `u128`
  and be priced exactly by both fee legs, or the taker is rejected
  untouched with `OrderBookError::FeeOverflow` (reject code 18) or
  `NotionalOverflow` (19), state `Rejected`. The worst-case notional is
  the worst reachable price × quantity (limit buy: the limit when it
  passes; other buys: the highest ask reached by walking the asks from the
  best one until their visible quantity covers the order, capped by the
  limit, so an absurd ask the order cannot reach never rejects it; sell:
  the best bid, the highest price a sell can trade at) or, for a
  `*_by_amount` order, the amount. A taker that cannot
  trade (empty opposite side, non-crossing limit, post-only) passes. It
  runs on every submission API (`add_order*`, `submit_market_order*`,
  `submit_market_order_by_amount*`, `match_market_order*`,
  `match_limit_order*`, the raw `match_order*`, and `update_order` before
  the original is cancelled), with or without a trade listener.
- **Ordering.** Under the submit gate the sweep holds, before any
  mutation: right after the #240 trade-id check on the `match_*` /
  `submit_market*` paths, and inside `validate_order_shape` (after the
  exhausted-generator check, before the fill-or-kill preflight) on the
  `add_order*` / modify paths. A validate-first modify runs it once,
  before the original is cancelled; the re-add takes it as done, so a
  `FeeOverflow` / `NotionalOverflow` never follows a cancel (a worse maker
  admitted in between is left to the backstop below).
- **Shared-gate limit.** Exact under the exclusive gate and for a single
  writer; best effort under the shared gate, like the #240 trade-id check:
  a maker admitted concurrently at a worse price than the pre-check saw
  can still be reached by the sweep.
- **Per-level backstop.** The base-quantity sweep re-checks every level
  priced above all prices verified so far (each new level of a buy, the
  first level of a sell). An unpriceable level aborts the sweep **before
  it is touched**: the committed prefix of the earlier levels is published
  like a partial fill and the submit returns `OrderBookError::MatchAborted`
  whose source is `PriceLevelError::InvalidOperation` (reject code 15,
  taker `Cancelled { MatchAborted }`), never `FeeOverflow`. The sequencer
  therefore classifies it as may-have-mutated, as it does every
  `MatchAborted`; `FeeOverflow` / `NotionalOverflow` are only raised
  before mutation. The backstop is seeded with the highest price the
  pre-check verified, so levels at or below it cost one comparison; it
  also covers makers the pre-check's visible-depth walk counts but the
  sweep skips without filling (self-trade prevention, no-progress makers).
  A backstop abort caused by a concurrent maker under the shared gate is
  not reproducible from the journal and replays as
  `ReplayError::OutcomeMismatch` by design; a sequencer feeding a single
  writer never hits it.
- **Residual.** With the pre-check and the backstop, building the
  `TradeResult` of a committed sweep cannot fail. It is handled rather than
  assumed: on failure the book logs at `ERROR`, emits no `TradeResult`, and
  counts it in `OrderBook::match_fold_failures` (and
  `orderbook_match_fold_failures_total`), which therefore covers both
  un-foldable level prefixes (#240) and un-buildable trade results (#244).

## Modify rollback and lost orders (#247)

`UpdatePrice`, `UpdatePriceAndQuantity` and `Replace` are cancel-then-add.
Every admission check runs on the projected order **before** the original
is cancelled (shape, trade-id and fee / notional preflights, modify-aware
risk, the #168 STP self-cross and #230 reserve-residual dry runs), and the
re-add takes that verdict as its admission: it does not re-run the kill
switch, the risk limits or the shape checks, so a kill switch engaged, a
risk limit consumed or a clock tick between the checks and the re-add
cannot fail it. A re-add can still fail after the cancel on a concurrent
mutation under the shared submit gate (the id taken by another submit, a
post-only now crossing) or a resource the book cannot observe beforehand
(a level refusing the admission, a refused allocation, a sweep abort). The
book resolves every such failure instead of losing the order silently:

- **`OrderBookError::ModifyRolledBack` (reject code 20).** The re-add
  failed before any trade. The original is re-rested with the same id,
  price, quantity (as it was when cancelled) and timestamp, its risk
  contribution is reserved again, special-order tracking is re-registered
  and its order state is set back to what it was before the modify
  (`Open` when it had none). It rests at the **back** of its level's queue:
  pricelevel 0.10 assigns a fresh insertion sequence and has no public way
  to reinstate the old one, so **time priority is lost**. The cancel and
  re-add level events were emitted. `source` carries the re-add's error.
- **`OrderBookError::ModifyOrderLost` (reject code 21).** The order is
  gone, with every index consistent (no location, user-index or risk entry;
  any level the attempt created is removed). Two shapes:
  - the re-add traded and then failed (its remainder could not rest, or
    self-trade prevention cancelled it): `executed_quantity > 0`,
    `restore_error: None`. The trades are real, so restoring the original
    would double-count them. A re-add sweep aborted by a failed level after
    trading keeps reporting `MatchAborted` (#240) instead;
  - the re-add failed before trading and the restore failed too:
    `executed_quantity == 0`, `restore_error` says why.
- **`CancelReason::RestFailed`.** The terminal state of an order the book
  could not rest after accepting it: a lost modify whose restore failed
  (`Cancelled { filled_quantity: prior fills, RestFailed }`), and any submit
  whose remainder the level or the risk reservation refused after the sweep
  traded (`Cancelled { filled_quantity: executed, RestFailed }`, the risk
  case returning `RiskRejectedAfterTrades` since #291; a taker that did
  not trade is `Rejected` under the error's code). No state is
  recorded when the failure is a duplicate id: that id's state belongs to
  the live order that owns it.

The restore only rests, it never matches. Under the shared gate an
opposite order can arrive between the cancel and the restore at a price
the original now crosses or locks (a post-only bid at 100 re-priced to 101,
a sell resting at 100 meanwhile, the re-add refused as post-only); resting
the original there would have the engine itself create a locked or crossed
book. The restore is then refused with `PriceCrossing` and the order is
reported lost (`restore_error: PriceCrossing`, `Cancelled { RestFailed }`).

A concurrent taker can also fill part of the order between the modify's
read and its cancel. The re-add is built from the order the cancel
**returned**, never from the earlier read, so no quantity is created:
`UpdatePrice` moves the cancelled remainder; `UpdatePriceAndQuantity` and
`Replace`, whose explicit quantity was chosen against a state that no
longer exists, restore the remainder and return `ModifyRolledBack` with
source `OrderChangedDuringModify` (the conservative option: the caller
decides what to do with the smaller order).

`filled_quantity` is cumulative in every state a re-add records (the fills
the order-state tracker knew for the original, plus fills that raced the
modify, plus the re-add's own). `ModifyOrderLost::executed_quantity` is the
re-add's fills only. The tracker does not record a resting maker's partial
fills, so "known fills" means what it recorded (a taker's pre-rest fills),
the same convention every cancel follows.

Order-state listener sequence on a rollback: `Cancelled { UserRequested }`
for the cancel, then the restored status. A re-add refused by its level
(rather than before it) first records its own resting state, because
since #288 that state is recorded before the level admits the order (see
below), so the sequence is the cancel, the re-add's `Open` /
`PartiallyFilled`, then the restored status. A re-add failure the modify
resolves records no `Rejected` state or reject metric of its own; a
failure raised inside the re-add's sweep (a self-trade-prevention cancel
with no fill, a failed post-only probe, an abort with an empty prefix) is
recorded by the sweep and then overwritten by the restored status. For a
protocol adapter a rollback answers the modify as a plain reject (FIX
`35=9`, order unchanged), but the order lost its time priority, which the
adapter must report separately.

A cancel that finds the order already gone (filled or cancelled
concurrently) returns `Ok(None)` and re-adds nothing. The fill-or-kill
guard on the re-add is a typed `InvalidOperation` raised before the
cancel. The self-trade-prevention maker cancel (`CancelMaker` /
`CancelBoth`) resolves a level failure like #248's single-order cancel
(the maker still rests, or the book completes the removal) and then stops
the sweep with the #240 abort semantics.

**Replay.** `SequencerResult::from(&err)` records both variants as
`RejectedWithCode` with `may_have_mutated: true`, and replay re-executes an
`UpdateOrder` journaled under code 20 or 21 (as under `MatchAborted`)
instead of skipping it, since the live book did change. Their causes (a
concurrent mutation, a level or allocator failure) are not in the journal,
so the re-execution normally succeeds and replay stops with
`ReplayError::OutcomeMismatch` **by design**: loud, never a silent
divergence. A sequencer feeding a single writer only meets them on
resource failures.

**Known limitation.** Replay stops at any journal containing a rollback or
a lost-order modify whose cause does not reproduce on the replay book,
which is every cause except a deterministic resource failure. Such a
journal is replayable only up to that event; recover from a snapshot taken
after it.

## Empty price-level removal (#247)

A level that becomes empty is removed from the bid / ask map by the
single-order cancel, the in-place `UpdateQuantity`, the sweep's drain of
emptied levels and the cleanup of a failed rest. Under the shared submit
gate a concurrent submit can admit into the same `Arc<PriceLevel>` after
the remover saw it empty; an unconditional `SkipMap::remove` then unlinked
the level with that live order inside, indexed in `order_locations` but
unreachable through `bids` / `asks`.

Design: a striped per-price `std::sync::RwLock<()>` (`OrderBook::level_locks`,
64 stripes by `price % 64`). Every admission into an existing level (`get`
plus `PriceLevel::add_order`) takes the stripe's **shared** side; an
admission that creates the level takes the **exclusive** side through
`get_or_insert_with`, the best-price generation bump (#309) and its own
`add_order`, so no join can return before the new level is visible to
`best_bid` / `best_ask`. Shared admissions
at the same price still run in parallel; removing an emptied
level takes the **exclusive** side and re-reads the level under it
(`OrderBook::remove_level_if_empty`): the entry is removed only if it is
still empty, so a refilled level, or one removed and re-created by someone
else, is left in place. Matching and in-place updates never add orders and
take no stripe. A guard is held for one level operation only, never across
a sweep, a listener call or another lock, and never upgraded (a failed
rest drops its shared guard before its cleanup takes the exclusive one), so
the stripes cannot deadlock. Poisoning can only follow a panic that
unwound while a guard was held; the data is `()`, so the guard is
recovered and the poison logged at `ERROR` (the submit gate itself now
engages the kill switch on poison, #249). Mass
cancels and eviction run under the exclusive submit gate and were already
safe.

Cost, measured against main with three interleaved rounds: a first design
with a `Mutex` per stripe serialised admissions at a hot price
(`concurrent_add_limit_orders` 1.7x to 4.5x slower at 2 to 16 threads, all
adding at one price); the shared side keeps concurrent admissions at
main's speed. Single-threaded adds pay one uncontended shared acquire and
release per rested order, and a removed level one exclusive acquire.

## Resting-order indices under concurrent sweeps (#288)

Under the shared submit gate a sweep can match an order from the moment
its level admits it. Everything else that identifies the order as resting
used to be published only after that admission: the `order_locations`
entry, the `user_orders` entry and the `Open` / `PartiallyFilled` state. A
concurrent sweep that consumed the order in between drained a maker with
no index to remove (and recorded `Filled`), after which the resting
thread inserted a location and a user-index entry for an order that no
longer rested and overwrote `Filled` with `Open`. An 8-thread stress test
(`src/orderbook/tests/concurrent_crossing_adds.rs`) failed 100/100 runs on
main.

Contract: `rest_on_level` (the single resting point for a submit's
remainder, a modify's re-add and a modify's restore) publishes, in order,
the risk reservation (#243), the location, the user-index entry and the
resting state, and only then admits the order to its level under the
price's shared stripe (#247). Special-order tracking, the strandable-maker
count (a strandable maker always rests under the exclusive gate, where no
sweep overlaps it), the level event and the depth gauges follow the
admission. A sweep that consumes the order therefore finds its location,
user entry and risk entry, removes them, and records `Filled` after the
resting state; a cancel finds it as soon as the level holds it.

**The location is the id's ownership token** (PR #290 review). It is
claimed atomically with `DashMap::entry` (an occupied entry is
`DuplicateOrderId`, with the reservation released), and every remover
releases it **last**: the sweep's drain, the single-order cancel
(`finish_removal`), the zero-quantity `UpdateQuantity`, a refused
admission's rollback (`withdraw_unrested`) untrack the user entry,
release the risk entry and (a cancel) unregister special orders first
(the raw `place_order_in_book`, which followed the same order, was
removed in #294). A user-index or risk entry for an id therefore only
exists while one admission owns the id, so an id reused as soon as the
previous order is gone (supported, see `strandable_maker_count.rs`) can
neither see nor remove the previous order's entries. A first version of
this fix pushed the user entry after the admission and re-checked the
location by value; that let a reuse of the id at the same price and side
keep a stale second entry (`test_id_reused_mid_rest_keeps_one_user_entry`
fails on it). Emptied `user_orders` entries are dropped with `remove_if`
on emptiness, so a push for the same user between the emptying and the
removal is kept.

Special-order tracking is registered after the admission, as before #288.
A registration that lands after a concurrent cancel leaves a stale id,
which the next repricing pass removes. A repricer releases a tracked id
only while no admission owns it (#291, below), so a same-id order admitted
after the pass read the id's previous order gone keeps its registration.

Cost, measured against main with three interleaved rounds: one location
claim instead of an insert, and the user-index push moved ahead of the
admission. `add_limit_orders` +0.3%, `add_only_hdr` p50 / p99 / p99.9
+0.0% / -1.5% / -0.7%; `concurrent_add_limit_orders` +4.3% at 2 threads
(two threads admitting for one account, whose single user-index entry is
the hot spot), -0.6% at 4, -4.0% at 8, -0.4% at 16;
`concurrent_mixed_operations` +0.2% to +1.3%; `mixed_70_20_10_hdr` p99 /
p99.9 -2.7% / -4.1%; `add_only_risk_hdr` p50 -6.2%.

If the level refuses the order (counter capacity, a poisoned level), the
user entry and reservation are withdrawn, then the location is released,
and the recorded state is followed by the caller's terminal one
(`Rejected`, `Cancelled { RestFailed }`, or a modify's restore). Readers
can briefly observe an order indexed but not yet on its level
(`get_order` returns `None`, a cancel returns `Ok(None)`); never an index
left behind for an order that no longer rests. No lock was added.

**Duplicate ids and replay.** The atomic claim means a same-id submit
that lost a concurrent admission race fails with `DuplicateOrderId`
**after its sweep may have traded**, so `SequencerResult::from` now
classifies `DuplicateOrderId` as `may_have_mutated: true` (the early,
pre-trade duplicate check raises the same error). Such a loser records no
order state (the id belongs to the winner; a terminal state would end the
winner on the tracker) but is counted in the reject metric when it
traded. Replay cannot reproduce it: replayed sequentially the loser meets
the winner resting and is refused by the early check with no fills, the
reject codes agree, and the missing trades surface only in
`snapshots_match`. Unique order ids, or submits serialized per id, are an
ingress / sequencing obligation (see the `sequencer::replay` module docs).

The same stress test exposed a second window, in the risk layer: two
sweeps can share a maker, and the one that consumes it last also calls
`on_maker_removed`. `RiskState::on_fill` zeroed a fully filled entry and
removed it in two steps, so that `on_maker_removed` could take the zeroed
entry in between and release a second open-order slot (and no anomaly was
counted, because the account's other orders kept the counter positive).
The full-fill removal now happens under the entry lock that zeroes it:
whichever of the two takes the entry releases the slot once.

## Post-trade risk rejections and repricer id reuse (#291)

**Post-trade risk rejection.** The pre-trade risk check admits a limit
order's whole quantity before the sweep, and `rest_on_level` then reserves
the residual's contribution (#243). That reservation can still be refused
after the sweep traded: concurrent admissions on the same account under
the shared submit gate, or a counter that cannot represent the residual.
Before #291 the taker returned the plain risk error (`RiskMaxNotional`,
`RiskMaxOpenOrders`), which `SequencerResult::from` classified as never
mutating and whose code replay skips (a `RiskConfig` is not part of
`ReplayBookConfig`), so replay dropped real trades and only
`snapshots_match` noticed.

Contract: a risk refusal of a residual after the taker traded is
`OrderBookError::RiskRejectedAfterTrades { order_id, executed_quantity,
source }` (reject code 22, `may_have_mutated: true`), wrapping the risk
error; the trades were published like a partial fill, the residual did
not rest, the taker ends `Cancelled { RestFailed }`, and nothing is left
indexed (the reservation is the first thing `rest_on_level` publishes, so
its refusal touches nothing). A refusal before any trade keeps the plain
risk error and its pre-mutation classification. A risk-map collision on
the id is the #288 duplicate race and stays `DuplicateOrderId`. A modify's
re-add refused this way is `ModifyOrderLost` with this error as `source`
(journaled as code 21, which only carries the outer reason: replay
re-executes the modify without a `RiskConfig`, the residual rests, and
replay stops with `ReplayError::OutcomeMismatch`, the same documented
limit as every `ModifyOrderLost` (#247); it never diverges silently).

Replay re-executes an `AddOrder` recorded under code 22 through a
crate-internal admission that runs every check and the sweep exactly like
`add_order` and then refuses the residual instead of resting it. The sweep
is a deterministic function of the book and the order, so the replayed
book ends like the live one without the source's `RiskConfig`; the
re-execution must fail under the same code, so a replay that fills the
whole order, or does not trade at all, stops with
`ReplayError::OutcomeMismatch`. Only the code is reconciled, as for every
`RejectedWithCode`; `snapshots_match` stays the oracle.

**Limitation.** Journals written before code 22 recorded such a failure
under a pre-trade risk code with `may_have_mutated: false`. Replay cannot
tell it from a pre-trade rejection and still skips it; for those journals
only `snapshots_match` detects the missing trades.

**Repricer id reuse.** `reprice_pegged_collecting` (and, before #286
moved trailing stops off book, `reprice_trailing_collecting`) releases the
tracker entry of an id whose
order `get_order` no longer finds (a maker the sweep filled is drained
without unregistering). A same-id order admitted between that read and
the release used to lose its registration: its own insert found the stale
entry and was a no-op, and the release then removed it. The release is
now conditional on the id being unowned: under the tracker's shard lock
(`DashSet::remove_if`) the repricer checks that no `order_locations` entry
exists for the id. The location is the id's ownership token (#288): an
order claims it before its level admits it and registers after the
admission, and every remover unregisters before it releases the location.
A registration made before the check therefore follows a claim the check
sees, and one made after it re-inserts the id. Lock order is tracker shard
then location shard; no path holds a location guard while touching the
tracker. A same-id order of another kind keeps the stale entry until it is
gone (a pass skips it: `get_order` finds a non-special order).

## Off-book trailing stops (#286)

Pending trailing stops (`special_orders`, `src/orderbook/stop_orders.rs`)
are held in the book's `PendingStops` store, never on a level, and are
evaluated against the prints of every call that trades (each sweep's
first and last print, recorded per matched level while a stop is
pending) before that call returns. What the evaluation relies on and
what it leaves on failure:

- **Gate mode.** Every mutation of the store (admission, modify, cancel,
  mass cancel, expiry, trail, election, restore) runs under the
  **exclusive** submit gate. While a stop is pending, every call that can
  trade takes the exclusive side (`acquire_submit_gate_for` with
  `can_trade`, re-checking the pending count once the shared side is
  held), and so does every call that targets a pending stop
  (`acquire_gate_for_target`: a cancel, or an `UpdateQuantity` /
  `Cancel` modify, takes the shared side, re-checks whether its target is
  a pending stop and only then upgrades). The stop set and count only
  change under the exclusive side, so what a shared holder reads stays
  true while it holds it: a post-only add or a cancel / resize of another
  order runs concurrently and never touches the store, and the
  evaluation it reaches only reads the print recorder (closed: the
  recorder is only opened by a trade, which runs exclusively). A modify's
  re-add, which may hold the shared side, refuses a trailing stop
  instead of admitting one. The cost: a book holding a pending stop
  serializes the calls that can trade, like a book with STP enabled.
- **Snapshots.** `create_snapshot` holds the shared side for the whole
  capture, so levels, pending stops (with their trailed terms) and the
  last trade price come from one state between two exclusive calls. It is
  a gated call: caller code running under the book's gate must not call
  it (the gate is not reentrant).
- **No reentrancy.** The elected stop's child order (an IOC market
  order, or an IOC limit at the collar price with a `StopProtection`,
  #302) runs through the ungated matching path (the body of `match_market_order_committed`:
  trade-id headroom, arithmetic preflight, `match_order_with_user_outcome`,
  `publish_match_outcome_from`) under the gate the entry point already
  holds; nothing re-acquires the gate. Its events land in the same
  emission scope, after the call's own events (#249).
- **Bounded cascade.** An elected stop leaves the store when it is
  elected, so it is elected at most once; every child order adds at
  most one segment to evaluate, so a cascade evaluates at most
  `pending + 1` segments. Trailing only ever tightens a stop and is
  idempotent at a given price. The bound is the only limit on the
  cascade's length: one print can run every pending stop's child within
  the call that printed it. A protection collar (#302) bounds each child's
  price, not how many run; the collar is a `Copy` value read once per
  elected stop, with `checked_sub` / `checked_add` clamped to `0` /
  `u128::MAX` (a band reaching or passing the bound is unbounded on that
  side), so it adds no failure mode. Classifying a collared
  remainder (`StopProtectionBand` vs `InsufficientLiquidity`) reads the
  best-price cache once; it cannot fail.
- **Arithmetic.** Trailing uses `checked_sub` / `checked_add` (a stop the
  trail cannot represent keeps its price), the admission sequence and the
  pending count are checked. The risk re-booking of a trailed stop
  (`RiskState::rebook_price`) keeps the old booking and counts an
  accounting anomaly when the new notional is not representable; a modify
  re-books in place (`rebook_order`, one step under the entry lock, no
  reservation generation consumed) or fails with nothing changed.
- **Ownership.** A stop's id is owned by its store entry, like a resting
  order's location (#288): admission checks both, the store entry is
  claimed before the `Open` state is recorded, and a cancel releases the
  risk entry and records the state before it removes the entry (under the
  exclusive gate, so no same-id admission can interleave). An elected
  stop's child-order id (UUIDv5 of the trade-id namespace) that is
  already in use is refused (`Rejected { DuplicateOrderId }`).
- **Unwind.** Caller code under the gate during an evaluation is the same
  as for any sweep (`Clock` through the tracker, metrics, `tracing`,
  `T::default()` in conversions) and follows the submit-gate policy: the
  kill switch is engaged. An unwind between an elected stop's removal and
  the end of its child order loses that stop (it is out of the store,
  its risk released, possibly `Triggered` without a terminal state); the
  rest of the book is consistent. The per-thread scratch buffers are
  taken out of their cell for the pass and simply not returned on an
  unwind.
- **Kill switch.** While it is engaged, elections are suspended (prints
  still trail): a stop the prints cross stays pending and is elected by
  the first print at or through its stop after the release. No market
  order is rejected by the kill switch, so no child-id state entry is
  recorded for it. The sequencer's commands are refused by the kill switch
  before they trade, so suspended elections only follow the raw `match_*`
  entry points.
- **Restore.** A snapshot's pending stops are validated in the prepare
  phase (kind, time-in-force, quantity, consistent terms, id uniqueness
  against the levels and each other) and their risk accumulated with
  checked arithmetic, so the commit installs them infallibly. A stop the
  snapshot's last trade price crosses is legal (suspended elections): it
  is elected by the next print. A trailing stop found on a level is
  refused with `StopOrdersUnsupported`.

## Listener emission and submit-gate poisoning (#249)

Before 0.14.0 the three listeners ran inline, often under the submit gate
and in several places before the mutation finished. A listener that
re-entered the book deadlocked (or hit the std `RwLock`'s unspecified
nested-read behaviour), and a listener panic under the exclusive side
poisoned the gate, which the book then recovered silently.

**Design** (`src/orderbook/emission.rs`, `SubmitGateGuard` in
`src/orderbook/book.rs`):

1. On a book with a listener installed, acquiring the submit gate opens a
   thread-local *emission scope* bound to that book. Every event the call
   produces (`TradeResult`, `PriceLevelChangedEvent`, order-state
   transition) is pushed into the scope's buffer instead of being
   delivered. Buffers are recycled per thread and through a small per-book
   lock-free pool (`crossbeam::queue::ArrayQueue`) when another thread
   drained them; nothing is buffered, and no scope is opened, on a book
   without listeners.
2. When the gate guard drops it commits the scope **while the gate is
   still held**: under the book's outbox mutex it stamps `engine_seq`
   (same checked mint and exhaustion suppression as before) and publishes
   the batch in the same critical section, so delivery order is
   `engine_seq` order; doing it under the gate makes it consistent with
   commit order. Uncontended (empty queue, no dispatcher) the committer
   claims the dispatcher role there and keeps its batch (*direct* path);
   otherwise the batch is queued, not ready, under a ticket from the
   committing thread's own counter.
3. The guard then releases the gate. A direct committer delivers its batch
   and drains what queued meanwhile. A queued batch is made ready with one
   atomic `fetch_max` on the thread's released-ticket counter (no lock;
   the thread's tickets come from its own counter shared by every book, so
   they grow across books and one book's release never readies another
   book's batch early), and the thread tries to become the book's single
   dispatcher (an atomic flag). The dispatcher takes the ready prefix of the queue under
   one lock and calls the listeners with no lock held; a thread that finds
   a dispatcher active returns and leaves its batch to it; a not-ready
   head stops the dispatcher and its owner dispatches it after releasing
   the gate. The flag, the released counters and the queue's non-empty flag
   are `SeqCst`, and a dispatcher that steps down re-checks the head, so no
   ready batch is stranded. A committing call takes the outbox lock once
   (plus a share of the dispatcher's drain locks: measured 1.23
   acquisitions per add at 8 threads). The lock is a test-and-test-and-set
   spin flag (exponential backoff, then yield) in front of a `std` mutex
   only the flag holder takes: a contended `std` mutex parks waiters in
   the kernel on macOS, which dominated the contended commit path.

A result-returning submit (`add_order_with_result`, `*_with_committed`)
needs its `TradeResult`'s `engine_seq` before it returns: at that point the
scope's pending events are committed early, followed by the trade, so the
caller's copy and the listener's copy carry the same sequence, minted in the
same position as before. That path clones the `TradeResult` once when a
trade listener is installed (the caller and the deferred listener each own
a copy); every other path moves it.

**Guarantee.** Per book, one total order consistent with commit order;
`engine_seq` strictly increases across the delivered trade + price-level
stream, also with concurrent submitters; a single thread observes exactly
the pre-#249 order (pinned by
`src/orderbook/tests/listener_emission.rs`, `single_thread_event_order_is_unchanged`).
Delivery happens on whichever thread is dispatching, so under concurrency
a submit can return before its events are delivered, and a listener can
observe a book newer than its event. Re-entrant calls from a listener
commit their own batch and return without dispatching; the active
dispatcher delivers it after the current batch.

**Panicking listener.** Not caught (`catch_unwind` is forbidden). The
unwind leaves the book consistent (the mutation committed before any
listener ran) and the gate unpoisoned (released before dispatch). A drop
guard releases the dispatcher role, puts the batches it had taken but not
started back at the head of the queue, counts the undelivered remainder of
the panicking batch in `dropped_listener_events` and the panic in
`listener_panics`, and logs at `ERROR`; the panic propagates out of the
book call that was dispatching. Queued batches are delivered, in order, by
the next dispatch on the book or by `flush_listener_events`. Operators
should call `flush_listener_events` when `listener_panics` increases, so
those batches do not wait for the next mutation on a quiet book.

**Restore after a panic.** A snapshot-package restore rewinds `engine_seq`
below any batch a panic left queued, so at its point of no return it
discards those batches (counted in `dropped_listener_events`, logged at
`WARN`): they describe the replaced book, and delivering them after the
restore would run the stream backwards (PR #289 review). Discarding, not
delivering, keeps caller code out of the restore; call
`flush_listener_events` first to deliver them.

**Backlog.** The outbox is unbounded by design: nothing is dropped or
rejected because listeners are slow. A stalled or slow listener on the
dispatching thread lets every other submitter's events accumulate, which
is the caller's contract to avoid (listeners must return quickly).
`OrderBook::pending_listener_events()` is the operational gauge (events
committed and not yet taken by the dispatcher); alert on growth. The outbox mutex
is never held across caller code; if it were ever poisoned it is recovered
and the poison cleared (every queue mutation is a single
`push_back` / `pop_front` / flag store).

**Engine unwind mid-mutation.** The scope is committed only on the
non-unwinding path: when the guard drops during an unwind its uncommitted
events are dropped and counted (batches it already committed early are
marked ready, since they describe committed trades) and nothing is
dispatched. The same drop engages the kill switch (below).

**Submit-gate poison.** With listeners out of the gate, an unwind through
a held gate means engine code, or caller code the engine runs
mid-mutation, panicked: a `Clock`, the metrics recorder, a `tracing`
subscriber, `T::default()` / `T::clone()`. The book may be inconsistent.
Sources are not limited to the exclusive side: every ordinary submit,
cancel and `UpdateQuantity` runs under the shared side, whose
`RwLockReadGuard` never poisons.

Detection (#294): `SubmitGateGuard`'s drop checks
`std::thread::panicking()` (one call per drop, compared with the value at
acquisition exactly as std's own poison flag does, so a gate taken by a
destructor during an unrelated unwind is not blamed). On an unwind, on
**either** side, it engages the kill switch and latches
`OrderBook::submit_gate_poisoned` (logged once at `ERROR`, with the gate
side) **before** the gate is released, so the next holder already sees
it. The guard's own commit phase (stamping and publishing the listener
batch, still under the gate) can run the `tracing` subscriber; a panic
there does not run the drop again, so a sentinel armed around that phase
and disarmed only after the gate is released applies the same policy
(PR #297 review). The exclusive side is also poisoned by std; the acquisition that
finds it poisoned applies the same policy (idempotent, no second log),
clears the poison and continues.

New flow and modifies then return `OrderBookError::KillSwitchActive`,
including the market-order paths that check the kill switch before taking
the gate; cancels and mass cancels keep working so the book can be
drained, and the kill switch is persisted in the snapshot package. An
operator's `release_kill_switch` resumes flow; the latch stays set. The
price-level stripe locks keep their recover-and-log policy (see the lock
inventory below).

**Cost of the ordering guarantee.** Stamping `engine_seq` and publishing
the batch must be one atomic step under the submit gate, so every commit
takes the outbox lock once (1.23 acquisitions per add at 8 threads,
dispatcher drains included). On a single thread, or with no listener
installed, that is free or absent; under many concurrent submitters with
a trivial listener it is visible. Measured against main c59d74f (3 to 5
interleaved Criterion rounds): `concurrent_add_limit_orders` with a
no-op trade + price-level listener is +3.8% / +6.7% / +3.7% at 2 / 8 / 16
threads (4 threads within noise), the one workload over the 5% budget.
That workload is the worst case by construction: all threads add at one
price for one account (already serialised on the level and the user
index), and a no-op listener makes delivery free, so the lock handoff is
the entire difference. Listener-free paths are unchanged within noise
(`add_limit_orders` +0.1%, `concurrent_add_limit_orders` -1.0% to
+0.7%, `concurrent_mixed_operations` -1.2% to +1.1%, HDR `add_only` and
`mixed_70_20_10` p50 0.0%, `aggressive_walk` p50 +2.4% over 10 rounds,
one 1 ns histogram bucket); with listeners, mixed and market-order workloads stay within
+5%. The alternatives measured or analysed are listed in the
`CHANGELOG.md` entry for #249; the maintainer accepted this cost for the
guarantee.

**Limits.** Emission scopes nest per book (PR #289 review): a gated call
on book B made by caller code running inside book A's mutation (a
`Clock`, `T::clone`) opens B's scope on top of A's, so B buffers its own
events and delivers them after B's gate is released, and A's scope is
restored intact. B's listeners then still run inside A's mutation on
that thread, so they must not drive A (A's gate may be held
exclusively). The released-ticket counter is per thread, so B's release
can make A's early-committed batches ready before A releases its gate;
they describe committed mutations and keep their order. The dispatcher role is
not bounded: under sustained load from other threads one thread can keep
delivering for longer than its own call needed.

## Core boundary gaps (#294)

Findings of the final audit (#260) the mechanical gate cannot see:

- **Shared-gate unwinds.** Covered by the submit-gate poison policy above.
- **Sweep drain.** Per filled maker, `track_state` (tracker `Clock`,
  `record_reject`), the strandable-maker `INFO` and
  `record_reserve_hidden_discarded` run before that maker's index release
  (strandable count, user index, location). The release cannot move ahead
  of the caller code: the location is the id's ownership token (#288), and
  releasing it before `Filled` is recorded would let a same-id order
  admitted meanwhile have its resting state overwritten by the old order's
  `Filled`. A drop guard (`FilledMakerRelease`, `src/orderbook/matching.rs`)
  releases each maker right after its caller code, and on an unwind
  releases every maker not released yet (crate-owned index work only; no
  allocation, no `catch_unwind`). Event order is unchanged.
- **Rest path.** `rest_on_level` publishes the risk reservation, the
  location, the user-index entry and the resting state before the level
  admits the order (#288), and between the claim and the admission runs
  caller code (`track_state`'s `Clock` and metrics, `T::default()` in the
  unit conversion). A drop guard (`UnrestedClaim`,
  `src/orderbook/modifications.rs`), disarmed as soon as the admission
  returns, withdraws on an unwind: first the resting state if it was
  recorded (`OrderStateTracker::withdraw_last_transition`: pops that
  transition and restores the previous status, or forgets an order whose
  only transition it was, without reading a clock or calling a listener;
  the deferred listener event is dropped with the unwinding emission
  scope), then in #288's release order the user-index entry and the
  reservation, then the location. Every rollback therefore happens while
  the attempt still owns the id, so a same-id order cannot claim it in
  between and have its own transition popped (PR #297 review). It then
  removes a level the attempt left empty. The guard is declared before
  the level-stripe guard, so the stripe is released before the drop takes
  its exclusive side, and the unit conversion (`T::default()`) runs before
  the stripe is taken.
- **Raw placement.** `OrderBook::place_order_in_book` was public and
  bypassed the gate, kill switch, risk, crossing, STP, the strandable rule
  and state tracking. It had no production caller and was removed (0.14 is
  a breaking release); `add_order` is the resting entry point.
- **Standalone tracker.** `OrderStateTracker::transition` queues a
  terminal id for eviction before invoking its listener, as the book's
  `record_transition` does, so a panicking listener no longer leaves the
  id unevictable.
- **Dispatcher progress.** When the dispatcher's delivery buffer cannot
  grow (`try_reserve` refused), it delivers the head batch in place
  instead of leaving it ready and re-dispatching forever. A refused
  outbox queue growth at commit keeps its drop-and-count policy
  (`dropped_listener_events`, `ERROR`).
- **IV `PriceSource::LastTrade`.** On a two-sided book that has not traded
  it returns `IVError::NoPriceAvailable` instead of silently using the mid.

## `std` lock inventory (#294)

Every `std::sync` lock the core engine holds, and its poison policy. The
submit gate is by design held across the mid-mutation caller code listed
in the table of caller-supplied surfaces. Under the other three the only
caller code that can run is the `tracing` subscriber, at the call sites
named in each row; `T::default()` (the unit conversion of a resting
order) runs before the level stripe is taken (PR #297 review). Every
stripe acquisition, and every outbox acquisition of a commit, is made
with the submit gate held (a commit from the gate guard's drop, covered
by its commit sentinel), so a panic there engages the kill switch through
the submit-gate policy. The dispatcher takes the outbox lock after the
gate is released; a panic there (only the poison-recovery log can raise
one) leaves the queue intact and releases the dispatcher role, like a
listener panic. Each row's own poison policy only has to keep the lock
usable.

| Lock | Where | Held across | Poison policy |
|---|---|---|---|
| Submit gate, `RwLock<()>` | `OrderBook::submit_gate` (`book.rs`) | One gated entry point (the mutation, including mid-mutation caller code: `Clock`, metrics, `tracing`, `T::default()` / `T::clone()`); never a listener | An unwind on either side engages the kill switch and latches `submit_gate_poisoned` in the guard's drop (#294); a poisoned exclusive side is cleared by the next acquisition, which applies the same policy (#249) |
| 64 level stripes, `[RwLock<()>; 64]` | `OrderBook::level_locks` (`book.rs`, #247) | One level admission (shared: a join on an existing level, `get` and pricelevel's `PriceLevel::add_order`; exclusive: level creation, `get_or_insert_with`, the #309 generation bump and the first `add_order`) or one emptied-level removal (exclusive: re-read, unlink and the generation bump); never another lock. Caller code under it: the `tracing` subscriber only, from pricelevel's failure-path events in `add_order` and from this crate's poison-recovery `ERROR` (logged while the recovered guard is held) | Data is `()`. A panic under the shared side leaves no poison; under the exclusive side it poisons the stripe, and every later acquisition recovers the guard and logs at `ERROR` (the poison is not cleared). The gate policy covers the mutation the unwind interrupted, and the rest path's claim guard removes a level it created empty |
| Listener outbox, `Mutex<OutboxState>` | `EventOutbox::state` (`emission.rs`, #249), behind a spin flag released by a drop guard | Stamping and publishing one batch (`engine_seq` minting, a single queue `push_back`), or taking the ready prefix (`pop_front`s); flag stores. Caller code under it: the `tracing` subscriber only, from the one-time `engine_seq` exhaustion `ERROR`, the refused-allocation `ERROR`s in `enqueue` / `commit_with_trade_seq`, and the poison-recovery `ERROR` | Recovered and cleared. Every log call sits before or after a single-step queue mutation, never inside one, so the queue is intact at every unwind point; a panic while stamping loses the batch being committed (minted `engine_seq` values it held are never delivered, a gap consumers see), and the gate guard's commit sentinel engages the kill switch |
| Terminal eviction queue, `Mutex<VecDeque<Id>>` | `OrderStateTracker::terminal_queue` (`order_state.rs`) | A push and the over-capacity pops; no map lock. Caller code under it: the `tracing` subscriber only, from the poison-recovery `WARN` | Recovered (logged at `WARN`) and cleared: the queue is an eviction hint re-checked per id (#250) |

The sequencer / journal and NATS locks are outside the core engine and
are documented with their subsystems (`Journal<T>` row above,
`JournalError::MutexPoisoned`).

## Enforcement

`make lint` (and CI's lint job) runs two layers, both absolute:

1. `cargo clippy --all-targets --all-features -- -D warnings` with the
   `[lints.clippy]` restriction lints in `Cargo.toml` (`unwrap_used`,
   `expect_used`, `panic`, `unreachable`, `todo`, `unimplemented`,
   `indexing_slicing`, `string_slice`, `arithmetic_side_effects`, the
   narrowing casts, `manual_assert`, `panic_in_result_fn`, `get_unwrap`,
   `exit`) and `clippy.toml`'s "in tests" toggles.
2. `scripts/check_panic_policy.py` (`make lint-panic`, also part of
   `make pre-push`): first its own fixture self-test
   (`scripts/panic_policy_fixtures/`), then a scan of `src/` for what clippy
   cannot see: the `assert!` / `debug_assert!` families, `catch_unwind` /
   `panic_any` / `resume_unwind`, `saturating_*` / `wrapping_*`, the
   panicking forms inside production-adjacent `#[cfg(test)]` seams, and any
   production `#[allow]` / `#[expect]` (inner, outer or inside `cfg_attr`)
   of a lint `[lints.clippy]` denies, or of a clippy group containing one
   (`clippy::restriction`, `clippy::pedantic`, and `clippy::all` as a
   conservative catch-all; the script records each denied lint's groups
   and refuses a denied lint it has no groups for).

There is no allowlist: any finding fails. The only exception form is an
inline `// panic-policy-allow-saturating: <reason>` marker on a reviewed,
compile-time-only `saturating_*` / `wrapping_*` expression (none is in use
today). Test code (`#[cfg(test)] mod tests` blocks, `src/**/tests/`) and
the test / bench crate roots may relax the lints, as listed in the
`Cargo.toml` comment above `[lints.clippy]`.

The temporary ratchet used during the 0.14 cycle (per-file
`#![allow(clippy::...)] // panic-policy-ratchet` markers,
`scripts/panic_policy_allowlist.txt`, `scripts/clippy_ratchet.txt` and
`scripts/check_clippy_ratchet.py`) was removed in #260 once every ledger
was empty.
