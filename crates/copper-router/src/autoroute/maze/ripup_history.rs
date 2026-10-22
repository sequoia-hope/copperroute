//! Negotiated-congestion history for ripup: every time a net's copper is ripped, ripping that net
//! again costs more, so contested nets stop trading places pass after pass.
//!
//! Off unless `COPPERROUTE_RIPUP_HISTORY` is a positive factor; the cost multiplier for an obstacle
//! is `1 + factor * times_its_net_was_ripped`.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::LazyLock;

static FACTOR: LazyLock<f64> = LazyLock::new(|| {
    std::env::var("COPPERROUTE_RIPUP_HISTORY")
        .ok()
        .and_then(|value| value.trim().parse::<f64>().ok())
        .filter(|factor| factor.is_finite() && *factor > 0.0)
        .unwrap_or(0.0)
});

thread_local! {
    static RIPS: RefCell<BTreeMap<i32, u32>> = const { RefCell::new(BTreeMap::new()) };
}

pub fn enabled() -> bool {
    *FACTOR > 0.0
}

pub fn reset() {
    RIPS.with(|rips| rips.borrow_mut().clear());
}

pub fn record(net_no: i32) {
    if enabled() {
        RIPS.with(|rips| *rips.borrow_mut().entry(net_no).or_insert(0) += 1);
    }
}

pub fn cost_multiplier(net_nos: impl IntoIterator<Item = i32>) -> f64 {
    if !enabled() {
        return 1.0;
    }
    let most = RIPS.with(|rips| {
        let rips = rips.borrow();
        net_nos
            .into_iter()
            .filter_map(|net| rips.get(&net).copied())
            .max()
            .unwrap_or(0)
    });
    1.0 + *FACTOR * f64::from(most)
}
