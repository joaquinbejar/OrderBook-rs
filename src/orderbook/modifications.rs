use crate::orderbook::book::{OrderBook, OrderLocation, SubmitGateGuard};
use crate::orderbook::error::OrderBookError;
use crate::orderbook::matching::MatchOutcome;
use crate::orderbook::matching::{FeasibilityScope, ShapeVerdict, SweepReservation};
use crate::orderbook::order_state::{CancelReason, OrderStatus};
use crate::orderbook::reject_reason::RejectReason;
use crate::orderbook::trade::{SubmitFailure, TradeResult};
use either::Either;
use pricelevel::{
    CapacityResource, DEFAULT_RESERVE_REPLENISH_AMOUNT, Id, OrderType, OrderUpdate, PriceLevel,
    PriceLevelError, Quantity, Side, TakerKind,
};
use std::sync::Arc;
use tracing::trace;

/// A trait to abstract quantity access and modification for different order types.
pub trait OrderQuantity<T = ()> {
    /// Returns the primary quantity used for display or simple matching.
    /// For iceberg orders, this is the visible quantity.
    fn quantity(&self) -> u64;

    /// Returns the total quantity of the order (visible + hidden for the
    /// two-tranche kinds, the single quantity otherwise), in quantity units.
    ///
    /// # Errors
    ///
    /// [`OrderBookError::QuantityOverflow`] when `visible + hidden`
    /// overflows `u64` for an iceberg or reserve order. Every order admitted
    /// through `add_order`, the submit APIs or the validate-first modify
    /// path has passed this check (#210), so it cannot fail for a
    /// book-resident order. Before 0.14.0 this method saturated at
    /// `u64::MAX` instead (#247).
    fn total_quantity(&self) -> Result<u64, OrderBookError>;

    /// Returns the total quantity, or `None` when `visible + hidden`
    /// overflows `u64` for a two-tranche order (Iceberg / Reserve). The
    /// direct add path rejects such orders before the risk gate, and every
    /// admission path rejects them before any match, listener, or map
    /// mutation (#210).
    #[must_use = "a None total means the order is unrepresentable and must be rejected"]
    fn checked_total_quantity(&self) -> Option<u64>;

    /// Sets the new quantity for an order, handling the logic for different types.
    ///
    /// This is the **user-facing quantity update** semantic: for the
    /// two-tranche kinds (iceberg and reserve) `new_quantity` applies to
    /// the **visible** tranche, matching [`Self::quantity`] (which returns
    /// the visible quantity) and the upstream
    /// [`OrderUpdate::UpdateQuantity`] / [`OrderType::with_reduced_quantity`]
    /// contract. The hidden tranche is left untouched, so the new total is
    /// `new_quantity + hidden` and an increase is honoured. Before #221 a
    /// reserve order read the argument as a **total** target and only ever
    /// reduced: a requested increase was silently dropped and a decrease
    /// was drawn across both tranches.
    ///
    /// To adjust an aggressive taker's **total** remainder before resting,
    /// use [`Self::set_total_remaining`] instead; applying a total to the
    /// visible tranche manufactures liquidity (#210).
    fn set_quantity(&mut self, new_quantity: u64);

    /// Distributes a **total** remaining quantity across the order's
    /// tranches before resting an aggressive taker's residual (#210).
    ///
    /// - One-tranche kinds: the quantity becomes `remaining_total`.
    /// - Iceberg: the submitted visible quantity acts as the display
    ///   size — `visible = min(display, remaining_total)`,
    ///   `hidden = remaining_total − visible`. A fill smaller than the
    ///   visible tranche shrinks only the display; a fill past it
    ///   consumes hidden; conservation always holds:
    ///   `visible + hidden == remaining_total`.
    /// - Reserve: the reduction is drawn from the visible tranche first
    ///   and then from hidden, after which the visible tranche is
    ///   refreshed out of hidden under `pricelevel`'s replenishment rule
    ///   (#230). The refresh happens **only with automatic replenishment
    ///   on**, and only while the post-reduction visible tranche is below
    ///   `max(replenish_threshold, 1)` — so an emptied tranche always
    ///   qualifies, and a partial fill that leaves the tranche under an
    ///   explicit threshold qualifies too. It adds the explicit
    ///   `replenish_amount`, or `pricelevel`'s
    ///   [`DEFAULT_RESERVE_REPLENISH_AMOUNT`] when there is none, capped by
    ///   the hidden tranche.
    ///
    ///   With `auto_replenish` off nothing is drawn from hidden. That only
    ///   ends the order when the fill **exhausted** the visible tranche: the
    ///   tranche is left empty and `add_order_inner` discards the residual,
    ///   exactly as `pricelevel` removes a depleted non-auto maker from its
    ///   level. A fill that leaves any visible quantity rests normally — a
    ///   10 visible / 20 hidden reserve filled for 5 rests 5 / 20.
    ///
    ///   The explicit `replenish_amount` is the **transfer**, not a target
    ///   display size: it is added to whatever visible quantity survived.
    ///   With amount 10, threshold 5 and a remainder of 2 visible, the
    ///   residual rests 12 visible. Without an explicit amount the transfer
    ///   is [`DEFAULT_RESERVE_REPLENISH_AMOUNT`] capped by hidden, so a
    ///   10 / 20 reserve filled for 10 refreshes with
    ///   `min(DEFAULT_RESERVE_REPLENISH_AMOUNT, 20) == 20` and rests 20
    ///   visible / 0 hidden — more than it first displayed.
    ///
    ///   This total-target policy belongs to
    ///   this method only; since #221 [`Self::set_quantity`] sets the
    ///   reserve's visible tranche like every other user-facing quantity
    ///   update.
    fn set_total_remaining(&mut self, remaining_total: u64);
}

impl<T> OrderQuantity<T> for OrderType<T> {
    #[inline]
    fn quantity(&self) -> u64 {
        match self {
            OrderType::Standard { quantity, .. } => quantity.as_u64(),
            OrderType::IcebergOrder {
                visible_quantity, ..
            } => visible_quantity.as_u64(),
            OrderType::PostOnly { quantity, .. } => quantity.as_u64(),
            OrderType::TrailingStop { quantity, .. } => quantity.as_u64(),
            OrderType::PeggedOrder { quantity, .. } => quantity.as_u64(),
            OrderType::MarketToLimit { quantity, .. } => quantity.as_u64(),
            OrderType::ReserveOrder {
                visible_quantity, ..
            } => visible_quantity.as_u64(),
        }
    }

    #[inline]
    fn total_quantity(&self) -> Result<u64, OrderBookError> {
        match self {
            OrderType::IcebergOrder {
                visible_quantity,
                hidden_quantity,
                ..
            }
            | OrderType::ReserveOrder {
                visible_quantity,
                hidden_quantity,
                ..
            } => visible_quantity
                .as_u64()
                .checked_add(hidden_quantity.as_u64())
                .ok_or_else(|| quantity_overflow(*visible_quantity, *hidden_quantity)),
            _ => Ok(self.quantity()),
        }
    }

    #[inline]
    fn checked_total_quantity(&self) -> Option<u64> {
        match self {
            OrderType::Standard { quantity, .. }
            | OrderType::PostOnly { quantity, .. }
            | OrderType::TrailingStop { quantity, .. }
            | OrderType::PeggedOrder { quantity, .. }
            | OrderType::MarketToLimit { quantity, .. } => Some(quantity.as_u64()),
            OrderType::IcebergOrder {
                visible_quantity,
                hidden_quantity,
                ..
            }
            | OrderType::ReserveOrder {
                visible_quantity,
                hidden_quantity,
                ..
            } => visible_quantity
                .as_u64()
                .checked_add(hidden_quantity.as_u64()),
        }
    }

    #[inline]
    fn set_quantity(&mut self, new_quantity: u64) {
        match self {
            OrderType::Standard { quantity, .. }
            | OrderType::PostOnly { quantity, .. }
            | OrderType::TrailingStop { quantity, .. }
            | OrderType::PeggedOrder { quantity, .. }
            | OrderType::MarketToLimit { quantity, .. } => *quantity = Quantity::new(new_quantity),

            OrderType::IcebergOrder {
                visible_quantity, ..
            }
            | OrderType::ReserveOrder {
                visible_quantity, ..
            } => {
                // Two-tranche kinds take `new_quantity` as the new visible
                // tranche, matching what `quantity()` reports and the
                // upstream `UpdateQuantity` contract (#221). The hidden
                // tranche is untouched, so the new total is
                // `new_quantity + hidden`.
                *visible_quantity = Quantity::new(new_quantity);
            }
        }
    }

    #[inline]
    fn set_total_remaining(&mut self, remaining_total: u64) {
        match self {
            OrderType::Standard { quantity, .. }
            | OrderType::PostOnly { quantity, .. }
            | OrderType::TrailingStop { quantity, .. }
            | OrderType::PeggedOrder { quantity, .. }
            | OrderType::MarketToLimit { quantity, .. } => {
                *quantity = Quantity::new(remaining_total)
            }

            OrderType::IcebergOrder {
                visible_quantity,
                hidden_quantity,
                ..
            } => {
                // The submitted visible quantity is the display size. The
                // residual rests with at most one display tranche visible
                // and the rest hidden — conservation by construction:
                // visible + hidden == remaining_total.
                // Exact split without arithmetic that could wrap: a
                // remainder of at least one display tranche shows the
                // display and hides the rest; a smaller one is all visible.
                let display = visible_quantity.as_u64();
                let (visible, hidden) = match remaining_total.checked_sub(display) {
                    Some(rest) => (display, rest),
                    None => (remaining_total, 0),
                };
                *visible_quantity = Quantity::new(visible);
                *hidden_quantity = Quantity::new(hidden);
            }
            OrderType::ReserveOrder { .. } => reduce_reserve_to_total(self, remaining_total),
        }
    }
}

/// Reserve-order reduction to a **total** target: draw the reduction from
/// the visible tranche first, then hidden, then refresh the visible tranche
/// from hidden under `pricelevel`'s replenishment rule.
/// Used only by `set_total_remaining` for the residual resting path (#210);
/// the user-facing `set_quantity` sets the visible tranche instead (#221).
///
/// The refresh mirrors `pricelevel`'s `match_against` for a resting maker
/// (#230). With `auto_replenish` on and hidden left, it triggers whenever
/// the post-reduction visible tranche falls **below the replenish
/// threshold** — `safe_threshold = max(replenish_threshold, 1)`, so an
/// emptied tranche always qualifies — and adds
/// `min(replenish_amount.unwrap_or(`[`DEFAULT_RESERVE_REPLENISH_AMOUNT`]`), hidden)`
/// to whatever visible quantity survived, drawing it out of hidden.
/// Upstream splits this into a depletion arm and a below-threshold arm;
/// both reduce to the single rule applied here.
///
/// With `auto_replenish` off nothing is transferred and a depleted visible
/// tranche is left empty — the same fate `pricelevel` gives a depleted
/// resting maker, which it removes from the level. `add_order_inner` reads
/// that empty tranche as "this residual must not rest" and ends the order
/// instead.
fn reduce_reserve_to_total<T>(order: &mut OrderType<T>, new_total_quantity: u64) {
    if let OrderType::ReserveOrder {
        visible_quantity,
        hidden_quantity,
        replenish_threshold,
        replenish_amount,
        auto_replenish,
        ..
    } = order
    {
        // Draw `total - new_total` from the visible tranche first, then
        // from hidden, as an exact case analysis (#247), with no arithmetic
        // that could wrap or saturate:
        //
        // - `new_total >= total`: nothing to reduce (a larger target is
        //   never applied here, exactly as before);
        // - `new_total >= hidden`: the reduction fits in the visible
        //   tranche, which keeps `new_total - hidden`; hidden is untouched;
        // - otherwise the visible tranche is emptied and hidden keeps
        //   `new_total`.
        //
        // An unrepresentable `visible + hidden` is larger than any `u64`
        // target, so it always reduces.
        //
        // Hidden may only ever DECREASE here, and only by a lot-aligned
        // amount (the executed remainder is lot-rounded by the sweep): the
        // #226 lot-size admission check validates the replenishment transfer
        // once, against the hidden tranche as submitted, and
        // `min(amount, hidden)` stays lot-aligned only while hidden stays
        // lot-aligned and never grows.
        let vis = visible_quantity.as_u64();
        let hid = hidden_quantity.as_u64();
        let reduces = vis
            .checked_add(hid)
            .is_none_or(|total| new_total_quantity < total);
        if reduces {
            match new_total_quantity.checked_sub(hid) {
                Some(visible_left) => *visible_quantity = Quantity::new(visible_left),
                None => {
                    *visible_quantity = Quantity::new(0);
                    *hidden_quantity = Quantity::new(new_total_quantity);
                }
            }
        }

        // #230: `auto_replenish` governs this refresh exactly as it governs
        // a resting maker's in `pricelevel`'s `match_against`, including the
        // below-threshold arm: upstream refreshes both when the visible
        // tranche is fully consumed and when a partial consume leaves it
        // under `safe_threshold`, with the same transfer in each case. A
        // zero threshold is read as 1 upstream, so the depletion arm is
        // just the threshold arm at its smallest. With the flag off the
        // whole branch is skipped: a depleted visible tranche stays empty
        // and `add_order_inner` ends the order, mirroring pricelevel's
        // removal of a depleted non-auto maker. This is also the transfer
        // the #226 lot rule validates at admission, under exactly this
        // condition.
        let safe_threshold = if replenish_threshold.as_u64() == 0 {
            1
        } else {
            replenish_threshold.as_u64()
        };
        if *auto_replenish
            && hidden_quantity.as_u64() > 0
            && visible_quantity.as_u64() < safe_threshold
        {
            let refresh = replenish_amount
                .map(|q| q.get())
                .unwrap_or(DEFAULT_RESERVE_REPLENISH_AMOUNT.get())
                .min(hidden_quantity.as_u64());
            // `refresh <= hidden`, and after the reduction above
            // `visible + hidden <= new_total_quantity`, a `u64`: both checked
            // forms below always succeed (#247). Both operands are
            // lot-aligned, so the refreshed tranche is too. Hidden only
            // decreases, by the validated lot-aligned transfer.
            if let (Some(visible), Some(hidden)) = (
                visible_quantity.as_u64().checked_add(refresh),
                hidden_quantity.as_u64().checked_sub(refresh),
            ) {
                *visible_quantity = Quantity::new(visible);
                *hidden_quantity = Quantity::new(hidden);
            } else {
                tracing::error!(
                    visible = visible_quantity.as_u64(),
                    hidden = hidden_quantity.as_u64(),
                    refresh,
                    "reserve refresh out of range; tranches left unrefreshed"
                );
            }
        }
    }
}

/// Accept `quantity` only when it is a whole multiple of the book's `lot`
/// size, in quantity units.
///
/// Every lot-size branch of `validate_order_shape` funnels through here so
/// the rejection carries the offending quantity — the tranche or the
/// replenishment transfer that failed — rather than the order total (#226).
///
/// # Errors
/// [`OrderBookError::InvalidLotSize`] carrying `quantity` and `lot`.
#[inline]
#[must_use = "lot-size validation errors must be handled"]
fn check_lot_multiple(quantity: u64, lot: u64) -> Result<(), OrderBookError> {
    if quantity.is_multiple_of(lot) {
        Ok(())
    } else {
        Err(invalid_lot_size(quantity, lot))
    }
}

/// Build the [`OrderBookError::InvalidLotSize`] rejection out of line.
#[cold]
#[inline(never)]
#[must_use]
fn invalid_lot_size(quantity: u64, lot_size: u64) -> OrderBookError {
    OrderBookError::InvalidLotSize { quantity, lot_size }
}

/// Build the [`OrderBookError::ReserveResidualWouldBeDiscarded`] rejection
/// out of line (#230).
#[cold]
#[inline(never)]
#[must_use]
fn reserve_residual_would_be_discarded(
    order_id: Id,
    visible_quantity: u64,
    crossable_quantity: u64,
    hidden_quantity: u64,
    discarded_quantity: u64,
) -> OrderBookError {
    OrderBookError::ReserveResidualWouldBeDiscarded {
        order_id,
        visible_quantity,
        crossable_quantity,
        hidden_quantity,
        // The residual the re-add would leave unmatched and then abandon,
        // `visible + hidden - crossable`, computed checked by the caller.
        discarded_quantity,
    }
}

/// Build the [`OrderBookError::QuantityOverflow`] rejection of a two-tranche
/// order whose `visible + hidden` does not fit `u64` (#210 / #247).
#[cold]
#[inline(never)]
#[must_use]
fn quantity_overflow(visible: Quantity, hidden: Quantity) -> OrderBookError {
    OrderBookError::QuantityOverflow {
        visible: visible.as_u64(),
        hidden: hidden.as_u64(),
    }
}

/// A resting order can never carry fill-or-kill, so a cancel-then-add
/// modify never re-adds one: the modify's submit gate mode was chosen once
/// at the boundary and a fill-or-kill window needs the exclusive gate. Kept
/// as a typed pre-cancel rejection rather than an assertion (#247), so a
/// future time-in-force change cannot silently void the #209 guarantee.
#[cold]
#[inline(never)]
#[must_use]
fn fill_or_kill_readd(order_id: Id) -> OrderBookError {
    OrderBookError::InvalidOperation {
        message: format!(
            "order {order_id} cannot be re-added as fill-or-kill by a cancel-then-add modify"
        ),
    }
}

/// The refusal a replay applies to the residual of a submit the journal
/// recorded as [`OrderBookError::RiskRejectedAfterTrades`] (#291). The live
/// refusal came from the risk layer, whose configuration a replay book does
/// not carry, so replay states the recorded outcome instead of re-deriving
/// it. Only the code is reconciled, never this message.
#[cold]
#[inline(never)]
#[must_use]
fn replay_refused_residual(order_id: Id) -> OrderBookError {
    OrderBookError::InvalidOperation {
        message: format!(
            "replay refused the residual of order {order_id}: the journal recorded a post-trade risk rejection"
        ),
    }
}

/// `PriceLevel::matchable_quantity` answered more than it was asked for.
/// Its contract bounds the answer by the request, so this is an upstream
/// invariant breach, reported before any mutation (#247).
#[cold]
#[inline(never)]
#[must_use]
fn matchable_exceeds_request(requested: u64, matchable: u64) -> OrderBookError {
    OrderBookError::InvalidOperation {
        message: format!(
            "price level reported {matchable} matchable units for a request of {requested}"
        ),
    }
}

/// Lot rounding of `quantity` by `lot` could not be computed (#247).
#[cold]
#[inline(never)]
#[must_use]
fn lot_rounding_failed(quantity: u64, lot: u64) -> OrderBookError {
    OrderBookError::InvalidOperation {
        message: format!("cannot round quantity {quantity} down to lot size {lot}"),
    }
}

/// Rounds `quantity` down to a whole multiple of `lot`, in quantity units,
/// exactly like the sweep's per-level cap; `lot <= 1` leaves it unchanged.
///
/// # Errors
/// Never for `lot > 1` (`quantity / lot * lot <= quantity`); the checked
/// forms report a breach as [`OrderBookError::InvalidOperation`] instead of
/// assuming it away (#247).
#[inline]
fn lot_floor(quantity: u64, lot: u64) -> Result<u64, OrderBookError> {
    if lot <= 1 {
        return Ok(quantity);
    }
    quantity
        .checked_div(lot)
        .and_then(|lots| lots.checked_mul(lot))
        .ok_or_else(|| lot_rounding_failed(quantity, lot))
}

/// `remaining - matchable` for a dry-run step that asked for `requested`
/// (`requested <= remaining`) and was answered `matchable`.
///
/// # Errors
/// [`OrderBookError::InvalidOperation`] when the level answered more than
/// it was asked for, an upstream contract breach (#247). Raised by the
/// validate-first checks only, before the original order is touched.
#[inline]
fn consume_matchable(
    remaining: u64,
    requested: u64,
    matchable: u64,
) -> Result<u64, OrderBookError> {
    if matchable > requested {
        return Err(matchable_exceeds_request(requested, matchable));
    }
    remaining
        .checked_sub(matchable)
        .ok_or_else(|| matchable_exceeds_request(remaining, matchable))
}

/// The untouched rejection of a crossing taker when the book's trade-id
/// generator is exhausted (#240): `CapacityExceeded { IdSequence }`, wire
/// code `RejectReason::CapacityExceeded` (16).
#[cold]
#[inline(never)]
pub(crate) fn trade_ids_exhausted_error() -> OrderBookError {
    OrderBookError::PriceLevelError(PriceLevelError::CapacityExceeded {
        resource: CapacityResource::IdSequence,
        additional: 1,
    })
}

/// Withdraws what [`OrderBook::rest_on_level`] published before its
/// admission if the rest path **unwinds** before the level admits the
/// order (#294).
///
/// Between the location claim and the admission the rest path runs caller
/// code: `track_state` (the tracker's `Clock`, the metrics recorder),
/// `T::default()` in the unit conversion. A panic there used to leave the
/// claimed location, the user-index entry, the risk reservation, the
/// recorded resting state and possibly a freshly created empty level
/// behind, for an order no level holds. Declared before the level stripe
/// guard, so the stripe is released first and the drop can take the
/// stripe's exclusive side to remove an emptied level. Disarmed by
/// [`Self::disarm`] once the admission returned, successfully or not (the
/// refusal path then withdraws explicitly, as before).
///
/// The drop withdraws the recorded state first (without reading a clock),
/// then follows #288's release order (user index and reservation, then
/// the location that owns the id), so every rollback happens while this
/// admission still owns the id; it then removes a level the order left
/// empty. It
/// runs crate-owned code only (no allocation, no metrics; the stripe's
/// poison `ERROR` log is the one `tracing` call), and the unwind itself
/// engages the kill switch through the submit-gate guard.
struct UnrestedClaim<'a, T: Clone + Send + Sync + Default + 'static> {
    /// The book resting the order.
    book: &'a OrderBook<T>,
    /// The order being rested.
    order: &'a OrderType<T>,
    /// Its price, in price ticks.
    price: u128,
    /// Its side.
    side: Side,
    /// The sequence it is tracked under in its owner's `user_orders` entry.
    user_seq: u64,
    /// The #243 reservation; `None` once disarmed.
    reservation: Option<crate::orderbook::risk::RiskReservation>,
    /// The resting state, once `track_state` recorded it.
    state: Option<OrderStatus>,
}

impl<T: Clone + Send + Sync + Default + 'static> UnrestedClaim<'_, T> {
    /// The admission returned: nothing to withdraw on unwind any more.
    /// Hands the reservation back to a refusal path that withdraws it.
    #[inline]
    fn disarm(&mut self) -> Option<crate::orderbook::risk::RiskReservation> {
        self.state = None;
        self.reservation.take()
    }
}

impl<T: Clone + Send + Sync + Default + 'static> Drop for UnrestedClaim<'_, T> {
    fn drop(&mut self) {
        let Some(reservation) = self.reservation.take() else {
            return;
        };
        // PR #297 review: the recorded state is withdrawn while this
        // admission still owns the id (its location), so a same-id order
        // cannot claim the id and record a transition this would pop.
        if let Some(state) = self.state.take() {
            self.book.withdraw_tracked_state(self.order.id(), &state);
        }
        self.book.withdraw_unrested(
            self.order,
            self.price,
            self.side,
            self.user_seq,
            reservation,
        );
        self.book.remove_level_if_empty(self.side, self.price);
        self.book.cache.invalidate();
    }
}

impl<T> OrderBook<T>
where
    T: Clone + Send + Sync + Default + 'static,
{
    /// Update an order's price and/or quantity
    ///
    /// # Queue priority
    ///
    /// The update variants follow conventional exchange price-time-priority
    /// rules. This is a public contract — external conformance tooling
    /// depends on it (see issue #203):
    ///
    /// - [`OrderUpdate::UpdateQuantity`] with a **decreased or unchanged**
    ///   total quantity (visible + hidden) updates the resting order in
    ///   place at its existing insertion sequence: the maker keeps its
    ///   queue position. Reducing size never forfeits time priority.
    /// - [`OrderUpdate::UpdateQuantity`] with an **increased** total
    ///   quantity demotes the order to the back of its price level's
    ///   queue. Sizing up loses time priority. The demoted order keeps
    ///   its original admission timestamp — only its insertion sequence
    ///   is refreshed. The demotion survives a snapshot round-trip:
    ///   since pricelevel 0.9 level snapshots materialize orders in
    ///   queue-consumption order, so
    ///   [`restore_from_snapshot`](OrderBook::restore_from_snapshot)
    ///   rebuilds the exact queue (#205). Snapshots captured with
    ///   pricelevel < 0.9 restore a demoted order at its old
    ///   `(timestamp, seq)` position — re-snapshot to pin the corrected
    ///   order.
    /// - [`OrderUpdate::UpdateQuantity`] with a **zero** `new_quantity`
    ///   cancels the entire order, including the hidden quantity of an
    ///   iceberg or reserve order. It is removed from the book, tracked as
    ///   `Cancelled { UserRequested }`, and its id becomes reusable. This
    ///   applies even when hidden liquidity remains: for a two-tranche
    ///   order `new_quantity` normally resizes only the visible tranche,
    ///   but zero is a removal, not a resize, so it is never applied to a
    ///   tranche. A zero-quantity maker can never fill, so resting one
    ///   only published a price level with no depth. Queue priority does
    ///   not arise — there is no order left to hold a position — and the
    ///   returned `Arc` is the order **as it rested**, not a projection
    ///   resized to zero, unlike every nonzero `UpdateQuantity`, which
    ///   returns the updated order.
    /// - [`OrderUpdate::UpdatePrice`], [`OrderUpdate::UpdatePriceAndQuantity`],
    ///   and [`OrderUpdate::Replace`] are implemented as cancel-then-add:
    ///   the order always re-enters at the back of its (possibly new)
    ///   price level and loses time priority — for `Replace` and
    ///   `UpdatePriceAndQuantity` even when the price is unchanged.
    ///   For iceberg / reserve orders `UpdatePriceAndQuantity::new_quantity`
    ///   and `Replace::quantity` set the **visible** tranche and leave hidden
    ///   untouched (as `UpdateQuantity` does); shape validation and risk
    ///   admission see the resulting `visible + hidden` total.
    ///
    /// # Errors
    /// Returns [`OrderBookError::KillSwitchActive`] when the kill switch
    /// is engaged and the update is anything other than
    /// [`OrderUpdate::Cancel`]. Cancels are explicitly allowed so that
    /// operators can drain resting orders while new flow is halted.
    ///
    /// A **nonzero** [`OrderUpdate::UpdateQuantity`] is validate-first
    /// (#211): the projected post-update order must pass the shared shape
    /// validator (tick / lot / min-max / two-tranche representability)
    /// and the modify-aware risk check, and any upstream
    /// [`PriceLevelError`] from applying the
    /// update is propagated as [`OrderBookError::PriceLevelError`] — a
    /// rejected update leaves the maker unchanged, and `Ok(None)` means
    /// only that the requested order is absent.
    ///
    /// Because the shared validator runs on the projected order, two
    /// previously-accepted shapes are now rejected on a nonzero
    /// `UpdateQuantity` like they already were on the #98 modify paths:
    /// an expired-but-unevicted GTD / DAY maker (`InvalidOperation`,
    /// expiry is evaluated against the book clock) and a resting
    /// post-only maker whose price meanwhile crosses the market
    /// (`PriceCrossing`).
    ///
    /// A **zero** `UpdateQuantity` is a removal and runs none of that:
    /// it bypasses the projected shape validator and the modify-aware
    /// risk check entirely, so neither a configured `min_order_size` nor
    /// a risk limit vetoes it, and it runs the same cancel
    /// [`OrderBook::cancel_order`] performs (`cancel_order_with_reason`
    /// with `UserRequested`). Only the kill-switch check above still
    /// applies to it, because it is submitted as a modify. This removal
    /// semantic belongs to `UpdateQuantity` alone: a zero quantity on
    /// [`OrderUpdate::Replace`] or [`OrderUpdate::UpdatePriceAndQuantity`]
    /// re-adds the order through validate-first, and what that produces
    /// depends on the kind. For an iceberg or an auto-replenishing reserve
    /// it sets the visible tranche to zero, leaves the hidden depth live
    /// and the order keeps resting and executing; a reserve with
    /// `auto_replenish` off is rejected with
    /// [`OrderBookError::ZeroVisibleTranche`] and keeps resting (#230); and
    /// a single-tranche maker is re-added carrying nothing, so the sweep
    /// returns `remaining_quantity == 0`, the residual never rests and the
    /// order ends as a terminal `Filled { filled_quantity: 0 }` — it
    /// disappears with a fill status and no fill. None of the three is a
    /// cancel, and none of them is the way to remove an order.
    ///
    /// The three cancel-then-add variants additionally run two pre-checks
    /// on the projected order, both **before** the original is cancelled so
    /// that a rejection leaves it resting untouched:
    ///
    /// - [`OrderBookError::SelfTradePrevented`] when the re-add would cross
    ///   into the same user's opposite-side liquidity under
    ///   [`CancelTaker`](crate::orderbook::stp::STPMode::CancelTaker) /
    ///   [`CancelBoth`](crate::orderbook::stp::STPMode::CancelBoth), which
    ///   would cancel the re-added order (#168).
    /// - [`OrderBookError::ReserveResidualWouldBeDiscarded`] when the
    ///   projected order is a `ReserveOrder` with `auto_replenish == false`
    ///   and a non-empty hidden tranche, and the depth it would cross is at
    ///   least its visible tranche but less than its total: the re-add's
    ///   residual would not rest and its hidden remainder would be
    ///   discarded, destroying the order (#230). Crossing into depth
    ///   smaller than the visible tranche is allowed (the residual rests
    ///   with a positive visible tranche), and so is a projected full fill
    ///   (it discards nothing).
    ///
    /// # What the gate covers
    ///
    /// The gate mode is chosen **before** anything is read, from
    /// `strandable_makers_resting` and
    /// the STP mode — never from a lookup of the order being modified,
    /// which could go stale between the lookup and the acquisition. The
    /// guard is then held across the *whole* operation: the order lookup,
    /// the shared shape validator, both pre-checks, the cancel and the
    /// re-add. Nothing is decided from state read outside it.
    ///
    /// On a book holding strandable makers, or with STP engaged, that mode
    /// is exclusive, so for the case the second pre-check exists to protect
    /// — re-pricing a non-replenishing reserve that carries hidden quantity
    /// — the crossable-depth dry run is **exact**: no concurrent mutation
    /// can move the opposite side between the estimate and the re-add's
    /// sweep, so such a re-price cannot destroy the order it modifies.
    /// On a book holding none, a re-price of some *other* order runs
    /// shared, where the #168 self-cross dry run keeps its existing
    /// best-effort character.
    ///
    /// # Failed re-adds (#240, #247)
    ///
    /// A re-price that crosses while the book's trade-id generator is
    /// exhausted is refused in the validate-first phase, **before** the
    /// original is cancelled: the original keeps resting and the call
    /// returns `PriceLevelError(CapacityExceeded)` (reject code 16).
    ///
    /// The re-add takes the validate-first verdict as its admission: it
    /// does not re-run the kill-switch, risk-limit or shape checks, so a
    /// kill switch engaged, a clock tick or a risk limit consumed between
    /// the checks and the re-add cannot fail it. What can still fail after
    /// the original was cancelled is a concurrent mutation under the shared
    /// gate (the id taken by another submit, a post-only now crossing) or a
    /// resource the book cannot observe beforehand (a price level that
    /// fails, a refused allocation). The outcome depends on whether the
    /// re-added order traded first:
    ///
    /// - **Nothing traded**: the original is restored and the call returns
    ///   [`OrderBookError::ModifyRolledBack`] carrying the re-add's error.
    ///   It rests again with the same id, price, quantity (as it was when
    ///   cancelled) and timestamp, and its order state is restored, but at
    ///   the **back** of its price level's queue: pricelevel assigns a new
    ///   insertion sequence and offers no way to reinstate the old one, so
    ///   time priority is lost. The cancel and re-add events were emitted.
    /// - **Nothing traded and the restore failed too**: the order is gone.
    ///   The call returns [`OrderBookError::ModifyOrderLost`] with
    ///   `restore_error` set; the indices hold no trace of the order and its
    ///   state is `Cancelled { reason: RestFailed }` (unless another live
    ///   order now owns the id). The restore only rests, never matches: if
    ///   the original's price now crosses or locks the best opposite price
    ///   (an opposite order arrived after the cancel), it is refused with
    ///   `PriceCrossing` rather than letting the engine lock the book.
    /// - **The re-add traded, then failed**: its trades are real and the
    ///   original cannot be restored. A sweep aborted by a failed level
    ///   returns [`OrderBookError::MatchAborted`] as before; any other
    ///   failure (the residual could not be rested, self-trade prevention
    ///   cancelled the remainder) returns [`OrderBookError::ModifyOrderLost`]
    ///   with the executed quantity. The remainder never rests and the
    ///   order ends in a terminal state.
    ///
    /// A cancel that finds the order already gone (filled or cancelled
    /// concurrently) returns `Ok(None)` and re-adds nothing.
    ///
    /// # Concurrent fills during a modify (#247)
    ///
    /// Under the shared submit gate a taker can fill part of the order
    /// between the modify's read and its cancel. The re-add is built from
    /// what the cancel removed, never from the earlier read, so no quantity
    /// is created: `UpdatePrice` moves the remaining quantity to the new
    /// price; `UpdatePriceAndQuantity` and `Replace`, whose new quantity
    /// was chosen against the old state, are not applied: the remainder is
    /// restored (back of its level) and the call returns
    /// [`OrderBookError::ModifyRolledBack`] with source
    /// [`OrderBookError::OrderChangedDuringModify`].
    ///
    /// # Order state across a modify (#247)
    ///
    /// `filled_quantity` in every state the re-add records is cumulative:
    /// the fills the tracker already knew for the original, plus fills that
    /// raced the modify, plus the re-add's own. The original's cancel is
    /// recorded as `Cancelled { UserRequested }`; a re-add failure the
    /// modify resolves records no `Rejected` state or reject metric of its
    /// own, so a rollback reads `Cancelled { UserRequested }` followed by
    /// the restored status. A failure raised inside the re-add's sweep (a
    /// self-trade-prevention cancel with no fill, a failed post-only probe,
    /// an abort with an empty prefix) is recorded by the sweep and then
    /// overwritten by the restored status.
    pub fn update_order(
        &self,
        update: OrderUpdate,
    ) -> Result<Option<Arc<OrderType<T>>>, OrderBookError> {
        // #209: submit gate for the whole modify — its internal
        // cancel-then-add sequences call the ungated inner variants.
        // #225: exclusive when STP is engaged and the variant re-adds an
        // order that can match, so the guard spans validation through the
        // re-add and no concurrent admission, cancel or modify can land
        // between the re-add's STP scan and its fill. Repricing inherits
        // this path, so pegged re-prices are covered too.
        // #286: a modify that can trade (a cancel-then-add) evaluates the
        // pending stops and runs exclusively while one is pending; a
        // quantity update or cancel only when it targets a pending stop.
        let _gate = match update {
            OrderUpdate::UpdatePrice { .. }
            | OrderUpdate::UpdatePriceAndQuantity { .. }
            | OrderUpdate::Replace { .. } => {
                self.acquire_coherent_submit_gate(self.modify_needs_exclusive_gate(&update))
            }
            OrderUpdate::UpdateQuantity { order_id, .. } | OrderUpdate::Cancel { order_id } => self
                .acquire_gate_for_target(self.modify_needs_exclusive_gate(&update), order_id, true),
        };
        let result = self.update_order_gated(update);
        // #286: a re-add that traded, or a modified pending stop, is
        // evaluated under the same gate.
        self.settle_pending_stops();
        result
    }

    /// The body of [`Self::update_order`], run under the gate it took.
    fn update_order_gated(
        &self,
        update: OrderUpdate,
    ) -> Result<Option<Arc<OrderType<T>>>, OrderBookError> {
        // Gate non-cancel variants on the kill switch. Cancel passes
        // through unchanged so operators can drain the book. The
        // existing order stays live — only the modification is
        // rejected — so we use `check_kill_switch` (no tracker
        // recording) rather than `check_kill_switch_or_reject` (which
        // would mark a live order as terminal-Rejected).
        let is_modify = matches!(
            &update,
            OrderUpdate::UpdatePrice { .. }
                | OrderUpdate::UpdateQuantity { .. }
                | OrderUpdate::UpdatePriceAndQuantity { .. }
                | OrderUpdate::Replace { .. }
        );
        if is_modify {
            self.check_kill_switch()?;
        }

        self.cache.invalidate();
        trace!("Order book {}: Updating order {:?}", self.symbol, update);
        // #286: a pending trailing stop rests on no level; its modify is
        // applied to the stop store (exclusive gate: stops are pending).
        #[cfg(feature = "special_orders")]
        if self.has_pending_stops()
            && self
                .pending_stops
                .contains(super::stop_orders::update_target(&update))
        {
            return self.update_pending_stop(update);
        }
        match update {
            OrderUpdate::UpdatePrice {
                order_id,
                new_price,
            } => {
                // Get the order location without locking
                let location = self.order_locations.get(&order_id).map(|val| *val);

                if let Some(OrderLocation {
                    price: old_price, ..
                }) = location
                {
                    // If price doesn't change, do nothing
                    if old_price == new_price.as_u128() {
                        return Err(OrderBookError::InvalidOperation {
                            message: "Cannot update price to the same value".to_string(),
                        });
                    }

                    // Get the original order without holding locks
                    let original_order = if let Some(order) = self.get_order(order_id) {
                        // Create a copy of the order
                        (*order).clone()
                    } else {
                        return Ok(None); // Order not found
                    };

                    // Create a new order with the updated price
                    let mut new_order = original_order.clone();

                    // Update the price based on order type
                    match &mut new_order {
                        OrderType::Standard { price, .. } => *price = new_price,
                        OrderType::IcebergOrder { price, .. } => *price = new_price,
                        OrderType::PostOnly { price, .. } => *price = new_price,
                        OrderType::TrailingStop { price, .. } => *price = new_price,
                        OrderType::PeggedOrder { price, .. } => *price = new_price,
                        OrderType::MarketToLimit { price, .. } => *price = new_price,
                        OrderType::ReserveOrder { price, .. } => *price = new_price,
                    }

                    self.cancel_then_readd(
                        order_id,
                        &original_order,
                        new_order,
                        ReAddQuantity::FollowsRemainder,
                    )
                } else {
                    Ok(None) // Order not found
                }
            }

            OrderUpdate::UpdateQuantity {
                order_id,
                new_quantity,
            } => {
                // A zero requested quantity is a removal, not a resize. For
                // a one-tranche order zero is also a zero total: pricelevel
                // keeps such a maker in its queue (`new_total <= live_total`
                // ⇒ keep in place), so applying the update rested a maker at
                // zero depth that held `best_bid` / `best_ask` on a level
                // with nothing to fill, made `will_cross_market` reject a
                // post-only at that price, and was eventually dropped by a
                // sweep with no trade and no cancel event, leaking its
                // `order_locations` entry (`cancel_order` then returned
                // `Ok(None)` while a re-add of the id reported
                // `DuplicateOrderId`). For an iceberg / reserve order the
                // field is the visible tranche, so the projected total may
                // be nonzero and the hidden depth would keep filling; zero
                // still cancels the whole order by contract, hidden depth
                // included. Cancel through `cancel_order_with_reason`, the
                // removal `OrderBook::cancel_order` and, since #247, the
                // `OrderUpdate::Cancel` arm below perform. This branch runs no validator at all — a
                // removal has no shape to validate — so a configured
                // `min_order_size` cannot veto it. Only `UpdateQuantity` has
                // this removal semantic: `Replace` / `UpdatePriceAndQuantity`
                // with a zero quantity re-add the order through
                // validate-first — an iceberg or auto-replenishing reserve
                // rests with a zero visible tranche and its hidden depth
                // live, a non-replenishing reserve is rejected with
                // `ZeroVisibleTranche` and keeps resting (#230), and a
                // single-tranche maker ends as a terminal
                // `Filled { filled_quantity: 0 }` carrying nothing.
                // Ungated: `update_order` holds the submit gate (#209 / #225).
                if new_quantity.as_u64() == 0 {
                    return self.cancel_order_with_reason(order_id, CancelReason::UserRequested);
                }

                // Get order location without locking
                let location = self.order_locations.get(&order_id).map(|val| *val);

                if let Some(OrderLocation {
                    price,
                    side,
                    user_id: owner,
                    user_seq,
                }) = location
                {
                    // Get the appropriate price levels map
                    let price_levels = match side {
                        Side::Buy => &self.bids,
                        Side::Sell => &self.asks,
                    };

                    // Attempt to update the order within the price level
                    let mut result = None;
                    let mut is_empty = false;

                    // Get the price level and update it
                    if let Some(entry) = price_levels.get(&price) {
                        let price_level = entry.value();

                        // Validate-first (#211, extending the #98 contract
                        // to quantity updates): project the exact order
                        // pricelevel will store (`with_reduced_quantity` —
                        // the same rewrite `UpdateQuantity` applies
                        // upstream) and run the shared shape validator
                        // plus the modify-aware risk check BEFORE mutating
                        // the level. A rejected update leaves the maker
                        // untouched. The source order is read off the
                        // level entry already in hand — no `Arc` churn, no
                        // second `order_locations` / level lookup.
                        let Some(current_unit) = price_level
                            .iter_orders()
                            .find(|resting| resting.id() == order_id)
                        else {
                            return Ok(None); // Order not found
                        };
                        let current = self.convert_from_unit_type(current_unit.as_ref());
                        let projected = current.with_reduced_quantity(new_quantity.as_u64());
                        self.validate_order_shape(&projected)?;
                        let projected_total = projected.total_quantity()?;
                        self.check_risk_modify_admission(
                            order_id,
                            projected.user_id(),
                            price,
                            projected_total,
                        )?;

                        // #243 review: pre-book an increase's notional
                        // BEFORE the level commits the larger quantity, so
                        // no risk failure path remains after the level
                        // mutation. Settled below once the level answers.
                        let risk_reservation = self
                            .risk_state
                            .reserve_quantity_update(order_id, projected_total)?;

                        let update = OrderUpdate::UpdateQuantity {
                            order_id,
                            new_quantity,
                        };

                        #[cfg(test)]
                        {
                            if let Some(hook) = self.update_interleave_hook.as_ref() {
                                hook(order_id);
                            }
                        }

                        // Propagate upstream validation / counter errors
                        // (#211): `Ok(None)` is reserved for a genuinely
                        // absent order, never an error swallowed silently.
                        match price_level.update_order(update) {
                            Ok(Some(order)) => {
                                // Keep the per-account risk counters in
                                // lockstep with the applied update. The
                                // level stores the validated projection, so
                                // its total is representable; should it not
                                // be, the validated projection is booked
                                // instead and the breach is logged (#247).
                                let applied_total = match OrderQuantity::<()>::total_quantity(
                                    order.as_ref(),
                                ) {
                                    Ok(total) => total,
                                    Err(err) => {
                                        tracing::error!(
                                            %order_id,
                                            projected_total,
                                            error = %err,
                                            "updated order total unrepresentable; risk booked at the validated projection"
                                        );
                                        projected_total
                                    }
                                };
                                self.risk_state
                                    .commit_quantity_update(risk_reservation, applied_total);
                                // notify price level changes
                                self.emit_level_changed(side, price_level);
                                result = Some(Arc::new(self.convert_from_unit_type(&order)));
                            }
                            Ok(None) => {
                                self.risk_state.rollback_quantity_update(risk_reservation);
                            }
                            Err(err) => {
                                self.risk_state.rollback_quantity_update(risk_reservation);
                                return Err(OrderBookError::PriceLevelError(err));
                            }
                        }

                        is_empty = price_level.order_count() == 0;
                    }

                    // If the price level is now empty, remove it
                    if is_empty {
                        self.remove_level_if_empty(side, price);
                        // #288: untrack before releasing the id.
                        self.untrack_user_order(owner, user_seq, &order_id);
                        self.order_locations.remove(&order_id);
                    }

                    self.cache.invalidate();
                    if is_empty {
                        // Refresh depth gauges now that a level was
                        // removed during the modification path.
                        self.record_depth_metric();
                    }
                    Ok(result)
                } else {
                    Ok(None) // Order not found
                }
            }

            OrderUpdate::UpdatePriceAndQuantity {
                order_id,
                new_price,
                new_quantity,
            } => {
                // Get order location without locking
                let location = self.order_locations.get(&order_id).map(|val| *val);

                if location.is_some() {
                    // Get the original order without holding locks
                    let original_order = if let Some(order) = self.get_order(order_id) {
                        // Create a copy of the order
                        (*order).clone()
                    } else {
                        return Ok(None); // Order not found
                    };

                    // Create a new order with the updated price and quantity
                    let mut new_order = original_order.clone();

                    // Update the price based on order type
                    match &mut new_order {
                        OrderType::Standard { price, .. } => *price = new_price,
                        OrderType::IcebergOrder { price, .. } => *price = new_price,
                        OrderType::PostOnly { price, .. } => *price = new_price,
                        OrderType::TrailingStop { price, .. } => *price = new_price,
                        OrderType::PeggedOrder { price, .. } => *price = new_price,
                        OrderType::MarketToLimit { price, .. } => *price = new_price,
                        OrderType::ReserveOrder { price, .. } => *price = new_price,
                    }

                    // Two-tranche kinds take this as the visible tranche and
                    // keep hidden untouched, like `UpdateQuantity` (#221).
                    new_order.set_quantity(new_quantity.as_u64());

                    self.cancel_then_readd(
                        order_id,
                        &original_order,
                        new_order,
                        ReAddQuantity::Explicit,
                    )
                } else {
                    Ok(None) // Order not found
                }
            }

            OrderUpdate::Cancel { order_id } => {
                // #247: the same removal `cancel_order` performs, so a level
                // failure is propagated (the order still rests, or the
                // removal was completed and reported) and a successful
                // cancel records `Cancelled { UserRequested }`, releases the
                // risk contribution and unregisters special-order tracking.
                // Ungated: `update_order` holds the submit gate (#209).
                self.cancel_order_with_reason(order_id, CancelReason::UserRequested)
            }

            OrderUpdate::Replace {
                order_id,
                price,
                quantity,
                side,
            } => {
                // Get the original order without holding locks
                let original_opt = self.get_order(order_id);

                if let Some(original) = original_opt {
                    // Create a new order by cloning and updating the original
                    let mut new_order = (*original).clone();

                    // Update the order fields based on order type
                    match &mut new_order {
                        OrderType::Standard {
                            id,
                            price: p,
                            quantity: q,
                            side: s,
                            ..
                        } => {
                            *id = order_id;
                            *p = price;
                            *q = quantity;
                            *s = side;
                        }
                        OrderType::IcebergOrder {
                            id,
                            price: p,
                            visible_quantity,
                            side: s,
                            ..
                        } => {
                            *id = order_id;
                            *p = price;
                            *visible_quantity = quantity;
                            *s = side;
                        }
                        OrderType::PostOnly {
                            id,
                            price: p,
                            quantity: q,
                            side: s,
                            ..
                        } => {
                            *id = order_id;
                            *p = price;
                            *q = quantity;
                            *s = side;
                        }
                        OrderType::TrailingStop {
                            id,
                            price: p,
                            quantity: q,
                            side: s,
                            ..
                        } => {
                            *id = order_id;
                            *p = price;
                            *q = quantity;
                            *s = side;
                        }
                        OrderType::PeggedOrder {
                            id,
                            price: p,
                            quantity: q,
                            side: s,
                            ..
                        } => {
                            *id = order_id;
                            *p = price;
                            *q = quantity;
                            *s = side;
                        }
                        OrderType::MarketToLimit {
                            id,
                            price: p,
                            quantity: q,
                            side: s,
                            ..
                        } => {
                            *id = order_id;
                            *p = price;
                            *q = quantity;
                            *s = side;
                        }
                        OrderType::ReserveOrder {
                            id,
                            price: p,
                            visible_quantity,
                            side: s,
                            ..
                        } => {
                            *id = order_id;
                            *p = price;
                            *visible_quantity = quantity;
                            *s = side;
                        }
                    }

                    self.cancel_then_readd(order_id, &original, new_order, ReAddQuantity::Explicit)
                } else {
                    Ok(None) // Original order not found
                }
            }
        }
    }

    /// Cancel an order by ID.
    ///
    /// Tracks the cancellation as `CancelReason::UserRequested` in the
    /// order state tracker (if configured).
    ///
    /// Returns `Ok(None)` when `order_id` is not resting in this book.
    ///
    /// # Errors
    ///
    /// [`OrderBookError::PriceLevelError`] when the order's price level
    /// refuses the removal (for example a level poisoned by an earlier
    /// failure, or a level counter with no headroom left). The order is then
    /// still resting and tracked, and no event was emitted. Before 0.14.0
    /// this case returned `Ok(None)`, indistinguishable from an absent order
    /// (#248).
    ///
    /// [`OrderBookError::OrderRemovedWithLevelFault`] when the level removed
    /// the order and then reported a failure. The order **is gone**: the
    /// book completed the removal exactly like a successful cancel (events,
    /// `Cancelled` state, indices, risk release), and the error reports the
    /// faulty level (#248).
    pub fn cancel_order(&self, order_id: Id) -> Result<Option<Arc<OrderType<T>>>, OrderBookError> {
        // #209: shared gate — a concurrent FOK's exclusive window must not
        // interleave with this cancel. #286: exclusive only when the target
        // is a pending trailing stop.
        let _gate = self.acquire_gate_for_target(false, order_id, false);
        self.cancel_order_with_reason(order_id, CancelReason::UserRequested)
    }

    /// Cancel an order by ID with an explicit cancellation reason.
    ///
    /// This is the internal implementation used by both `cancel_order`
    /// and mass cancel operations to track the correct
    /// [`CancelReason`] in the order state tracker.
    ///
    /// # Errors
    ///
    /// A level that fails the removal is resolved by what the level still
    /// holds afterwards, so the book's indices never disagree with it
    /// (#248):
    ///
    /// - the order **still rests**: [`OrderBookError::PriceLevelError`];
    ///   no index, risk, state or level-map change and no event (only the
    ///   price-level cache was invalidated up front, which is harmless);
    /// - the order is **gone** (pricelevel can commit a removal and then
    ///   report a broken level invariant, poisoning the level): the removal
    ///   is completed on the book side exactly like a successful cancel
    ///   (level event, `Cancelled { reason }`, location, user index, risk,
    ///   special-order tracking, empty-level removal), logged at `ERROR`,
    ///   and reported as [`OrderBookError::OrderRemovedWithLevelFault`].
    pub(super) fn cancel_order_with_reason(
        &self,
        order_id: Id,
        reason: CancelReason,
    ) -> Result<Option<Arc<OrderType<T>>>, OrderBookError> {
        self.cache.invalidate();
        // First, we find the order's location (price and side) without locking
        let location = self.order_locations.get(&order_id).map(|val| *val);

        let Some(location @ OrderLocation { price, side, .. }) = location else {
            // #286: a pending trailing stop rests on no level.
            #[cfg(feature = "special_orders")]
            if self.has_pending_stops() {
                return Ok(self.cancel_pending_stop(order_id, reason));
            }
            return Ok(None);
        };
        let price_levels = match side {
            Side::Buy => &self.bids,
            Side::Sell => &self.asks,
        };

        // Attempt to cancel the order from the price level
        let mut removed: Option<RemovedOrder> = None;
        let mut empty_level = false;

        if let Some(entry) = price_levels.get(&price) {
            let price_level = entry.value();
            // #248: a refused removal (the order still rests) is returned
            // before anything is touched.
            removed = self
                .remove_resolved(price_level, order_id)
                .map_err(OrderBookError::PriceLevelError)?;

            // notify price level changes
            if removed.is_some() {
                self.emit_level_changed(side, price_level);
            }

            // Check if the level became empty
            empty_level = price_level.order_count() == 0;
        }

        self.cache.invalidate();
        let Some(removed) = removed else {
            return Ok(None);
        };

        self.finish_removal(order_id, Some(location), &removed, reason);

        // If the level became empty, remove it, unless a concurrent
        // admission refilled it meanwhile (#247: checked under the price's
        // stripe).
        if empty_level && self.remove_level_if_empty(side, price) {
            // Refresh the depth gauges now that a level was removed. No-op
            // when the `metrics` feature is disabled.
            self.record_depth_metric();
        }

        match removed {
            RemovedOrder::Clean(order) => Ok(Some(Arc::new(self.convert_from_unit_type(&order)))),
            RemovedOrder::Faulted(error) => {
                Err(self.order_removed_with_level_fault(order_id, side, price, reason, error))
            }
        }
    }

    /// Asks `price_level` to remove `order_id` and resolves a failure by
    /// what the level still holds afterwards (#248):
    ///
    /// - `Ok(None)`: the level does not hold the order;
    /// - `Ok(Some(Clean(order)))`: removed;
    /// - `Ok(Some(Faulted(error)))`: the level removed the order and then
    ///   reported `error`; the caller completes the removal;
    /// - `Err(error)`: refused, the order still rests and nothing changed.
    fn remove_resolved(
        &self,
        price_level: &PriceLevel,
        order_id: Id,
    ) -> Result<Option<RemovedOrder>, PriceLevelError> {
        match self.remove_from_level(price_level, order_id) {
            Ok(None) => Ok(None),
            Ok(Some(order)) => Ok(Some(RemovedOrder::Clean(order))),
            Err(error) => {
                // Cold path: one scan of this level.
                if price_level
                    .iter_orders()
                    .any(|order| order.id() == order_id)
                {
                    Err(error)
                } else {
                    Ok(Some(RemovedOrder::Faulted(error)))
                }
            }
        }
    }

    /// The book-side effects of a removal the level committed, shared by
    /// every single-order cancel (#248 / #247): the `Cancelled { reason }`
    /// state (keeping any prior fill), the location, the per-account risk
    /// contribution, the user index, the strandable-maker count and
    /// special-order tracking. Level-map removal stays with the caller.
    ///
    /// `location` is the order's location when the caller already read it
    /// (the user index removal then needs no second lookup); `None` reads
    /// it from `order_locations`.
    fn finish_removal(
        &self,
        order_id: Id,
        location: Option<OrderLocation>,
        removed: &RemovedOrder,
        reason: CancelReason,
    ) {
        // Track the cancellation in the order state tracker
        let prev_filled = self
            .order_state_tracker
            .as_ref()
            .and_then(|t| t.get(order_id))
            .map(|s| s.filled_quantity())
            .unwrap_or(0);
        self.track_state(
            order_id,
            OrderStatus::Cancelled {
                filled_quantity: prev_filled,
                reason,
            },
        );

        // Pre-trade risk hook: drop the per-account counter contribution
        // before the order leaves the index. The risk state stores
        // `account` and `remaining_qty` itself, so this needs no order
        // body. No-op when no `RiskConfig` is installed.
        self.risk_state.on_cancel(order_id);

        match removed {
            RemovedOrder::Clean(order) => {
                // Remove the order from the user_orders index
                match location {
                    Some(location) => {
                        self.untrack_user_order(location.user_id, location.user_seq, &order_id);
                    }
                    None => self.untrack_located_order(&order_id),
                }
                // #230: this helper is the funnel for user cancels, the
                // cancel-then-add modifies, mass cancel, expiry eviction and
                // the self-trade-prevention maker cancel, so one decrement
                // here covers all of them.
                self.note_removed_order(order.as_ref());
            }
            RemovedOrder::Faulted(_) => {
                // The level kept no body for the order it removed, so the
                // owner is read from the order's location (released
                // below, after this, per #288). The
                // strandable-maker count is deliberately NOT decremented:
                // without the body it cannot tell whether the order was
                // one, and an over-count only makes sweeps take the
                // exclusive gate (safe), where an under-count would not be.
                self.untrack_located_order(&order_id);
            }
        }

        // Unregister pegged orders from re-pricing tracking
        #[cfg(feature = "special_orders")]
        self.special_order_tracker
            .unregister_pegged_order(&order_id);

        // Release the id last (#288): the location is its ownership token,
        // so a new same-id admission can only start once every index of
        // this order is gone.
        self.order_locations.remove(&order_id);
    }

    /// Asks `price_level` to remove `order_id`. In `cfg(test)` builds the
    /// `cancel_fault_hook` can make the removal fail, before or after the
    /// level commits it (#248).
    #[inline]
    fn remove_from_level(
        &self,
        price_level: &PriceLevel,
        order_id: Id,
    ) -> Result<Option<Arc<OrderType<()>>>, PriceLevelError> {
        let update = OrderUpdate::Cancel { order_id };
        #[cfg(test)]
        {
            use super::book::CancelFault;
            match self
                .cancel_fault_hook
                .as_ref()
                .and_then(|hook| hook(order_id))
            {
                None => price_level.update_order(update),
                Some(CancelFault::Refuse(error)) => Err(error),
                Some(CancelFault::RemoveThenFail(error)) => {
                    price_level.update_order(update)?;
                    Err(error)
                }
            }
        }
        #[cfg(not(test))]
        {
            price_level.update_order(update)
        }
    }

    /// Logs a removal the level committed and then failed, and builds its
    /// error (#248).
    #[cold]
    #[inline(never)]
    fn order_removed_with_level_fault(
        &self,
        order_id: Id,
        side: Side,
        price: u128,
        reason: CancelReason,
        error: PriceLevelError,
    ) -> OrderBookError {
        tracing::error!(
            symbol = %self.symbol,
            %order_id,
            %side,
            price,
            %reason,
            %error,
            "price level failed after removing a cancelled order; the book completed the removal, the level is likely poisoned"
        );
        OrderBookError::OrderRemovedWithLevelFault {
            order_id,
            source: Box::new(error),
        }
    }

    /// Apply the side-effects of cancelling a single resting `order_id` that is
    /// known to live on the already-held `price_level` (resting on `side`),
    /// **without** removing the level from the bid/ask map.
    ///
    /// This mirrors the per-order effects of [`Self::cancel_order_with_reason`]
    /// — level-change event, `Cancelled { reason }` state transition, per-account
    /// risk release, `user_orders` / `order_locations` untrack, and special-order
    /// deregistration — but it deliberately does **not** touch the bid/ask
    /// `SkipMap`. The caller owns level removal (the matching loop drains
    /// `empty_price_levels` after the walk), so this is safe to invoke mid-walk:
    /// it never removes a level the iterator still references and never
    /// re-resolves `order_locations`, so a sequence of cancels on the same held
    /// level cannot skip a later order. Used by the STP `CancelMaker` /
    /// `CancelBoth` arms (#95). `Ok(())` if `order_id` is not resting on the
    /// level.
    ///
    /// # Errors
    ///
    /// A level that fails the removal is resolved exactly like
    /// [`Self::cancel_order_with_reason`] resolves it (#248, #247): if the
    /// maker still rests nothing changed; if the level removed it first, the
    /// removal is completed on the book side (event, state, indices, risk)
    /// and logged at `ERROR`. Either way the level's `PriceLevelError` is
    /// returned so the sweep stops at this level with the #240 abort
    /// semantics (committed prefix published, taker
    /// `Cancelled { MatchAborted }`), instead of carrying on as if the
    /// maker had been cancelled.
    pub(super) fn cancel_resting_maker_on_level(
        &self,
        price_level: &PriceLevel,
        side: Side,
        order_id: Id,
        reason: CancelReason,
    ) -> Result<(), PriceLevelError> {
        let Some(removed) = self.remove_resolved(price_level, order_id)? else {
            return Ok(());
        };
        self.cache.invalidate();
        self.emit_level_changed(side, price_level);
        self.finish_removal(order_id, None, &removed, reason);
        match removed {
            RemovedOrder::Clean(_) => Ok(()),
            RemovedOrder::Faulted(error) => {
                tracing::error!(
                    symbol = %self.symbol,
                    %order_id,
                    %side,
                    price = price_level.price(),
                    %reason,
                    %error,
                    "price level failed after removing a self-trade-prevention maker; the book completed the removal and the sweep stops here"
                );
                Err(error)
            }
        }
    }

    /// Validate the *shape* of an order against this book's admission
    /// rules **without** mutating any book state.
    ///
    /// This is the single source of truth for the non-risk admission
    /// checks that [`Self::add_order`] performs, in the same order and
    /// returning the same typed [`OrderBookError`] variants. Unlike
    /// `add_order` it is pure: it never calls
    /// [`track_state`](Self::track_state), [`reject_with_risk`](Self::reject_with_risk),
    /// emits metrics, or invalidates the cache. Every check here is a
    /// function of the new order plus the *opposite* book side, so it
    /// yields the same verdict whether evaluated before or after the
    /// original (same-side) order has been cancelled — which is what
    /// makes the validate-first atomic modify (#98) safe.
    ///
    /// Checks, in order:
    /// 1. Two-tranche total representability (`QuantityOverflow`).
    /// 2. Non-auto reserve's visible tranche non-empty
    ///    (`ZeroVisibleTranche` — see below).
    /// 3. STP `MissingUserId` (when STP is enabled and `user_id` is zero).
    /// 4. Tick size (`InvalidTickSize`).
    /// 5. Lot size (`InvalidLotSize`, per order kind — see below).
    /// 6. Min/max order size (`OrderSizeOutOfRange`).
    /// 7. Expiry (`InvalidOperation` — already expired).
    /// 8. Post-only would cross (`PriceCrossing`).
    /// 9. FOK feasibility (`InsufficientLiquidity`).
    ///
    /// # Zero visible tranche
    ///
    /// A `ReserveOrder` with `auto_replenish == false` whose
    /// `visible_quantity` is zero while `hidden_quantity > 0` is rejected
    /// with [`OrderBookError::ZeroVisibleTranche`] (#230). It displays
    /// nothing on its level, adds no visible depth, and `pricelevel`'s
    /// `match_against` returns `(0, None, 0, remaining)` for it: the maker is
    /// removed without a trade and its whole hidden tranche is stranded, the
    /// first time a taker reaches it.
    ///
    /// The rule is deliberately **that shape only**. The other zero-visible
    /// two-tranche shapes execute rather than vanishing, so they stay
    /// admissible: an `IcebergOrder` draws its entire hidden tranche into
    /// visible on match (upstream's "degenerate guard", which exists to keep
    /// the sweep making progress), and an auto-replenishing `ReserveOrder`
    /// refreshes `min(replenish_amount_or_default, hidden)` and re-queues.
    ///
    /// Because the rule lives here it covers
    /// `add_order` and the projected order of every quantity-carrying
    /// modify. Only `UpdatePriceAndQuantity` and `Replace` can actually
    /// reach it: both set the **visible** tranche since #221, so a zero
    /// quantity on either projects this shape out of a healthy resting
    /// reserve. Single-tranche kinds are
    /// unaffected, and so is a `(0, 0)` reserve, which carries
    /// nothing to strand.
    ///
    /// Interaction with #223: `UpdateQuantity` never reaches this rejection.
    /// A nonzero `new_quantity` leaves a positive visible tranche, and a
    /// zero one is a removal — the arm cancels the order through
    /// `cancel_order_with_reason` before any validator runs — so the shape
    /// is never projected on that variant. The other two are unaffected.
    ///
    /// # Lot size
    ///
    /// When the book carries a lot size, every quantity the engine can make
    /// *visible on a level* must be a whole multiple of it. The check is
    /// matched exhaustively over [`OrderType`], per kind (#226):
    ///
    /// - `Standard`, `PostOnly`, `TrailingStop`, `PeggedOrder` and
    ///   `MarketToLimit` carry a single quantity — that quantity is checked.
    /// - `IcebergOrder` is checked per tranche: `visible_quantity` and
    ///   `hidden_quantity` individually, because the hidden tranche becomes
    ///   the visible one as the order refills.
    /// - `ReserveOrder` is checked per tranche exactly like an iceberg and,
    ///   in addition, on the **capped transfer** that replenishment will move
    ///   from hidden into the visible tranche. That transfer is a quantity
    ///   the book will display, so it must be lot-aligned too. It is checked
    ///   only while `hidden_quantity > 0` (with no hidden tranche nothing is
    ///   ever transferred) and only while `auto_replenish` is on, which is
    ///   the single flag that decides whether anything is ever transferred
    ///   on either path (#230):
    ///   - `auto_replenish == true` with `replenish_amount == Some(a)`:
    ///     `min(a, hidden)` is checked.
    ///   - `auto_replenish == true` with `replenish_amount == None`:
    ///     `min(DEFAULT_RESERVE_REPLENISH_AMOUNT, hidden)` is checked — that
    ///     is the amount `pricelevel`'s `match_against` transfers when the
    ///     visible tranche is depleted or falls below the threshold, and the
    ///     amount the residual-resting helper behind
    ///     [`OrderQuantity::set_total_remaining`] falls back to.
    ///   - `auto_replenish == false`: no transfer check, whatever
    ///     `replenish_amount` says. `pricelevel` removes a resting maker
    ///     whose visible tranche is depleted instead of refreshing it, and
    ///     the residual helper leaves the visible tranche empty so
    ///     [`Self::add_order`] ends the order rather than resting it —
    ///     a depleted visible tranche ends the order on both paths, and the
    ///     book can never display a non-aligned quantity for it.
    ///
    /// `replenish_threshold` is unrestricted: it is only ever *compared*
    /// against the visible tranche, never transferred, so a non-aligned
    /// threshold cannot produce a non-aligned quantity.
    ///
    /// Validating the transfer **once, at admission** is sound because the
    /// hidden tranche of an admitted order stays **lot-aligned and never
    /// increases**: fills, [`reduce_reserve_to_total`] and `pricelevel`'s
    /// `new_hidden = hidden − replenish_qty` only ever shrink it, each by a
    /// lot-aligned amount (a lot-rounded fill or a validated transfer), and
    /// the quantity-rewriting paths (`with_reduced_quantity`,
    /// [`OrderQuantity::set_quantity`], `OrderUpdate::Replace`) touch the
    /// *visible* tranche only. Monotonicity alone would not do: the cap
    /// `min(amount, hidden)` is lot-aligned only because both operands are,
    /// so the alignment of `hidden` must be preserved as it shrinks. A cap
    /// that holds at admission therefore keeps holding.
    ///
    /// One intended consequence follows from the cap. A reserve order that
    /// relies on the default amount — `replenish_amount == None` with
    /// `auto_replenish == true` — is validated on
    /// `min(`[`DEFAULT_RESERVE_REPLENISH_AMOUNT`]`, hidden)`. While
    /// `hidden < 80` that transfer is the lot-aligned hidden tranche itself
    /// and the order is admitted (lot 25: 25 visible / 50 hidden passes,
    /// `min(80, 50) = 50`). Once `hidden >= 80` the transfer is exactly the
    /// default, so on a lot size that does not divide 80 (100, 25, 30, 60,
    /// 3, …) the order is rejected with `InvalidLotSize { quantity: 80, .. }`
    /// (lot 25: 25 / 100 fails). Such books must set an explicit lot-aligned
    /// `replenish_amount` for larger hidden tranches.
    ///
    /// Iceberg and Reserve therefore share identical visible / hidden
    /// validation, while Reserve additionally validates its applicable
    /// replenishment — so the two kinds can still reach different verdicts
    /// for the same `(visible, hidden)` pair.
    ///
    /// # Errors
    /// Returns the first failing check's typed [`OrderBookError`].
    pub(super) fn validate_order_shape(
        &self,
        order: &OrderType<T>,
    ) -> Result<ShapeVerdict, OrderBookError> {
        // Two-tranche total representability (#210): an Iceberg / Reserve
        // whose visible + hidden overflows u64 cannot be tracked by any of
        // the engine's quantity arithmetic — reject it before every other
        // check. The checked total is then used by every check below.
        let total = order.total_quantity()?;

        // Zero visible tranche (#230): a NON-AUTO-REPLENISHING reserve that
        // displays nothing is a ghost — no visible depth, and `pricelevel`
        // removes it with its hidden tranche stranded, without a trade, the
        // first time a taker reaches it. Rejected here so `add_order` and
        // every quantity-carrying modify projection are covered by one rule.
        //
        // Deliberately NOT extended to the other two-tranche shapes: an
        // iceberg draws its whole hidden tranche into visible on match
        // (upstream's "degenerate guard"), and an auto-replenishing reserve
        // refreshes `min(amount_or_default, hidden)` and re-queues, so both
        // execute rather than vanishing and neither is a ghost.
        if let Some(hidden_quantity) = Self::is_zero_visible_ghost(order) {
            return Err(OrderBookError::ZeroVisibleTranche {
                order_id: order.id(),
                hidden_quantity,
            });
        }

        // STP user_id enforcement: when STP is enabled, all orders must carry
        // a non-zero user_id so that self-trade checks can identify the owner.
        if self.stp_mode != crate::orderbook::stp::STPMode::None
            && order.user_id() == pricelevel::Hash32::zero()
        {
            return Err(OrderBookError::MissingUserId {
                order_id: order.id(),
            });
        }

        // Tick size validation: reject orders whose price is not a multiple of tick_size
        if let Some(tick) = self.tick_size
            && tick > 0
            && !order.price().as_u128().is_multiple_of(tick)
        {
            return Err(OrderBookError::InvalidTickSize {
                price: order.price().as_u128(),
                tick_size: tick,
            });
        }

        // Lot size validation: reject orders carrying a quantity the book
        // could display that is not a multiple of lot_size. Matched
        // exhaustively per kind (see the `# Lot size` section above) so a
        // future `OrderType` variant must choose its own rule instead of
        // silently inheriting the single-quantity check (#226).
        if let Some(lot) = self.lot_size
            && lot > 0
        {
            match order {
                OrderType::Standard { quantity, .. }
                | OrderType::PostOnly { quantity, .. }
                | OrderType::TrailingStop { quantity, .. }
                | OrderType::PeggedOrder { quantity, .. }
                | OrderType::MarketToLimit { quantity, .. } => {
                    check_lot_multiple(quantity.as_u64(), lot)?;
                }
                OrderType::IcebergOrder {
                    visible_quantity,
                    hidden_quantity,
                    ..
                } => {
                    check_lot_multiple(visible_quantity.as_u64(), lot)?;
                    check_lot_multiple(hidden_quantity.as_u64(), lot)?;
                }
                OrderType::ReserveOrder {
                    visible_quantity,
                    hidden_quantity,
                    replenish_amount,
                    auto_replenish,
                    ..
                } => {
                    // Per-tranche rule, identical to the iceberg one: both
                    // tranches take their turn on a level.
                    check_lot_multiple(visible_quantity.as_u64(), lot)?;
                    let hidden = hidden_quantity.as_u64();
                    check_lot_multiple(hidden, lot)?;

                    // Reserve-only: the replenishment transfer is itself a
                    // quantity the book will display, capped by whatever is
                    // left hidden. With no hidden tranche nothing moves.
                    if hidden > 0 {
                        let transfer = match (replenish_amount, auto_replenish) {
                            // Replenishing automatically with an explicit
                            // amount: that amount, capped by hidden.
                            (Some(amount), true) => Some(amount.get().min(hidden)),
                            // `pricelevel` falls back to its default amount
                            // when replenishing automatically without one,
                            // and so does the residual-resting helper.
                            (None, true) => {
                                Some(DEFAULT_RESERVE_REPLENISH_AMOUNT.get().min(hidden))
                            }
                            // Nothing ever transfers without
                            // `auto_replenish` (#230): `pricelevel` removes
                            // a depleted resting maker and the residual
                            // helper leaves the visible tranche empty, which
                            // ends the order. The explicit amount is dead
                            // configuration in that case.
                            (_, false) => None,
                        };
                        if let Some(transfer) = transfer {
                            check_lot_multiple(transfer, lot)?;
                        }
                    }
                }
            }
        }

        // Min/max order size validation
        let qty = total;
        if let Some(min) = self.min_order_size
            && qty < min
        {
            return Err(OrderBookError::OrderSizeOutOfRange {
                quantity: qty,
                min: Some(min),
                max: self.max_order_size,
            });
        }
        if let Some(max) = self.max_order_size
            && qty > max
        {
            return Err(OrderBookError::OrderSizeOutOfRange {
                quantity: qty,
                min: self.min_order_size,
                max: Some(max),
            });
        }

        if self.has_expired(order) {
            return Err(OrderBookError::InvalidOperation {
                message: "Order has already expired".to_string(),
            });
        }

        // #286: a trailing stop never sweeps at its stop price (it is held
        // off book and executes as a market order when elected, with the
        // market path's own trade-id and arithmetic preflights), so the
        // crossing checks below do not apply to it.
        if matches!(order, OrderType::TrailingStop { .. }) {
            return Ok(ShapeVerdict {
                fok: None,
                arithmetic_verified_price: 0,
            });
        }

        if order.is_post_only() && self.will_cross_market(order.price().as_u128(), order.side()) {
            return Err(self.price_crossing(order));
        }

        // For FOK orders, first check if the entire quantity can be matched
        // without altering the book. Use the faithful feasibility check (lot_size
        // + STP aware), not the raw-depth `peek_match`, so fill-or-kill stays
        // all-or-nothing and never emits a partial fill it then reports as killed (#96).
        //
        // Exhausted trade-id generator (#240): a crossing taker would mint at
        // least one trade id, and none is left, so its sweep would abort at
        // the first level. Reject it untouched here instead — for the modify
        // path this runs before the original is cancelled, so the original
        // keeps resting. Post-only takers never trade and are exempt. It runs
        // under the submit / modify gate the sweep holds; exact under the
        // exclusive gate, best-effort under the shared one (see
        // `check_trade_id_headroom` in book.rs).
        if !order.is_post_only()
            && self.transaction_id_generator.is_exhausted()
            && self.will_cross_market(order.price().as_u128(), order.side())
        {
            self.latch_trade_ids_exhausted();
            return Err(trade_ids_exhausted_error());
        }

        //
        // Trade arithmetic preflight (#244): the worst-case notional this
        // taker can reach (worst crossable price × total quantity) must fit
        // `u128` and be priced exactly by both fee legs, so no committed
        // trade can carry a clamped or dropped fee. Post-only takers never
        // trade and are exempt; a non-crossing order passes. Runs before
        // the original is cancelled on the modify path.
        // The re-add of a validate-first modify takes this whole verdict
        // as its admission and never re-runs it (#244, #247).
        let arithmetic_verified_price = if order.is_post_only() {
            0
        } else {
            self.check_trade_arithmetic(order.side(), total, Some(order.price().as_u128()))?
        };

        //
        // Fill-or-kill preflight (#240, #293): a later level can fail after
        // earlier levels committed, which would turn the FOK into a partial
        // fill. Everything the sweep can exhaust is checked here, before any
        // mutation: per level (`fok_fillable_quantity` in `Preflight` scope)
        // poisoning, counter headroom and a stopping maker step; then the
        // trade-id headroom of the book's `UuidGenerator` against the exact
        // trade ids the sweep takes. The result buffers are reserved by the
        // sweep itself from `reservation` before it touches the first level.
        // A failed dry run (`match_requirements` / insertion-sequence view)
        // propagates as a kill, never as zero depth. This runs under the
        // exclusive gate `add_order` takes for every fill-or-kill submit.
        if order.is_fill_or_kill() {
            let feasibility = self.fok_fillable_quantity(
                order.side(),
                total,
                Some(order.price().as_u128()),
                order.user_id(),
                order.id(),
                FeasibilityScope::Preflight,
            )?;
            if feasibility.fillable < total {
                return Err(OrderBookError::InsufficientLiquidity {
                    side: order.side(),
                    requested: total,
                    available: feasibility.fillable,
                });
            }
            if self.transaction_id_generator.remaining() < feasibility.trade_ids {
                return Err(OrderBookError::PriceLevelError(
                    PriceLevelError::CapacityExceeded {
                        resource: CapacityResource::IdSequence,
                        additional: usize::try_from(feasibility.trade_ids).unwrap_or(usize::MAX),
                    },
                ));
            }
            return Ok(ShapeVerdict {
                fok: Some(feasibility),
                arithmetic_verified_price,
            });
        }

        Ok(ShapeVerdict {
            fok: None,
            arithmetic_verified_price,
        })
    }

    /// STP self-cross pre-check for the validate-first atomic modify (#168).
    ///
    /// Closes the one post-match modify-atomicity gap #98 left open. Under
    /// [`STPMode::CancelTaker`](crate::orderbook::stp::STPMode::CancelTaker) /
    /// [`CancelBoth`](crate::orderbook::stp::STPMode::CancelBoth), if a
    /// re-priced order would cross into the **same user's** resting liquidity on
    /// the opposite side, `add_order` matches post-cancel and cancels the taker
    /// (the re-added order) — *after* the original was already removed,
    /// destroying it. This dry-runs the crossable opposite side and, if the
    /// sweep would reach a same-user maker while the taker still has unfilled
    /// quantity (the exact condition under which the engine sets
    /// `stp_taker_cancelled`), returns [`OrderBookError::SelfTradePrevented`]
    /// **before** the original is cancelled, so it survives unchanged.
    ///
    /// Reachability is decided per level exactly as the sweep decides it:
    /// the level's orders are read in insertion-sequence (consumption)
    /// order and handed to `check_stp_at_level`, whose `safe_quantity` is
    /// the non-self depth queued ahead of the first same-user maker. The
    /// engine pre-matches up to that depth and only then cancels the
    /// taker if quantity is still left, so a taker the non-self depth
    /// satisfies never reaches its own maker — at that level or any
    /// deeper one — and the modify is admitted. A same-user maker resting
    /// at a crossed level is therefore not by itself a reason to reject.
    ///
    /// How much that pre-match actually delivers is asked of
    /// `PriceLevel::matchable_quantity`, the same authoritative dry run the
    /// no-conflict arm uses, bounded by the non-self prefix. `safe_quantity`
    /// is a sum of *visible* quantities and can overstate what the sweep
    /// executes — a no-progress maker is set aside, and a replenish whose
    /// checked net delta would overflow the level's visible counter aborts
    /// the sweep untouched (#124) — and overstating it here would admit a
    /// reprice the sweep then kills after the original was already
    /// cancelled.
    ///
    /// No-op when STP is off, the taker is anonymous, or the mode is
    /// [`CancelMaker`](crate::orderbook::stp::STPMode::CancelMaker) (which
    /// cancels the maker and rests the taker — it never destroys the re-added
    /// order). Like the other validate-first checks (#98) it is a pure function
    /// of the new order plus the *opposite* book side, so evaluating it while
    /// the same-side original still rests yields the same verdict as after
    /// cancel.
    pub(super) fn check_modify_stp_self_cross(
        &self,
        new_order: &OrderType<T>,
    ) -> Result<(), OrderBookError> {
        use crate::orderbook::stp::{STPAction, STPMode, check_stp_at_level};

        let taker_user_id = new_order.user_id();
        // Only CancelTaker / CancelBoth cancel the taker; None / CancelMaker
        // rest it, so the re-added order is never destroyed.
        match self.stp_mode {
            STPMode::CancelTaker | STPMode::CancelBoth => {}
            _ => return Ok(()),
        }
        if taker_user_id == pricelevel::Hash32::zero() {
            return Ok(());
        }

        let side = new_order.side();
        let new_price = new_order.price().as_u128();
        let opposite = match side {
            Side::Buy => &self.asks,
            Side::Sell => &self.bids,
        };
        // Walk the crossable opposite side in price-time priority — asks
        // ascending for a Buy, bids descending for a Sell — exactly the sweep's
        // visit order.
        let iter = match side {
            Side::Buy => Either::Left(opposite.iter()),
            Side::Sell => Either::Right(opposite.iter().rev()),
        };

        let lot = self.lot_size.unwrap_or(1);
        let mut remaining = new_order.total_quantity()?;
        for entry in iter {
            // Lot-round the remaining budget exactly like the sweep's
            // `StopCondition::level_qty_cap`. A spent budget is a complete
            // fill, and a residual below one lot is dust the sweep stops on
            // before it scans another level (it rests, STP never consulted),
            // so neither can reach a same-user maker → the engine never
            // cancels the taker.
            //
            // Returning here — rather than skipping the level — also matches
            // the sweep's `StopCondition::zero_cap_is_terminal`: a modify is
            // always a base-quantity taker, and a base cap is the lot-rounded
            // residual, independent of the level price. Zero here is zero at
            // every level still ahead whichever way the walk runs, so there
            // is no side asymmetry to mirror. Only the sweep's
            // quote-notional sell arm keeps walking on a zero cap, because
            // its per-level cap rises again as bids get cheaper, and no
            // modify ever takes that arm.
            let cap = lot_floor(remaining, lot)?;
            if cap == 0 {
                return Ok(());
            }
            let price = *entry.key();
            let crosses = match side {
                Side::Buy => new_price >= price,
                Side::Sell => new_price <= price,
            };
            if !crosses {
                // Price-sorted levels: no further level can cross.
                break;
            }
            let level = entry.value();
            // Insertion-sequence order is the sweep's consumption order (#132),
            // so `safe_quantity` below is exactly the non-self depth the engine
            // pre-matches before it decides on the same-user maker.
            let orders = level.snapshot_by_insertion_seq()?;
            match check_stp_at_level(&orders, taker_user_id, self.stp_mode) {
                STPAction::NoConflict => {
                    // No same-user maker at this level: the taker consumes its
                    // full matchable depth under the lot-rounded cap (the
                    // authoritative upstream dry run), then walks on.
                    remaining = consume_matchable(
                        remaining,
                        cap,
                        level.matchable_quantity(cap, new_order.id())?,
                    )?;
                }
                STPAction::CancelTaker { safe_quantity }
                | STPAction::CancelBoth { safe_quantity, .. } => {
                    // The sweep pre-matches `min(cap, safe_quantity)` against
                    // the non-self depth queued ahead of the same-user maker
                    // and cancels the taker only if quantity is still left
                    // after that. A modify is always base quantity, and a
                    // base residual is never walked past (only quote-notional
                    // dust is), so any residual here is the engine's cancel
                    // verdict; a taker the non-self depth satisfies never
                    // reaches its own maker.
                    //
                    // What the sweep subtracts is what `PriceLevel::match_order`
                    // *executes*, not the depth `check_stp_at_level` counted:
                    // `safe_quantity` sums the **visible** quantity of the
                    // makers ahead, and a maker can be counted there and still
                    // deliver less — a maker that makes no progress is set
                    // aside, and a replenish whose checked net delta would
                    // overflow the level's visible counter aborts the sweep
                    // with that maker untouched (#124 / PriceLevel#130). Taking
                    // `safe_quantity` at face value would admit a reprice the
                    // sweep then kills, which is precisely the destruction
                    // #168 exists to prevent, so the pre-match is bounded by
                    // the same authoritative dry run the `NoConflict` arm uses.
                    // Capping its request at `cap.min(safe_quantity)` keeps it
                    // inside the non-self prefix, so it never counts depth
                    // behind the same-user maker.
                    let request = cap.min(safe_quantity);
                    remaining = consume_matchable(
                        remaining,
                        request,
                        level.matchable_quantity(request, new_order.id())?,
                    )?;
                    if remaining > 0 {
                        return Err(OrderBookError::SelfTradePrevented {
                            mode: self.stp_mode,
                            taker_order_id: new_order.id(),
                            user_id: taker_user_id,
                        });
                    }
                    return Ok(());
                }
                // Unreachable: the mode filter above returned for CancelMaker,
                // which cancels the maker and never the taker.
                STPAction::CancelMaker => return Ok(()),
            }
        }
        Ok(())
    }

    /// Reserve-residual pre-check for the validate-first atomic modify
    /// (#230, extending #98 / #168).
    ///
    /// The three cancel-then-add arms (`UpdatePrice`,
    /// `UpdatePriceAndQuantity`, `Replace`) cancel the original and then
    /// re-add it as an aggressive taker. Since #230 a re-added
    /// [`OrderType::ReserveOrder`] with `auto_replenish == false` whose
    /// sweep exhausts its visible tranche does **not** rest: its hidden
    /// remainder is discarded and the order ends. Without this check the
    /// modify would cancel the original, destroy the re-added order and
    /// still report `Ok(Some(..))` — exactly the silent destruction the
    /// validate-first contract exists to prevent.
    ///
    /// Rejects with [`OrderBookError::ReserveResidualWouldBeDiscarded`]
    /// **before** the original is cancelled, so it keeps resting unchanged,
    /// when all of the following hold for the projected order:
    ///
    /// - it is a `ReserveOrder` with `auto_replenish == false`;
    /// - its hidden tranche is non-empty (nothing to discard otherwise);
    /// - the depth it would cross at its projected price is non-zero, at
    ///   least its visible tranche — the exact condition under which the
    ///   residual guard fires — **and** strictly less than its total. A
    ///   projected **full** fill is allowed through: it executes everything
    ///   and discards nothing.
    ///
    /// A non-crossing re-price, `crossable == 0`, rewrites no tranche and is
    /// allowed as well.
    ///
    /// A projected visible tranche of zero cannot reach here: the shared
    /// validator rejects that shape with
    /// [`OrderBookError::ZeroVisibleTranche`] first.
    ///
    /// The crossable depth comes from [`Self::fok_fillable_quantity`], the
    /// same lot-size- and STP-aware feasibility walk fill-or-kill uses, so
    /// the estimate matches what the sweep would actually fill rather than
    /// raw level depth. Like the other validate-first checks it is a pure
    /// function of the projected order plus the *opposite* book side, so
    /// evaluating it while the same-side original still rests yields the
    /// same verdict as after cancel.
    ///
    /// The dry run is **exact**, not best-effort. Whenever this check can
    /// fire, the order being modified is itself a strandable maker, so the
    /// book's `strandable_makers_resting` is at least one and
    /// [`modify_needs_exclusive_gate`](Self::modify_needs_exclusive_gate)
    /// has already put the whole modify on the exclusive side: no
    /// concurrent mutation can move the opposite side between this estimate
    /// and the re-add's sweep. (The #168 self-cross check keeps its
    /// best-effort character, because it also runs on books that hold no
    /// strandable maker and therefore modify on the shared side.)
    /// Auto-replenishing reserves, icebergs, single-tranche kinds and
    /// non-crossing re-prices never reach the walk.
    ///
    /// The walk runs in [`FeasibilityScope::DepthOnly`] scope, which
    /// deliberately relaxes `PriceLevel::match_requirements`' exclusivity
    /// precondition: only the fillable quantity is used, the same advisory
    /// dry run as `PriceLevel::matchable_quantity`, so the estimate would
    /// stay sound (advisory, never a counter or reservation decision) even
    /// if this check ever ran under the shared gate.
    ///
    /// # Errors
    /// [`OrderBookError::ReserveResidualWouldBeDiscarded`] carrying the
    /// order id, the projected visible tranche, the crossable quantity, the
    /// projected `hidden_quantity` and the `discarded_quantity` that would
    /// actually be destroyed (`visible + hidden - crossable`).
    pub(super) fn check_modify_reserve_residual(
        &self,
        new_order: &OrderType<T>,
    ) -> Result<(), OrderBookError> {
        let OrderType::ReserveOrder {
            visible_quantity,
            hidden_quantity,
            auto_replenish: false,
            ..
        } = new_order
        else {
            return Ok(());
        };
        let visible = visible_quantity.as_u64();
        let hidden = hidden_quantity.as_u64();
        if hidden == 0 {
            return Ok(());
        }

        let total = new_order.total_quantity()?;
        let crossable = self
            .fok_fillable_quantity(
                new_order.side(),
                total,
                Some(new_order.price().as_u128()),
                new_order.user_id(),
                new_order.id(),
                FeasibilityScope::DepthOnly,
            )?
            .fillable;
        // `crossable < visible`: the sweep leaves a positive visible tranche
        // and the residual rests normally. `crossable >= total`: the order
        // fills completely, so nothing is discarded. Only the band in
        // between destroys quantity. `crossable == 0` is defense in depth:
        // `validate_order_shape` already rejects a projected zero visible
        // tranche, and without that rule a non-crossing re-price of such an
        // order would fall inside the band vacuously.
        // `total - crossable` is positive exactly when `crossable < total`.
        if crossable > 0
            && crossable >= visible
            && let Some(discarded) = total.checked_sub(crossable)
            && discarded > 0
        {
            return Err(reserve_residual_would_be_discarded(
                new_order.id(),
                visible,
                crossable,
                hidden,
                discarded,
            ));
        }
        Ok(())
    }

    /// Record the terminal state transition (and metric) that the direct
    /// [`Self::add_order`] path historically emitted for each shape
    /// rejection returned by [`Self::validate_order_shape`].
    ///
    /// Keeping this mapping next to the validator preserves the exact
    /// pre-#98 reject side-effects of `add_order` while letting the
    /// validate-first modify path reuse the same pure validator without
    /// recording any state. Errors that previously had no side-effect
    /// (e.g. the already-expired `InvalidOperation`) are intentionally
    /// no-ops here.
    pub(super) fn record_shape_rejection(&self, order: &OrderType<T>, err: &OrderBookError) {
        match err {
            OrderBookError::MissingUserId { .. } => {
                self.track_state(
                    order.id(),
                    OrderStatus::Rejected {
                        reason: RejectReason::MissingUserId,
                    },
                );
            }
            OrderBookError::QuantityOverflow { .. } | OrderBookError::ZeroVisibleTranche { .. } => {
                self.track_state(
                    order.id(),
                    OrderStatus::Rejected {
                        reason: RejectReason::InvalidQuantity,
                    },
                );
            }
            OrderBookError::InvalidTickSize { .. } => {
                self.track_state(
                    order.id(),
                    OrderStatus::Rejected {
                        reason: RejectReason::InvalidPrice,
                    },
                );
            }
            OrderBookError::InvalidLotSize { .. } => {
                self.track_state(
                    order.id(),
                    OrderStatus::Rejected {
                        reason: RejectReason::InvalidQuantity,
                    },
                );
            }
            OrderBookError::OrderSizeOutOfRange { .. } => {
                self.track_state(
                    order.id(),
                    OrderStatus::Rejected {
                        reason: RejectReason::OrderSizeOutOfRange,
                    },
                );
            }
            OrderBookError::PriceCrossing { .. } => {
                self.track_state(
                    order.id(),
                    OrderStatus::Rejected {
                        reason: RejectReason::PostOnlyWouldCross,
                    },
                );
            }
            OrderBookError::InsufficientLiquidity { .. } => {
                self.track_state(
                    order.id(),
                    OrderStatus::Cancelled {
                        filled_quantity: 0,
                        reason: CancelReason::InsufficientLiquidity,
                    },
                );
                crate::orderbook::metrics::record_reject(RejectReason::InsufficientLiquidity);
            }
            // The trade arithmetic preflight (#244): the book is untouched.
            OrderBookError::FeeOverflow { .. } | OrderBookError::NotionalOverflow { .. } => {
                let reason = RejectReason::from(err);
                self.track_state(order.id(), OrderStatus::Rejected { reason });
                crate::orderbook::metrics::record_reject(reason);
            }
            // A fill-or-kill preflight kill (#240): the feasibility dry run
            // failed or a resource the sweep would exhaust (trade-id
            // headroom) is short. The book is untouched; record the
            // dedicated resource code.
            OrderBookError::PriceLevelError(_) => {
                let reason = RejectReason::from(err);
                self.track_state(order.id(), OrderStatus::Rejected { reason });
                crate::orderbook::metrics::record_reject(reason);
            }
            // The already-expired `InvalidOperation` path historically
            // recorded no terminal transition; preserve that.
            _ => {}
        }
    }

    /// Add a new order to the book, automatically matching it if it's aggressive.
    ///
    /// This convenience method calls the same implementation as
    /// [`Self::add_order_with_result`] but discards the trade result. When no
    /// trade listener is installed, the `TradeResult` is never constructed, so
    /// this path stays free of the extra `MatchResult` clone.
    ///
    /// # Two-tranche takers
    ///
    /// An aggressive iceberg or reserve sweeps with its **total** quantity,
    /// not with its visible tranche: `add_order_inner` passes
    /// `total_quantity()` to matching. A reserve of 10 visible / 20 hidden
    /// submitted into 20 units of contra liquidity therefore executes 20.
    /// The identical order **resting** as a maker without automatic
    /// replenishment executes only its 10 visible units, because
    /// `pricelevel` removes a depleted non-auto maker from its level and
    /// strands the 20 hidden. That asymmetry between the aggressive and the
    /// resting side is upstream behaviour and is deliberately left as is.
    ///
    /// What #230 reconciled is the *residual*: whatever the sweep leaves
    /// unmatched now follows `auto_replenish` the same way the maker does.
    /// With it on, a visible tranche left below
    /// `max(replenish_threshold, 1)` is refreshed out of hidden (explicit
    /// `replenish_amount` or [`DEFAULT_RESERVE_REPLENISH_AMOUNT`], capped by
    /// hidden) and the residual rests; with it off the residual does not
    /// rest at all and its hidden remainder is discarded. The accounting
    /// rule holds in every case, and discarded quantity is never counted as
    /// executed:
    ///
    /// ```text
    /// submitted = executed + resting (visible + hidden) + discarded
    /// ```
    ///
    /// With `auto_replenish` off the residual is discarded **only when the
    /// fill exhausted the visible tranche**. A shallower fill rests
    /// normally: the 10 visible / 20 hidden reserve above, filled for 5,
    /// rests 5 / 20 with nothing discarded.
    ///
    /// An explicit `replenish_amount` is the transfer, not a target display
    /// size: it is added to whatever visible quantity survived, so amount
    /// 10 with threshold 5 and a remainder of 2 visible rests 12 visible.
    /// Without one the transfer is `DEFAULT_RESERVE_REPLENISH_AMOUNT` capped
    /// by hidden, so the same reserve filled for 10 rests 20 visible / 0
    /// hidden — more than it first displayed, since
    /// `min(DEFAULT_RESERVE_REPLENISH_AMOUNT, 20) == 20`.
    ///
    /// ## What the returned order holds
    ///
    /// The `Arc<OrderType<T>>` this call returns describes the outcome, so
    /// its tranches differ per branch:
    ///
    /// - **Fully matched**: the order as submitted. Its tranches still read
    ///   as they were sent, because nothing was left to redistribute.
    /// - **Rested**: the resting residual, with the tranches the book now
    ///   holds (post-reduction and post-refresh).
    /// - **Discarded**: the ended order with **both tranches at zero**, so
    ///   `total_quantity()` is `0`. The discarded hidden quantity is
    ///   deliberately not reported there — the order rests nowhere and can
    ///   never trade again; read the dropped amount from the
    ///   `orderbook_reserve_hidden_discarded_total` metric or the `INFO`
    ///   trace the guard emits.
    ///
    /// The resting side reports the same loss the same way, with
    /// `path = "maker"`, when `pricelevel` removes a depleted non-auto
    /// reserve maker. That report costs a pre-match pass over the level's
    /// resting orders, so it is gated on a monotonic per-book flag read once
    /// per sweep: a book that has never rested such a maker pays that single
    /// relaxed atomic load and nothing more, while a book that has pays the
    /// pass on every level holding hidden depth. The pass is not cheap —
    /// `PriceLevel::iter_orders` read-locks every shard of the level's
    /// `DashMap` regardless of how few orders rest there.
    ///
    /// # Aborted sweeps (#240)
    ///
    /// When a price level fails mid-sweep (pricelevel reports it through
    /// `MatchResult::error()`, e.g. an exhausted trade-id sequence or level
    /// counter, or a refused allocation) the sweep stops at that level and
    /// never trades at a worse price. The trades committed before the
    /// failure are real and are published exactly like a partial fill —
    /// trade listener, price-level listener, risk, maker states — and the
    /// remainder never rests, whatever the time-in-force. The call returns
    /// [`OrderBookError::MatchAborted`] and the taker ends
    /// `Cancelled { filled_quantity, reason: CancelReason::MatchAborted }`.
    /// Use [`Self::add_order_with_committed`] to receive the committed
    /// `TradeResult` with the error.
    ///
    /// A fill-or-kill taker is killed in one of two shapes, both before any
    /// mutation: not enough reachable depth is
    /// [`OrderBookError::InsufficientLiquidity`] with
    /// `Cancelled { InsufficientLiquidity }`; a resource shortfall (trade-id
    /// headroom, result buffers, a failed feasibility dry run) is
    /// [`OrderBookError::PriceLevelError`] with `Rejected` under
    /// `RejectReason::CapacityExceeded` (16) / `CounterExhausted` (17).
    ///
    /// A crossing taker of any time-in-force is rejected the same untouched
    /// way when the book's trade-id generator is exhausted
    /// ([`Self::trade_ids_exhausted`]).
    ///
    /// A fill-or-kill taker is preflighted before any mutation, under the
    /// exclusive submit gate: every level it will reach is dry-run for the
    /// quantity the sweep will ask of it (`PriceLevel::match_requirements`),
    /// its trade-id headroom is checked against the exact trade ids the
    /// sweep takes and its result buffers are reserved for the exact trade
    /// count. A poisoned level, a per-level counter without headroom, a maker
    /// step that would stop the sweep or a buffer / trade-id shortfall
    /// rejects it untouched with [`OrderBookError::PriceLevelError`]
    /// (`InvalidOperation`, `CounterExhausted`, `CapacityExceeded`).
    /// Residual: an allocator refusal inside pricelevel while a level
    /// matches, and levels where self-trade prevention `CancelMaker` cancels
    /// makers first (not dry-run exactly), can still stop a FOK mid-sweep,
    /// following the rules above. See `doc/panic-boundaries.md` for every
    /// residual.
    ///
    /// # Errors
    /// Returns [`OrderBookError::KillSwitchActive`] when the kill switch
    /// is engaged. The check runs before any cache invalidation, STP
    /// validation, tick/lot validation, or matching work. Returns
    /// [`OrderBookError::MatchAborted`] for an aborted sweep (see above).
    /// Returns [`OrderBookError::RiskRejectedAfterTrades`] when the taker
    /// traded and the risk layer then refused to reserve its residual
    /// (#291): the trades are real, the residual did not rest and the taker
    /// ends `Cancelled { RestFailed }`. A risk refusal before any trade is
    /// the plain risk error (`RiskMaxOpenOrders`, `RiskMaxNotional`, ...).
    #[inline]
    pub fn add_order(&self, order: OrderType<T>) -> Result<Arc<OrderType<T>>, OrderBookError> {
        // #209: shared gate for ordinary submits, exclusive for FOK so its
        // feasibility + sweep window excludes every concurrent mutation.
        // #225: also exclusive for an STP-relevant submit, so the per-level
        // STP scan and the fill it authorises see the same queue state. A
        // post-only submit never reaches that scan, so it stays shared.
        let _gate = self.acquire_add_gate(&order);
        let result = self.add_order_inner(order, false, false, Admission::Submit);
        // #286: pending stops are evaluated under the same gate.
        self.settle_pending_stops();
        result
            .map(|(order, _)| order)
            .map_err(|failure| failure.into_submit().into_error())
    }

    /// Acquires the submit gate for an `add_order*` submit: the mode of
    /// [`Self::add_order_needs_exclusive_gate`], and a post-only submit,
    /// which cannot trade, keeps the shared side while trailing stops are
    /// pending (#286).
    #[inline]
    fn acquire_add_gate(&self, order: &OrderType<T>) -> SubmitGateGuard<'_, T> {
        self.acquire_submit_gate_for(
            self.add_order_needs_exclusive_gate(order),
            !order.is_post_only(),
        )
    }

    /// The submit gate mode of an `add_order*` submit (#209 / #225 / #230 /
    /// #286): [`Self::submit_needs_exclusive_gate`] for the order, plus
    /// the exclusive side for the admission of a pending trailing stop.
    #[inline]
    fn add_order_needs_exclusive_gate(&self, order: &OrderType<T>) -> bool {
        Self::admits_pending_stop(order)
            || self.submit_needs_exclusive_gate(
                order.is_fill_or_kill(),
                order.user_id(),
                order.is_post_only(),
                // #230: admitting a strandable maker is exclusive in every
                // STPMode, so no sweep can consume one it never captured.
                Self::is_strandable_maker(order),
            )
    }

    /// Add a new order to the book, automatically matching it if it's
    /// aggressive, and additionally return the [`TradeResult`] produced by the
    /// match directly to the caller.
    ///
    /// The trade result is `None` when the order produced no fills (it rested
    /// on the book, or was admitted without matching). When a trade listener
    /// is installed, the listener is invoked with the exact same `TradeResult`
    /// that is returned here — same fills, same fees, same `engine_seq`.
    ///
    /// Per-call attribution: concurrent submits on the same book each receive
    /// exactly their own fills; the result is built from this call's private
    /// match outcome, never from shared capture state. The engine holds no
    /// cross-call trade accumulator — each returned `TradeResult` is
    /// constructed from the `MatchResult` produced by this invocation alone —
    /// so two threads submitting crossing orders concurrently cannot observe
    /// each other's fills in their own returned result.
    ///
    /// On error paths that follow real fills (an unfillable IOC remainder, or
    /// a self-trade-prevention cancellation after earlier non-self fills) the
    /// typed error is returned instead, so those fills reach the trade
    /// listener only.
    ///
    /// Two-tranche takers (iceberg / reserve) sweep with their **total**
    /// quantity and their residual follows `auto_replenish`: see the
    /// "Two-tranche takers" section on [`Self::add_order`] for the
    /// accounting rule and for what the returned order holds on each of the
    /// fully-matched, rested and discarded branches.
    ///
    /// Every trade-producing call consumes one `engine_seq` tick, even when no
    /// trade listener is installed (plain [`Self::add_order`] only consumes one
    /// when a listener is present). `engine_seq` is per-instance and not
    /// replay-reproducible; consumers that need a stable ordering key should
    /// use the journal's `sequence_num` / `timestamp_ns` instead.
    ///
    /// # Errors
    /// Returns [`OrderBookError::KillSwitchActive`] when the kill switch
    /// is engaged. The check runs before any cache invalidation, STP
    /// validation, tick/lot validation, or matching work. Returns
    /// [`OrderBookError::MatchAborted`] when the sweep stopped at a failed
    /// price level (#240): the committed prefix reached the trade listener
    /// but is not returned here; use [`Self::add_order_with_committed`] to
    /// receive it with the error.
    pub fn add_order_with_result(
        &self,
        order: OrderType<T>,
    ) -> Result<(Arc<OrderType<T>>, Option<TradeResult>), OrderBookError> {
        // #209 / #225 / #286: same gating as `add_order`.
        let _gate = self.acquire_add_gate(&order);
        let result = self.add_order_inner(order, true, false, Admission::Submit);
        self.settle_pending_stops();
        result.map_err(|failure| failure.into_submit().into_error())
    }

    /// [`Self::add_order_with_result`] for callers that must record what a
    /// failed submit committed — typically a sequencer journaling the
    /// outcome (#240).
    ///
    /// Identical matching, gating and publication; the only difference is
    /// the error type. A submit can execute real trades and *then* fail
    /// (a sweep aborted by a failed price level, an unfillable IOC
    /// remainder, a taker self-trade prevention cancels after non-self
    /// fills, a residual that cannot be admitted); the returned
    /// [`SubmitFailure`] carries that typed error together with the
    /// committed [`TradeResult`] — the very value the trade listener
    /// received — so the caller can build
    /// [`SequencerResult::from_submit_failure`](crate::SequencerResult::from_submit_failure).
    ///
    /// # Errors
    ///
    /// A [`SubmitFailure`] whose `error` is exactly what
    /// [`Self::add_order_with_result`] returns for the same call, and whose
    /// `committed` is `Some` when trades executed before the failure.
    pub fn add_order_with_committed(
        &self,
        order: OrderType<T>,
    ) -> Result<(Arc<OrderType<T>>, Option<TradeResult>), SubmitFailure> {
        // #209 / #225 / #230 / #286: same gating as `add_order`.
        let _gate = self.acquire_add_gate(&order);
        let result = self.add_order_inner(order, true, true, Admission::Submit);
        self.settle_pending_stops();
        result.map_err(AdmitFailure::into_submit)
    }

    /// Replays an `add_order` whose live execution traded and then had its
    /// residual refused by the risk layer
    /// ([`OrderBookError::RiskRejectedAfterTrades`], #291).
    ///
    /// Identical to [`Self::add_order`] (gating, admission checks, sweep,
    /// publication) except that a residual that would rest is refused
    /// instead: the live refusal depended on the source book's `RiskConfig`
    /// and concurrent state, neither of which replay has, while the sweep is
    /// a deterministic function of the book and the order. The replayed
    /// book therefore ends exactly like the live one (the same trades, no
    /// residual resting).
    ///
    /// # Errors
    ///
    /// [`OrderBookError::RiskRejectedAfterTrades`] when the sweep traded
    /// and the residual was refused, which is the outcome the journal
    /// recorded. Anything else means the replay diverged: `Ok` when the
    /// order filled completely (nothing left to refuse), the refusal's
    /// `InvalidOperation` when the sweep did not trade, or whatever error
    /// [`Self::add_order`] would return before the residual step.
    pub(crate) fn replay_add_order_refusing_residual(
        &self,
        order: OrderType<T>,
    ) -> Result<Arc<OrderType<T>>, OrderBookError> {
        // Same gating as `add_order`.
        let _gate = self.acquire_add_gate(&order);
        let result = self.add_order_inner(order, false, false, Admission::ReplayRefusingResidual);
        self.settle_pending_stops();
        result
            .map(|(order, _)| order)
            .map_err(|failure| failure.into_submit().into_error())
    }

    /// Shared implementation behind [`Self::add_order`] and
    /// [`Self::add_order_with_result`]. `want_result` gates `TradeResult`
    /// construction so the plain `add_order` path only pays for it when an
    /// installed trade listener needs it anyway.
    ///
    /// `want_committed` (set only by [`Self::add_order_with_committed`])
    /// hands the committed `TradeResult` back inside a failure. Every other
    /// caller drops it, so the failure paths that follow real fills (IOC
    /// remainder, STP taker cancel, residual admission, abort) do not box a
    /// `TradeResult` just to discard it.
    ///
    /// `admission` is [`Admission::ReAdd`] only for the re-add of a
    /// validate-first modify, which ran every admission check before it
    /// cancelled the original; the re-add takes that verdict instead of
    /// re-running them (#244, #247).
    ///
    /// Every failure carries what the taker executed before it
    /// ([`AdmitFailure::traded`]), which decides whether a failed modify
    /// re-add can restore the original (#247). Every failure also leaves the
    /// taker in a terminal order state, except a duplicate id, whose state
    /// belongs to the live order that owns the id.
    pub(super) fn add_order_inner(
        &self,
        mut order: OrderType<T>,
        want_result: bool,
        want_committed: bool,
        admission: Admission,
    ) -> Result<(Arc<OrderType<T>>, Option<TradeResult>), AdmitFailure> {
        let committed = |trade_result: Option<TradeResult>| {
            if want_committed { trade_result } else { None }
        };
        // #286: a trailing stop never sweeps or rests on a level. It is
        // held off book as a pending stop (`special_orders`) or rejected
        // untouched. A modify never re-adds one: `update_order` handles
        // pending stops itself.
        if matches!(order, OrderType::TrailingStop { .. }) {
            // A re-add can run under the shared gate, where no stop may be
            // admitted; none reaches here (no stop rests on a level), so
            // refuse it rather than rely on that.
            if matches!(admission, Admission::ReAdd { .. }) {
                return Err(OrderBookError::StopOrdersUnsupported {
                    order_id: order.id(),
                }
                .into());
            }
            return self
                .admit_trailing_stop(order, admission.records_rejections())
                .map(|order| (order, None))
                .map_err(AdmitFailure::from);
        }
        let total = match admission {
            Admission::Submit | Admission::ReplayRefusingResidual => {
                self.check_kill_switch_or_reject(order.id())?;
                // Representability gate (#210): an unrepresentable
                // two-tranche total must be rejected before the risk gate
                // below, which could not evaluate the account's notional.
                // `validate_order_shape` re-checks this for the shared
                // modify path.
                let total = match order.total_quantity() {
                    Ok(total) => total,
                    Err(err) => {
                        self.record_shape_rejection(&order, &err);
                        return Err(err.into());
                    }
                };
                // Pre-trade risk gate: per-account open-orders / notional /
                // price band. No-op when no `RiskConfig` is installed.
                // Documented order: kill_switch → risk → STP → fees → match.
                // On the cold reject path, record an `OrderStatus::Rejected`
                // transition with the closed `RejectReason` taxonomy before
                // propagating the typed error.
                if let Err(err) =
                    self.check_risk_limit_admission(order.user_id(), order.price().as_u128(), total)
                {
                    self.reject_with_risk(order.id(), &err);
                    return Err(err.into());
                }
                total
            }
            // Validated before the original was cancelled; the kill switch
            // and the modify-aware risk check ran there too, under the same
            // gate. Re-running them here could only fail the re-add after
            // the original is gone (#247).
            Admission::ReAdd { .. } => order.total_quantity()?,
        };

        // Reject a duplicate order id: an order with this id is already
        // resting on the book. Admitting it would overwrite the existing
        // order's entry in `order_locations` and orphan the live order (it
        // could no longer be cancelled or modified by id). This is an
        // `add_order`-specific structural check and deliberately does NOT
        // live in `validate_order_shape`: the validate-first atomic modify
        // (#98) runs that shared validator while the original, same-id
        // order is still resting, so a check there would false-reject every
        // modify. We also do NOT record an `OrderStatus::Rejected`
        // transition — the id belongs to a different, still-live order
        // whose tracked state must not be clobbered. The metric plus the
        // typed error (which the wire layer maps to
        // `RejectReason::DuplicateOrderId`) are sufficient.
        //
        // This is the pre-trade fast path, not the concurrency guard: the
        // check and the rest straddle the match walk, so two concurrent
        // `add_order` calls with the same *fresh* id can both pass here.
        // `rest_on_level` claims the location atomically (#288), so only
        // one of them rests; the other fails there with `DuplicateOrderId`,
        // possibly after trading. Serializing order ids is still the
        // ingress / sequencing layer's job.
        if self.order_locations.contains_key(&order.id()) || self.id_is_pending_stop(order.id()) {
            if admission.records_rejections() {
                crate::orderbook::metrics::record_reject(RejectReason::DuplicateOrderId);
            }
            return Err(OrderBookError::DuplicateOrderId {
                order_id: order.id(),
            }
            .into());
        }

        trace!(
            "Order book {}: Adding order {} at price {}",
            self.symbol,
            order.id(),
            order.price()
        );

        // Non-risk admission checks are owned by `validate_order_shape`
        // (the single source of truth shared with the validate-first
        // atomic modify path, #98). On the cold reject path we still
        // record the matching terminal state transition / metric here so
        // the direct (non-modify) `add_order` behavior is preserved
        // exactly.
        // `fok` is the fill-or-kill preflight measured by the feasibility
        // walk (#240): trade-id headroom already checked, buffer
        // reservation handed to the sweep below.
        let ShapeVerdict {
            fok,
            arithmetic_verified_price,
        } = match admission {
            Admission::Submit | Admission::ReplayRefusingResidual => {
                match self.validate_order_shape(&order) {
                    Ok(verdict) => verdict,
                    Err(err) => {
                        self.record_shape_rejection(&order, &err);
                        return Err(err.into());
                    }
                }
            }
            Admission::ReAdd { verdict, .. } => verdict,
        };

        // Residual-admission headroom pre-check (#211): a non-immediate
        // taker may rest its residual at a same-side level whose checked
        // aggregate counters cannot absorb it. pricelevel would reject
        // that admission — but only AFTER the sweep has emitted
        // irreversible trades. Reject up front instead. Gated on
        // `will_cross_market` (one best-price cache read): a non-crossing
        // add emits no trades, so its admission failure is already atomic
        // via the cleanup path below — only a crossing taker needs the
        // pre-trade guard, and it is about to pay for a full sweep anyway.
        // The check is conservative (it uses the full submitted total; the
        // actual residual is never larger) and best-effort under
        // concurrency — the authoritative, validated admission below still
        // guards the racy remainder, now with cleanup (#211).
        if !order.is_immediate() && self.will_cross_market(order.price().as_u128(), order.side()) {
            let same_side = match order.side() {
                Side::Buy => &self.bids,
                Side::Sell => &self.asks,
            };
            if let Some(entry) = same_side.get(&order.price().as_u128()) {
                // A counter-inconsistency error from the level's checked
                // aggregate is rejected with the same observable
                // lifecycle/metric surface as the overflow branch below —
                // both are pre-mutation, so the book is still pristine.
                let level_total = match entry.value().total_quantity() {
                    Ok(total) => total,
                    Err(err) => {
                        self.reject_admission(admission, &order, RejectReason::InvalidQuantity);
                        return Err(OrderBookError::PriceLevelError(err).into());
                    }
                };
                if level_total.checked_add(total).is_none() {
                    let err = OrderBookError::InvalidOperation {
                        message: format!(
                            "resting order {} would overflow the aggregate capacity of level {}",
                            order.id(),
                            order.price()
                        ),
                    };
                    self.reject_admission(admission, &order, RejectReason::InvalidQuantity);
                    return Err(err.into());
                }
            }
        }

        self.cache.invalidate();
        // Attempt to match the order immediately (with STP user_id propagation).
        // The outcome also carries whether STP cancelled the taker (#97) and
        // whether a per-level post-only guard refused to trade (#209).
        // Threading the taker's real kind gives post-only its structural
        // never-trades guarantee under every interleaving — the
        // `will_cross_market` precheck in `validate_order_shape` remains
        // only a fast-path reject.
        // Deliberately total over today's `TakerKind`: everything that is
        // not post-only — including MarketToLimit, which is MEANT to take
        // liquidity — sweeps as `Standard`. A future third `TakerKind`
        // variant must be routed here explicitly.
        let taker_kind = if order.is_post_only() {
            TakerKind::PostOnly
        } else {
            TakerKind::Standard
        };
        // An `Err` from the sweep is raised before any trade: an untouched
        // rejection (a failed post-only probe or fill-or-kill reservation)
        // or a self-trade prevention cancel with no fills. The one `Err`
        // raised after trades, an aborted prefix whose executed quantity
        // cannot be summed, is ruled out by the `MatchResult` invariant
        // (every fold is a checked subtraction from the `u64` budget).
        let MatchOutcome {
            result: match_result,
            taker_stp_cancelled,
            taker_post_only_rejected,
            aborted,
        } = self.match_order_with_user_outcome(
            order.id(),
            order.side(),
            total, // Use total quantity for matching
            Some(order.price().as_u128()),
            order.user_id(),
            taker_kind,
            fok.map_or(SweepReservation::NONE, |fok| fok.reservation),
            arithmetic_verified_price,
        )?;

        // #209: the sweep reached a crossable level with a post-only taker.
        // pricelevel structurally refused to trade (zero fills), so reject
        // exactly like the precheck would have — the race between precheck
        // and sweep can no longer make a post-only order take liquidity.
        if taker_post_only_rejected {
            self.reject_admission(admission, &order, RejectReason::PostOnlyWouldCross);
            return Err(self.price_crossing(&order).into());
        }

        // Emit trades BEFORE any early return below: the STP taker-cancel and
        // unfillable-IOC paths return `Err` after real (non-self) fills already
        // executed, and those fills must still reach the metrics and the trade
        // listener. The `TradeResult` is only constructed when someone consumes
        // it — the installed listener and/or an `add_order_with_result` caller —
        // so the plain `add_order` hot path skips the `MatchResult` clone.
        let trade_result = self.publish_trades(&match_result, want_result);

        // True (non-self) executed quantity. `remaining_quantity` only
        // decrements on real trades, so STP-prevented self-fills never count
        // toward it, and it never exceeds the budget the sweep was given.
        let remaining = match_result.remaining_quantity().as_u64();
        let Some(filled_qty) = total.checked_sub(remaining) else {
            return Err(self.remaining_exceeds_total(
                &order,
                total,
                remaining,
                &match_result,
                committed(trade_result),
                admission,
            ));
        };
        // `filled_quantity` in every order state below is cumulative: a
        // modify's re-add adds its fills to what the original had executed
        // (#247). A new submit starts from zero.
        let prior_filled = admission.prior_filled();
        let state_filled = cumulative_filled(order.id(), prior_filled, filled_qty);

        // #240: the sweep stopped at a failed price level. The committed
        // prefix was published above exactly like a partial fill; the
        // taker's terminal `Cancelled { MatchAborted }` state was recorded
        // by the sweep. The remainder must never rest, whatever the TIF.
        if let Some(err) = aborted {
            // The sweep recorded the re-add's own executed quantity; a
            // re-add of a partially filled original restates it
            // cumulatively (a second `Cancelled { MatchAborted }`
            // transition, only when the original had tracked fills).
            if prior_filled > 0 {
                self.track_state(
                    order.id(),
                    OrderStatus::Cancelled {
                        filled_quantity: state_filled,
                        reason: CancelReason::MatchAborted,
                    },
                );
            }
            return Err(AdmitFailure::after(
                err,
                committed(trade_result),
                filled_qty,
            ));
        }

        // If STP cancelled the taker, the residual must NOT rest — even though some
        // non-self fills already occurred at earlier levels. Record the terminal
        // SelfTradePrevention state with the true filled quantity and surface the STP
        // error (#97). The zero-fills case already returned this error from the match.
        if taker_stp_cancelled {
            self.track_state(
                order.id(),
                OrderStatus::Cancelled {
                    filled_quantity: state_filled,
                    reason: CancelReason::SelfTradePrevention,
                },
            );
            crate::orderbook::metrics::record_reject(RejectReason::SelfTradePrevention);
            return Err(AdmitFailure::after(
                OrderBookError::SelfTradePrevented {
                    mode: self.stp_mode,
                    taker_order_id: order.id(),
                    user_id: order.user_id(),
                },
                committed(trade_result),
                filled_qty,
            ));
        }

        // If the order was not fully filled, add the remainder to the book
        if remaining > 0 {
            if order.is_immediate() {
                // IOC/FOK orders should not have a resting part.
                // If FOK, it should have been fully filled or cancelled before this point.
                // If IOC, this is the remaining part that couldn't be filled, so we just drop it.
                self.track_state(
                    order.id(),
                    OrderStatus::Cancelled {
                        filled_quantity: filled_qty,
                        reason: CancelReason::InsufficientLiquidity,
                    },
                );
                crate::orderbook::metrics::record_reject(RejectReason::InsufficientLiquidity);
                // `requested` is the total the taker swept with and
                // `available` what it executed (#247: before 0.14.0 both
                // were read off the visible tranche and `available`
                // saturated at zero for a two-tranche taker).
                return Err(AdmitFailure::after(
                    OrderBookError::InsufficientLiquidity {
                        side: order.side(),
                        requested: total,
                        available: filled_qty,
                    },
                    committed(trade_result),
                    filled_qty,
                ));
            }

            // Rest the taker's residual. `remaining_quantity` is the TOTAL
            // unmatched quantity, so distribute it across the tranches with
            // `set_total_remaining` (#210): for a partially-filled iceberg
            // the submitted visible quantity acts as the display size and
            // the rest stays hidden — assigning the total to the visible
            // tranche (the old `set_quantity` semantics) manufactured
            // liquidity by keeping the original hidden tranche on top.
            if remaining < total {
                order.set_total_remaining(remaining);

                // #230: a reserve residual whose visible tranche the sweep
                // exhausted, with hidden left behind and no automatic
                // replenishment, must NOT rest. `reduce_reserve_to_total`
                // deliberately left the visible tranche empty because
                // `auto_replenish` is off, mirroring `pricelevel`'s removal
                // of a depleted non-auto maker from its level; resting here
                // would admit a zero-visible order (pricelevel's `add_order`
                // does not reject one) that displays nothing and can never
                // refill. The hidden remainder is discarded, exactly as the
                // maker path discards a stranded hidden tranche, and is
                // never counted as executed:
                // `submitted = executed + resting + discarded`.
                //
                // Scoped on purpose. An auto-replenishing reserve was
                // refreshed above. `validate_order_shape` rejects a
                // two-tranche order submitted with a zero visible tranche
                // and a non-empty hidden one, so every admitted iceberg
                // carries a positive display size and its residual keeps
                // `min(display, remaining) > 0` visible: no iceberg reaches
                // this branch. A reserve that did not trade at all never
                // enters this block and rests as submitted.
                let discarded_hidden = match &order {
                    OrderType::ReserveOrder {
                        visible_quantity,
                        hidden_quantity,
                        auto_replenish: false,
                        ..
                    } if visible_quantity.as_u64() == 0 => hidden_quantity.as_u64(),
                    _ => 0,
                };
                if discarded_hidden > 0 {
                    // INFO, not DEBUG: dropping resting quantity is a
                    // notable per-order event an operator wants in the
                    // default log, and it is bounded by the rate of
                    // exhausted non-auto reserve residuals.
                    tracing::info!(
                        path = "taker",
                        order_id = %order.id(),
                        executed_quantity = filled_qty,
                        discarded_hidden_quantity = discarded_hidden,
                        "reserve residual discarded: visible tranche exhausted without auto-replenishment"
                    );
                    crate::orderbook::metrics::record_reserve_hidden_discarded(discarded_hidden);
                    self.track_state(
                        order.id(),
                        OrderStatus::Filled {
                            filled_quantity: state_filled,
                        },
                    );
                    // Hand back a shape that matches the outcome: the order
                    // ended holding nothing. Leaving the hidden tranche in
                    // place would report `total_quantity() == hidden` for an
                    // order that rests nowhere and can never trade again.
                    if let OrderType::ReserveOrder {
                        hidden_quantity, ..
                    } = &mut order
                    {
                        *hidden_quantity = Quantity::new(0);
                    }
                    return Ok((Arc::new(order), trade_result));
                }
            }

            // Rest the remainder (#243 risk reservation first, then the
            // level). The pre-sweep headroom check above makes a level
            // refusal concurrent-only; if it still happens after the
            // sweep's irreversible trades, the remainder does not rest, the
            // taker ends in a terminal state and the error is surfaced
            // loudly (#211, #247).
            //
            // State: Open (no fills) or PartiallyFilled (some fills,
            // resting), counting a modified original's earlier fills.
            // `rest_on_level` records it before the order becomes
            // matchable, so a concurrent sweep's `Filled` lands after it
            // (#288).
            let resting_state = if state_filled > 0 {
                OrderStatus::PartiallyFilled {
                    original_quantity: cumulative_filled(order.id(), prior_filled, total),
                    filled_quantity: state_filled,
                }
            } else {
                OrderStatus::Open
            };
            // #291: a replay of a submit whose residual the live risk layer
            // refused after trading. The sweep above reproduced the live
            // trades; the residual must not rest, whatever the replay
            // book's (absent) risk configuration would say.
            if matches!(admission, Admission::ReplayRefusingResidual) {
                return Err(self.rest_failed(
                    &order,
                    RestFailure::Risk(replay_refused_residual(order.id())),
                    filled_qty,
                    committed(trade_result),
                    admission,
                ));
            }
            let unit_order_arc = match self.rest_on_level(&order, remaining, resting_state) {
                Ok(admitted) => admitted,
                Err(failure) => {
                    return Err(self.rest_failed(
                        &order,
                        failure,
                        filled_qty,
                        committed(trade_result),
                        admission,
                    ));
                }
            };

            // Convert back to generic type for return
            let generic_order = self.convert_from_unit_type(&unit_order_arc);
            Ok((Arc::new(generic_order), trade_result))
        } else {
            // The order was fully matched
            self.track_state(
                order.id(),
                OrderStatus::Filled {
                    filled_quantity: cumulative_filled(order.id(), prior_filled, total),
                },
            );
            Ok((Arc::new(order), trade_result))
        }
    }

    /// Rejects a trailing stop untouched on a book built without
    /// `special_orders` (#286): pending stops need the feature. Records
    /// `Rejected { StopOrdersUnsupported }` (code 23) and the reject metric.
    ///
    /// # Errors
    ///
    /// Always [`OrderBookError::StopOrdersUnsupported`].
    #[cfg(not(feature = "special_orders"))]
    #[cold]
    #[inline(never)]
    fn admit_trailing_stop(
        &self,
        order: OrderType<T>,
        records_rejections: bool,
    ) -> Result<Arc<OrderType<T>>, OrderBookError> {
        let order_id = order.id();
        if records_rejections {
            self.track_state(
                order_id,
                OrderStatus::Rejected {
                    reason: RejectReason::StopOrdersUnsupported,
                },
            );
        }
        tracing::debug!(
            symbol = %self.symbol,
            %order_id,
            "trailing stop rejected: the special_orders feature is disabled"
        );
        Err(OrderBookError::StopOrdersUnsupported { order_id })
    }

    /// Records the `Rejected { reason }` state and reject metric of an
    /// untouched admission failure, except for a modify's re-add, whose
    /// failure the modify resolves (#247).
    #[cold]
    #[inline(never)]
    fn reject_admission(&self, admission: Admission, order: &OrderType<T>, reason: RejectReason) {
        if admission.records_rejections() {
            self.track_state(order.id(), OrderStatus::Rejected { reason });
            crate::orderbook::metrics::record_reject(reason);
        }
    }

    /// The post-only `PriceCrossing` rejection of `order`, carrying the best
    /// opposite price, or `None` when that side emptied in the meantime
    /// (#247: it used to report `0`).
    #[cold]
    #[inline(never)]
    fn price_crossing(&self, order: &OrderType<T>) -> OrderBookError {
        OrderBookError::PriceCrossing {
            price: order.price().as_u128(),
            side: order.side(),
            opposite_price: match order.side() {
                Side::Buy => self.best_ask(),
                Side::Sell => self.best_bid(),
            },
        }
    }

    /// The sweep reported more remaining quantity than it was given, which
    /// the `MatchResult` invariant rules out. Handled rather than assumed
    /// (#247): nothing rests, the taker ends `Cancelled { RestFailed }` and
    /// the breach is logged at `ERROR`. The executed quantity is the sum of
    /// the committed trades; should that be unavailable too while trades
    /// exist, the taker's whole budget is reported, so a failed modify
    /// re-add never restores an original whose re-added order traded.
    #[cold]
    #[inline(never)]
    fn remaining_exceeds_total(
        &self,
        order: &OrderType<T>,
        total: u64,
        remaining: u64,
        match_result: &pricelevel::MatchResult,
        committed: Option<TradeResult>,
        admission: Admission,
    ) -> AdmitFailure {
        let traded = !match_result.trades().as_vec().is_empty();
        let executed = match match_result.executed_quantity() {
            Ok(executed) if executed.as_u64() > 0 => executed.as_u64(),
            _ if traded => total,
            _ => 0,
        };
        let err = OrderBookError::InvalidOperation {
            message: format!(
                "sweep of order {} left {remaining} of {total} units unmatched",
                order.id()
            ),
        };
        tracing::error!(
            order_id = %order.id(),
            total,
            remaining,
            "sweep remainder exceeds the taker's quantity; remainder not rested"
        );
        self.track_state(
            order.id(),
            OrderStatus::Cancelled {
                filled_quantity: cumulative_filled(order.id(), admission.prior_filled(), executed),
                reason: CancelReason::RestFailed,
            },
        );
        AdmitFailure::after(err, committed, executed)
    }

    /// Resolves a remainder the book could not rest after the sweep (#211,
    /// #243, #247): the taker ends in a terminal state and the failure is
    /// logged at `ERROR`.
    ///
    /// - A duplicate id (a same-id order won a concurrent admission race)
    ///   records no state: the id's state belongs to that live order, and
    ///   writing a terminal one would end it on the tracker. A loser that
    ///   traded is counted in the reject metric (#288).
    /// - Otherwise, a taker that traded ends
    ///   `Cancelled { filled_quantity, reason: RestFailed }`, with
    ///   `filled_quantity` cumulative for a modify's re-add (the original's
    ///   earlier fills plus the re-add's); one that did not is `Rejected`
    ///   under the error's reject code, except a modify's re-add, whose
    ///   untraded failure the modify resolves (restore or `RestFailed`).
    /// - A risk refusal after the taker traded is surfaced as
    ///   [`OrderBookError::RiskRejectedAfterTrades`] wrapping the risk
    ///   error (#291), so the pre-trade risk codes keep meaning that the
    ///   book was not touched. An untraded one keeps the risk error.
    #[cold]
    #[inline(never)]
    fn rest_failed(
        &self,
        order: &OrderType<T>,
        failure: RestFailure,
        filled_qty: u64,
        committed: Option<TradeResult>,
        admission: Admission,
    ) -> AdmitFailure {
        let err = match failure {
            // #291: the risk layer refused the residual after the sweep
            // traded. Surfaced as its own variant, so the pre-trade risk
            // rejections keep meaning "the book was not touched". A risk
            // map collision is the #288 duplicate race, reported as such.
            RestFailure::Risk(source)
                if filled_qty > 0 && !matches!(source, OrderBookError::DuplicateOrderId { .. }) =>
            {
                OrderBookError::RiskRejectedAfterTrades {
                    order_id: order.id(),
                    executed_quantity: filled_qty,
                    source: Box::new(source),
                }
            }
            failure => failure.into_error(),
        };
        if matches!(err, OrderBookError::DuplicateOrderId { .. }) {
            // #288: the id is owned by the admission that won the race, so
            // its state is not touched; a loser that traded is still a
            // reject after fills and is counted like the arms below.
            if filled_qty > 0 {
                crate::orderbook::metrics::record_reject(RejectReason::from(&err));
            }
        } else {
            if filled_qty > 0 {
                self.track_state(
                    order.id(),
                    OrderStatus::Cancelled {
                        filled_quantity: cumulative_filled(
                            order.id(),
                            admission.prior_filled(),
                            filled_qty,
                        ),
                        reason: CancelReason::RestFailed,
                    },
                );
                crate::orderbook::metrics::record_reject(RejectReason::from(&err));
            } else if admission.records_rejections() {
                self.reject_with_risk(order.id(), &err);
            }
        }
        if matches!(admission, Admission::ReplayRefusingResidual) {
            // Replay reproducing a journaled failure, not a new one.
            tracing::debug!(
                order_id = %order.id(),
                executed_quantity = filled_qty,
                error = %err,
                "replay refused the residual the journal recorded as refused"
            );
        } else {
            tracing::error!(
                order_id = %order.id(),
                price = order.price().as_u128(),
                executed_quantity = filled_qty,
                error = %err,
                "remainder could not be rested; taker ended, level cleaned up"
            );
        }
        AdmitFailure::after(err, committed, filled_qty)
    }

    /// Rests `order` with `quantity` units on its price level and indexes
    /// it, recording `state` as its order state.
    ///
    /// From the moment the level admits it, the order is matchable by a
    /// concurrent sweep on the shared submit gate, and cancellable. Indexing
    /// it only after the admission (before #288) let a sweep drain an order
    /// whose indices did not exist yet; they were then inserted for an
    /// order that no longer rested. So (#288):
    ///
    /// - published **before** the admission: the #243 risk reservation,
    ///   the location, the user-index entry and `state`. The location is
    ///   claimed atomically and acts as the id's ownership token: a same-id
    ///   order that won a concurrent admission race is refused here
    ///   instead of being overwritten, and every remover (the sweep's
    ///   drain, a cancel, this method's own rollback) untracks the user
    ///   entry and releases the risk entry **before** it releases the
    ///   location. A user-index entry for an id therefore only ever exists
    ///   while one admission owns that id, so a reuse of the id can never
    ///   see, or have removed, a previous admission's entry. A sweep that
    ///   consumes the order finds all of them and records its `Filled`
    ///   after `state`;
    /// - published **after** it: special-order tracking (as before #288; a
    ///   repricer releases a tracked id `get_order` cannot find only while
    ///   no admission owns the id (#291), so this registration is never
    ///   lost to a pass that saw the id's previous order gone, and one that
    ///   lands after a concurrent cancel is cleaned up by the next pass), the
    ///   strandable-maker count (a strandable maker always rests under the
    ///   exclusive gate, so no sweep overlaps it), the level event and the
    ///   depth gauges.
    ///
    /// # Errors
    ///
    /// [`RestFailure::Risk`] when the risk reservation is refused and
    /// [`RestFailure::Duplicate`] when another admission owns the id;
    /// neither records `state` or touches the book. [`RestFailure::Level`]
    /// when the level refuses the order: the user-index entry and the
    /// reservation are withdrawn, then the location is released, and a
    /// level this call left empty is removed, so no phantom level or index
    /// is exposed. `state` was already recorded; the caller records the
    /// terminal state over it.
    ///
    /// # Unwinding (#294)
    ///
    /// Caller code runs between the location claim and the admission
    /// (`track_state`'s `Clock` and metrics recorder, `T::default()`). If
    /// it panics, an [`UnrestedClaim`] drop guard withdraws what was
    /// published (the recorded state, the user index and reservation, then
    /// the location) and removes a level left empty, so no ghost index
    /// survives the unwind.
    fn rest_on_level(
        &self,
        order: &OrderType<T>,
        quantity: u64,
        state: OrderStatus,
    ) -> Result<Arc<OrderType<()>>, RestFailure> {
        let order_id = order.id();
        let price = order.price().as_u128();
        let side = order.side();
        let price_levels = match side {
            Side::Buy => &self.bids,
            Side::Sell => &self.asks,
        };

        // Pre-trade risk hook (#243): reserve the resting remainder's
        // contribution to the per-account counters BEFORE the order is
        // placed on its level, so a reservation that cannot be
        // represented (only reachable when concurrent admissions on the
        // same account raced past the pre-trade check) rejects the
        // remainder instead of resting it untracked. Checked and
        // all-or-nothing; released below if the placement fails. No-op
        // when no `RiskConfig` is installed.
        #[cfg(test)]
        if let Some(error) = self
            .rest_risk_fault_hook
            .as_ref()
            .and_then(|hook| hook(order_id))
        {
            return Err(RestFailure::Risk(error));
        }
        let risk_reservation = self
            .risk_state
            .on_admission(order_id, order.user_id(), price, quantity)
            .map_err(RestFailure::Risk)?;

        // The user index hands out the sequence the location carries, so it
        // is updated first. Nothing reads it for this id before the claim
        // below: `cancel_orders_by_user` and its stale-id purge run under
        // the exclusive submit gate, and every removal is keyed by the
        // sequence of a location that already exists.
        let user_seq = self.track_user_order(order.user_id(), order_id);
        // #288: claim the id before the order becomes matchable. The map's
        // shard guard is dropped at the end of the `match`, before anything
        // else is locked.
        let claimed = match self.order_locations.entry(order_id) {
            dashmap::Entry::Occupied(_) => false,
            dashmap::Entry::Vacant(slot) => {
                slot.insert(OrderLocation::new(price, side, order.user_id(), user_seq));
                true
            }
        };
        if !claimed {
            self.untrack_user_order(order.user_id(), user_seq, &order_id);
            self.risk_state.release_reservation(risk_reservation);
            return Err(RestFailure::Duplicate(order_id));
        }
        // #294: from here to the admission, an unwind (caller code: the
        // tracker's `Clock`, metrics, `T::default()`) withdraws the claim.
        // Declared before `stripe` so the stripe is released first.
        let mut claim = UnrestedClaim {
            book: self,
            order,
            price,
            side,
            user_seq,
            reservation: Some(risk_reservation),
            state: None,
        };
        self.track_state(order_id, state.clone());
        claim.state = Some(state);

        // #247: admission into the level runs under the shared side of the
        // price's stripe, so a concurrent removal of the level (it was
        // empty a moment ago) either completes before `get_or_insert_with`
        // (a fresh level is created) or waits and then sees this order and
        // leaves the level in place. Concurrent admissions do not exclude
        // each other. Released before the listener runs.
        // PR #297 review: the unit conversion (`T::default()`, caller
        // code) runs before the stripe is taken, so no caller code of ours
        // runs under the stripe. The claim guard covers it.
        // #259: `get_or_insert_with`, not `get_or_insert`: the level is only
        // built when the price has none. An eager `PriceLevel::new` built
        // and dropped a whole level (its order map's shard array is about
        // 16 KB) on every add to an existing level.
        let unit_order = self.convert_to_unit_type(order);
        let stripe = self.lock_level(price);
        let price_level =
            price_levels.get_or_insert_with(price, || Arc::new(PriceLevel::new(price)));
        let level = price_level.value();

        // Admission into the level is validated upstream since pricelevel
        // 0.9 (duplicate id, counter capacity). If it fails, remove the level
        // when it is left empty — `best_bid` / `best_ask`, the cache, and
        // the depth gauges must never expose a phantom level (#211).
        let admission = self.admit_to_level(level, unit_order);
        let reservation = claim.disarm();
        drop(claim);
        let admitted = match admission {
            Ok(admitted) => admitted,
            Err(err) => {
                drop(stripe);
                if let Some(risk_reservation) = reservation {
                    self.withdraw_unrested(order, price, side, user_seq, risk_reservation);
                }
                self.remove_level_if_empty(side, price);
                self.cache.invalidate();
                self.record_depth_metric();
                return Err(RestFailure::Level(err));
            }
        };
        drop(stripe);
        #[cfg(test)]
        if let Some(hook) = self.rest_interleave_hook.as_ref() {
            hook(order_id);
        }
        // #230: this is the single point where the book rests an order on a
        // level (the untouched submit, the partially-filled residual and a
        // restored modify original), so flagging here covers the whole
        // admission path. Enables the sweep's strandable-maker scan for this
        // book from now on.
        self.note_rested_order(admitted.as_ref());
        self.emit_level_changed(side, level);

        // Refresh the depth gauges. The level may be brand-new
        // (`get_or_insert` created it) or pre-existing — either way the
        // gauge reflects current state. No-op when the `metrics` feature is
        // disabled.
        self.record_depth_metric();

        // Register special orders for re-pricing tracking, now that
        // `get_order` finds the order (see the method docs).
        #[cfg(feature = "special_orders")]
        if let OrderType::PeggedOrder { id, .. } = order {
            self.special_order_tracker.register_pegged_order(*id);
        }
        Ok(admitted)
    }

    /// Withdraws what [`Self::rest_on_level`] published before an
    /// admission its level then refused (#288): the user-index entry and
    /// the risk reservation while this admission still owns the id, then
    /// the location that owns it. The order never rested, so no sweep or
    /// cancel can have removed any of them, and no other admission can
    /// have claimed the id meanwhile.
    #[cold]
    #[inline(never)]
    fn withdraw_unrested(
        &self,
        order: &OrderType<T>,
        price: u128,
        side: Side,
        user_seq: u64,
        risk_reservation: crate::orderbook::risk::RiskReservation,
    ) {
        let order_id = order.id();
        self.untrack_user_order(order.user_id(), user_seq, &order_id);
        // Keyed by the reservation's generation, so this can never
        // release a same-id order's entry (#243 review).
        self.risk_state.release_reservation(risk_reservation);
        self.order_locations.remove_if(&order_id, |_, location| {
            *location == OrderLocation::new(price, side, order.user_id(), user_seq)
        });
    }

    /// Adds `order` to `level`. In `cfg(test)` builds the `rest_fault_hook`
    /// can make the admission fail with nothing mutated (#247).
    #[inline]
    fn admit_to_level(
        &self,
        level: &PriceLevel,
        order: OrderType<()>,
    ) -> Result<Arc<OrderType<()>>, PriceLevelError> {
        #[cfg(test)]
        if let Some(error) = self
            .rest_fault_hook
            .as_ref()
            .and_then(|hook| hook(order.id()))
        {
            return Err(error);
        }
        level.add_order(order)
    }

    /// The validate-first cancel-then-add behind `UpdatePrice`,
    /// `UpdatePriceAndQuantity` and `Replace` (#98, #168, #230, #244, #247).
    ///
    /// Every admission check runs on `new_order` **before** the original is
    /// cancelled, so a rejection leaves it resting untouched: the shared
    /// shape validator (including the #240 trade-id and #244 arithmetic
    /// preflights), the modify-aware risk check, the #168 STP self-cross
    /// dry run and the #230 reserve-residual dry run. These checks are pure
    /// functions of the new order plus the *opposite* book side, so
    /// evaluating them while the same-side original still rests yields the
    /// same verdict as after the cancel. The re-add then takes that verdict
    /// as its admission. A re-add that still fails is resolved by
    /// [`Self::resolve_failed_readd`].
    ///
    /// `snapshot` is the order as the caller read it to build `new_order`.
    /// Under the shared submit gate a concurrent taker can fill part of it
    /// before the cancel lands, so the re-add is built from the order the
    /// cancel **returned**, never from the snapshot, and quantity is never
    /// created (#247):
    ///
    /// - [`ReAddQuantity::FollowsRemainder`] (`UpdatePrice`): the re-add
    ///   carries the cancelled remainder at the new price. The pre-cancel
    ///   verdict still holds: every check is monotone in the quantity, and
    ///   the remainder is never larger than what was validated (a
    ///   concurrent size-up takes the same path and rolls back instead).
    /// - [`ReAddQuantity::Explicit`] (`UpdatePriceAndQuantity`, `Replace`):
    ///   the caller named a new quantity against a state that no longer
    ///   exists, so the modify is not applied. The remainder is restored
    ///   and the call returns [`OrderBookError::ModifyRolledBack`] whose
    ///   source is [`OrderBookError::OrderChangedDuringModify`].
    ///
    /// Returns `Ok(None)` when the cancel finds no order to remove (it was
    /// filled or cancelled concurrently): nothing is re-added, so a
    /// finished order is never resurrected.
    pub(super) fn cancel_then_readd(
        &self,
        order_id: Id,
        snapshot: &OrderType<T>,
        new_order: OrderType<T>,
        quantity: ReAddQuantity,
    ) -> Result<Option<Arc<OrderType<T>>>, OrderBookError> {
        // The gate mode was chosen once at the boundary by
        // `modify_needs_exclusive_gate` and is never upgraded here, so the
        // re-add must never be a fill-or-kill (whose all-or-nothing window
        // always requires the exclusive gate). Unreachable today — an FOK
        // never rests, so it can never be modified — but rejected with a
        // typed error before the cancel (#209, #247).
        if new_order.is_fill_or_kill() {
            return Err(fill_or_kill_readd(order_id));
        }
        let verdict = self.validate_order_shape(&new_order)?;
        self.check_risk_modify_admission(
            order_id,
            new_order.user_id(),
            new_order.price().as_u128(),
            new_order.total_quantity()?,
        )?;

        // #168: reject a re-price that would self-cross the same user's
        // opposite-side liquidity under CancelTaker / CancelBoth BEFORE
        // cancelling the original, so the original survives.
        self.check_modify_stp_self_cross(&new_order)?;

        // #230: reject a re-price whose re-add would exhaust a
        // non-auto-replenishing reserve's visible tranche and discard its
        // hidden remainder, which would destroy the order after the
        // original was already cancelled.
        self.check_modify_reserve_residual(&new_order)?;

        #[cfg(test)]
        self.fire_modify_hook(order_id, ModifyPhase::BeforeCancel);

        // All checks passed: cancel the original and re-add the updated
        // order. Ungated inner variants: `update_order` already holds the
        // submit gate (#209 / #225); the public wrappers would re-acquire it
        // (std RwLock is not reentrant).
        let prior_status = self.order_status(order_id);
        let Some(original) =
            self.cancel_order_with_reason(order_id, CancelReason::UserRequested)?
        else {
            return Ok(None);
        };

        #[cfg(test)]
        self.fire_modify_hook(order_id, ModifyPhase::AfterCancel);

        // What the tracker knew the order had executed, plus any fill that
        // landed between the caller's read and the cancel (a smaller
        // cancelled remainder than the snapshot). A larger remainder is a
        // concurrent size-up, not a fill.
        let snapshot_total = snapshot.total_quantity()?;
        let original_total = original.total_quantity()?;
        let tracked_filled = prior_status
            .as_ref()
            .map_or(0, OrderStatus::filled_quantity);
        let prior_filled = match snapshot_total.checked_sub(original_total) {
            Some(raced_fills) => cumulative_filled(order_id, tracked_filled, raced_fills),
            // A larger cancelled remainder: a concurrent size-up, no fill.
            None => tracked_filled,
        };
        let changed = !same_quantities(snapshot, original.as_ref());

        let new_order = match (changed, quantity) {
            (false, _) => new_order,
            (true, ReAddQuantity::FollowsRemainder) => {
                let mut remainder = (*original).clone();
                set_order_price(&mut remainder, new_order.price());
                remainder
            }
            (true, ReAddQuantity::Explicit) => {
                let changed = OrderBookError::OrderChangedDuringModify {
                    order_id,
                    read_quantity: snapshot_total,
                    cancelled_quantity: original_total,
                };
                return Err(self.resolve_failed_readd(
                    order_id,
                    original.as_ref(),
                    prior_status,
                    prior_filled,
                    AdmitFailure::from(changed),
                ));
            }
        };
        match self.add_order_inner(
            new_order,
            false,
            false,
            Admission::ReAdd {
                verdict,
                prior_filled,
            },
        ) {
            Ok((order, _)) => Ok(Some(order)),
            Err(failure) => Err(self.resolve_failed_readd(
                order_id,
                original.as_ref(),
                prior_status,
                prior_filled,
                failure,
            )),
        }
    }

    /// Test-only interleaving point of a cancel-then-add modify (#247).
    #[cfg(test)]
    fn fire_modify_hook(&self, order_id: Id, phase: ModifyPhase) {
        if let Some(hook) = self.modify_interleave_hook.as_ref() {
            hook(self, order_id, phase);
        }
    }

    /// Resolves a modify re-add that failed after the original was
    /// cancelled (#247); see the "Failed re-adds" section of
    /// [`Self::update_order`].
    ///
    /// - The re-add traded: the original cannot be restored. A #240 abort is
    ///   returned as is (`MatchAborted`); any other failure becomes
    ///   [`OrderBookError::ModifyOrderLost`] with the re-add's executed
    ///   quantity. The terminal state `add_order_inner` recorded is already
    ///   cumulative (`prior_filled` plus the re-add's fills).
    /// - Nothing traded: the original is restored and the result is
    ///   [`OrderBookError::ModifyRolledBack`]. Should the restore fail too,
    ///   the order is gone ([`OrderBookError::ModifyOrderLost`] with
    ///   `restore_error`) and ends
    ///   `Cancelled { filled_quantity: prior_filled, RestFailed }`, unless a
    ///   live order now owns its id.
    #[cold]
    #[inline(never)]
    fn resolve_failed_readd(
        &self,
        order_id: Id,
        original: &OrderType<T>,
        prior_status: Option<OrderStatus>,
        prior_filled: u64,
        failure: AdmitFailure,
    ) -> OrderBookError {
        let traded = failure.traded();
        let AdmitFailure {
            error: source,
            executed_quantity,
            ..
        } = failure;
        if traded {
            if matches!(source, OrderBookError::MatchAborted { .. }) {
                return source;
            }
            // `add_order_inner` recorded the terminal
            // `Cancelled { RestFailed }` (cumulative), except for a
            // `DuplicateOrderId`: the id now belongs to the live order that
            // won the admission race, so its state is left to that order
            // (#288). The re-add's fills are in the reject metric and in
            // `executed_quantity`.
            tracing::error!(
                symbol = %self.symbol,
                %order_id,
                executed_quantity,
                error = %source,
                "modify re-add failed after trading; the remainder did not rest and the order is gone"
            );
            return OrderBookError::ModifyOrderLost {
                order_id,
                executed_quantity,
                source: Box::new(source),
                restore_error: None,
            };
        }
        match self.restore_cancelled_order(original, prior_status.clone()) {
            Ok(()) => {
                tracing::warn!(
                    symbol = %self.symbol,
                    %order_id,
                    error = %source,
                    "modify re-add failed; original order restored at the back of its level"
                );
                OrderBookError::ModifyRolledBack {
                    order_id,
                    source: Box::new(source),
                }
            }
            Err(restore_error) => {
                if !matches!(restore_error, OrderBookError::DuplicateOrderId { .. }) {
                    self.track_state(
                        order_id,
                        OrderStatus::Cancelled {
                            filled_quantity: prior_filled,
                            reason: CancelReason::RestFailed,
                        },
                    );
                }
                tracing::error!(
                    symbol = %self.symbol,
                    %order_id,
                    error = %source,
                    restore_error = %restore_error,
                    "modify re-add failed and the original could not be restored; the order is gone"
                );
                OrderBookError::ModifyOrderLost {
                    order_id,
                    executed_quantity: 0,
                    source: Box::new(source),
                    restore_error: Some(Box::new(restore_error)),
                }
            }
        }
    }

    /// Re-rests a modify's cancelled `original` (#247): same id, price,
    /// quantity and timestamp, at the back of its level's queue, with its
    /// order state set back to `prior_status` (`Open` when it had none).
    ///
    /// The restore never matches: it only rests. Under the shared submit
    /// gate an opposite order can arrive between the cancel and the
    /// restore at a price the original now crosses or locks; resting there
    /// would have the engine itself create a crossed or locked book, so
    /// the restore is refused instead.
    ///
    /// # Errors
    ///
    /// [`OrderBookError::DuplicateOrderId`] when another order took the id
    /// meanwhile; [`OrderBookError::PriceCrossing`] when the original's
    /// price now crosses or locks the best opposite price; otherwise the
    /// risk or level error that refused the order. Nothing is left indexed
    /// on failure.
    fn restore_cancelled_order(
        &self,
        original: &OrderType<T>,
        prior_status: Option<OrderStatus>,
    ) -> Result<(), OrderBookError> {
        let order_id = original.id();
        if self.order_locations.contains_key(&order_id) {
            return Err(OrderBookError::DuplicateOrderId { order_id });
        }
        if self.will_cross_market(original.price().as_u128(), original.side()) {
            return Err(self.price_crossing(original));
        }
        let quantity = original.total_quantity()?;
        self.rest_on_level(
            original,
            quantity,
            prior_status.unwrap_or(OrderStatus::Open),
        )
        .map_err(RestFailure::into_error)?;
        self.cache.invalidate();
        Ok(())
    }
}

/// How [`OrderBook::add_order_inner`] admits an order (#247).
#[derive(Debug, Clone, Copy)]
pub(super) enum Admission {
    /// A new submit: every admission check runs.
    Submit,
    /// A replayed submit whose live execution traded and then had its
    /// residual refused by the risk layer (#291): every admission check
    /// runs as for [`Self::Submit`], and a residual that would rest is
    /// refused instead, as the journal recorded. Only
    /// [`OrderBook::replay_add_order_refusing_residual`] uses it.
    ReplayRefusingResidual,
    /// The re-add of a validate-first modify: the kill-switch, risk and
    /// shape checks ran before the original was cancelled, and their
    /// verdict is taken as is. Rejections are not recorded as order state
    /// or reject metrics: the modify resolves them (#247).
    ReAdd {
        /// The pre-cancel admission verdict.
        verdict: ShapeVerdict,
        /// What the original had executed before the modify, in quantity
        /// units; every order state the re-add records adds its own fills
        /// to it, so `filled_quantity` stays cumulative (#247).
        prior_filled: u64,
    },
}

impl Admission {
    /// Whether rejections are recorded as order state and reject metrics.
    #[inline]
    fn records_rejections(self) -> bool {
        matches!(self, Self::Submit | Self::ReplayRefusingResidual)
    }

    /// Fills the order executed before this admission.
    #[inline]
    fn prior_filled(self) -> u64 {
        match self {
            Self::Submit | Self::ReplayRefusingResidual => 0,
            Self::ReAdd { prior_filled, .. } => prior_filled,
        }
    }
}

/// How a cancel-then-add modify sizes its re-add when the order changed
/// between the caller's read and the cancel (#247).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ReAddQuantity {
    /// `UpdatePrice`: the re-add carries whatever the cancel removed.
    FollowsRemainder,
    /// `UpdatePriceAndQuantity` / `Replace`: the caller named the quantity;
    /// a changed order rolls the modify back.
    Explicit,
}

/// Where a cancel-then-add modify is when the test-only
/// `modify_interleave_hook` fires (#247).
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ModifyPhase {
    /// Every pre-cancel check passed; the original still rests.
    BeforeCancel,
    /// The original was cancelled; the re-add has not run.
    AfterCancel,
}

/// Whether two views of an order carry the same tranches.
fn same_quantities<T>(a: &OrderType<T>, b: &OrderType<T>) -> bool {
    a.quantity() == b.quantity() && a.checked_total_quantity() == b.checked_total_quantity()
}

/// Sets `order`'s limit price.
fn set_order_price<T>(order: &mut OrderType<T>, new_price: pricelevel::Price) {
    match order {
        OrderType::Standard { price, .. }
        | OrderType::IcebergOrder { price, .. }
        | OrderType::PostOnly { price, .. }
        | OrderType::TrailingStop { price, .. }
        | OrderType::PeggedOrder { price, .. }
        | OrderType::MarketToLimit { price, .. }
        | OrderType::ReserveOrder { price, .. } => *price = new_price,
    }
}

/// `prior + more`, the cumulative executed quantity of an order, in
/// quantity units. Both are bounded by quantities the order held, so the
/// sum fits; a breach is logged and the larger operand kept rather than
/// wrapped (#247).
fn cumulative_filled(order_id: Id, prior: u64, more: u64) -> u64 {
    match prior.checked_add(more) {
        Some(total) => total,
        None => {
            tracing::error!(%order_id, prior, more, "cumulative filled quantity overflows u64");
            prior.max(more)
        }
    }
}

/// A failed [`OrderBook::add_order_inner`] (#247): the submit failure plus
/// whether the taker traded before it failed.
#[derive(Debug)]
pub(super) struct AdmitFailure {
    /// The typed error the submit APIs return.
    error: OrderBookError,
    /// The committed trades, when the caller asked for them. Kept flat
    /// rather than as a nested [`SubmitFailure`] so the failure stays below
    /// clippy's large-error threshold.
    committed: Option<Box<TradeResult>>,
    /// Quantity the taker executed before the failure, in quantity units.
    /// Positive exactly when the sweep committed a trade (every trade
    /// carries a positive quantity), which is what decides whether a failed
    /// modify re-add may restore the original.
    executed_quantity: u64,
}

impl AdmitFailure {
    /// A failure raised after the sweep.
    #[inline]
    fn after(
        error: OrderBookError,
        committed: Option<TradeResult>,
        executed_quantity: u64,
    ) -> Self {
        Self {
            error,
            committed: committed.map(Box::new),
            executed_quantity,
        }
    }

    /// Whether the taker traded before the failure.
    #[inline]
    fn traded(&self) -> bool {
        self.executed_quantity > 0
    }

    /// The submit API's failure.
    #[inline]
    pub(super) fn into_submit(self) -> SubmitFailure {
        SubmitFailure {
            error: self.error,
            committed: self.committed,
        }
    }
}

impl From<OrderBookError> for AdmitFailure {
    /// A failure raised before any trade.
    #[inline]
    fn from(error: OrderBookError) -> Self {
        Self {
            error,
            committed: None,
            executed_quantity: 0,
        }
    }
}

/// Why [`OrderBook::rest_on_level`] could not rest an order (#247).
#[derive(Debug)]
enum RestFailure {
    /// The per-account risk reservation was refused; nothing was touched.
    Risk(OrderBookError),
    /// Another live order holds the id (#288: the location is claimed
    /// atomically); the reservation was released and nothing was touched.
    Duplicate(Id),
    /// The price level refused the order; the reservation was released.
    Level(PriceLevelError),
}

impl RestFailure {
    /// The typed error to surface.
    #[inline]
    fn into_error(self) -> OrderBookError {
        match self {
            Self::Risk(error) => error,
            Self::Duplicate(order_id) => OrderBookError::DuplicateOrderId { order_id },
            Self::Level(error) => OrderBookError::PriceLevelError(error),
        }
    }
}

/// What a single-order cancel removed from its level (#248).
enum RemovedOrder {
    /// The level removed the order and returned its body.
    Clean(Arc<OrderType<()>>),
    /// The level removed the order and then reported this failure.
    Faulted(PriceLevelError),
}
