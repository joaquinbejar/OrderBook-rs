// cancel_only_hdr — pre-loaded book + cancel workload.
// Measures `cancel_order` lookup + unlink cost.

#[path = "hdr_common.rs"]
mod common;

use common::{Rng, new_histogram, persist, record, report, submit_gtc};
use pricelevel::Id;

const SCENARIO: &str = "cancel_only";
const PRELOAD_OPS: u64 = 1_000_000;
const SEED: u64 = 0xA5A5_A5A5_A5A5_A5A5;

fn main() {
    let book = common::fresh_book();
    let mut rng = Rng::new(SEED);
    let mut hist = new_histogram();

    // Pre-load the book with PRELOAD_OPS resting orders. The id space is
    // 1..=PRELOAD_OPS so cancel ids are deterministic and present.
    for i in 0..PRELOAD_OPS {
        submit_gtc(&book, &mut rng, i + 1);
    }

    // Cancel each one, in order. No warmup phase needed — cancel cost is
    // dominated by `DashMap::remove` which has a stable distribution.
    for i in 0..PRELOAD_OPS {
        let id = Id::from_u64(i + 1);
        record(&mut hist, || {
            let _ = book.cancel_order(id);
        });
    }

    report(SCENARIO, &hist);
    persist(SCENARIO, &hist).expect("persist hgrm");
}
