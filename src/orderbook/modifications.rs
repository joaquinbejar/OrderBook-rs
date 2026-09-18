use crate::orderbook::book::OrderBook;
use crate::orderbook::book_change_event::PriceLevelChangedEvent;
use crate::orderbook::error::OrderBookError;
use crate::orderbook::matching::MatchOutcome;
use crate::orderbook::order_state::{CancelReason, OrderStatus};
use crate::orderbook::reject_reason::RejectReason;
use crate::orderbook::trade::TradeResult;
use either::Either;
use pricelevel::{
    DEFAULT_RESERVE_REPLENISH_AMOUNT, Id, OrderType, OrderUpdate, PriceLevel, Quantity, Side,
    TakerKind,
};
use std::sync::Arc;
use tracing::trace;

/// A trait to abstract quantity access and modification for different order types.
pub trait OrderQuantity<T = ()> {
    /// Returns the primary quantity used for display or simple matching.
    /// For iceberg orders, this is the visible quantity.
    fn quantity(&self) -> u64;

    /// Returns the total quantity of the order (e.g., visible + hidden).
    ///
    /// Saturates on `visible + hidden` overflow for the two-tranche kinds.
    /// Every order admitted through `add_order` / the submit APIs / the
    /// validate-first modify path has already passed
    /// [`Self::checked_total_quantity`] validation (#210), so the
    /// saturating arm is unreachable for those book-resident orders; use
    /// the checked variant at admission boundaries. Snapshot restore
    /// trusts its (checksummed) source and does not re-validate totals —
    /// consistent with its existing saturating risk rebuild.
    fn total_quantity(&self) -> u64;

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
    fn total_quantity(&self) -> u64 {
        match self {
            OrderType::Standard { quantity, .. } => quantity.as_u64(),
            OrderType::IcebergOrder {
                visible_quantity,
                hidden_quantity,
                ..
            } => visible_quantity
                .as_u64()
                .saturating_add(hidden_quantity.as_u64()),
            OrderType::PostOnly { quantity, .. } => quantity.as_u64(),
            OrderType::TrailingStop { quantity, .. } => quantity.as_u64(),
            OrderType::PeggedOrder { quantity, .. } => quantity.as_u64(),
            OrderType::MarketToLimit { quantity, .. } => quantity.as_u64(),
            OrderType::ReserveOrder {
                visible_quantity,
                hidden_quantity,
                ..
            } => visible_quantity
                .as_u64()
                .saturating_add(hidden_quantity.as_u64()),
        }
    }

    #[inline]
    fn checked_total_quantity(&self) -> Option<u64> {
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
                .checked_add(hidden_quantity.as_u64()),
            _ => Some(self.total_quantity()),
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
                let display = visible_quantity.as_u64();
                let visible = display.min(remaining_total);
                *visible_quantity = Quantity::new(visible);
                *hidden_quantity = Quantity::new(remaining_total - visible);
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
        let original_total = visible_quantity
            .as_u64()
            .saturating_add(hidden_quantity.as_u64());
        let amount_to_reduce = original_total.saturating_sub(new_total_quantity);

        let vis = visible_quantity.as_u64();
        let filled_from_visible = amount_to_reduce.min(vis);
        *visible_quantity = Quantity::new(vis.saturating_sub(filled_from_visible));

        let remaining_to_reduce = amount_to_reduce - filled_from_visible;
        // Hidden may only ever DECREASE here, and only by a lot-aligned
        // amount (the executed remainder is lot-rounded by the sweep): the
        // #226 lot-size admission check validates the replenishment transfer
        // once, against the hidden tranche as submitted, and
        // `min(amount, hidden)` stays lot-aligned only while hidden stays
        // lot-aligned and never grows.
        *hidden_quantity =
            Quantity::new(hidden_quantity.as_u64().saturating_sub(remaining_to_reduce));

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
            // Cannot saturate: `visible + refresh <= visible + hidden`, and
            // admission rejects a two-tranche order whose `visible + hidden`
            // overflows `u64` (#210). Both operands are lot-aligned, so the
            // refreshed tranche is too.
            *visible_quantity = Quantity::new(visible_quantity.as_u64().saturating_add(refresh));
            // Decrease only, by the validated lot-aligned transfer, for the same reason as above.
            *hidden_quantity = Quantity::new(hidden_quantity.as_u64().saturating_sub(refresh));
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
) -> OrderBookError {
    OrderBookError::ReserveResidualWouldBeDiscarded {
        order_id,
        visible_quantity,
        crossable_quantity,
        hidden_quantity,
        // The residual the re-add would leave unmatched and then abandon.
        // Cannot underflow: the caller only builds this error inside the
        // band `crossable < visible + hidden`.
        discarded_quantity: visible_quantity
            .saturating_add(hidden_quantity)
            .saturating_sub(crossable_quantity),
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
    /// [`PriceLevelError`](pricelevel::PriceLevelError) from applying the
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
        // this path, so pegged / trailing-stop re-prices are covered too.
        let _gate = self.acquire_coherent_submit_gate(self.modify_needs_exclusive_gate(&update));
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
        match update {
            OrderUpdate::UpdatePrice {
                order_id,
                new_price,
            } => {
                // Get the order location without locking
                let location = self.order_locations.get(&order_id).map(|val| *val);

                if let Some((old_price, _)) = location {
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

                    // Validate-first atomic modify (#98): validate the new
                    // order's shape and run the modify-aware risk check
                    // *before* removing the original. On any rejection we
                    // return the typed error and the original order is
                    // never cancelled — no book mutation, no events, no
                    // trades. These checks are pure functions of the new
                    // order + the opposite book side, so evaluating them
                    // while the same-side original still rests yields the
                    // same verdict as after cancel.
                    self.validate_order_shape(&new_order)?;
                    self.check_risk_modify_admission(
                        order_id,
                        new_order.user_id(),
                        new_order.price().as_u128(),
                        new_order.total_quantity(),
                    )?;

                    // #168: reject a re-price that would self-cross the same
                    // user's opposite-side liquidity under CancelTaker/CancelBoth
                    // BEFORE cancelling the original, so the original survives.
                    self.check_modify_stp_self_cross(&new_order)?;

                    // #230: reject a re-price whose re-add would exhaust a
                    // non-auto-replenishing reserve's visible tranche and
                    // discard its hidden remainder, which would destroy the
                    // order after the original was already cancelled.
                    self.check_modify_reserve_residual(&new_order)?;

                    // All checks passed: cancel the original and add the
                    // updated order. `add_order` re-runs its own checks;
                    // post-cancel the account count is restored so its risk
                    // check passes — consistent with the pre-guard.
                    // Ungated inner variants: `update_order` already holds
                    // the submit gate (#209 / #225); the public wrappers
                    // would re-acquire it (std RwLock is not reentrant).
                    // The gate mode was chosen once at the boundary by
                    // `modify_needs_exclusive_gate` — exclusive whenever STP
                    // is engaged — and it is never upgraded here, so the
                    // re-add must never be a fill-or-kill (whose
                    // all-or-nothing window always requires the exclusive
                    // gate, including on an `STPMode::None` book).
                    // Unreachable today — an FOK never rests, so it can
                    // never be modified — but enforced so a future TIF
                    // change cannot silently void the #209 guarantee.
                    debug_assert!(
                        !new_order.is_fill_or_kill(),
                        "a resting order can never carry FOK; the re-add cannot upgrade the gate"
                    );
                    self.cancel_order_with_reason(order_id, CancelReason::UserRequested)?;
                    let result = self.add_order_inner(new_order, false)?.0;
                    Ok(Some(result))
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
                // removal `OrderBook::cancel_order` performs (not the
                // `OrderUpdate::Cancel` arm below, which is a separate
                // implementation). This branch runs no validator at all — a
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

                if let Some((price, side)) = location {
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
                        self.check_risk_modify_admission(
                            order_id,
                            projected.user_id(),
                            price,
                            projected.total_quantity(),
                        )?;

                        let update = OrderUpdate::UpdateQuantity {
                            order_id,
                            new_quantity,
                        };

                        // Propagate upstream validation / counter errors
                        // (#211): `Ok(None)` is reserved for a genuinely
                        // absent order, never an error swallowed silently.
                        match price_level.update_order(update) {
                            Ok(Some(order)) => {
                                // Keep the per-account risk counters in
                                // lockstep with the applied update.
                                self.risk_state.on_quantity_update(
                                    order_id,
                                    OrderQuantity::<()>::total_quantity(order.as_ref()),
                                );
                                // notify price level changes
                                if let Some(ref listener) = self.price_level_changed_listener {
                                    let engine_seq = self.next_engine_seq();
                                    listener(PriceLevelChangedEvent {
                                        side,
                                        price: price_level.price(),
                                        quantity: price_level.visible_quantity(),
                                        engine_seq,
                                    })
                                }
                                result = Some(Arc::new(self.convert_from_unit_type(&order)));
                            }
                            Ok(None) => {}
                            Err(err) => {
                                return Err(OrderBookError::PriceLevelError(err));
                            }
                        }

                        is_empty = price_level.order_count() == 0;
                    }

                    // If the price level is now empty, remove it
                    if is_empty {
                        price_levels.remove(&price);
                        self.order_locations.remove(&order_id);
                        self.untrack_order_by_id(&order_id);
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

                    // Validate-first atomic modify (#98): validate the new
                    // order's shape and run the modify-aware risk check
                    // *before* removing the original. On any rejection the
                    // original order is never cancelled.
                    self.validate_order_shape(&new_order)?;
                    self.check_risk_modify_admission(
                        order_id,
                        new_order.user_id(),
                        new_order.price().as_u128(),
                        new_order.total_quantity(),
                    )?;

                    // #168: reject a re-price that would self-cross the same
                    // user's opposite-side liquidity under CancelTaker/CancelBoth
                    // BEFORE cancelling the original, so the original survives.
                    self.check_modify_stp_self_cross(&new_order)?;

                    // #230: reject a re-price whose re-add would exhaust a
                    // non-auto-replenishing reserve's visible tranche and
                    // discard its hidden remainder, which would destroy the
                    // order after the original was already cancelled.
                    self.check_modify_reserve_residual(&new_order)?;

                    // All checks passed: cancel the original and add the
                    // updated order.
                    // Ungated inner variants: `update_order` already holds
                    // the submit gate (#209 / #225); the public wrappers
                    // would re-acquire it (std RwLock is not reentrant).
                    // The gate mode was chosen once at the boundary by
                    // `modify_needs_exclusive_gate` — exclusive whenever STP
                    // is engaged — and it is never upgraded here, so the
                    // re-add must never be a fill-or-kill (whose
                    // all-or-nothing window always requires the exclusive
                    // gate, including on an `STPMode::None` book).
                    // Unreachable today — an FOK never rests, so it can
                    // never be modified — but enforced so a future TIF
                    // change cannot silently void the #209 guarantee.
                    debug_assert!(
                        !new_order.is_fill_or_kill(),
                        "a resting order can never carry FOK; the re-add cannot upgrade the gate"
                    );
                    self.cancel_order_with_reason(order_id, CancelReason::UserRequested)?;
                    let result = self.add_order_inner(new_order, false)?.0;
                    Ok(Some(result))
                } else {
                    Ok(None) // Order not found
                }
            }

            OrderUpdate::Cancel { order_id } => {
                // Get order location without locking
                let location = self.order_locations.get(&order_id).map(|val| *val);

                if let Some((price, side)) = location {
                    // Get the appropriate price levels map
                    let price_levels = match side {
                        Side::Buy => &self.bids,
                        Side::Sell => &self.asks,
                    };

                    // Attempt to cancel the order
                    let mut result = None;
                    let mut is_empty = false;

                    // Get the current order first
                    if let Some(current_order) = self.get_order(order_id) {
                        result = Some(current_order);

                        // Remove the order directly from the price level
                        if let Some(entry) = price_levels.get(&price) {
                            let price_level = entry.value();
                            let cancel_update = OrderUpdate::Cancel { order_id };
                            let result = price_level.update_order(cancel_update);
                            // notify price level changes
                            if let Some(ref listener) = self.price_level_changed_listener
                                && let Ok(updated_order) = result
                                && updated_order.is_some()
                            {
                                let engine_seq = self.next_engine_seq();
                                listener(PriceLevelChangedEvent {
                                    side,
                                    price: price_level.price(),
                                    quantity: price_level.visible_quantity(),
                                    engine_seq,
                                })
                            }
                            is_empty = price_level.order_count() == 0;
                        }

                        // Remove from order locations tracking
                        self.order_locations.remove(&order_id);
                        // Remove from user_orders index
                        self.untrack_order_by_id(&order_id);
                    }

                    // If price level is empty, remove it
                    if is_empty {
                        price_levels.remove(&price);
                    }

                    Ok(result)
                } else {
                    Ok(None) // Order not found
                }
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

                    // Validate-first atomic modify (#98): validate the new
                    // order's shape and run the modify-aware risk check
                    // *before* removing the original. On any rejection the
                    // original order is never cancelled — no book mutation,
                    // no events, no trades.
                    self.validate_order_shape(&new_order)?;
                    self.check_risk_modify_admission(
                        order_id,
                        new_order.user_id(),
                        new_order.price().as_u128(),
                        new_order.total_quantity(),
                    )?;

                    // #168: reject a re-price that would self-cross the same
                    // user's opposite-side liquidity under CancelTaker/CancelBoth
                    // BEFORE cancelling the original, so the original survives.
                    self.check_modify_stp_self_cross(&new_order)?;

                    // #230: reject a re-price whose re-add would exhaust a
                    // non-auto-replenishing reserve's visible tranche and
                    // discard its hidden remainder, which would destroy the
                    // order after the original was already cancelled.
                    self.check_modify_reserve_residual(&new_order)?;

                    // All checks passed: cancel the original and add the
                    // new order.
                    // Ungated inner variants: `update_order` already holds
                    // the submit gate (#209 / #225); the public wrappers
                    // would re-acquire it (std RwLock is not reentrant).
                    // The gate mode was chosen once at the boundary by
                    // `modify_needs_exclusive_gate` — exclusive whenever STP
                    // is engaged — and it is never upgraded here, so the
                    // re-add must never be a fill-or-kill (whose
                    // all-or-nothing window always requires the exclusive
                    // gate, including on an `STPMode::None` book).
                    // Unreachable today — an FOK never rests, so it can
                    // never be modified — but enforced so a future TIF
                    // change cannot silently void the #209 guarantee.
                    debug_assert!(
                        !new_order.is_fill_or_kill(),
                        "a resting order can never carry FOK; the re-add cannot upgrade the gate"
                    );
                    self.cancel_order_with_reason(order_id, CancelReason::UserRequested)?;
                    let result = self.add_order_inner(new_order, false)?.0;
                    Ok(Some(result))
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
    pub fn cancel_order(&self, order_id: Id) -> Result<Option<Arc<OrderType<T>>>, OrderBookError> {
        // #209: shared gate — a concurrent FOK's exclusive window must not
        // interleave with this cancel.
        let _gate = self.submit_gate_read();
        self.cancel_order_with_reason(order_id, CancelReason::UserRequested)
    }

    /// Cancel an order by ID with an explicit cancellation reason.
    ///
    /// This is the internal implementation used by both `cancel_order`
    /// and mass cancel operations to track the correct
    /// [`CancelReason`] in the order state tracker.
    pub(super) fn cancel_order_with_reason(
        &self,
        order_id: Id,
        reason: CancelReason,
    ) -> Result<Option<Arc<OrderType<T>>>, OrderBookError> {
        self.cache.invalidate();
        // First, we find the order's location (price and side) without locking
        let location = self.order_locations.get(&order_id).map(|val| *val);

        if let Some((price, side)) = location {
            // Obtener el mapa de niveles de precio apropiado
            let price_levels = match side {
                Side::Buy => &self.bids,
                Side::Sell => &self.asks,
            };

            // Create the update to cancel
            let update = OrderUpdate::Cancel { order_id };

            // Attempt to cancel the order from the price level
            let mut result = None;
            let mut empty_level = false;

            if let Some(entry) = price_levels.get(&price) {
                let price_level = entry.value();
                // Try to cancel the order
                if let Ok(cancelled) = price_level.update_order(update) {
                    result = cancelled;

                    // notify price level changes
                    if result.is_some()
                        && let Some(ref listener) = self.price_level_changed_listener
                    {
                        let engine_seq = self.next_engine_seq();
                        listener(PriceLevelChangedEvent {
                            side,
                            price: price_level.price(),
                            quantity: price_level.visible_quantity(),
                            engine_seq,
                        })
                    }

                    // Check if the level became empty
                    empty_level = price_level.order_count() == 0;
                }
            }

            self.cache.invalidate();
            // If we got a result and the order was canceled
            if let Some(ref cancelled_order) = result {
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

                // Remove the order from the locations map
                self.order_locations.remove(&order_id);

                // Pre-trade risk hook: drop the per-account counter
                // contribution before the order leaves the index. Does
                // not depend on `cancelled_order` because the risk
                // state already stores `account` and `remaining_qty`.
                // No-op when no `RiskConfig` is installed.
                self.risk_state.on_cancel(order_id);

                // Remove the order from the user_orders index
                self.untrack_user_order(cancelled_order.user_id(), &order_id);

                // #230: this helper is the funnel for user cancels, the
                // cancel-then-add modifies, mass cancel and expiry eviction,
                // so one decrement here covers all of them.
                self.note_removed_order(cancelled_order.as_ref());

                // Unregister special orders from re-pricing tracking
                #[cfg(feature = "special_orders")]
                {
                    self.special_order_tracker
                        .unregister_pegged_order(&order_id);
                    self.special_order_tracker
                        .unregister_trailing_stop(&order_id);
                }

                // If the level became empty, remove it
                if empty_level {
                    price_levels.remove(&price);
                    // Refresh the depth gauges now that a level was
                    // removed. No-op when the `metrics` feature is
                    // disabled.
                    self.record_depth_metric();
                }
            }

            Ok(result.map(|order| Arc::new(self.convert_from_unit_type(&order))))
        } else {
            Ok(None)
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
    /// `CancelBoth` arms (#95). No-op if `order_id` is not resting on the level.
    pub(super) fn cancel_resting_maker_on_level(
        &self,
        price_level: &PriceLevel,
        side: Side,
        order_id: Id,
        reason: CancelReason,
    ) {
        let Ok(Some(cancelled)) = price_level.update_order(OrderUpdate::Cancel { order_id }) else {
            return;
        };
        self.cache.invalidate();

        // 1. Notify the level change (same shape as cancel_order_with_reason).
        if let Some(ref listener) = self.price_level_changed_listener {
            let engine_seq = self.next_engine_seq();
            listener(PriceLevelChangedEvent {
                side,
                price: price_level.price(),
                quantity: price_level.visible_quantity(),
                engine_seq,
            });
        }

        // 2. Record the terminal cancellation, preserving any prior fill.
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

        // 3. Drop the per-account risk contribution, then untrack the order.
        self.order_locations.remove(&order_id);
        self.risk_state.on_cancel(order_id);
        self.untrack_user_order(cancelled.user_id(), &order_id);
        // #230: the self-trade-prevention maker cancel is the one removal
        // that does not go through `cancel_order_with_reason`.
        self.note_removed_order(cancelled.as_ref());

        #[cfg(feature = "special_orders")]
        {
            self.special_order_tracker
                .unregister_pegged_order(&order_id);
            self.special_order_tracker
                .unregister_trailing_stop(&order_id);
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
    pub(super) fn validate_order_shape(&self, order: &OrderType<T>) -> Result<(), OrderBookError> {
        // Two-tranche total representability (#210): an Iceberg / Reserve
        // whose visible + hidden overflows u64 cannot be tracked by any of
        // the engine's quantity arithmetic — reject it before every other
        // check so the saturating `total_quantity` below (and everywhere
        // downstream) is provably unreachable for admitted orders.
        if order.checked_total_quantity().is_none() {
            return Err(OrderBookError::QuantityOverflow {
                visible: order.visible_quantity().as_u64(),
                hidden: order.hidden_quantity().as_u64(),
            });
        }

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
        let qty = order.total_quantity();
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

        if order.is_post_only() && self.will_cross_market(order.price().as_u128(), order.side()) {
            return Err(OrderBookError::PriceCrossing {
                price: order.price().as_u128(),
                side: order.side(),
                opposite_price: if order.side() == Side::Buy {
                    self.best_ask().unwrap_or(0)
                } else {
                    self.best_bid().unwrap_or(0)
                },
            });
        }

        // For FOK orders, first check if the entire quantity can be matched
        // without altering the book. Use the faithful feasibility check (lot_size
        // + STP aware), not the raw-depth `peek_match`, so fill-or-kill stays
        // all-or-nothing and never emits a partial fill it then reports as killed (#96).
        if order.is_fill_or_kill() {
            let potential_match = self.fok_fillable_quantity(
                order.side(),
                order.total_quantity(),
                Some(order.price().as_u128()),
                order.user_id(),
                order.id(),
            );
            if potential_match < order.total_quantity() {
                return Err(OrderBookError::InsufficientLiquidity {
                    side: order.side(),
                    requested: order.total_quantity(),
                    available: potential_match,
                });
            }
        }

        Ok(())
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
        let mut remaining = new_order.total_quantity();
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
            let cap = if lot <= 1 {
                remaining
            } else {
                remaining - (remaining % lot)
            };
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
            let orders = level.snapshot_by_insertion_seq();
            match check_stp_at_level(&orders, taker_user_id, self.stp_mode) {
                STPAction::NoConflict => {
                    // No same-user maker at this level: the taker consumes its
                    // full matchable depth under the lot-rounded cap (the
                    // authoritative upstream dry run), then walks on.
                    remaining =
                        remaining.saturating_sub(level.matchable_quantity(cap, new_order.id()));
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
                    remaining = remaining.saturating_sub(
                        level.matchable_quantity(cap.min(safe_quantity), new_order.id()),
                    );
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

        let total = visible.saturating_add(hidden);
        let crossable = self.fok_fillable_quantity(
            new_order.side(),
            total,
            Some(new_order.price().as_u128()),
            new_order.user_id(),
            new_order.id(),
        );
        // `crossable < visible`: the sweep leaves a positive visible tranche
        // and the residual rests normally. `crossable >= total`: the order
        // fills completely, so nothing is discarded. Only the band in
        // between destroys quantity. `crossable == 0` is defense in depth:
        // `validate_order_shape` already rejects a projected zero visible
        // tranche, and without that rule a non-crossing re-price of such an
        // order would fall inside the band vacuously.
        if crossable > 0 && crossable >= visible && crossable < total {
            return Err(reserve_residual_would_be_discarded(
                new_order.id(),
                visible,
                crossable,
                hidden,
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
    fn record_shape_rejection(&self, order: &OrderType<T>, err: &OrderBookError) {
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
    /// # Errors
    /// Returns [`OrderBookError::KillSwitchActive`] when the kill switch
    /// is engaged. The check runs before any cache invalidation, STP
    /// validation, tick/lot validation, or matching work.
    #[inline]
    pub fn add_order(&self, order: OrderType<T>) -> Result<Arc<OrderType<T>>, OrderBookError> {
        // #209: shared gate for ordinary submits, exclusive for FOK so its
        // feasibility + sweep window excludes every concurrent mutation.
        // #225: also exclusive for an STP-relevant submit, so the per-level
        // STP scan and the fill it authorises see the same queue state. A
        // post-only submit never reaches that scan, so it stays shared.
        let _gate = self.acquire_coherent_submit_gate(self.submit_needs_exclusive_gate(
            order.is_fill_or_kill(),
            order.user_id(),
            order.is_post_only(),
            // #230: admitting a strandable maker is exclusive in every
            // STPMode, so no sweep can consume one it never captured.
            Self::is_strandable_maker(&order),
        ));
        self.add_order_inner(order, false).map(|(order, _)| order)
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
    /// validation, tick/lot validation, or matching work.
    pub fn add_order_with_result(
        &self,
        order: OrderType<T>,
    ) -> Result<(Arc<OrderType<T>>, Option<TradeResult>), OrderBookError> {
        // #209 / #225: same gating as `add_order`.
        let _gate = self.acquire_coherent_submit_gate(self.submit_needs_exclusive_gate(
            order.is_fill_or_kill(),
            order.user_id(),
            order.is_post_only(),
            // #230: admitting a strandable maker is exclusive in every
            // STPMode, so no sweep can consume one it never captured.
            Self::is_strandable_maker(&order),
        ));
        self.add_order_inner(order, true)
    }

    /// Shared implementation behind [`Self::add_order`] and
    /// [`Self::add_order_with_result`]. `want_result` gates `TradeResult`
    /// construction so the plain `add_order` path only pays for it when an
    /// installed trade listener needs it anyway.
    fn add_order_inner(
        &self,
        mut order: OrderType<T>,
        want_result: bool,
    ) -> Result<(Arc<OrderType<T>>, Option<TradeResult>), OrderBookError> {
        self.check_kill_switch_or_reject(order.id())?;
        // Representability gate (#210): an unrepresentable two-tranche
        // total must be rejected before the risk gate below, which would
        // otherwise evaluate the account's notional against the SATURATED
        // `u64::MAX` total and reject with a misleading risk-family error.
        // `validate_order_shape` re-checks this for the shared modify path;
        // the duplicate check is a single jump-table match + checked_add.
        if order.checked_total_quantity().is_none() {
            let err = OrderBookError::QuantityOverflow {
                visible: order.visible_quantity().as_u64(),
                hidden: order.hidden_quantity().as_u64(),
            };
            self.record_shape_rejection(&order, &err);
            return Err(err);
        }
        // Pre-trade risk gate: per-account open-orders / notional /
        // price band. No-op when no `RiskConfig` is installed.
        // Documented order: kill_switch → risk → STP → fees → match.
        // On the cold reject path, record an `OrderStatus::Rejected`
        // transition with the closed `RejectReason` taxonomy before
        // propagating the typed error.
        if let Err(err) = self.check_risk_limit_admission(
            order.user_id(),
            order.price().as_u128(),
            order.total_quantity(),
        ) {
            self.reject_with_risk(order.id(), &err);
            return Err(err);
        }

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
        // This is a sequential guard, not a concurrency guard: the check
        // and the eventual `order_locations.insert` straddle the match
        // walk, so two concurrent `add_order` calls with the same *fresh*
        // id can both pass here and both rest (last-writer-wins on insert).
        // Serializing order ids is the ingress / sequencing layer's job.
        if self.order_locations.contains_key(&order.id()) {
            crate::orderbook::metrics::record_reject(RejectReason::DuplicateOrderId);
            return Err(OrderBookError::DuplicateOrderId {
                order_id: order.id(),
            });
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
        if let Err(err) = self.validate_order_shape(&order) {
            self.record_shape_rejection(&order, &err);
            return Err(err);
        }

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
                        self.track_state(
                            order.id(),
                            OrderStatus::Rejected {
                                reason: RejectReason::InvalidQuantity,
                            },
                        );
                        crate::orderbook::metrics::record_reject(RejectReason::InvalidQuantity);
                        return Err(OrderBookError::PriceLevelError(err));
                    }
                };
                if level_total.checked_add(order.total_quantity()).is_none() {
                    let err = OrderBookError::InvalidOperation {
                        message: format!(
                            "resting order {} would overflow the aggregate capacity of level {}",
                            order.id(),
                            order.price()
                        ),
                    };
                    self.track_state(
                        order.id(),
                        OrderStatus::Rejected {
                            reason: RejectReason::InvalidQuantity,
                        },
                    );
                    crate::orderbook::metrics::record_reject(RejectReason::InvalidQuantity);
                    return Err(err);
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
        let MatchOutcome {
            result: match_result,
            taker_stp_cancelled,
            taker_post_only_rejected,
        } = self.match_order_with_user_outcome(
            order.id(),
            order.side(),
            order.total_quantity(), // Use total quantity for matching
            Some(order.price().as_u128()),
            order.user_id(),
            taker_kind,
        )?;

        // #209: the sweep reached a crossable level with a post-only taker.
        // pricelevel structurally refused to trade (zero fills), so reject
        // exactly like the precheck would have — the race between precheck
        // and sweep can no longer make a post-only order take liquidity.
        if taker_post_only_rejected {
            self.track_state(
                order.id(),
                OrderStatus::Rejected {
                    reason: RejectReason::PostOnlyWouldCross,
                },
            );
            crate::orderbook::metrics::record_reject(RejectReason::PostOnlyWouldCross);
            return Err(OrderBookError::PriceCrossing {
                price: order.price().as_u128(),
                side: order.side(),
                opposite_price: if order.side() == Side::Buy {
                    self.best_ask().unwrap_or(0)
                } else {
                    self.best_bid().unwrap_or(0)
                },
            });
        }

        // Emit trades BEFORE any early return below: the STP taker-cancel and
        // unfillable-IOC paths return `Err` after real (non-self) fills already
        // executed, and those fills must still reach the metrics and the trade
        // listener. The `TradeResult` is only constructed when someone consumes
        // it — the installed listener and/or an `add_order_with_result` caller —
        // so the plain `add_order` hot path skips the `MatchResult` clone.
        let trades_emitted = match_result.trades().len() as u64;
        let trade_result = if trades_emitted > 0 {
            crate::orderbook::metrics::record_trades(trades_emitted);
            let listener = self.trade_listener.as_ref();
            if want_result || listener.is_some() {
                let mut trade_result = TradeResult::with_fees(
                    self.symbol.clone(),
                    match_result.clone(),
                    self.fee_schedule,
                );
                trade_result.engine_seq = self.next_engine_seq();
                if let Some(listener) = listener {
                    listener(&trade_result) // emit trade events to listener
                }
                Some(trade_result)
            } else {
                None
            }
        } else {
            None
        };

        // True (non-self) executed quantity. `remaining_quantity` only decrements on
        // real trades, so STP-prevented self-fills never count toward it.
        let original_qty = order.total_quantity();
        let filled_qty = original_qty.saturating_sub(match_result.remaining_quantity().as_u64());

        // If STP cancelled the taker, the residual must NOT rest — even though some
        // non-self fills already occurred at earlier levels. Record the terminal
        // SelfTradePrevention state with the true filled quantity and surface the STP
        // error (#97). The zero-fills case already returned this error from the match.
        if taker_stp_cancelled {
            self.track_state(
                order.id(),
                OrderStatus::Cancelled {
                    filled_quantity: filled_qty,
                    reason: CancelReason::SelfTradePrevention,
                },
            );
            crate::orderbook::metrics::record_reject(RejectReason::SelfTradePrevention);
            return Err(OrderBookError::SelfTradePrevented {
                mode: self.stp_mode,
                taker_order_id: order.id(),
                user_id: order.user_id(),
            });
        }

        // If the order was not fully filled, add the remainder to the book
        if match_result.remaining_quantity().as_u64() > 0 {
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
                return Err(OrderBookError::InsufficientLiquidity {
                    side: order.side(),
                    requested: order.quantity(), // Now uses the trait method
                    available: order
                        .quantity()
                        .saturating_sub(match_result.remaining_quantity().as_u64()),
                });
            }

            // Rest the taker's residual. `remaining_quantity` is the TOTAL
            // unmatched quantity, so distribute it across the tranches with
            // `set_total_remaining` (#210): for a partially-filled iceberg
            // the submitted visible quantity acts as the display size and
            // the rest stays hidden — assigning the total to the visible
            // tranche (the old `set_quantity` semantics) manufactured
            // liquidity by keeping the original hidden tranche on top.
            if match_result.remaining_quantity().as_u64() < order.total_quantity() {
                order.set_total_remaining(match_result.remaining_quantity().as_u64());

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
                            filled_quantity: filled_qty,
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

            let price = order.price().as_u128();
            let side = order.side();

            let price_levels = match side {
                Side::Buy => &self.bids,
                Side::Sell => &self.asks,
            };

            let price_level = price_levels.get_or_insert(price, Arc::new(PriceLevel::new(price)));
            let level = price_level.value();

            // Convert to unit type for PriceLevel compatibility. Admission
            // into the level is validated upstream since pricelevel 0.9
            // (duplicate id, counter capacity). The pre-sweep headroom
            // check above makes a failure here concurrent-only; if it
            // still happens, remove the level when this call created it
            // empty — `best_bid` / `best_ask`, the cache, and the depth
            // gauges must never expose a phantom level — and surface the
            // error loudly: the sweep's trades are already irreversible
            // (#211).
            let unit_order = self.convert_to_unit_type(&order);
            let unit_order_arc = match price_level.value().add_order(unit_order) {
                Ok(admitted) => admitted,
                Err(err) => {
                    if level.order_count() == 0 {
                        price_levels.remove(&price);
                    }
                    self.cache.invalidate();
                    self.record_depth_metric();
                    tracing::error!(
                        order_id = %order.id(),
                        price,
                        error = %err,
                        "residual admission failed after irreversible trades; level cleaned up"
                    );
                    return Err(OrderBookError::PriceLevelError(err));
                }
            };
            // #230: this is the single point where `add_order` rests an
            // order on a level — both the untouched submit and the
            // partially-filled residual reach it — so flagging here covers
            // the whole admission path. Enables the sweep's
            // strandable-maker scan for this book from now on.
            self.note_rested_order(unit_order_arc.as_ref());
            // notify price level changes
            if let Some(ref listener) = self.price_level_changed_listener {
                let engine_seq = self.next_engine_seq();
                listener(PriceLevelChangedEvent {
                    side,
                    price: level.price(),
                    quantity: level.visible_quantity(),
                    engine_seq,
                })
            }
            self.order_locations
                .insert(unit_order_arc.id(), (price, side));

            // Refresh the depth gauges. The level may be brand-new
            // (`get_or_insert` created it) or pre-existing — either
            // way the gauge reflects current state. No-op when the
            // `metrics` feature is disabled.
            self.record_depth_metric();

            // Pre-trade risk hook: register the resting order with
            // the risk state so per-account counters are updated and
            // future checks see the new contribution. No-op when no
            // `RiskConfig` is installed.
            self.risk_state.on_admission(
                unit_order_arc.id(),
                order.user_id(),
                price,
                match_result.remaining_quantity().as_u64(),
            );

            // Track the order in the user_orders index
            self.track_user_order(order.user_id(), unit_order_arc.id());

            // Register special orders for re-pricing tracking
            #[cfg(feature = "special_orders")]
            match &order {
                OrderType::PeggedOrder { id, .. } => {
                    self.special_order_tracker.register_pegged_order(*id);
                }
                OrderType::TrailingStop { id, .. } => {
                    self.special_order_tracker.register_trailing_stop(*id);
                }
                _ => {}
            }

            // Track state: Open (no fills) or PartiallyFilled (some fills, resting)
            if filled_qty > 0 {
                self.track_state(
                    order.id(),
                    OrderStatus::PartiallyFilled {
                        original_quantity: original_qty,
                        filled_quantity: filled_qty,
                    },
                );
            } else {
                self.track_state(order.id(), OrderStatus::Open);
            }

            // Convert back to generic type for return
            let generic_order = self.convert_from_unit_type(&unit_order_arc);
            Ok((Arc::new(generic_order), trade_result))
        } else {
            // The order was fully matched
            self.track_state(
                order.id(),
                OrderStatus::Filled {
                    filled_quantity: original_qty,
                },
            );
            Ok((Arc::new(order), trade_result))
        }
    }
}
