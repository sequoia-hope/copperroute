use std::cell::Cell;
use web_time::Instant;

use copper_board::TimeLimit;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum StopRequestState {
    #[default]
    None,
    AutoRouterOnly,
    All,
}

impl StopRequestState {
    pub fn ordinal(self) -> i32 {
        match self {
            StopRequestState::None => 0,
            StopRequestState::AutoRouterOnly => 1,
            StopRequestState::All => 2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PassRecord {
    pub pass: i32,
    pub score: f32,
    pub incomplete_count: usize,
    pub clearance_violations: usize,
    pub via_count: usize,
    pub trace_count: usize,
}

pub type CancelPoll = Box<dyn Fn(&RouterStop)>;

pub struct RouterStop {
    state: Cell<StopRequestState>,
    deadline: Option<TimeLimit>,
    timed_out: Cell<bool>,
    cancel_poll: Option<CancelPoll>,
}

#[derive(Debug)]
pub struct DeterministicWorkBudget {
    limit: u64,
    spent: Cell<u64>,
}

impl DeterministicWorkBudget {
    pub fn new(limit: u64) -> Self {
        Self {
            limit,
            spent: Cell::new(0),
        }
    }

    pub fn poll(&self) -> bool {
        let spent = self.spent.get();
        if spent >= self.limit {
            return true;
        }
        self.spent.set(spent + 1);
        false
    }

    pub fn exhausted(&self) -> bool {
        self.spent.get() >= self.limit
    }

    pub fn spent(&self) -> u64 {
        self.spent.get()
    }

    pub fn limit(&self) -> u64 {
        self.limit
    }
}

impl std::fmt::Debug for RouterStop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RouterStop")
            .field("state", &self.state)
            .field("deadline", &self.deadline)
            .field("timed_out", &self.timed_out)
            .field("cancel_poll", &self.cancel_poll.is_some())
            .finish()
    }
}

impl Default for RouterStop {
    fn default() -> Self {
        Self::new()
    }
}

impl RouterStop {
    pub fn new() -> RouterStop {
        RouterStop {
            state: Cell::new(StopRequestState::None),
            deadline: None,
            timed_out: Cell::new(false),
            cancel_poll: None,
        }
    }

    pub fn with_deadline(limit_ms: i32) -> RouterStop {
        RouterStop {
            state: Cell::new(StopRequestState::None),
            deadline: Some(TimeLimit::new(limit_ms)),
            timed_out: Cell::new(false),
            cancel_poll: None,
        }
    }

    pub fn with_cancel_poll(mut self, poll: CancelPoll) -> RouterStop {
        self.cancel_poll = Some(poll);
        self
    }

    pub fn request_stop(&self) {
        self.state.set(StopRequestState::All);
    }

    pub fn request_stop_auto_router(&self) {
        if self.state.get() == StopRequestState::None {
            self.state.set(StopRequestState::AutoRouterOnly);
        }
    }

    pub fn begin_optimizer_stage(&self) {
        if self.state.get() == StopRequestState::AutoRouterOnly {
            self.state.set(StopRequestState::None);
        }
    }

    pub fn is_stop_requested(&self) -> bool {
        self.state.get() == StopRequestState::All
    }

    pub fn is_stop_auto_router_requested(&self) -> bool {
        self.state.get() != StopRequestState::None
    }

    pub fn state(&self) -> StopRequestState {
        self.state.get()
    }

    pub fn poll_deadline(&self) -> bool {
        let Some(deadline) = self.deadline.as_ref() else {
            return false;
        };
        if !deadline.is_exceeded() {
            return false;
        }
        self.request_stop();
        self.timed_out.set(true);
        true
    }

    /// The check a search runs between expansions: a stop already requested, or the job
    /// deadline seen to have passed since the last poll. A connection that starts just before
    /// the deadline would otherwise search for its whole per-connection time limit.
    pub fn is_stopped_or_expired(&self) -> bool {
        self.poll_cancel();
        self.poll_deadline();
        self.is_stop_requested()
    }

    /// Milliseconds left before the deadline, if there is one; negative once it has passed.
    pub fn remaining_ms(&self) -> Option<i64> {
        let deadline = self.deadline.as_ref()?.deadline()?;
        let now = web_time::Instant::now();
        Some(if deadline >= now {
            i64::try_from((deadline - now).as_millis()).unwrap_or(i64::MAX)
        } else {
            -i64::try_from((now - deadline).as_millis()).unwrap_or(i64::MAX)
        })
    }

    pub fn poll_cancel(&self) {
        if let Some(poll) = self.cancel_poll.as_ref() {
            poll(self);
        }
    }

    pub fn is_timed_out(&self) -> bool {
        self.timed_out.get()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RouterBudget {
    pub opt_changed_area_ms: i32,
    pub fanout_ms_per_pin: i32,
    pub board_update_throttle_ms: i32,
    pub progress_throttle_ms: i32,
}

impl Default for RouterBudget {
    /// same `-mp 20` run with the jar's DEBUG log on — which slows it down — is back to
    fn default() -> Self {
        RouterBudget {
            opt_changed_area_ms: 0,
            ..RouterBudget::from_fixed_budget()
        }
    }
}

impl RouterBudget {
    pub fn from_fixed_budget() -> RouterBudget {
        RouterBudget {
            opt_changed_area_ms: 1000,
            fanout_ms_per_pin: 10000,
            board_update_throttle_ms: 250,
            progress_throttle_ms: 1000,
        }
    }

    pub fn disabled() -> RouterBudget {
        RouterBudget {
            opt_changed_area_ms: 0,
            fanout_ms_per_pin: i32::MAX,
            board_update_throttle_ms: 0,
            progress_throttle_ms: 0,
        }
    }

    pub fn opt_changed_area_limit(&self) -> Option<TimeLimit> {
        if self.opt_changed_area_ms > 0 {
            Some(TimeLimit::new(self.opt_changed_area_ms))
        } else {
            None
        }
    }

    pub fn fanout_limit_for_pass(&self, pass_no: i32) -> TimeLimit {
        let max_milliseconds = f64::from(self.fanout_ms_per_pin) * f64::from(pass_no + 1);
        TimeLimit::new(max_milliseconds as i32)
    }

    pub fn board_update_throttler(&self) -> ProgressThrottler {
        ProgressThrottler::board_update_gate(self.board_update_throttle_ms)
    }

    pub fn progress_throttler(&self) -> ProgressThrottler {
        ProgressThrottler::new(self.progress_throttle_ms)
    }
}

#[derive(Debug)]
pub struct ProgressThrottler {
    interval_ms: i32,
    strict: bool,
    last_update: Cell<Option<Instant>>,
}

impl ProgressThrottler {
    pub fn new(interval_ms: i32) -> ProgressThrottler {
        ProgressThrottler {
            interval_ms,
            strict: false,
            last_update: Cell::new(None),
        }
    }

    pub fn board_update_gate(interval_ms: i32) -> ProgressThrottler {
        ProgressThrottler {
            interval_ms,
            strict: true,
            last_update: Cell::new(None),
        }
    }

    pub fn interval_ms(&self) -> i32 {
        self.interval_ms
    }

    pub fn should_update(&self) -> bool {
        self.should_update_at(Instant::now())
    }

    pub fn should_update_at(&self, now: Instant) -> bool {
        if self.interval_ms <= 0 {
            self.last_update.set(Some(now));
            return true;
        }
        let Some(last) = self.last_update.get() else {
            self.last_update.set(Some(now));
            return true;
        };
        let elapsed_ms = now.saturating_duration_since(last).as_millis() as i128;
        let limit = i128::from(self.interval_ms);
        let fires = if self.strict {
            elapsed_ms > limit
        } else {
            elapsed_ms >= limit
        };
        if fires {
            self.last_update.set(Some(now));
        }
        fires
    }

    pub fn reset(&self) {
        self.last_update.set(None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_stop_is_none() {
        let stop = RouterStop::default();
        assert_eq!(stop.state(), StopRequestState::None);
        assert!(!stop.is_stop_requested());
        assert!(!stop.is_stop_auto_router_requested());
        assert!(!stop.is_timed_out());
    }

    #[test]
    fn the_state_lattice_is_declaration_order() {
        assert!(StopRequestState::None < StopRequestState::AutoRouterOnly);
        assert!(StopRequestState::AutoRouterOnly < StopRequestState::All);
        assert_eq!(StopRequestState::None.ordinal(), 0);
        assert_eq!(StopRequestState::AutoRouterOnly.ordinal(), 1);
        assert_eq!(StopRequestState::All.ordinal(), 2);
    }

    #[test]
    fn the_cancel_poll_seam_is_a_no_op_until_a_closure_is_installed() {
        let bare = RouterStop::new();
        bare.poll_cancel();
        assert_eq!(bare.state(), StopRequestState::None);
        assert!(!bare.is_timed_out());

        let quiet = RouterStop::new().with_cancel_poll(Box::new(|_| {}));
        quiet.poll_cancel();
        assert_eq!(quiet.state(), StopRequestState::None);

        let loud = RouterStop::new().with_cancel_poll(Box::new(RouterStop::request_stop));
        assert_eq!(loud.state(), StopRequestState::None);
        loud.poll_cancel();
        assert_eq!(loud.state(), StopRequestState::All);
    }

    #[test]
    fn the_cancel_poll_seam_and_the_deadline_poll_are_independent() {
        let ran = std::rc::Rc::new(std::cell::Cell::new(0_u32));
        let counted = std::rc::Rc::clone(&ran);
        let stop = RouterStop::with_deadline(3_600_000)
            .with_cancel_poll(Box::new(move |_| counted.set(counted.get() + 1)));

        assert!(!stop.poll_deadline());
        assert_eq!(
            ran.get(),
            0,
            "poll_deadline must not run the cancel closure"
        );

        stop.poll_cancel();
        assert_eq!(ran.get(), 1);
        assert!(!stop.is_timed_out());
        assert_eq!(stop.state(), StopRequestState::None);
    }

    #[test]
    fn the_fanout_limit_scales_with_the_pass_number() {
        let budget = RouterBudget::default();
        assert_eq!(budget.fanout_limit_for_pass(0).limit_ms(), 10_000);
        assert_eq!(budget.fanout_limit_for_pass(2).limit_ms(), 30_000);
        assert_eq!(
            RouterBudget::disabled().fanout_limit_for_pass(9).limit_ms(),
            i32::MAX
        );
    }

    #[test]
    fn deterministic_work_budget_stops_before_the_next_step() {
        let budget = DeterministicWorkBudget::new(2);

        assert!(!budget.poll());
        assert!(!budget.poll());
        assert!(budget.poll());
        assert_eq!(budget.spent(), 2);
        assert!(budget.exhausted());
    }
}
