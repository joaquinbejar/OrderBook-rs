//! The per-user resting-order index behind `OrderBook::user_orders`.

use pricelevel::Id;

/// Dead slots below this count are never compacted: a short list is
/// cheaper to skip over than to rebuild.
const MIN_COMPACT_DEAD: usize = 32;

/// One admitted id and the per-user sequence it was admitted under; `id`
/// is `None` once the order left the index.
#[derive(Debug, Clone, Copy)]
struct Slot {
    seq: u64,
    id: Option<Id>,
}

/// The resting order ids one user owns, in admission order.
///
/// Every admission appends a slot under the next per-user sequence and
/// returns that sequence, which the order's
/// [`OrderLocation`](super::book::OrderLocation) carries. Slots are appended
/// in sequence order, so they stay sorted: a removal finds its slot by binary
/// search on the sequence and marks it dead instead of shifting the slots
/// behind it. Dead slots are dropped in one pass once they outnumber the live
/// ones, so a removal is amortized O(log n). The previous `Vec<Id>` layout
/// searched and shifted the whole list on every removal, so a single owner
/// with `n` resting orders paid O(n) per cancel or fill, and every anonymous
/// order shares `Hash32::zero()`.
///
/// Neither marking nor compacting reorders the live slots, so
/// [`Self::to_vec`] yields the admission order that `cancel_orders_by_user`
/// reports (#252).
#[derive(Debug, Default, Clone)]
pub(crate) struct UserOrderIds {
    /// The sequence the next admitted id is tracked under.
    next_seq: u64,
    /// Number of live slots.
    live: usize,
    /// Every slot not yet compacted away, ascending by `seq`.
    slots: Vec<Slot>,
}

impl UserOrderIds {
    /// Track `id` as this user's most recently admitted order and return the
    /// sequence it is tracked under, for its location to carry.
    ///
    /// The sequence only grows while the entry exists, so it cannot run out
    /// in practice (2^64 admissions for one user without the entry ever
    /// emptying). Should it, later ids share `u64::MAX`; slots stay sorted
    /// and [`Self::remove`] still finds them through its id fallback.
    #[inline]
    pub(crate) fn insert(&mut self, id: Id) -> u64 {
        let seq = self.next_seq;
        self.next_seq = seq.checked_add(1).unwrap_or(seq);
        self.slots.push(Slot { seq, id: Some(id) });
        if let Some(live) = self.live.checked_add(1) {
            self.live = live;
        }
        seq
    }

    /// Stop tracking `id`, admitted under `seq`. Returns whether it was
    /// tracked.
    #[inline]
    pub(crate) fn remove(&mut self, seq: u64, id: &Id) -> bool {
        let index = match self.slots.binary_search_by_key(&seq, |slot| slot.seq) {
            Ok(index)
                if self
                    .slots
                    .get(index)
                    .is_some_and(|slot| slot.id == Some(*id)) =>
            {
                Some(index)
            }
            _ => self.position_of(id),
        };
        self.kill(index)
    }

    /// Stop tracking `id` without its sequence: one scan of the slots. Cold
    /// fallback for a removal whose location is already gone.
    #[cold]
    pub(crate) fn remove_by_id(&mut self, id: &Id) -> bool {
        let index = self.position_of(id);
        self.kill(index)
    }

    /// Whether no id is tracked.
    #[must_use]
    #[inline]
    pub(crate) fn is_empty(&self) -> bool {
        self.live == 0
    }

    /// Keep only the ids for which `keep` returns `true`, in their order.
    /// Also drops every dead slot.
    pub(crate) fn retain(&mut self, mut keep: impl FnMut(&Id) -> bool) {
        self.slots
            .retain(|slot| slot.id.as_ref().is_some_and(&mut keep));
        self.live = self.slots.len();
    }

    /// The tracked ids in admission order.
    #[must_use]
    pub(crate) fn to_vec(&self) -> Vec<Id> {
        let mut ids = Vec::with_capacity(self.live);
        ids.extend(self.slots.iter().filter_map(|slot| slot.id));
        ids
    }

    /// Whether `id` is tracked.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn contains(&self, id: &Id) -> bool {
        self.position_of(id).is_some()
    }

    /// Number of tracked ids.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn len(&self) -> usize {
        self.live
    }

    /// The id tracked under `seq`, if that slot is live.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn id_at(&self, seq: u64) -> Option<Id> {
        self.slots
            .binary_search_by_key(&seq, |slot| slot.seq)
            .ok()
            .and_then(|index| self.slots.get(index))
            .and_then(|slot| slot.id)
    }

    /// Index of the live slot holding `id`. Cold: one scan of the slots.
    #[cold]
    fn position_of(&self, id: &Id) -> Option<usize> {
        self.slots.iter().position(|slot| slot.id == Some(*id))
    }

    /// Mark the slot at `index` dead, then compact if dead slots outnumber
    /// live ones. Returns whether a slot was marked. Both callers resolve
    /// `index` to a slot holding the id they remove, so it is live.
    #[inline]
    fn kill(&mut self, index: Option<usize>) -> bool {
        let Some(slot) = index.and_then(|index| self.slots.get_mut(index)) else {
            return false;
        };
        slot.id = None;
        if let Some(live) = self.live.checked_sub(1) {
            self.live = live;
        }
        // Dead slots at the back are free to drop: no live slot moves.
        while self.slots.last().is_some_and(|slot| slot.id.is_none()) {
            self.slots.pop();
        }
        // `live` counts a subset of the slots, so this never underflows.
        let dead = self.slots.len().checked_sub(self.live);
        if dead.is_some_and(|dead| dead >= MIN_COMPACT_DEAD && dead > self.live) {
            self.slots.retain(|slot| slot.id.is_some());
        }
        true
    }
}

#[cfg(test)]
mod tests {
    // tests may panic: rules/global_rules.md § Testing
    use super::*;

    fn ids(raw: &[u64]) -> Vec<Id> {
        raw.iter().copied().map(Id::from_u64).collect()
    }

    /// Inserts `raw` in order and returns each id with its sequence.
    fn filled(raw: &[u64]) -> (UserOrderIds, Vec<(u64, Id)>) {
        let mut index = UserOrderIds::default();
        let tracked = ids(raw)
            .into_iter()
            .map(|id| (index.insert(id), id))
            .collect();
        (index, tracked)
    }

    #[test]
    fn test_user_order_ids_to_vec_follows_admission_order() {
        let (index, tracked) = filled(&[5, 3, 9, 1]);
        assert_eq!(index.to_vec(), ids(&[5, 3, 9, 1]));
        assert_eq!(index.len(), 4);
        let seqs: Vec<u64> = tracked.iter().map(|(seq, _)| *seq).collect();
        assert_eq!(seqs, vec![0, 1, 2, 3]);
    }

    #[test]
    fn test_user_order_ids_remove_keeps_remaining_order() {
        let (mut index, tracked) = filled(&[1, 2, 3, 4, 5]);
        let (seq_1, id_1) = tracked[0];
        let (seq_4, id_4) = tracked[3];
        assert!(index.remove(seq_1, &id_1));
        assert!(index.remove(seq_4, &id_4));
        assert!(!index.remove(seq_4, &id_4), "second removal is a miss");
        assert_eq!(index.to_vec(), ids(&[2, 3, 5]));
        assert!(index.contains(&Id::from_u64(3)));
        assert!(!index.contains(&id_4));
        assert_eq!(index.len(), 3);
    }

    #[test]
    fn test_user_order_ids_remove_with_stale_seq_falls_back_to_id() {
        let (mut index, tracked) = filled(&[1, 2, 3]);
        let (_, id_2) = tracked[1];
        assert!(
            index.remove(999, &id_2),
            "a wrong sequence still finds the id"
        );
        assert!(
            !index.remove(0, &id_2),
            "the slot at seq 0 holds another id"
        );
        assert_eq!(index.to_vec(), ids(&[1, 3]));
    }

    #[test]
    fn test_user_order_ids_remove_by_id() {
        let (mut index, _) = filled(&[1, 2, 3]);
        assert!(index.remove_by_id(&Id::from_u64(2)));
        assert!(!index.remove_by_id(&Id::from_u64(2)));
        assert_eq!(index.to_vec(), ids(&[1, 3]));
    }

    #[test]
    fn test_user_order_ids_compaction_keeps_order_and_sequences() {
        let raw: Vec<u64> = (0..200).collect();
        let (mut index, tracked) = filled(&raw);
        // Remove every id except each tenth one, front to back: the dead
        // slots outnumber the live ones and are compacted away midway.
        for (n, (seq, id)) in tracked.iter().enumerate() {
            if n % 10 != 0 {
                assert!(index.remove(*seq, id));
            }
        }
        assert!(index.slots.len() < 200, "dead slots were compacted");
        let kept: Vec<u64> = (0..200).filter(|n| n % 10 == 0).collect();
        assert_eq!(index.to_vec(), ids(&kept));
        // The sequences survive compaction: the binary search still hits.
        for (n, (seq, id)) in tracked.iter().enumerate() {
            if n % 10 == 0 {
                assert!(index.remove(*seq, id));
            }
        }
        assert!(index.is_empty());
        assert!(index.slots.is_empty());
    }

    #[test]
    fn test_user_order_ids_trailing_dead_slots_are_dropped() {
        let (mut index, tracked) = filled(&[1, 2, 3]);
        for (seq, id) in tracked.iter().rev() {
            assert!(index.remove(*seq, id));
        }
        assert!(index.slots.is_empty());
        assert!(index.is_empty());
    }

    #[test]
    fn test_user_order_ids_retain_keeps_remaining_order() {
        let (mut index, _) = filled(&[1, 2, 3, 4]);
        index.retain(|id| *id != Id::from_u64(2));
        assert_eq!(index.to_vec(), ids(&[1, 3, 4]));
        assert_eq!(index.len(), 3);
        index.retain(|_| false);
        assert!(index.is_empty());
    }

    #[test]
    fn test_user_order_ids_sequence_exhaustion_stays_removable() {
        let (mut index, _) = filled(&[7]);
        index.next_seq = u64::MAX;
        let a = index.insert(Id::from_u64(8));
        let b = index.insert(Id::from_u64(9));
        assert_eq!((a, b), (u64::MAX, u64::MAX));
        assert!(index.remove(b, &Id::from_u64(9)));
        assert!(index.remove(a, &Id::from_u64(8)));
        assert_eq!(index.to_vec(), ids(&[7]));
    }
}
