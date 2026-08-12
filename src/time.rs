use std::time::Duration;
#[cfg(not(target_arch = "wasm32"))]
use std::time::Instant;
#[cfg(target_arch = "wasm32")]
use web_time::Instant;

use crate::{thread::ThreadData, types::Score};

#[derive(Clone, Debug)]
pub enum Limits {
    Infinite,
    Depth(i32),
    Time(u64),
    Nodes(u64),
    Mate(u64),
    Fischer(u64, u64),
    Cyclic(u64, u64, u64),
}

const TIME_OVERHEAD_MS: u64 = 15;

#[derive(Clone)]
pub struct TimeManager {
    limits: Limits,
    start_time: Instant,
    soft_bound: Duration,
    hard_bound: Duration,
}

impl TimeManager {
    pub fn new(limits: Limits, fullmove_number: usize, move_overhead: u64) -> Self {
        let soft;
        let mut hard;

        match limits {
            Limits::Time(ms) => {
                // `go movetime` must honour Move Overhead like the other
                // limits do. Only the fixed TIME_OVERHEAD_MS was being
                // subtracted below, so a GUI configured with the default
                // 100 ms overhead still got ms - 15 of thinking and could flag
                // on a slow connection. `saturating_sub` matches the Fischer
                // and Cyclic branches.
                let ms = ms.saturating_sub(move_overhead);
                soft = ms;
                hard = ms;
            }
            Limits::Fischer(main, inc) => {
                let soft_scale = 0.0594 - 0.0492 * (-0.0386 * fullmove_number as f64).exp();
                let hard_scale = 0.7281;

                let soft_bound = (soft_scale * main.saturating_sub(move_overhead) as f64 + 0.75 * inc as f64) as u64;
                let hard_bound = (hard_scale * main.saturating_sub(move_overhead) as f64 + 0.75 * inc as f64) as u64;

                soft = soft_bound.min(main.saturating_sub(move_overhead));
                hard = hard_bound.min(main.saturating_sub(move_overhead));
            }
            Limits::Cyclic(main, inc, moves) => {
                let main = main.saturating_sub(move_overhead);
                let base = (main as f64 / moves as f64) + 0.75 * inc as f64;

                // At movestogo 1 the increment is only credited *after* this
                // move is played, so it isn't part of the pool actually
                // available for it -- crediting it here let hard exceed the
                // real remaining clock whenever inc was large enough to flag.
                let pool = if moves > 1 { main + inc } else { main };

                soft = ((1.0 * base) as u64).min(pool);
                // With a small movestogo, 5x base already exceeds the pool,
                // so the clamp alone made hard consume the entire remaining
                // clock regardless of the 5x multiplier -- even though more
                // moves are due before the control replenishes. Reserving
                // one more base-sized allocation (when there's a next move
                // to reserve it for) keeps the hard bound from spending
                // everything on a single move.
                let reserve = if moves > 1 { base as u64 } else { 0 };
                hard = ((5.0 * base) as u64).min(pool.saturating_sub(reserve));
                // The reserve must never push hard below soft: that would
                // make the hard cutoff fire first and silently disable the
                // entire soft/hard split (and the multiplier() extension
                // logic that depends on it) for these moves.
                hard = hard.max(soft);
            }
            _ => {
                soft = u64::MAX;
                hard = u64::MAX;
            }
        }

        Self {
            limits,
            start_time: Instant::now(),
            soft_bound: Duration::from_millis(soft.saturating_sub(TIME_OVERHEAD_MS)),
            hard_bound: Duration::from_millis(hard.saturating_sub(TIME_OVERHEAD_MS)),
        }
    }

    pub fn elapsed(&self) -> Duration {
        self.start_time.elapsed()
    }

    /// Time spent on our own clock: while pondering the clock has not started,
    /// and after a `ponderhit` it runs from the moment the hit was received.
    fn search_elapsed(&self, td: &ThreadData) -> Duration {
        match *td.shared.ponderhit_time.lock().unwrap() {
            Some(ponderhit) => ponderhit.elapsed(),
            None => self.start_time.elapsed(),
        }
    }

    pub fn soft_limit(&self, td: &ThreadData, multiplier: impl Fn() -> f32) -> bool {
        if td.shared.ponder.load(std::sync::atomic::Ordering::Acquire) {
            return false;
        }

        match self.limits {
            Limits::Infinite | Limits::Depth(_) | Limits::Mate(_) => false,
            Limits::Nodes(maximum) => td.shared.nodes.aggregate() >= maximum,
            // Compared against `soft_bound`, not the raw `maximum`. `soft_bound`
            // is `maximum - move_overhead - TIME_OVERHEAD_MS`, and `hard_bound`
            // is the same value, so testing the raw figure here meant the hard
            // limit always fired first and this branch never decided anything --
            // taking `go movetime` out of the soft/hard split and out of the
            // `multiplier()` extension logic entirely.
            Limits::Time(_) => self.search_elapsed(td) >= self.soft_bound,
            _ => {
                // The multiplier is a product of five independent factors, none
                // of which is bounded jointly with the others. Its ceiling is
                // ~8.4x with a perfectly stable best move and ~16-18x once the
                // best move has changed a few times.
                //
                // The Fischer hard/soft ratio is 0.7281/soft_scale, and
                // soft_scale rises with move number: 28x at move 10, 19.9x at
                // move 20, 16.6x at move 30, 13.3x at move 60. So from the
                // middlegame onward an unstable position produces a scaled soft
                // bound that sits *past* the hard bound -- the soft limit stops
                // binding entirely and the hard bound decides the move.
                //
                // That is the worst of both: the soft/hard split silently
                // disables itself exactly when instability means it matters
                // most, and because the hard bound is polled mid-search rather
                // than between iterations, the cutoff lands part-way through an
                // iteration and that partial work is discarded.
                //
                // Clamping to the hard bound keeps the stop on an iteration
                // boundary and preserves the split at every move number. It
                // cannot make the engine think longer than it already would.
                let scaled = self.soft_bound.as_secs_f32() * multiplier();
                let capped = scaled.min(self.hard_bound.as_secs_f32());
                self.search_elapsed(td) >= Duration::from_secs_f32(capped)
            }
        }
    }

    pub fn check_time(&self, td: &ThreadData) -> bool {
        // Depth 1 used to be uninterruptible: this returned `false` outright
        // until an iteration had completed, so the HARD bound -- the one that
        // exists precisely to stop us forfeiting -- could not fire during the
        // one iteration that has no TT, no move ordering and the widest root
        // list. On a pathological position that is exactly where a search can
        // sit for far longer than its allowance.
        //
        // The guard is still needed in spirit: aborting before anything is
        // scored leaves no legal `bestmove` to emit. But that is a question of
        // whether a root move has a SCORE, not of whether a whole iteration
        // finished. Once the root has picked up a move we can always answer, so
        // from that point the hard bound must be allowed to do its job.
        if td.completed_depth == 0 && !td.root_moves.iter().any(|rm| rm.score != -Score::INFINITE) {
            return false;
        }

        if td.shared.ponder.load(std::sync::atomic::Ordering::Acquire) {
            return false;
        }

        match self.limits {
            Limits::Infinite | Limits::Depth(_) | Limits::Mate(_) => false,
            // Gated behind the same periodic mask as the other branches:
            // aggregate() sums every shard in Counter (at least 512, see
            // ThreadPool::available_threads), so calling it unconditionally
            // on every node was a severe NPS penalty specific to node-limited
            // search -- exactly the mode used for deterministic SPRT/self-play
            // testing.
            //
            // The thread-local count is cheap, so it also gates the case the
            // mask alone would miss: with a limit under 2048 the mask never
            // fires before the limit is passed, and the search would overrun
            // it entirely -- upstream polls `aggregate()` every node and has
            // no such gap.
            Limits::Nodes(maximum) => {
                // `>=`, matching `soft_limit` above. The two disagreed by one
                // node, which under a node limit -- the mode used for
                // deterministic self-play -- is exactly the kind of off-by-one
                // that makes a "reproducible" run not reproduce.
                (td.nodes() >= maximum || td.nodes() & 2047 == 2047) && td.shared.nodes.aggregate() >= maximum
            }
            _ => td.nodes() & 2047 == 2047 && self.search_elapsed(td) >= self.hard_bound,
        }
    }

    pub fn limits(&self) -> Limits {
        self.limits.clone()
    }

    pub fn use_time_management(&self) -> bool {
        matches!(self.limits, Limits::Fischer(..) | Limits::Cyclic(..) | Limits::Time(_))
    }

    /// Whether time saved now can be spent on a later move.
    ///
    /// Deliberately excludes `Limits::Time` (`go movetime`). Under a movetime
    /// there is no clock to bank into: the allowance belongs to this move and
    /// nothing carries forward, so stopping early does not buy anything -- it
    /// just returns a shallower answer than the GUI asked for. That matters for
    /// analysis and for test harnesses that drive fixed-movetime searches.
    ///
    /// `use_time_management` is still the right test for whether a soft limit
    /// applies at all; this one is only for the "stop early and pocket the
    /// difference" decisions.
    pub fn can_bank_time(&self) -> bool {
        matches!(self.limits, Limits::Fischer(..) | Limits::Cyclic(..))
    }
}