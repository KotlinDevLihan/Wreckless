use std::sync::atomic::Ordering;

use crate::{
    evaluation::correct_eval,
    history::{LowPlyHistory, PieceToHistory},
    movepick::{MovePicker, Stage},
    parameters as p,
    thread::{PlyArray, RootMove, Status, ThreadData},
    time::Limits,
    transposition::{Bound, TtDepth},
    types::{
        ArrayVec, Color, MAX_PLY, Move, Piece, PieceType, Score, Square, draw, is_decisive, is_loss, is_valid, is_win,
        mate_in, mated_in,
    },
};

#[cfg(feature = "syzygy")]
use crate::{
    tb,
    types::{tb_loss_in, tb_win_in},
};

#[allow(unused_imports)]
use crate::misc::{dbg_hit, dbg_stats};

#[derive(Copy, Clone, Eq, PartialEq)]
pub enum Report {
    None,
    Minimal,
    Full,
}

pub trait NodeType {
    const PV: bool;
    const ROOT: bool;
}

struct Root;
impl NodeType for Root {
    const PV: bool = true;
    const ROOT: bool = true;
}

struct PV;
impl NodeType for PV {
    const PV: bool = true;
    const ROOT: bool = false;
}

struct NonPV;
impl NodeType for NonPV {
    const PV: bool = false;
    const ROOT: bool = false;
}

pub fn start(td: &mut ThreadData, report: Report, thread_count: usize) {
    td.completed_depth = 0;
    td.low_ply_history.shift();

    // Carried over from the *previous* `go` otherwise. Two plies have been
    // played since it was written, so `previous_pv[ply]` no longer lines up
    // with the position at that ply, and `follow_pv` matches moves belonging to
    // a different line -- handing IIR's exemption to the wrong nodes for the
    // first iteration or two of every search after the first. It is refilled
    // from `root_moves[0]` at the end of each completed depth, so clearing here
    // costs nothing beyond depth 1 of the first iteration.
    td.previous_pv.clear();

    td.pv_table.clear(0);
    td.nnue.full_refresh(&td.board);

    td.multi_pv = td.multi_pv.min(td.root_moves.len());

    // `previous_best_score` is whatever `root_moves[0].score` held when the last
    // search ended, and `RootMove::default()` seeds that at `-Score::INFINITE`.
    // A search stopped before any root move scored therefore leaves a sentinel
    // behind, and it centres the next search's first aspiration window on
    // -32001: the `average^2 / 26394` term below becomes 38799, so the window
    // opens at [-32001, 6798] -- entirely below the true score and useless for
    // an iteration or two.
    //
    // `is_valid` does not catch this: it only tests against `Score::NONE`, and
    // `-Score::INFINITE` passes. Testing the magnitude does. Mate scores are
    // deliberately left alone -- they widen the window, which is wanted when the
    // previous search found a forced line.
    let centre = if td.previous_best_score.abs() < Score::INFINITE { td.previous_best_score } else { Score::ZERO };
    let mut average = vec![centre; td.multi_pv];

    // `average` is seeded with `centre` so that every reader before the first
    // completed depth has a usable value -- which also made the
    // `== Score::NONE` test at the update site unreachable, so depth 1's result
    // was blended with the PREVIOUS search's final score instead of replacing
    // it. Every window for the rest of the search then centred on a stale value
    // decaying toward the truth one bit per iteration.
    //
    // Tracked explicitly rather than by re-seeding with `Score::NONE`, because
    // `best_avg` below reads `average` unconditionally and would inherit the
    // sentinel.
    let mut average_seeded = vec![false; td.multi_pv];

    // Iterations this thread has actually completed; see the `iter_values` note
    // below. Not the same as `depth` once a helper starts skipping.
    let mut iters_done = 0usize;
    let mut last_best_rootmove = RootMove::default();

    let mut eval_stability = 0;
    let mut pv_stability = 0;
    let mut soft_stop_voted = false;
    // Last iteration's time multiplier, so skipped depths can still vote.
    let mut last_multiplier = 1.0f32;

    // Ring buffer of best scores from recent iterations, so the time manager
    // can compare against the score four iterations ago (as in Stockfish).
    // Same sentinel guard as `centre` above: this seeds the time manager's
    // two-horizon `recent` term, and `-Score::INFINITE` there pegs the trend
    // factor to its clamp for the first four iterations.
    let mut iter_values = [centre; 4];

    if td.root_moves.is_empty() {
        if report == Report::Full {
            td.print_uci_info(0);
        }
        return;
    }

    // Lazy SMP thread differentiation.
    //
    // Every thread ran `1..MAX_PLY` identically, and the *only* thing making
    // helpers diverge was the `td.id`-seeded jitter on the LMR reduction. That
    // desynchronises which lines get reduced, but it does not stop every thread
    // arriving at the same depth at roughly the same time and re-deriving the
    // same iteration. Threads that reach different depths at different times
    // put a wider spread of depths and bounds into the shared TT, which is the
    // entire mechanism by which Lazy SMP gains anything.
    //
    // This is the classic skip-phase/skip-size schedule: helper `i` searches
    // only depths where `(depth + ply + phase[i]) % size[i] == 0`, so different
    // helpers walk different subsequences and none of them tracks thread 0.
    // Mixing in `fullmove_number` keeps a given helper from drawing the same
    // subsequence on every move of the game.
    //
    // Thread 0 is deliberately excluded: it reports the move, it owns the time
    // manager, and both early exits above depend on it seeing every depth.
    // Every (size, phase) pair is distinct and `phase < size`. Stockfish's
    // historical table contains pairs where `phase >= size`, which alias onto
    // `phase % size` -- with that table threads 3 and 5 draw an identical
    // subsequence and duplicate each other's work outright, which is precisely
    // what this schedule exists to prevent.
    const SKIP_SIZE: [i32; 20] = [1, 2, 2, 3, 3, 3, 1, 2, 2, 3, 3, 3, 1, 2, 2, 3, 3, 3, 1, 2];
    const SKIP_PHASE: [i32; 20] = [0, 0, 1, 0, 1, 2, 0, 0, 1, 0, 1, 2, 0, 0, 1, 0, 1, 2, 0, 0];

    // Iterative Deepening
    for depth in 1..MAX_PLY as i32 {
        // This used to be a `continue` straight past the loop body, which took
        // the soft-stop vote block with it. The stop needs a 65% majority and
        // votes are cast only at iteration boundaries, so a helper inside one
        // of the longer iterations this schedule creates could neither cast nor
        // retract a vote; with enough helpers stalled the majority never landed,
        // thread 0 sailed past its soft bound, and only the hard bound stopped
        // it -- 72.8% of the remaining clock on one move. The skip path now
        // votes before continuing, using the multiplier from its last real
        // iteration, so vote cadence no longer depends on the schedule.
        //
        // Skip sizes stay capped at 3 regardless: a size-6 helper jumping depth
        // 20 -> 26 does ~64x the work of one iteration at EBF 2, which is a long
        // time to hold a stale vote even though it can now hold one at all.
        if td.id > 0 && p::lazy_smp_skip() > 0 {
            let i = (td.id - 1) % SKIP_SIZE.len();
            let size = SKIP_SIZE[i];
            if size > 1 && (depth + td.board.fullmove_number() as i32 + SKIP_PHASE[i]) % size != 0 {
                // Vote before skipping. A skipped depth does no work, but the
                // clock still runs, and a thread that never reaches an
                // iteration boundary can neither cast nor retract a vote --
                // which is what let the 65% majority stall. Its search state is
                // unchanged since its last real iteration, so that iteration's
                // multiplier is the correct stand-in.
                soft_stop_vote(td, thread_count, &mut soft_stop_voted, last_multiplier);

                if td.shared.status.get() == Status::STOPPED {
                    break;
                }

                continue;
            }
        }

        if td.id == 0
            && let Limits::Depth(maximum) = td.time_manager.limits()
            && depth > maximum
        {
            td.shared.status.set(Status::STOPPED);
            break;
        }

        td.sel_depth = 0;
        td.root_depth = depth;
        td.best_move_changes = 0;

        // A root fail-low means the move we have been playing for is worse than
        // the last iteration promised and we do not yet have a replacement.
        // That is the single most valuable moment to keep thinking, and it was
        // the one instability signal the multiplier did not see: PV stability,
        // eval stability and best-move changes all describe how the answer is
        // moving, none of them that the answer just got *worse*.
        let mut root_fail_lows = 0;

        td.pv_start = 0;
        td.pv_end = 0;

        for rm in &mut td.root_moves {
            rm.previous_score = rm.score;
        }

        for index in 0..td.multi_pv {
            td.pv_index = index;

            // Aspiration window floor: keep a minimum delta so very stable
            // positions still search wide enough to catch a sudden tactical
            // shift, rather than triggering hairline-window re-searches.
            //
            // Both of these are per-PV-line state, and are reset here rather
            // than once per depth -- Stockfish likewise resets its own
            // failedHighCnt inside the pvIdx loop. Declared outside it, `delta`
            // accumulated the `average^2` term plus every window widening from
            // earlier PV lines, and `reduction` kept every earlier line's
            // fail-high depth cut, so under MultiPV each successive line was
            // searched with a wider window and at a shallower depth than the
            // one before it. Single-PV search runs this body once and is
            // unaffected.
            let mut delta = (p::asp_delta_base() - eval_stability.min(pv_stability).min(p::asp_delta_stab_cap())).max(p::asp_delta_floor());
            let mut reduction = 0;

            if td.pv_index == td.pv_end {
                td.pv_start = td.pv_end;
                while td.pv_end < td.root_moves.len() {
                    if td.root_moves[td.pv_end].tb_rank != td.root_moves[td.pv_start].tb_rank {
                        break;
                    }
                    td.pv_end += 1;
                }
            }

            // Aspiration Windows
            delta += average[td.pv_index] * average[td.pv_index] / 26394;

            let mut alpha = (average[td.pv_index] - delta).max(-Score::INFINITE);
            let mut beta = (average[td.pv_index] + delta).min(Score::INFINITE);

            let best_avg = ((td.shared.best_stats[td.pv_index].load(Ordering::Acquire) & 0xffff) as i32 - 32768
                + average[td.pv_index])
                / 2;
            td.optimism[td.board.side_to_move()] = 113 * best_avg / (best_avg.abs() + 201);
            td.optimism[!td.board.side_to_move()] = -td.optimism[td.board.side_to_move()];

            loop {
                td.stack.reset();
                td.stack[0].follow_pv = true;
                td.cutoff_count = PlyArray::default();
                td.excluded = PlyArray::default();
                td.root_delta = beta - alpha;

                // Root Search
                let score = search::<Root>(td, alpha, beta, (depth - reduction).max(1), false, 0);

                td.root_moves[td.pv_index..td.pv_end].sort_by_key(|rm| std::cmp::Reverse(rm.score));

                if td.shared.status.get() == Status::STOPPED {
                    break;
                }

                match score {
                    s if s <= alpha => {
                        // Re-centre the window on the score that failed low:
                        // [score - delta, score]. Anchoring beta to the *new*
                        // alpha keeps every re-search exactly `delta` wide.
                        //
                        // The fork previously collapsed beta to the old alpha
                        // instead, which is 1.5x wider on a shallow fail-low
                        // and 3x wider on a deep one -- the opposite of the
                        // "keeps re-searches narrow" rationale it carried.
                        alpha = (score - delta).max(-Score::INFINITE);
                        beta = (alpha + delta).min(beta);
                        delta += p::asp_widen_num() * delta / 128;
                        root_fail_lows += 1;
                    }
                    s if s >= beta => {
                        alpha = (beta - delta).max(alpha);
                        beta = (score + delta).min(Score::INFINITE);
                        if !is_decisive(score) {
                            reduction += 1;
                        } else {
                            reduction = reduction.min(1)
                        }
                        delta += 60 * delta / 128;
                    }
                    _ => {
                        average[td.pv_index] = if !average_seeded[td.pv_index] {
                            average_seeded[td.pv_index] = true;
                            score
                        } else {
                            (average[td.pv_index] + score) / 2
                        };

                        td.shared.best_stats[td.pv_index].fetch_max(
                            ((depth as u32) << 16) | (average[td.pv_index] + 32768) as u32,
                            Ordering::AcqRel,
                        );

                        break;
                    }
                }

                td.root_moves[td.pv_start..=td.pv_index].sort_by_key(|rm| std::cmp::Reverse(rm.score));

                if report == Report::Full && td.shared.nodes.aggregate() > 10_000_000 {
                    td.print_uci_info(depth);
                }
            }
        }

        if td.shared.status.get() != Status::STOPPED {
            td.completed_depth = depth;

            // `td.pv_table.line()` reads slot 0, but the root deliberately
            // never writes its own slot -- `pv_table.update()` is called only
            // under `!NODE::ROOT`, and `commit_full_root_pv` writes each root
            // move's *own* table, not this one. So `line()` was always empty
            // here, `previous_pv` stayed empty for the entire search, every
            // `previous_pv.get(ply)` in `make_move` missed, and `follow_pv`
            // was false at every ply below the root.
            //
            // That silently disabled the Internal Iterative Reductions
            // exemption: nodes on the previous iteration's principal variation
            // were supposed to keep their full depth, and instead every one of
            // them was reduced like any other. The per-node cost of
            // maintaining `follow_pv` was still being paid for no effect.
            //
            // The line has to lead with the root move itself, because
            // `make_move` at ply p tests `previous_pv[p]` -- at the root that
            // is the root move, and `RootMove::pv` only holds the
            // continuation from ply 1 onwards.
            td.previous_pv.clear();
            td.previous_pv.push(td.root_moves[0].mv);
            td.previous_pv.extend_from_slice(td.root_moves[0].pv.line());
        }

        if (td.root_moves[0].score - average[0]).abs() < 12 {
            eval_stability += 1;
        } else {
            eval_stability = 0;
        }

        if last_best_rootmove.mv == td.root_moves[0].mv {
            pv_stability += 1;
        } else {
            pv_stability = 0;
        }

        let last_score = last_best_rootmove.score;

        let is_forgotten_mate = last_score != -Score::INFINITE
            && is_decisive(last_score)
            && (td.root_moves[0].score.abs() < last_score.abs()
                || td.root_moves[0].upperbound
                || td.root_moves[0].lowerbound);

        let is_aborted_loss = td.shared.status.get() == Status::STOPPED
            && td.root_moves[0].score != -Score::INFINITE
            && is_loss(td.root_moves[0].score)
            && !td.root_moves[0].upperbound
            && !td.root_moves[0].lowerbound;

        if is_aborted_loss || is_forgotten_mate {
            if let Some(pos) = td.root_moves.iter().position(|rm| rm.mv == last_best_rootmove.mv) {
                td.root_moves.remove(pos);
                td.root_moves.insert(0, last_best_rootmove.clone());
                td.root_moves[0].upperbound = false;
                td.root_moves[0].lowerbound = false;
            } else if is_aborted_loss {
                td.root_moves[0].lowerbound = true;
            }
        } else if td.shared.status.get() != Status::STOPPED {
            last_best_rootmove = td.root_moves[0].clone();
        }

        if report == Report::Full
            && !(is_loss(td.root_moves[0].display_score) && td.shared.status.get() == Status::STOPPED)
            && (td.shared.status.get() == Status::STOPPED
                || td.pv_index + 1 == td.multi_pv
                || td.shared.nodes.aggregate() > 10_000_000)
        {
            td.print_uci_info(depth);
        }

        if td.shared.status.get() == Status::STOPPED {
            break;
        }

        // `.abs()` here meant a mate *against* us satisfied the condition just as
        // well as one we had found, so `go mate 3` in a lost position stopped
        // and reported the mate being delivered to us as the answer. UCI asks
        // for a mate in N, not for any decisive line within N; searching on is
        // the same thing that already happens when no mate exists at all.
        if td.id == 0
            && let Limits::Mate(moves) = td.time_manager.limits()
            && is_win(td.root_moves[0].score)
            && Score::MATE - td.root_moves[0].score <= moves as i32 * 2
        {
            td.shared.status.set(Status::STOPPED);
            break;
        }

        // Indexed by ITERATIONS COMPLETED, not by absolute depth.
        //
        // `depth % 4` is a four-iteration lookback only while a thread walks
        // every depth. Under `lazy_smp_skip` a helper skips depths, so the slot
        // it lands on was written 8 or 12 plies ago and the score-trend term
        // compares against a much staler value than it believes -- silently, and
        // only on helper threads. Counting completed iterations makes the
        // lookback four *of this thread's own* iterations whatever schedule it
        // is following.
        //
        // Identical behaviour when the skip schedule is off, since the counter
        // then advances in lockstep with depth.
        let iter_value = iter_values[iters_done % 4];
        iter_values[iters_done % 4] = td.root_moves[0].score;
        iters_done += 1;

        let multiplier = || {
            let nodes = {
                let fraction = td.root_moves[0].nodes as f32 / td.nodes() as f32;
                (3.1838 - 2.6554 * fraction).max(0.5460)
            };

            let score_trend = {
                let difference = centre - td.root_moves[0].score;
                let recent = iter_value - td.root_moves[0].score;

                // The linear structure is Stockfish's (their 2.035:0.968
                // coefficient ratio is preserved), but SF's Value scale puts a
                // pawn at ~208 where ours is ~321-382.
                //
                // BE CLEAR ABOUT WHAT THIS DOES AS SHIPPED. An earlier revision
                // rescaled these constants so the ceiling sat ~0.37 pawns out,
                // matching SF's band in pawn terms; that was reverted in favour
                // of upstream's untouched fixed-point values, and this comment
                // went on describing the rescale that no longer exists. With
                // base 7426, diff 480 and the [7214, 14031] clamp, the ceiling
                // is reached after a 13.8-unit (~0.04 pawn) drop and the floor
                // after 0.4 units -- i.e. on any gain at all. It is a bang-bang
                // switch with essentially no proportional band, and
                // `tm_trend_recent` adds a second saturating term on top of an
                // already-saturated one.
                //
                // Left at upstream's values because those are the measured ones
                // and this is a time-management path where a bad constant costs
                // games rather than nodes. Reaching the SF-equivalent band would
                // mean roughly diff 51 / recent 25; that is a change to test,
                // not to assume, and it explains why SPSA has never found signal
                // here -- there is no gradient to find inside the clamp.
                //
                // Held in fixed point (1e-4) rather than f32 so the constants
                // can live in `parameters.rs` and actually be tuned now that
                // there is a gradient to tune against. The integer values are
                // exactly the old floats' digits, so this reintroduces no
                // rounding of its own.
                // Bounds ordered before clamping. `i32::clamp` PANICS when
                // min > max, and these two are independently tuned parameters
                // whose declared SPSA ranges overlap: `tm_trend_min` reaches
                // 10821 and `tm_trend_max` starts at 7016. A tuning run that
                // lands anywhere in [7016, 10821] on both would abort the engine
                // mid-search -- which in a game means a forfeit, and in a tuning
                // run means a corrupted result nobody attributes to a crash.
                //
                // This is the only clamp in the engine with that exposure: every
                // other tunable pair is either structurally ordered (an all-negative
                // `lo` against an all-positive `hi`) or clamps against a literal.
                let trend_lo = p::tm_trend_min();
                let trend_hi = p::tm_trend_max().max(trend_lo);

                let trend = (p::tm_trend_base()
                    + p::tm_trend_diff() * difference
                    + p::tm_trend_recent() * recent)
                    .clamp(trend_lo, trend_hi);

                trend as f32 / 10000.0
            };

            let pv_stability = (1.2881 - 0.0440 * pv_stability as f32).max(0.7160);

            let eval_stability = (1.2664 - 0.0416 * eval_stability as f32).max(0.8642);

            let best_move_stability = 1.1500 + (0.2526 * td.best_move_changes as f32).ln_1p();

            // Capped: the first fail-low carries nearly all the information,
            // and repeated ones at the same depth are the aspiration window
            // widening, not new evidence.
            let fail_low = 1.0
                + p::tm_fail_low() as f32 / 1000.0
                    * root_fail_lows.min(p::tm_fail_low_cap()) as f32;

            nodes * pv_stability * eval_stability * score_trend * best_move_stability * fail_low
        };

        // A proven forced mate needs no further thought: any forced mate wins,
        // so a shorter one found two iterations later is worth nothing on the
        // clock. Nothing stopped for this before -- the only mate-aware exit is
        // the `go mate` branch above -- and the ordinary damping signals barely
        // notice. At a freshly-proven mate the node fraction collapses to ~0.66
        // and `score_trend` pins to its 0.7214 floor, but `pv_stability`,
        // `eval_stability` (which *resets* on the eval jump) and best-move
        // stability push back, leaving ~0.83x of a full allocation spent on a
        // mate in 1.
        //
        // Guards: wins only, since being mated is exactly when to keep looking
        // for a defence; true mate scores only, not TB wins, hence
        // `MATE_IN_MAX` rather than `is_win`; an exact score, since a bound is
        // an artifact of an unresolved window; and single-PV only, because
        // analysis asked for the other lines. `depth` must clear the mate
        // distance by `tm_mate_confirm` so one iteration's fail-high cannot end
        // the search on its own.
        // Thread 0 is the one that reports the move and there is no best-thread
        // vote, so a helper proving the mate and stopping everyone could leave
        // thread 0 emitting whatever it happened to have -- not the mate. Only
        // the reporting thread may end the search on its own result.
        // `!pondering`: both this and the single-move exit below exist to bank
        // clock, and pondering spends no clock. Firing them during a ponder
        // ends the search early and leaves the thread idle until `ponderhit`,
        // throwing away TT and accumulator work for a position we may well
        // reach -- a pure loss, and one that only shows up with Ponder on.
        if td.id == 0
            && !td.shared.ponder.load(Ordering::Acquire)
            && td.time_manager.can_bank_time()
            && td.multi_pv == 1
            && td.root_moves[0].score >= Score::MATE_IN_MAX
            && !td.root_moves[0].upperbound
            && !td.root_moves[0].lowerbound
        {
            let mate_plies = Score::MATE - td.root_moves[0].score;
            if mate_plies <= 2 * p::tm_mate_moves() && depth >= mate_plies + p::tm_mate_confirm() {
                td.shared.status.set(Status::STOPPED);
                break;
            }
        }

        // With one legal move there is nothing to choose between, and every
        // further iteration buys only a deeper ponder move. Kept searching to a
        // small floor so a ponder move and a sane score still exist, then stop
        // and bank the clock -- forced recaptures and single-reply checks are
        // common enough in real games for this to be worth real time.
        if td.id == 0
            && !td.shared.ponder.load(Ordering::Acquire)
            && p::tm_single_move_depth() > 0
            && td.time_manager.can_bank_time()
            && td.root_moves.len() == 1
            && depth >= p::tm_single_move_depth()
        {
            td.shared.status.set(Status::STOPPED);
            break;
        }

        // Evaluated eagerly and cached so a subsequent skipped depth can vote
        // with this thread's most recent view; it is a handful of float ops.
        last_multiplier = multiplier();
        soft_stop_vote(td, thread_count, &mut soft_stop_voted, last_multiplier);

        if td.shared.status.get() == Status::STOPPED {
            break;
        }
    }

    if matches!(td.time_manager.limits(), Limits::Infinite) {
        // Sleeps rather than spins. `go infinite` holds every thread here until
        // the GUI sends `stop`, which under analysis is minutes -- one pinned
        // core per thread doing nothing, heating the machine and starving
        // whatever else the user is running.
        //
        // A millisecond of latency on `stop` is irrelevant next to the round trip
        // that produced it, and `status` is polled, not waited on, so there is
        // nothing to miss.
        while td.shared.status.get() != Status::STOPPED {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    if report == Report::Minimal {
        td.print_uci_info(td.root_depth);
    }

    td.previous_best_score = td.root_moves[0].score;
}

fn search<NODE: NodeType>(
    td: &mut ThreadData, mut alpha: i32, mut beta: i32, depth: i32, cut_node: bool, ply: isize,
) -> i32 {
    debug_assert!(ply as usize <= MAX_PLY);
    debug_assert!(-Score::INFINITE <= alpha && alpha < beta && beta <= Score::INFINITE);
    debug_assert!(NODE::PV || alpha == beta - 1);

    let stm = td.board.side_to_move();
    let in_check = td.board.in_check();
    let excluded = td.excluded[ply].is_present();

    if !NODE::ROOT && NODE::PV {
        td.pv_table.clear(ply as usize);
    }

    if td.shared.status.get() == Status::STOPPED {
        return Score::ZERO;
    }

    // Qsearch Dive
    if depth <= 0 {
        return qsearch::<NODE>(td, alpha, beta, ply);
    }

    let draw_score = draw(td);
    if !NODE::ROOT && alpha < draw_score && td.board.upcoming_repetition(ply as usize) {
        alpha = draw_score;
        if alpha >= beta {
            return alpha;
        }
    }

    if NODE::PV {
        td.sel_depth = td.sel_depth.max(ply as i32);
    }

    // Thread 0 polls the clock; helpers back it up on a coarser interval.
    //
    // Gating this on `td.id == 0` alone meant that if the main thread was
    // descheduled -- routine under the concurrency an SPRT harness runs at --
    // NOTHING enforced the hard bound until it was scheduled again. The helpers
    // were searching happily with no way to end the move.
    //
    // `nodes & 16383` implies `nodes & 2047` (check_time's own mask), so helpers
    // reach the elapsed-time test roughly one eighth as often as thread 0: enough
    // to stop a forfeit, rare enough that N threads are not contending on the
    // clock mutex.
    if (td.id == 0 || td.nodes() & 16383 == 16383) && td.time_manager.check_time(td) {
        td.shared.status.set(Status::STOPPED);
        return Score::ZERO;
    }

    if !NODE::ROOT {
        if td.board.is_draw(ply) {
            return draw(td);
        }

        if ply as usize >= MAX_PLY - 1 {
            return if in_check { draw(td) } else { td.nnue.evaluate(&td.board) };
        }

        // Mate Distance Pruning (MDP)
        alpha = alpha.max(mated_in(ply));
        beta = beta.min(mate_in(ply + 1));

        if alpha >= beta {
            return alpha;
        }
    }

    #[cfg(feature = "syzygy")]
    let mut max_score = Score::INFINITE;

    let mut best_score = -Score::INFINITE;

    let mut depth = depth.min(MAX_PLY as i32 - 1);

    // Computed before the TT probe so the work overlaps the prefetched cache
    // line's arrival instead of serializing after the lookup.
    //
    // SHARED SIGNAL -- read two different ways, which is deliberate:
    //
    //   signed, via `correct_eval`: folded into `eval` itself, so every test
    //     comparing `eval` against alpha/beta already carries it.
    //   magnitude, via `.abs()`: razoring, RFP, futility, both singular
    //     margins, LMR, FDS and qsearch SEE each add their own term.
    //
    // These are not double-counting. The signed value says which way the
    // evaluation is wrong; the magnitude says how unsure we are, and margins
    // widen with uncertainty regardless of direction. Removing the apparent
    // duplication would leave eight margins with no confidence input at all.
    //
    // What does need care is scale: `corr_weight_div` divides the blend, so
    // changing how many tables `eval_correction` sums rescales all eight
    // consumers at once -- the defect the material/minor/major tables caused
    // once already (see `corr_weight_div` in parameters.rs).
    let correction_value = eval_correction(td, ply);

    let hash = td.board.hash();
    let entry = td.shared.tt.read(hash, td.board.fiftymove_clock(), ply);

    let mut tt_depth = 0;
    let mut tt_move = Move::NULL;
    let mut tt_score = Score::NONE;
    let mut tt_bound = Bound::None;
    let mut tt_pv = NODE::PV;
    let mut tt_was_pv = false;

    // Search early TT cutoff
    if let Some(entry) = &entry {
        tt_depth = entry.depth;
        tt_move = entry.mv;
        tt_score = entry.score;
        tt_bound = entry.bound;
        tt_pv |= entry.tt_pv;
        tt_was_pv = entry.tt_pv;

        if !NODE::PV
            && !excluded
            && tt_depth > depth - (tt_score < beta) as i32
            && is_valid(tt_score)
            && match tt_bound {
                Bound::Upper => tt_score <= alpha && (!cut_node || depth > 5),
                Bound::Lower => tt_score >= beta && (cut_node || depth > 5),
                _ => true,
            }
        {
            // Both the bonus and the return live under the fifty-move guard.
            //
            // The bonus rewards `tt_move` for producing a cutoff, so it is only
            // earned if the cutoff actually happens. Above a clock of 90 the
            // return is declined -- the cached score is no longer trustworthy
            // that close to the draw -- and crediting the move anyway paid for a
            // cutoff that did not occur, on a node that then went on to search
            // normally and apply its own history updates on top.
            if td.board.fiftymove_clock() < 90 {
                if tt_move.is_quiet() && tt_score >= beta && td.stack[ply - 1].move_count < 4 {
                    let quiet_bonus = (p::ttcut_quiet_slope() * depth - 81).min(p::ttcut_quiet_cap());
                    let cont_bonus = (p::ttcut_cont_slope() * depth - 73).min(p::ttcut_cont_cap());

                    td.quiet_history.update(td.board.all_threats(), stm, tt_move, quiet_bonus);
                    update_continuation_histories_in_check(
                        td,
                        ply,
                        td.board.moved_piece(tt_move),
                        tt_move.to(),
                        cont_bonus,
                        in_check,
                    );
                }

                return tt_score;
            }
        }
    }

    // Tablebases Probe
    #[cfg(feature = "syzygy")]
    if !NODE::ROOT
        && !excluded
        && !td.shared.stop_probing_tb.load(Ordering::Relaxed)
        && td.board.fiftymove_clock() == 0
        && td.board.castling().raw() == 0
        && {
            // Engage tablebases per SyzygyProbeLimit/SyzygyProbeDepth: probe at
            // the piece-count boundary only from the configured depth onwards.
            let cardinality = tb::size().min(td.shared.syzygy_probe_limit.load(Ordering::Relaxed));
            let pieces = td.board.occupancies().popcount();
            pieces <= cardinality
                && (pieces < cardinality || depth >= td.shared.syzygy_probe_depth.load(Ordering::Relaxed))
        }
        && let Some(outcome) = tb::probe(&td.board)
    {
        td.shared.tb_hits.increment(td.id);

        let (score, bound) = match outcome {
            tb::GameOutcome::Win => (tb_win_in(ply), Bound::Lower),
            tb::GameOutcome::Loss => (tb_loss_in(ply), Bound::Upper),
            tb::GameOutcome::Draw => (Score::ZERO, Bound::Exact),
        };

        if bound == Bound::Exact
            || (bound == Bound::Lower && score >= beta)
            || (bound == Bound::Upper && score <= alpha)
        {
            let depth = (depth + 6).min(MAX_PLY as i32 - 1);
            td.shared.tt.write(hash, depth, Score::NONE, score, bound, Move::NULL, ply, tt_pv, false);
            return score;
        }

        if NODE::PV {
            if bound == Bound::Lower {
                best_score = score;
                alpha = alpha.max(best_score);
            } else {
                max_score = score;
            }
        }
    }

    let raw_eval;
    let eval;

    // Evaluation
    if in_check {
        raw_eval = Score::NONE;
        eval = Score::NONE;
    } else if excluded {
        raw_eval = Score::NONE;
        eval = td.stack[ply].eval;
    } else if let Some(entry) = &entry {
        raw_eval = if is_valid(entry.raw_eval) { entry.raw_eval } else { td.nnue.evaluate(&td.board) };
        eval = correct_eval(td, raw_eval, correction_value);
    } else {
        raw_eval = td.nnue.evaluate(&td.board);
        eval = correct_eval(td, raw_eval, correction_value);

        td.shared.tt.write(hash, TtDepth::SOME, raw_eval, Score::NONE, Bound::None, Move::NULL, ply, tt_pv, false);
    }

    // Prefer the TT entry to tighten the evaluation when its bound aligns with
    // the current alpha-beta window; otherwise, retain the unbounded evaluation
    let estimated_score = if !in_check
        && !excluded
        && is_valid(tt_score)
        && match tt_bound {
            Bound::Upper => tt_score < eval,
            Bound::Lower => tt_score > eval,
            _ => true,
        } {
        tt_score
    } else {
        eval
    };

    td.stack[ply].eval = eval;
    td.stack[ply].tt_move = tt_move;
    td.stack[ply].tt_pv = tt_pv;
    td.stack[ply].reduction = 0;
    td.stack[ply].move_count = 0;

    // `cutoff_count[ply]` counts beta cutoffs produced by this node's children.
    // Written at the `break` in the move loop; each node clears its
    // grandchild slot here so the count a child reads starts fresh.
    //
    // SHARED SIGNAL -- FIVE CONSUMERS. Every one reads `cutoff_count[ply + 1]`
    // and each coefficient was set as if it were the only reader:
    //
    //   razoring   `razor_cutoff`         (> 3)   fork-only
    //   NMP        `nmp_cutoff`           (< 2)   upstream
    //   SEE prune  `see_q/n_cutoff`       (> 2)   fork-only
    //   LMR        `lmr_cutoff(_node)`    (> 2)   upstream
    //   FDS        `fds_cutoff(_node)`    (> 2)   upstream
    //
    // Three are fork additions layered onto a signal upstream already read
    // twice. Before tuning any one of them, or adding a sixth, account for the
    // others: this is exactly the shape of the IIR/`lmr_cutnode_null` defect,
    // where a second mechanism was added on a signal whose existing consumer
    // kept a coefficient tuned for being the only one. That cost roughly a ply
    // of reduction before anyone noticed, and nothing about it was visible at
    // any single site.
    td.cutoff_count[ply + 2] = 0;

    // Quiet move ordering using eval difference
    if !NODE::ROOT
        && !in_check
        && !excluded
        && td.stack[ply - 1].mv.is_quiet()
        && is_valid(td.stack[ply - 1].eval)
        && (depth < 6 || entry.is_none())
    {
        let value = 812 * (-(eval + td.stack[ply - 1].eval)) / 128;
        let bonus = value.clamp(-144, 324);

        td.quiet_history.update(td.board.prior_threats(), !stm, td.stack[ply - 1].mv, bonus);
    }

    // Hindsight reductions
    if !NODE::ROOT && !in_check && !excluded && is_valid(td.stack[ply - 1].eval) {
        let eval_delta = eval + td.stack[ply - 1].eval;
        let reduction = td.stack[ply - 1].reduction;

        if reduction >= p::hindsight_reduction() && eval_delta < 0 {
            depth += 1;
        }

        if !tt_pv && depth >= 2 && reduction > 0 && eval_delta > p::hindsight_eval_delta() {
            depth -= 1;
        }
    }

    let potential_singularity = depth >= 5 + tt_pv as i32
        && tt_depth >= depth - 3
        && tt_bound != Bound::Upper
        && is_valid(tt_score)
        && !is_decisive(tt_score);

    let improvement = if in_check {
        0
    } else if is_valid(td.stack[ply - 2].eval) {
        eval - td.stack[ply - 2].eval
    } else if is_valid(td.stack[ply - 4].eval) {
        eval - td.stack[ply - 4].eval
    } else if is_valid(td.stack[ply - 6].eval) {
        // Extends the existing ply-2/ply-4 fallback chain one step further,
        // for long same-side-to-move gaps (e.g. extended check-evasion
        // sequences) where neither is available but ply-6 is.
        eval - td.stack[ply - 6].eval
    } else {
        0
    }
    .clamp(p::improvement_lo(), p::improvement_hi());

    let improving = improvement > 0;

    // Restored: present in 0.1.2 (`4135b69`), silently dropped since with no
    // comment anywhere explaining the removal -- out of step with every other
    // change in this file, which documents even single-constant tweaks at
    // length. Feeds the RFP `rfp_worsening` term below, which was dropped
    // alongside it for the same reason.
    let opponent_worsening = !in_check && is_valid(td.stack[ply - 1].eval) && eval > -td.stack[ply - 1].eval;

    // How far the static evaluation and the transposition table's search score
    // disagree about this position. Stormphrax and Viridithas both carry this
    // (as `complexity` / `tt_complexity`) and feed it into pruning margins and
    // reductions; this fork had no equivalent.
    //
    // It is *not* the same signal as `correction_value`, which is what
    // correction history has learned about positions that hash alike. This is
    // about the position in front of us: a large gap means a real search
    // already found something the static eval does not see, so pruning margins
    // should widen and reductions shrink. Zero when there is no usable TT
    // score, which makes it a no-op on nodes with nothing to compare against.
    // `is_valid(eval)` is load-bearing, not defensive: `eval` is `Score::NONE`
    // (32002) whenever this node is in check, and `Score::NONE` is a sentinel,
    // not a number. Without this guard the subtraction produces ~32000 and the
    // LMR consumer -- which has no `!in_check` of its own -- subtracts ~15 plies
    // of reduction at every in-check node holding a TT score, silently
    // disabling LMR there. Nothing crashes, because `reduced_depth` is clamped;
    // it just quietly searches a different tree.
    // Capped as well as validity-gated. The `is_valid(eval)` guard stops the
    // `Score::NONE` sentinel getting in, but a *legitimate* value can still be
    // far outside the range this signal was sized for: `eval` is clamped to
    // +/-(TB_WIN_IN_MAX - 1) and a non-decisive `tt_score` can sit just under
    // TB_WIN_IN_MAX, so the difference can reach ~63000 where the intended
    // range is 0-800. Uncapped that is ~30 plies of reduction from one term
    // against the 0.38 it is meant to contribute.
    //
    // The cap also matches what the signal means: past a certain disagreement
    // the eval and the search have simply diverged, and "more diverged" is not
    // more informative. Same treatment `qs_see_corr_cap` already gives
    // `correction_value` in qsearch.
    // Dynamic contextual processing: how tactical is this position, right now,
    // for the side to move? Counts our own pieces standing on squares the
    // opponent attacks. Both bitboards are already materialised, so this costs
    // one AND and one popcount per node. Consumers: RFP and FP margins.
    let our_threatened = td.board.all_threats() & td.board.colors(stm);

    // Split into the boolean RFP actually uses and the count only the scaled
    // terms use. Both scaling coefficients ship at 0 -- the additive form of
    // this term cost ~60 Elo and was reverted -- so with the count computed
    // eagerly every node paid a POPCNT and a MIN to produce a value that
    // `threat_scaled` then discarded on its `coefficient == 0` early return.
    //
    // Rust evaluates arguments eagerly, so that early return could not save the
    // work; the guard has to be here. In the default build both parameters are
    // `const fn`s returning 0, so this folds to `0` and the POPCNT disappears
    // from the binary entirely. Under the `spsa` feature they are real reads and
    // the count is computed as before.
    //
    // Behaviour is identical either way: `min(cap) == 0` holds exactly when the
    // set is empty or the cap is 0, which is what `no_threats` tests.
    // Purely `is_empty`, which is what this meant before 1.0.0. Folding the
    // `cap == 0` case in here was wrong: that equivalence holds for
    // `threat_density` (whose `min(cap)` really is 0 when the cap is 0) but not
    // for `no_threats`, which asks a different question. `spsa.config` allows a
    // lower bound of 0 on the cap, and at 0 the folded form made `no_threats`
    // unconditionally true -- so RFP would subtract `rfp_no_threats` (54) from
    // its margin at every node, threatened or not.
    let no_threats = our_threatened.is_empty();

    let threat_density = if p::rfp_threat_density() != 0 || p::fp_threat_density() != 0 {
        (our_threatened.popcount() as i32).min(p::threat_density_cap())
    } else {
        0
    };

    let complexity = if is_valid(eval) && is_valid(tt_score) && !is_decisive(tt_score) {
        (eval - tt_score).abs().min(p::complexity_cap())
    } else {
        0
    };

    // Seed the child's extension budget before any recursion reaches `ply + 1`.
    //
    // The per-move increment at the bottom of the move loop was the only writer,
    // so every recursion that happens BEFORE the loop -- null move and both
    // ProbCut searches -- handed the child whatever an unrelated sibling had
    // left in `stack[ply + 1]`. The child then read that at its own singular
    // check as if it described the current line.
    //
    // It went wrong in both directions: a leftover high count silently disabled
    // double and triple extensions for a whole null-move subtree, and a leftover
    // 0 let a deep line reset a budget it had already spent. `max_double_extensions`
    // moves ~12.7% of bench nodes, so neither direction was cosmetic.
    //
    // Seeding here means the slot always describes the path actually taken; the
    // loop's write stays as the per-move increment on top of it.
    td.stack[ply + 1].double_extensions = td.stack[ply].double_extensions;

    // Razoring
    // Restored the `razor_corr` eval-correction term and the `cutoff_count[ply
    // + 1] > 3` bonus, both present in 0.1.2 and both silently dropped since
    // with no comment justifying the removal.
    if !NODE::PV
        && !in_check
        && estimated_score
            < alpha - p::razor_base() - p::razor_quad() * depth * depth
                - p::razor_corr() * correction_value.abs() / 1024
                + p::razor_cutoff() * (td.cutoff_count[ply + 1] > 3) as i32
        && alpha < 2048
        // `!is_quiet()`, which is TRUE for `Move::NULL` -- deliberately.
        //
        // This was briefly "fixed" to `is_noisy()` on the theory that admitting
        // TT misses was a bug. It is not: Stockfish's condition is
        // `!(ttMove && !ttMove.isCapture())`, which allows a null TT move by
        // construction, and `!is_quiet()` is exactly that. A TT miss means no
        // cached move contradicts the static evaluation, which is a reason to
        // trust the margin, not to distrust it.
        //
        // `is_noisy()` requires a capture to ALREADY be in the table, so razoring
        // fired almost nowhere.
        && !tt_move.is_quiet()
        && tt_bound != Bound::Lower
    {
        // Floored at `best_score` -- a guard, not a gain.
        //
        // `best_score` is still `-Score::INFINITE` here unless a tablebase probe
        // raised it, so the `.max()` almost never binds. It stays because the
        // alternative -- handing back a raw qsearch score that could sit below
        // something this node has already proven -- is safe only by accident of
        // where `best_score` happens to be initialised, and that is the kind of
        // property that breaks silently when code above it moves.
        let razor_score = qsearch::<NonPV>(td, alpha, beta, ply);

        return razor_score.max(best_score);
    }

    // The improving correction, scaled with depth rather than flat.
    //
    // Artemis applies it as `improving * futilityMult / 1024`, where
    // `futilityMult` is the same per-depth coefficient that builds the margin --
    // so the correction stays a constant FRACTION of the margin at every depth.
    // This search applied it as a flat `improvement / 1024`: a large share of an
    // 11-unit margin at depth 1, a negligible one at depth 24. Same
    // flat-term-on-a-scaled-base shape that cost 87 Elo in LMR's move-count term
    // and ~60 Elo in `threat_density`.
    //
    // `rfp_improvement_ref` is the depth at which the new form reproduces the old
    // magnitude exactly, so only the slope across depth changes. 0 restores flat.
    let rfp_improvement_term = if p::rfp_improvement_ref() > 0 {
        p::rfp_improvement() * improvement * depth / (1024 * p::rfp_improvement_ref())
    } else {
        p::rfp_improvement() * improvement / 1024
    };

    // Reverse Futility Pruning (RFP)
    // Restored the TT-move quiet-history guard and the `rfp_worsening` term,
    // both present in 0.1.2 and both silently dropped since with no comment
    // justifying the removal. The guard specifically stops RFP from firing
    // when the TT move is a quiet move already known to be bad
    // (quiet_history < -2048) -- without it RFP can return early based on a
    // static margin even when search already has evidence the position's
    // best-looking move doesn't hold up.
    if !tt_pv
        && !in_check
        && !excluded
        && (!tt_move.is_quiet() || td.quiet_history.get(td.board.all_threats(), stm, tt_move) >= p::rfp_tt_hist_gate())
        && estimated_score
            >= beta
                + threat_scaled(
                    p::rfp_depth_quad() * depth * depth / 128 - rfp_improvement_term
                    + p::rfp_depth_lin() * depth
                    + p::rfp_corr() * correction_value.abs() / 1024
                    + p::rfp_complexity() * complexity / 1024
                    - p::rfp_no_threats() * no_threats as i32
                    - p::rfp_worsening() * opponent_worsening as i32
                    // Artemis shrinks the whole margin on a TT miss
                    // (`futilityMult -= 20 * !ttHit`, on a 40-80 multiplier), so a
                    // node with no table evidence prunes more readily. Applied
                    // proportionally to depth, for the same reason the improving
                    // term above is. 0 disables.
                    - p::rfp_tt_miss() * depth * entry.is_none() as i32 / 16
                    - p::rfp_base(),
                    p::rfp_threat_density(),
                    threat_density,
                )
                .max(2)
        && !is_loss(beta)
        && !is_win(estimated_score)
    {
        return lerp(estimated_score, beta, 0.6945);
    }

    // Null Move Pruning (NMP)
    if cut_node
        && !in_check
        && !excluded
        && !potential_singularity
        && estimated_score
            >= beta
                + (-p::nmp_depth() * depth + p::nmp_ttpv() * tt_pv as i32
                    - p::nmp_improvement() * improvement / 1024
                    - p::nmp_cutoff() * (td.cutoff_count[ply + 1] < 2) as i32
                    + p::nmp_base())
                .max(2)
        && ply as i32 >= td.nmp_min_ply
        // Zugzwang guard, scoped to the side to move.
        //
        // Two corrections over upstream's `material() > 491`. First, `material()`
        // counts pawns, so a pawn-heavy piece-empty endgame -- the textbook
        // zugzwang case this exists to catch -- could clear the threshold on
        // pawn mass alone. Second, whole-board non-pawn material is still the
        // wrong scope: zugzwang is about *the mover* having no useful piece
        // move, and K+P vs K+Q passes a both-sides test comfortably while the
        // side to move has none.
        // Zugzwang guard. Upstream gates on `material() > 491`, which counts
        // pawns; this fork switched to `non_pawn_material()` -- the correct
        // signal, since zugzwang risk is about having no useful *piece* moves
        // -- and non_pawn_material() already nets out the board's actual
        // current pawn count (see board.rs), so no compensating offset is
        // needed here at all. The previous `1744 +` term assumed a full
        // 16-pawn set regardless of how many pawns remain, which double-
        // subtracts pawn mass as pawns come off the board: in a 4-pawns-a-
        // side endgame it demanded total material above ~3100 before NMP was
        // even considered, against upstream's 491, effectively disabling NMP
        // for most of the game -- backwards for a guard whose whole point is
        // to stay enabled through material simplification and only bite in
        // truly bare, piece-empty positions. Comparing non_pawn_material()
        // straight against nmp_material() matches the semantic fix this
        // comment already argued for, without the extra offset.
        //
        // The 491 magnitude itself is still unverified against this
        // (corrected) quantity -- exposed as a tunable rather than silently
        // re-guessed. Let SPSA settle it.
        // Back to whole-board non-pawn material. Narrowing this to the side to
        // move is defensible in principle -- zugzwang is about *the mover*
        // having no useful piece move -- but it was shipped against upstream's
        // unchanged 491, and on the `PieceType::value()` scale a knight is 403
        // and a bishop 435. So the narrowed form switched NMP off entirely
        // whenever the mover was down to one minor: a very common state, and
        // NMP is the largest node reducer in the engine. Re-narrow only with a
        // threshold sized for the new quantity, behind its own SPRT.
        && td.board.non_pawn_material() > p::nmp_material()
        && !is_loss(beta)
        && !is_win(estimated_score)
        && !(tt_bound == Bound::Lower
            && tt_move.is_capture()
            // `capture_sq()`, not `to()`. For an en-passant capture the taken
            // pawn sits on neither square the move names, so `piece_on(to)`
            // returned `Piece::None` (value 0) and this guard -- which exists to
            // stop NMP when the TT move wins a minor or better -- silently passed
            // on every en-passant capture. Every other capture site in the
            // codebase already uses `capture_sq()`.
            && td.board.piece_on(tt_move.capture_sq()).value() >= PieceType::Knight.value())
    {
        debug_assert_ne!(td.stack[ply - 1].mv, Move::NULL);

        let r = (p::nmp_r_base()
            + p::nmp_r_improving() * improving as i32
            + p::nmp_r_depth() * depth
            + p::nmp_r_beta() * (estimated_score - beta).clamp(0, p::nmp_r_beta_max()) / 128)
            / 1024;

        // Saved, because everything below this block still needs it.
        //
        // Nulling these for the null-move search is correct -- the child must not
        // credit a previous move that was never played. Leaving them nulled
        // afterwards was not. Every path after NMP at this node then saw a null
        // previous move, and the visible casualty is the singular block: `else if
        // singular_score > tt_score && td.stack[ply].mv != Move::NULL` becomes
        // unsatisfiable at any node where NMP ran, so control falls through to
        // the negative extension `extension = -3`.
        //
        // That is the same defect already fixed for the singular search a few
        // hundred lines down, reintroduced by its neighbour. `conthist` and
        // `contcorrhist` matter too: left pointing at the sentinel, every
        // continuation update and lookup for the rest of the node writes to and
        // reads from a shared scratch table instead of the real one.
        let saved_conthist = td.stack[ply].conthist;
        let saved_contcorrhist = td.stack[ply].contcorrhist;
        let saved_piece = td.stack[ply].piece;
        let saved_mv = td.stack[ply].mv;

        td.stack[ply].conthist = td.stack.sentinel().conthist;
        td.stack[ply].contcorrhist = td.stack.sentinel().contcorrhist;
        td.stack[ply].piece = Piece::None;
        td.stack[ply].mv = Move::NULL;
        td.stack[ply + 1].follow_pv = false;

        td.board.make_null_move();
        td.shared.tt.prefetch(td.board.hash());

        let bound = if is_valid(tt_score) && beta > tt_score && tt_bound == Bound::Lower && depth - 2 <= tt_depth {
            tt_score
        } else {
            beta
        };

        let score = -search::<NonPV>(td, -bound, -bound + 1, depth - r, false, ply + 1);

        td.board.undo_null_move();

        td.stack[ply].conthist = saved_conthist;
        td.stack[ply].contcorrhist = saved_contcorrhist;
        td.stack[ply].piece = saved_piece;
        td.stack[ply].mv = saved_mv;

        if td.shared.status.get() == Status::STOPPED {
            return Score::ZERO;
        }

        // NMP must prove BETA, not merely `bound`, before this node returns a
        // value.
        //
        // A null move establishes a lower bound. When `bound` is a TT lower
        // bound sitting under beta, a result in [bound, beta) re-states what the
        // TT already said -- but returning it as this node's value labels a
        // lower bound as a fail-low, which is an upper-bound claim nothing
        // established, and the parent reads a good move as refuted. The window
        // is `[bound - 1, bound]` yet the search is fail-soft, so the returned
        // score can exceed `bound` and `>= beta` is answerable.
        //
        // Upstream returns on `>= bound` and this fork inherited that. This cuts
        // strictly less often, and only where the proof actually holds.
        //
        // No correction-history update on either return. Upstream has none, and
        // the fork's addition skewed the table's sample balance: `score > eval`
        // is the right bound-consistency test for a fail-high, so every sample
        // NMP contributed was valid *and* positive, arriving at high frequency
        // beside a main site that contributes both signs.
        if score >= bound && !is_win(score) {
            // `>= bound`, as upstream. Requiring `>= beta` here is sound in
            // principle -- a null move proves a lower bound, so a sub-beta
            // result is not an upper bound the parent can trust -- but the
            // search window is `[bound - 1, bound]`, so a fail-soft return
            // lands at or just above `bound` and almost never reaches beta when
            // `bound` came from a sub-beta TT entry. In practice that did not
            // tighten the proof, it deleted the TT-bound path from NMP.
            if td.nmp_min_ply > 0 || depth < 16 {
                return score;
            }

            let reduced_depth = depth - r;

            // Restored, not zeroed. A verification search that itself reaches a
            // verifying NMP node used to clear the guard on its way out, so the
            // OUTER search resumed with zugzwang verification switched off for
            // the rest of its subtree -- the failure this mechanism exists to
            // prevent, caused by the mechanism itself.
            // The verification search re-enters at `ply`, not `ply + 1`, so it
            // runs a whole search over this node's own stack slot and overwrites
            // everything in it. `eval` and `tt_pv` feed the pruning decisions
            // still to come; `reduction` is read by the child's hindsight
            // adjustments; `move_count` and `tt_move` are read one ply down by
            // the TT-cutoff bonus and `update_prior_move_histories`.
            //
            // The singular block does exactly this save/restore around its own
            // same-ply re-entry -- this one was simply missing it.
            let saved_eval = td.stack[ply].eval;
            let saved_move_count = td.stack[ply].move_count;
            let saved_tt_move = td.stack[ply].tt_move;
            let saved_tt_pv = td.stack[ply].tt_pv;
            let saved_reduction = td.stack[ply].reduction;

            let saved_nmp_min_ply = td.nmp_min_ply;
            td.nmp_min_ply = ply as i32 + 3 * reduced_depth / 4;
            let verified_score = search::<NonPV>(td, bound - 1, bound, reduced_depth, false, ply);
            td.nmp_min_ply = saved_nmp_min_ply;

            td.stack[ply].eval = saved_eval;
            td.stack[ply].move_count = saved_move_count;
            td.stack[ply].tt_move = saved_tt_move;
            td.stack[ply].tt_pv = saved_tt_pv;
            td.stack[ply].reduction = saved_reduction;

            if td.shared.status.get() == Status::STOPPED {
                return Score::ZERO;
            }

            if verified_score >= bound {
                return score;
            }
        }
    }

    // Internal Iterative Reductions (IIR): at sufficient depth, reduce PV and
    // expected cut nodes that have no TT move to anchor move ordering. Nodes
    // on the previous iteration's PV are exempt (as in Stockfish).
    let iir_applied =
        !NODE::ROOT
            && !td.stack[ply].follow_pv
            && (NODE::PV || cut_node)
            && depth >= p::iir_depth()
            // A TT move backed by a search far shallower than this one tells us
            // little more than no move at all. `slack == 0` disables the second
            // arm entirely, reproducing the original `tt_move.is_null()` test.
            && (tt_move.is_null()
                || (p::iir_tt_depth_slack() > 0 && tt_depth + p::iir_tt_depth_slack() < depth));

    if iir_applied {
        depth -= 1;
    }

    // ProbCut
    // Dynamic contextual processing applied to ProbCut's own threshold. The
    // qsearch draft proposes, the shallow search verifies; `probcut_history`
    // is the running gravity-bounded record of how often that verification
    // agreed. When it keeps disagreeing the draft is miscalibrated for this
    // search, so raise the bar and let ProbCut fire less often; when it keeps
    // agreeing, lower it. Sign: history negative -> subtracting it widens.
    // Bounded to a quarter of `probcut_base`. Without the clamp the term is
    // `probcut_hist * history / 8192` with history free to reach +/-8192, so a
    // tuned-up coefficient could move the threshold further than the base it is
    // adjusting -- at which point it stops being an adjustment and becomes the
    // decision. A quarter keeps it a correction.
    // `.max(0)` on the bound, not just tidiness: `clamp` panics when min > max,
    // and `set_parameter` accepts any value with no range check -- the
    // `spsa.config` bounds only constrain a tuner that respects them. A typed
    // `setoption name probcut_base value -100` would give `clamp(25, -25)` and
    // take the process down mid-match. Same defect class as the four `.max(1)`
    // divisor guards elsewhere in this file.
    let probcut_bound = (p::probcut_base() / 4).max(0);
    let probcut_shift = (p::probcut_hist() * td.probcut_history / 8192).clamp(-probcut_bound, probcut_bound);

    let mut probcut_beta =
        beta + p::probcut_base() - p::probcut_improving() * improving as i32 - probcut_shift;

    // The `!in_check` guard is not cosmetic. At an in-check node there is no
    // static evaluation, so `eval` is the Score::NONE sentinel (32002):
    //
    //   - the `eval >= beta` arm below was therefore true for every non-mate
    //     beta, so ProbCut ran at essentially *every* in-check cut node that
    //     had no usable TT score, and
    //   - the move picker's SEE threshold (`probcut_beta - eval`) came out
    //     around -31500, which every capture passes -- so every evasion was
    //     classified GoodNoisy and the `Stage::BadNoisy` break below could
    //     never fire, leaving no bound on how many were tried.
    //
    // Both the razoring and RFP/NMP steps above already carry this guard.
    // Stockfish skips this step in check for the same reason: its in-check
    // path jumps straight to `moves_loop:`, past razoring, futility, null move
    // and ProbCut. Note the TT-only ProbCut immediately below sits *after*
    // that label upstream and reads no static eval, so it is deliberately
    // left running in check.
    // RESTORED: depth >= 5 gate removed. At depth < 5, base_depth is 0
    // and the verification search is skipped (probcut_depth == 0), so ProbCut
    // cuts directly on the qsearch result. That is less rigorous, but depths
    // 1-4 account for the overwhelming majority of tree nodes, and the +62
    // build had no gate. The gate was added on soundness grounds without
    // measuring the node cost; the PGN depth metric shows it cost ~1.9 plies.
    // `!excluded`, as razoring (`!excluded`), null move (`!excluded`) and the
    // TT-only ProbCut below (`!excluded`) all have. Without it, ProbCut runs
    // inside a singular verification search and can return a cutoff derived
    // from the very position whose TT move is being excluded -- which defeats
    // the exclusion, and (through the write below) caches a `Bound::Lower` for
    // this hash produced by a search that deliberately suppressed the best move.
    // `base_depth > 0`, i.e. depth >= 5 (>= 6 when improving), restored as an
    // entry condition rather than left to the loop.
    //
    // `base_depth = (depth - 4 - improving).max(0)` is 0 below that, so
    // `probcut_depth` is 0, so no verification search runs, so `verified` is
    // false -- and with verification now required the cutoff CANNOT fire. The
    // loop still ran the move picker and a full qsearch per noisy move to reach
    // a branch that was unreachable, at the depths holding most of the tree.
    //
    // Skipping it cannot change any returned score; it only stops paying for a
    // decision that was already impossible.
    if (p::probcut_require_verify() == 0 || (depth - 4 - improving as i32) > 0)
        && cut_node
        && !excluded
        && !in_check
        && !is_win(beta)
        && if is_valid(tt_score) { tt_score >= probcut_beta && !is_decisive(tt_score) } else { eval >= beta }
        // `!is_quiet()`, TRUE for `Move::NULL`; see the razoring gate above.
        // Same erroneous "fix", same restoration -- ProbCut with `is_noisy()`
        // only ran when the table already held a capture, which is the case where
        // it is least needed.
        && !tt_move.is_quiet()
    {
        let mut move_picker = MovePicker::new(Move::NULL, Some(probcut_beta - eval));

        while let Some(mv) = move_picker.next::<NODE>(td, true, ply) {
            if move_picker.stage() == Stage::BadNoisy {
                break;
            }

            if mv == td.excluded[ply] {
                continue;
            }

            make_move(td, ply, mv);

            let mut score = -qsearch::<NonPV>(td, -probcut_beta, -probcut_beta + 1, ply + 1);

            let base_depth = (depth - 4 - improving as i32).max(0);
            let mut probcut_depth = (base_depth - (score - probcut_beta) / p::probcut_score_div().max(1)).clamp(0, base_depth);

            // Floored at 1 whenever a verification is possible at all.
            //
            // The expression above shrinks the verification depth as the draft
            // gets STRONGER -- and once it reaches 0, `verified` is false and
            // `probcut_require_verify` refuses the cutoff. So the drafts that beat
            // `probcut_beta` most convincingly were exactly the ones that could
            // not cut, while marginal ones could. At depth 5-6 `base_depth` is 1,
            // so any draft ahead by about a pawn (`probcut_score_div`) fell off
            // the edge immediately.
            //
            // Reducing the depth for a strong draft is the right instinct -- less
            // verification is needed when the margin is large -- but the floor has
            // to stay at "verify something", not "verify nothing".
            if score >= probcut_beta && base_depth > 0 {
                probcut_depth = probcut_depth.max(1);
            }

            // Only outcomes where a verification search actually ran are
            // evidence about verification. If the qsearch draft never cleared
            // the bar there is nothing to have agreed or disagreed with, and
            // scoring those would just measure the draft against itself.
            let verified = score >= probcut_beta && probcut_depth > 0;

            if verified {
                let adjusted_beta =
                    (probcut_beta + p::probcut_beta_step() * (base_depth - probcut_depth)).min(Score::INFINITE);

                score = -search::<NonPV>(td, -adjusted_beta, -adjusted_beta + 1, probcut_depth, false, ply + 1);

                if score < adjusted_beta && probcut_beta < adjusted_beta {
                    probcut_depth = base_depth;
                    score = -search::<NonPV>(td, -probcut_beta, -probcut_beta + 1, probcut_depth, false, ply + 1);
                } else {
                    probcut_beta = adjusted_beta;
                }
            }

            undo_move(td, mv);

            if td.shared.status.get() == Status::STOPPED {
                return Score::ZERO;
            }

            // Feed the verification outcome back into the threshold above.
            if verified {
                update_probcut_history(
                    td,
                    if score >= probcut_beta { p::probcut_hist_bonus() } else { -p::probcut_hist_malus() },
                );
            }

            // `verified` gate: at depth <= 4 (<= 5 when improving) `base_depth`
            // is 0, so `probcut_depth` is 0 and no verification search runs --
            // yet this arm still returned a cutoff and wrote `Bound::Lower` at
            // depth 1. The draft behind it is a qsearch capped at two moves per
            // node, so those were fabricated bounds, cached as real ones, at the
            // depths holding most of the tree.
            //
            // Removing the old `depth >= 5` gate bought ~1.9 plies, which is
            // exactly what an unsound cutoff buys: depth that was never searched.
            // This engine has been burned by that signature before ("same depth,
            // thinner search"). Requiring verification keeps ProbCut at every
            // depth but only lets it cut when something actually checked the
            // draft.
            //
            // `probcut_require_verify = 0` restores the previous behaviour, so
            // the two are directly comparable in one SPRT.
            if score >= probcut_beta && (verified || p::probcut_require_verify() == 0) {
                td.shared.tt.write(hash, probcut_depth + 1, raw_eval, score, Bound::Lower, mv, ply, tt_pv, false);

                if is_decisive(score) {
                    return score;
                }
                return lerp(score, beta, 0.2695);
            }
        }
    }

    // A small ProbCut idea (as in Stockfish): a lower-bound TT entry from a
    // near-full-depth search whose score comfortably exceeds beta is trusted
    // as a cutoff without any search.
    let probcut_beta_tt = beta + p::probcut_tt_margin();
    //
    // Gated to non-PV, non-excluded nodes. This is the only cutoff in the
    // function that returns a score nothing ever searched -- it hands back
    // `beta + margin` purely on the strength of a cached entry -- so it has to
    // be held to at least the same standard as its neighbours: razoring is
    // `!NODE::PV`, RFP is `!tt_pv && !excluded`, null move is `!excluded`.
    //
    // Ungated it could fire at PV nodes, which is how a fabricated score ends
    // up in the principal variation and decides the move actually played. It
    // was also the one early return reachable at the ROOT (root is a PV node),
    // where it returns before any root move has been scored, leaving
    // `root_moves` stale. And inside a singular verification search it could
    // return a cutoff derived from the very TT entry whose move is being
    // excluded, which defeats the point of the exclusion.
    //
    // This matters beyond tidiness: across 562 games the engine's own
    // evaluation drifted +0.051 pawns per move against the opponent's reply
    // (18.4 sigma; upstream drifts -0.024 the other way), i.e. its positions
    // kept turning out worse than its search claimed. Unverified scores
    // reaching the PV are a direct mechanism for that.
    // The depth floor is not cosmetic. `TtDepth::SOME` is -1, so at depth 3 or
    // below `tt_depth >= depth - 4` reduces to `tt_depth >= -1` and a *qsearch*
    // entry satisfies it -- the one thing this cutoff must never trust, since
    // its whole premise is a near-full-depth search. Clamping the floor at 0
    // admits only entries from a real search. Depths 4 and up are unaffected.
    if !NODE::PV
        && !excluded
        && matches!(tt_bound, Bound::Lower | Bound::Exact)
        && tt_depth >= (depth - 4).max(0)
        && is_valid(tt_score)
        && tt_score >= probcut_beta_tt
        && !is_decisive(beta)
        && !is_decisive(tt_score)
        && td.board.fiftymove_clock() < 90
    {
        return probcut_beta_tt.min(Score::TB_WIN_IN_MAX - 1);
    }

    // Singular Extensions (SE)
    let mut extension = 0;

    // Whether `extension` came from the singular test rather than from LDSE.
    //
    // A singular extension is a statement about the TT MOVE specifically, but it
    // is applied below to `move_count == 1`. Those coincide only while
    // `Stage::HashMove` actually yields the TT move first, and it yields it only
    // `if is_legal(tt_move)` -- so on a 16-bit key collision the picker drops the
    // entry, the first move searched is an unrelated capture, and the extension
    // lands on that.
    //
    // Gating the singular block on `is_legal` would also fix it, but that repeats
    // a non-trivial legality test (piece lookups, castling paths, attack sets) at
    // every depth-5-and-up node, and `MovePicker` runs the same test moments
    // later. Comparing the move at the application site costs one `Move` compare
    // and gives exactly the same guarantee.
    //
    // LDSE's extension is a property of the NODE, not of a particular move, so it
    // keeps the plain `move_count == 1` treatment.
    let mut singular_extension = false;
    let mut singular_score = Score::NONE;

    // `tt_move` must be present AND legal, because the extension this block
    // computes is applied to `move_count == 1` rather than to the move itself.
    //
    // That equivalence only holds while `Stage::HashMove` actually yields the TT
    // move first, and it yields it only `if is_legal(tt_move)`. On a 16-bit key
    // collision the entry carries a move from another position: the picker drops
    // it, the first move searched is an unrelated capture, and the extension
    // derived from the TT entry lands on that instead.
    //
    // `is_present` covers the other end: `potential_singularity` requires a valid
    // non-decisive score and a non-Upper bound, none of which imply a stored
    // move. With a null `tt_move`, `excluded[ply] = Move::NULL` excludes nothing,
    // so the "singular" search is a plain reduced re-search and its score is
    // compared against `singular_beta` as though it meant something. That
    // combination looks unreachable through the current write sites, but it is
    // one TT-write change away from being live.
    if !NODE::ROOT && !excluded && potential_singularity && tt_move.is_present() && !is_shuffling(td, tt_move, ply) {
        debug_assert!(is_valid(tt_score));

        let singular_margin = if tt_bound == Bound::Exact { (depth as u32).div_ceil(4) as i32 } else { depth }
            + depth * (tt_pv && !NODE::PV) as i32;
        let singular_beta = tt_score - singular_margin;
        let singular_depth = (depth - 1) / 2;

        td.excluded[ply] = tt_move;

        // Saved and restored around the exclusion search.
        //
        // Nulling it is deliberate -- children of the exclusion search should not
        // credit or blame a "previous move" that we are in the middle of pretending
        // does not exist. Leaving it null afterwards was not: nothing restored it,
        // so the null leaked into the rest of this node.
        //
        // Two consequences, both silent. The `else if singular_score > tt_score &&
        // td.stack[ply].mv != Move::NULL` arm below became unsatisfiable, so
        // `tt_move` was never cleared and control fell through to the negative
        // extension (`extension = -3`) far more often than intended. And every
        // later sibling at this ply saw a null previous move, which disables
        // `update_prior_move_histories`, `is_recapture`, `bnfp_recapture` and the
        // ply-1 continuation lag for the remainder of the node.
        let saved_mv = td.stack[ply].mv;
        td.stack[ply].mv = Move::NULL;
        singular_score = search::<NonPV>(td, singular_beta - 1, singular_beta, singular_depth, cut_node, ply);
        td.stack[ply].mv = saved_mv;
        td.excluded[ply] = Move::NULL;
        td.stack[ply].tt_pv = tt_pv;

        if td.shared.status.get() == Status::STOPPED {
            return Score::ZERO;
        }

        if singular_score < singular_beta {
            // The is_quiet() coefficients are 16/19, the values 0.1.2
            // (`4135b69`) carried. They were raised to Stockfish's 152/188 in
            // `d527508` on the grounds that they matched upstream's
            // `!ttCapture` terms "in shape, not magnitude". Reverted, because
            // that transplant was measured and it is expensive:
            //
            //   - these margins are *subtracted* from singular_beta before the
            //     comparison, so scaling them ~10x makes
            //     `singular_score < singular_beta - margin` far easier to
            //     satisfy, and double/triple extensions fire much more often;
            //   - `d527508` grew the bench tree 23% (3.02M -> 3.70M nodes at
            //     fixed depth) and the engine went from neutral to roughly
            //     -96 Elo, searching ~0.9 ply shallower than base at equal
            //     time in real games.
            //
            // The "match upstream" argument does not survive contact with the
            // rest of the expression either. Stockfish's version is
            // `-2 + 204 * PvNode - 152 * !ttCapture - ...` and
            // `70 + 279 * PvNode - 188 * !ttCapture + 81 * ss->ttPv - ...`;
            // this one has no -2/+70 constants, uses 195/230 for the PV term,
            // and gates its second term on `PV && !tt_was_pv` rather than
            // upstream's `+81 * ttPv`. Two coefficients lifted out of a
            // differently shaped formula are not the same formula, and the
            // surrounding constants were tuned against 16/19.
            let double_margin = (195 * NODE::PV as i32 + 48 * (NODE::PV && !tt_was_pv) as i32
                - 16 * tt_move.is_quiet() as i32
                - 16 * correction_value.abs() / 128
                - 1175 * td.tt_move_history / 114178
                - 38 * (ply as i32 > td.root_depth) as i32)
                .max(0);
            let triple_margin = (230 * NODE::PV as i32 + 56 * (NODE::PV && !tt_was_pv) as i32
                - 19 * tt_move.is_quiet() as i32
                - 15 * correction_value.abs() / 128
                - 43 * (ply as i32 > td.root_depth) as i32
                + 36)
                .max(0);

            // Upper tiers gated on how many have already been granted above
            // this node. Each singular node can hand out +3 plies, and nothing
            // tracked the cumulative total, so a tactical line could keep
            // extending -- `MAX_PLY` bounds the recursion but not the tree.
            // Stockfish and Berserk both gate their tiers this way
            // (`ss->doubleExtensions`, `ss->de`).
            //
            // The limit is deliberately loose. This is a backstop against
            // runaway lines, not a tuning knob: at the default it does not bind
            // in normal play, so ordinary singular behaviour is unchanged.
            singular_extension = true;

            let de = td.stack[ply].double_extensions;
            extension = 1;
            // `<= 0` means unlimited, not "never extend".
            //
            // Read literally, 0 makes `de < 0` unsatisfiable and removes double
            // and triple extensions from the engine entirely -- the opposite of
            // what a zero reads as everywhere else in this file, where it
            // disables the guard. `set_parameter` enforces no range, so a tuner
            // or a hand edit reaching 0 silently deletes a mechanism the file's
            // own note measures at 12.7% of bench nodes.
            if p::max_double_extensions() <= 0 || de < p::max_double_extensions() {
                extension += (singular_score < singular_beta - double_margin) as i32;
                extension += (singular_score < singular_beta - triple_margin) as i32;
            }
        }
        // Multi-Cut
        else if singular_score >= beta && !is_decisive(singular_score) {
            update_tt_move_history(td, p::tt_move_history_multicut_base() - p::tt_move_history_multicut_depth() * depth);

            // Stored, like every other cutoff in this function.
            //
            // This arm returns having just paid for a full exclusion search -- the
            // most expensive way this node can reach a conclusion -- and wrote
            // nothing, so every revisit re-derived it from scratch. The bound is a
            // genuine lower bound at `singular_depth`, which is what the exclusion
            // search actually proved, so that is the depth recorded rather than
            // the node's own.
            let multicut_score = lerp(singular_score, beta, 0.4027);
            td.shared.tt.write(hash, singular_depth, raw_eval, multicut_score, Bound::Lower, tt_move, ply, tt_pv, false);

            return multicut_score;
        } else if singular_score > tt_score && td.stack[ply].mv != Move::NULL {
            tt_move = Move::NULL;

            // The stack copy has to follow. `stack[ply].tt_move` was written
            // before the singular block and is read one ply down by
            // `update_prior_move_histories` as `prior_move == stack[ply-1].tt_move`
            // -- so leaving it set credits the child's prior-move bonus to a move
            // this node has just decided not to treat as the TT move at all.
            td.stack[ply].tt_move = Move::NULL;
        }
        // Negative Extensions
        else if tt_score >= beta || cut_node {
            singular_extension = true;
            extension = -3;
        }
    }
    // Low Depth Singular Extensions (LDSE)
    //
    // `!excluded` and `!is_shuffling(..)` mirror the guards on the singular
    // branch this is the `else` of. Without them the shuffling guard did the
    // opposite of its purpose over the low-depth slice: a node detected as
    // shuffling -- precisely where extensions are meant to be switched off to
    // stop search explosion -- fell through to here and was extended a ply
    // instead. And a singular verification search (`excluded`) could extend its
    // own first move, deepening the search whose `(depth - 1) / 2` depth is what
    // makes the singularity test affordable in the first place.
    else if depth <= 7
        && !in_check
        && !excluded
        && cut_node
        && estimated_score <= alpha - 25
        && !is_shuffling(td, tt_move, ply)
    {
        extension = 1;
    }

    let mut best_move = Move::NULL;
    let mut bound = Bound::Upper;

    let mut quiet_moves = ArrayVec::<Move, 32>::new();
    let mut noisy_moves = ArrayVec::<Move, 32>::new();

    // Flat divisor for the history contribution to LMP and futility pruning,
    // matching upstream Reckless.
    //
    // A fork-only depth-indexed table sat here:
    //
    //   [1221, 936, 927, 987, 1065, 1124, 1057, 927,
    //     931, 1043, 1043, 1027, 1045, 1004, 1037, 1189]
    //
    // borrowed in spirit from Stockfish's lmrDivisor. Its mean is 1035, so it
    // looked harmless, but the per-depth swing is +19% at depth 1 and -9.5% at
    // depths 3/8/9 -- and LMP and futility pruning are its only consumers, both
    // gated to low depth.
    //
    // The games say that is where this engine loses. Across 2008 games the
    // per-move eval drift gap against upstream is +0.045 at root depth 9,
    // +0.033 at 11, +0.010 at 13, and then 0.0005 at depth 14 -- vanishing
    // exactly at the futility-pruning cutoff, with depths 15+ statistically
    // identical to upstream. At low root depth a far larger share of the tree
    // is FP/LMP-eligible, so a mistuned divisor there is felt across the whole
    // search; deeper searches simply outgrow it.
    //
    // Restoring 1024 removes the only fork-specific term in those two prunes.
    // The table is kept above rather than deleted: it may well be fine once
    // SPSA has seen it, but it has never been tested and it is the best
    // remaining explanation for a depth-localised deficit.
    let history_divisor = 1024;

    let mut move_count = 0;
    let mut move_picker = MovePicker::new(tt_move, None);
    let mut skip_quiets = false;
    let mut current_search_count = 0;
    let mut tt_move_score = Score::NONE;
    // How many times a move at this node has already raised alpha. A node that
    // has raised alpha several times has been searching a while without a
    // cutoff and has a best move it keeps improving on, so a move arriving this
    // late is a worse bet than its move number alone suggests. Fed into the LMR
    // reduction below.
    let mut alpha_raises = 0;

    // Continuation-history subtable pointers for the two lags read by the quiet
    // `history` sum below. They depend only on `ply`, so they are constant for
    // the whole node, but used to go through a per-lag helper that re-read
    // `stack[ply - n].conthist` on every call -- strided loads into a large
    // array per quiet move, where two for the entire node will do. The
    // raw-pointer read stops the optimiser hoisting them itself.
    //
    // Two, not six: the sum below reads lags 1 and 2 only. `score_quiet` still
    // uses all six for move ordering and builds its own array there.
    let conthist: [*mut PieceToHistory<i16>; 2] = std::array::from_fn(|i| td.stack[ply - 1 - i as isize].conthist);

    // Hoisted for the same reason as `conthist` directly above, which the comment
    // there already argues for: the board does not change between iterations --
    // `make_move` is matched by `undo_move` before the next one -- so the threat
    // set is identical at the top of every pass, yet it was re-read at both the
    // quiet and the noisy history lookup on every move.
    let threats = td.board.all_threats();

    while let Some(mv) = move_picker.next::<NODE>(td, skip_quiets, ply) {
        if mv == td.excluded[ply] {
            continue;
        }

        if NODE::ROOT && !td.root_moves[td.pv_index..td.pv_end].iter().any(|rm| rm.mv == mv) {
            continue;
        }

        current_search_count = 0;

        move_count += 1;
        td.stack[ply].move_count = move_count;

        let is_quiet = mv.is_quiet();

        // Resolved once and reused by both continuation lags below.
        let moved = td.board.piece_on(mv.from());
        let to = mv.to();

        let history = if is_quiet {
            // Lags 1, 2 and 6 only, as in 0.1.2 (`4135b69`). Lags 3, 4 and 5
            // were folded in later without touching anything downstream, which
            // is the same defect as the `corr_weight_div` normalization bug
            // documented in the README: this sum is not consumed on its own
            // scale, it is consumed by seven separately tuned coefficients --
            // lmp_history, fp_history, bnfp_history, hp_margin, see_q_hist,
            // see_n_hist, lmr_quiet_hist/fds_quiet_hist. Adding ~40% more
            // magnitude to `history` silently rescaled every one of them at
            // once, so LMP, futility, history pruning, SEE pruning and both
            // reduction formulas all started reacting far more strongly to
            // history than the values they were tuned with assume.
            //
            // Adding lags back is fine, but only together with a divisor that
            // holds the sum's scale fixed -- exactly the discipline
            // `corr_weight_div` now documents for the correction blend.
            // Upstream Reckless sums `quiet_history + conthist(1) + conthist(2)`
            // and tuned seven coefficients against that magnitude:
            // lmp_history, fp_history, bnfp_history, hp_margin, see_q_hist,
            // see_n_hist and lmr_quiet_hist/fds_quiet_hist. This fork adds a
            // lag-6 term, which is worth keeping -- but it made the
            // continuation part of the sum 2.5 units instead of 2, ~20% wider,
            // so all seven fired harder than their tuned values assume.
            //
            // Normalized rather than dropped: scaling the continuation group by
            // 4/5 puts 2.5 units back on upstream's 2, so lag 6 still
            // contributes its information and every downstream coefficient
            // keeps the scale it was tuned for. Same discipline
            // `corr_weight_div` documents for the correction blend.
            // Both continuation lags, held at upstream's scale. This sum is
            // upstream's two (lags 1 and 2) and is a different consumer from
            // movepick's `CONTHIST_WEIGHTS`, which reads six.
            //
            // Two problems were stacked here. First, lags 3 and 5 were being
            // *written* by update_continuation_histories_in_check but read by
            // nothing at all -- a third of every continuation-history update
            // was dead work on a hot path. Second, every lag this fork added
            // beyond upstream's lag1+lag2 widened the sum without anyone
            // rescaling the seven coefficients tuned against it (lmp_history,
            // fp_history, bnfp_history, hp_margin, see_q_hist, see_n_hist,
            // lmr_quiet_hist/fds_quiet_hist), so all seven fired harder than
            // their values assume.
            //
            // Both are fixed by reading the lags that are written, at the same
            // relative strengths the update uses (lag3 = 195/700 ~ 2/7,
            // lag5 = 89/700 ~ 1/8, lag6 = 1/2), and then normalising the group
            // back onto upstream's two units: the raw sum is 3.911 units, so
            // dividing by 2 (0.50) restores 1.95. The extra lags now contribute
            // their information and nothing downstream is silently rescaled.
            // Lags 1 and 2 only, as upstream and as the initial commit had it.
            //
            // This sum runs for EVERY quiet move at EVERY node -- it feeds LMP,
            // futility, BNFP, history pruning, SEE pruning and the LMR/FDS
            // history terms -- and each `get` is a random read into a 5.3 MB
            // table. Reading six lags here instead of two tripled the random
            // memory traffic in the hottest loop in the engine.
            //
            // That cost is invisible on an idle machine and severe under
            // concurrency, which is exactly the pattern the games showed: on
            // unresolved positions this fork searched 16.4 ply against base's
            // 14.5 when games ran one at a time, and 14.5 against 14.5 at
            // concurrency 8 -- while base, which never added the reads, sat at
            // 14.5 in both. Roughly two ply of search, spent on cache misses.
            //
            // The extra lags are still WRITTEN by the update, and `score_quiet`
            // still reads all six for move ordering. This is only about the
            // per-move pruning input, where the traffic is multiplied by the
            // move count.
            td.quiet_history.get(threats, stm, mv)
                + td.continuation_history.get(conthist[0], moved, to)
                + td.continuation_history.get(conthist[1], moved, to)
        } else {
            let captured_type = td.board.type_on(mv.capture_sq());
            // `moved_piece(mv)` is `mailbox[mv.from()]`, the same lookup as the
            // hoisted `moved` above.
            td.noisy_history.get(threats, moved, to, captured_type)
        };

        if !NODE::ROOT && !is_loss(best_score) {
            // Only the pruning heuristics below consult this, so it is not
            // computed for root nodes or for nodes still sitting on a losing
            // best score, where none of them run.
            let is_direct_check = td.board.is_direct_check(mv);

            // Late Move Pruning (LMP)
            if !in_check
                && !is_direct_check
                && is_quiet
                && !is_win(beta)
                && move_count as i64
                    >= (p::lmp_base()
                        + p::lmp_improvement() * improvement / 16
                        + p::lmp_quad() * depth * depth
                        + p::lmp_history() * history / history_divisor)
                        // Artemis divides its threshold by `(2 - improving)`, so
                        // the NOT-improving case is halved -- stricter, pruning
                        // sooner when the position is going the wrong way. The
                        // improving case is the baseline, not a bonus.
                        //
                        // Getting that backwards doubles the improving threshold
                        // instead, which at depth 8 asks for 174 moves before LMP
                        // fires -- more than exist -- and silently disables LMP
                        // for every improving node.
                        //
                        // `lmp_improvement` is zeroed alongside this: leaving the
                        // additive term live too would penalise `improving` twice
                        // in one threshold.
                        // Widened for the multiply. `lmp_quad * depth * depth`
                        // reaches ~2.1M at depth 40, and multiplying that by up
                        // to 1024 overflows i32 -- which in release WRAPS, and a
                        // wrapped threshold can come out negative, making
                        // `move_count >= threshold` true on the first move and
                        // pruning every quiet at the node. The pre-existing form
                        // had no multiply and so no exposure; this is the cost of
                        // adding one, and i64 here is free.
                        as i64
                        // Floored at 256. At the top of its SPSA range
                        // `lmp_improving_mult` is 1024, which makes this scale 0
                        // and the whole threshold 0 -- LMP would then fire at
                        // move_count 0 and prune EVERY quiet on every
                        // non-improving node. `set_parameter` enforces no range
                        // either. A floor of 256 caps the effect at "prune 4x
                        // sooner", which is aggressive but survivable.
                        * (1024 - p::lmp_improving_mult() * !improving as i32).max(256) as i64
                        / (1024 * 1024)
            {
                skip_quiets = true;
                continue;
            }

            // Futility Pruning (FP)
            // The threat term scales the depth component rather than being added
            // to the total: `fp_depth * depth` is what carries the depth
            // dependence, and the flat parts (`eval`, history, corr) must not be
            // multiplied by a position feature.
            // Futility on the depth the move will ACTUALLY be searched at, not
            // the raw depth. Stockfish and Artemis both use `lmrDepth` here, and
            // the reasoning is the shape of the question: futility asks whether a
            // quiet move can recover a deficit, and a move about to be reduced a
            // ply is a weaker candidate than raw depth implies.
            //
            // Only the DEPTH component of the reduction is subtracted, not the
            // history component. Artemis routes history into futility solely
            // through `lmrDepth` and has no separate history term; this search
            // already has `fp_history` below, so folding history in here too
            // would count the same signal twice -- the defect class that cost
            // this engine 47% extra reduction on a missing TT move.
            // Kept in 1/1024 units. Reductions are stored in 1024ths of a ply and
            // are under one ply for every depth futility runs at (`depth < 14`),
            // so subtracting `r / 1024` as whole plies rounds to zero every time
            // and the whole term becomes a no-op. Scaling the margin by
            // `(depth * 1024 - r)` keeps the fraction.
            let fp_scaled_depth = if p::fp_lmr_depth() > 0 {
                (depth * 1024 - p::lmr_ilog() * depth.ilog2() as i32).max(1024)
            } else {
                depth * 1024
            };

            let futility_value = eval
                + threat_scaled(p::fp_depth() * fp_scaled_depth / 1024, p::fp_threat_density(), threat_density)
                + p::fp_history() * history / history_divisor
                + p::fp_beta_bonus() * (eval >= beta) as i32
                + p::fp_corr() * correction_value.abs() / 1024
                - p::fp_base();

            if !in_check && !is_direct_check && is_quiet && depth < 14 && futility_value <= alpha {
                if !is_decisive(best_score) && best_score < futility_value {
                    best_score = futility_value;
                }
                skip_quiets = true;
                continue;
            }

            // Bad Noisy Futility Pruning (BNFP)
            let noisy_futility_value = eval
                + p::bnfp_depth() * depth
                + p::bnfp_history() * history / 1024
                + p::bnfp_base()
                + p::bnfp_recapture() * (!NODE::ROOT && td.stack[ply - 1].mv.is_present() && mv.to() == td.stack[ply - 1].mv.to())
                    as i32;

            if !in_check
                && !is_direct_check
                && depth < 11
                && move_picker.stage() == Stage::BadNoisy
                && noisy_futility_value <= alpha
            {
                if !is_decisive(best_score) && best_score < noisy_futility_value {
                    best_score = noisy_futility_value;
                }
                // Prune the remaining bad noisy moves, but still allow any
                // deferred bad quiets to be searched.
                move_picker.skip_bad_noisy();
                continue;
            }

            // History Pruning (HP)
            // No check exemption, as in 0.1.2 (`4135b69`) -- same reasoning as
            // the SEE-pruning guard below.
            if !in_check && is_quiet && depth < 5 && history < -p::hp_margin() * depth {
                continue;
            }

            // Static Exchange Evaluation Pruning (SEE Pruning)
            // The cutoff_count term matches the same signal already used in
            // lmr_cutoff/fds_cutoff: many recent cutoffs at
            // this node's children is a signal to prune more freely here too.
            let threshold = if is_quiet {
                // The linear term is ADDED here, unlike the noisy branch below.
                // That asymmetry is deliberate, and 0.1.2 (`4135b69`) has it;
                // it was flipped to a subtraction at some point on the reasoning
                // that both branches should push the threshold the same way,
                // which is wrong for two reasons.
                //
                // First, the `.min(0)` clamp is only meaningful if this can go
                // positive. With the term added, the expression is positive
                // through depth 5 and the clamp pins the threshold at 0 --
                // "prune any quiet that loses material at all" -- before easing
                // off from depth 6 as the quadratic takes over. With it
                // subtracted the expression is negative at every depth (-41,
                // -133, -249, -389, ...) and the clamp becomes dead code, which
                // is the giveaway that the sign was not meant to be flipped.
                //
                // Second, the strength effect is large and in the wrong
                // direction: at depth 4 it moves the threshold from 0 to -389,
                // so instead of pruning every losing quiet the engine only
                // prunes those dropping nearly four pawns.
                (-p::see_q_quad() * depth * depth + p::see_q_lin() * depth - p::see_q_hist() * history / 1024
                    // Flat, deliberately. A d^2-scaled version was tried on the
                    // reasoning that a term should stay a constant fraction of
                    // the base it modifies -- but that contradicts the
                    // measurement it was meant to address. Excess eval
                    // volatility sits at the mover's depth 8-23 (ratio
                    // 1.15-1.19) and is absent at 24+ (1.00), and d^2 scaling
                    // made this term 2-8x LARGER across 12-24, i.e. more
                    // pruning inside the problem band. The magnitude is reduced
                    // Scaled by d^2/64 to hold at constant ~8% of base throughout.
                    // Flat 48 was 69.6% of base at depth 6, 16.4% at depth 8, 0.9% at depth 24.
                    // This volatility analysis shows excess eval at depth 8-23 (ratio 1.15-1.19),
                    // absent at 24+ (1.00). The d^2 scaling keeps the term proportional to its base.
                    + p::see_q_cutoff() * ((td.cutoff_count[ply + 1] > 2 && depth >= 6) as i32) * depth * depth / 64
                    + p::see_q_base())
                .min(0)
            } else {
                (-p::see_n_quad() * depth * depth - p::see_n_lin() * depth - p::see_n_hist() * history / 1024
                    // Same d^2/64 scaling as see_q_cutoff above, matching its -7d^2 base.
                    + p::see_n_cutoff() * ((td.cutoff_count[ply + 1] > 2 && depth >= 6) as i32) * depth * depth / 64
                    + p::see_n_base())
                .min(0)
            };

            // No check exemption here, as in 0.1.2 (`4135b69`). It was added
            // later by analogy with LMP/FP/HP, on the reasoning that a static
            // exchange estimate can't see a check's follow-up value.
            //
            // The analogy does not carry, because the exemptions are not the
            // same size. LMP/FP/HP each decline to prune a narrow, already
            // heavily qualified slice of quiet moves. SEE pruning is the
            // engine's main filter for losing captures, and `is_direct_check`
            // is broad -- so exempting it waves through every checking move at
            // every depth however much material it drops, and those all get
            // searched. That is one of the larger contributors to this
            // engine's tree being ~47% bigger than 0.1.2's at fixed depth.
            if !in_check && !td.board.see(mv, threshold) {
                continue;
            }
        }

        let extension_applies = move_count == 1 && (!singular_extension || mv == tt_move);
        let mut new_depth = depth - 1 + if extension_applies { extension } else { 0 };

        // Recapture extension: a capture landing on the square the
        // opponent's last move *captured* on, that doesn't lose material
        // itself, gets a full extra ply -- compensating for the horizon
        // effect at the end of a forced capture sequence. Checking the prior
        // move actually was a capture (not just that it ended on this
        // square) matters: otherwise a normal capture of a piece that
        // happened to move here would be misidentified as a recapture. A
        // different technique from the (removed) check extension: gated on
        // square repetition and SEE, not on giving check.
        //
        // The `new_depth`/`extension` conditions come first so the SEE call --
        // by far the most expensive term -- is only paid when the move would
        // otherwise drop straight into qsearch, instead of on every recapture
        // at every depth. It still has to be evaluated before `make_move`,
        // since SEE reads the pre-move board.
        // `!NODE::ROOT` first, matching `bnfp_recapture` above. Reading
        // `stack[ply - 1]` at ply 0 lands in the pre-root slot; that is inert
        // today only because the slot defaults to `Move::NULL` and
        // `is_capture()` is false for it. Relying on the default value of memory
        // outside the search is not a guard.
        let is_recapture = !NODE::ROOT
            && new_depth == 0
            && extension >= 0
            && !is_quiet
            && td.stack[ply - 1].mv.is_capture()
            && mv.to() == td.stack[ply - 1].mv.to()
            && td.board.see(mv, 0);

        // Only ever read back on the root's per-move node accounting.
        let initial_nodes = if NODE::ROOT { td.nodes() } else { 0 };

        make_move(td, ply, mv);

        // Pre-qsearch TT-move extension: at PV nodes, give a well-established
        // TT move one full ply instead of dropping it straight into qsearch.
        // Never override a negative (singular) extension decision.
        if NODE::PV && mv == tt_move && new_depth == 0 && extension >= 0 && tt_depth >= depth {
            new_depth = 1;
        }

        if is_recapture {
            new_depth = 1;
        }

        // Carry the extension budget down -- AFTER every extension has been
        // applied, and derived from the depth actually granted.
        //
        // This used to be written from the singular `extension` alone, before the
        // two assignments above ran. Both of those set `new_depth = 1` outright
        // at a node that would otherwise have dropped into qsearch, so both
        // extended without consuming budget and without being bounded by it --
        // and they fire precisely in the forcing lines (recapture chains, deep PV
        // TT moves) that the budget exists to bound.
        //
        // Measuring `new_depth - (depth - 1)` instead of reading `extension`
        // captures whatever granted the plies. The `- 1` keeps the original
        // policy: the first ply of extension is ordinary and free, only what
        // compounds beyond it is charged.
        let applied_extension = new_depth - (depth - 1);
        td.stack[ply + 1].double_extensions =
            td.stack[ply].double_extensions + (applied_extension - 1).max(0);

        let mut score = Score::ZERO;

        // Late Move Reductions (LMR)
        if depth >= 2 && move_count >= 2 {
            let mut reduction = p::lmr_ilog() * depth.ilog2() as i32;

            // Move-count scaling: later moves at this node get progressively
            // reduced more, via log2(moveCount) (as in Stockfish's
            // reductions[depth] * reductions[moveNumber] product -- kept
            // additive here to match this formula's existing style). Move
            // ordering already sorts likely-best moves first, so a later
            // move index is itself evidence the move is less promising.
            // Multiplicative in log(depth) x log(move_count), as every
            // reference formula is -- NOT additive.
            //
            // That distinction is what made this term cost 87 Elo at 240. Added
            // to the base rather than scaled by it, the move-count penalty is
            // identical at depth 2 and depth 32, so at shallow depths it swamps
            // the base: at depth 2 / move 32 it was 1200 units of extra
            // reduction on a 269-unit base -- 446%, or 1.17 plies on top of
            // 0.26. The measured signature was "same depth, thinner search"
            // (20.41 vs 20.65 against base), which is exactly over-reduction
            // that never converts into depth.
            //
            // Scaled by log2(depth) the term is a roughly constant fraction of
            // the base at every depth (~13% at move 8, ~22% at move 32), which
            // is what late-move reduction is supposed to express: reduce late
            // moves more, in proportion to how much you were reducing anyway.
            reduction += p::lmr_movecount_ilog() * depth.ilog2() as i32 * (move_count as u32).ilog2() as i32 / 16;

            reduction -= (p::lmr_improvement() * improvement / 128).clamp(p::lmr_improvement_lo(), p::lmr_improvement_hi());
            reduction -= p::lmr_corr() * correction_value.abs() / 1024;

            reduction += p::lmr_exact() * (bound == Bound::Exact) as i32;
            // Scales from the *second* raise, not the first: `lmr_exact` above
            // already covers "at least one raise" as a flat term, so counting
            // the first raise again here would double-fire both terms on the
            // same event (`bound == Bound::Exact` and `alpha_raises >= 1` are
            // set together, in the same branch). The previous version instead
            // kept `lmr_exact` hand-offset to `1412 - lmr_alpha_raise` so the
            // *sum* came out right on the first raise -- correct, but fragile:
            // retuning either constant independently (which is the entire
            // point of exposing them to SPSA) silently reintroduces the
            // overlap. Subtracting 1 here makes the two terms cover disjoint
            // raise counts by construction, so `lmr_exact` is free to return
            // to upstream's own value (see its definition) and either constant
            // can move independently without the other needing to compensate.
            // `.max(1)` on the cap before subtracting. `i32::clamp` PANICS when min > max,
            // so a cap of 0 makes this `clamp(0, -1)` and aborts the engine mid-search.
            // `spsa.config` starts the range at 3, but `set_parameter` enforces no range
            // at all -- the same exposure already guarded for `tm_trend_min/max` and the
            // four `.max(1)` divisors, missed here.
            reduction += p::lmr_alpha_raise() * (alpha_raises - 1).clamp(0, p::lmr_alpha_raise_cap().max(1) - 1);

            reduction += p::lmr_tt_alpha() * (is_valid(tt_score) && tt_score <= alpha) as i32;
            reduction += p::lmr_tt_depth() * (is_valid(tt_score) && tt_depth < depth) as i32;
            reduction += p::lmr_win_beta() * is_win(beta) as i32;

            if is_quiet {
                reduction += p::lmr_quiet_base();
                reduction -= p::lmr_quiet_hist() * history / 1024;
                // `is_valid` guard, as `complexity` has. In check `eval` is the
                // `Score::NONE` sentinel (32002) and `estimated_score` falls
                // through to it, so `alpha - estimated_score` is about -32000
                // and pins to the -65 clamp at every in-check node. The clamp
                // stops it being catastrophic, but the term then contributes a
                // CONSTANT reduction offset that measures nothing -- it is meant
                // to express how far below alpha the node sits.
                //
                // Same sentinel-leak class as the `complexity` defect documented
                // earlier in this file, which produced ~15 plies of negative
                // reduction before it was guarded. This one was missed.
                if is_valid(estimated_score) {
                    reduction += p::lmr_quiet_alpha() * ((alpha - estimated_score).clamp(-65, 91)) / 128;
                }
            } else {
                reduction += p::lmr_noisy_base();
                reduction -= p::lmr_noisy_hist() * history / 1024;
            }

            if NODE::PV {
                // `root_delta` is a plain field that starts at 0 and is only
                // assigned inside the aspiration loop. It is always >= 1 by the
                // time any node runs (that assignment is `beta - alpha` on a
                // window where alpha < beta), so the clamp changes nothing --
                // it just stops the one arrangement that would divide by zero
                // from being a crash instead of a no-op.
                reduction -= p::lmr_pv_base() + p::lmr_pv_delta() * (beta - alpha) / td.root_delta.max(1);
            }

            if tt_pv {
                reduction -= p::lmr_ttpv();
                reduction -= p::lmr_ttpv_score() * (is_valid(tt_score) && tt_score > alpha) as i32;
                reduction -= p::lmr_ttpv_depth() * (is_valid(tt_score) && tt_depth >= depth) as i32;
            } else if cut_node {
                reduction += p::lmr_cutnode();
                reduction += p::lmr_cutnode_null() * tt_move.is_null() as i32;
                // Compensation for IIR having already penalised the same
                // "no TT move" signal, applied only where IIR actually fired.
                // It needs depth >= 6 and a non-PV-line node; this bonus fires
                // at any depth on any cut node, so an unconditional subtraction
                // under-reduced late moves at depth 2-5 and on previous-PV
                // nodes, where there was no double-count to correct.
                // Gated on `iir_applied` alone. The extra `tt_move.is_null()` was
                // redundant while IIR could only fire on a null TT move, and
                // became wrong the moment the shallow-TT-entry arm was enabled:
                // those firings reduce `depth` exactly the same way, so they need
                // the same compensation. Leaving the old gate would have let the
                // new trigger reduce twice -- once via IIR, once via LMR -- with
                // nothing giving the ply back.
                reduction -= p::lmr_iir_comp() * iir_applied as i32;
            }

            // Capped: `alpha_raises` is bounded only by the move count, so at a
            // node where many moves improve in turn this term could reach
            // double-digit plies on its own. The `reduced_depth` clamp stops
            // that being unsound, but it would mean every late move searched at
            // depth 1 because of one signal. The cap is also what the signal
            // means -- the first few raises say "this node is still improving";
            // the twentieth says nothing the third did not.
            //
            // Applied above, alongside `lmr_exact`, now that the two terms
            // cover disjoint raise counts rather than both firing on the first
            // raise -- see the comment there.
            reduction -= p::lmr_complexity() * complexity / 1024;

            if td.board.in_check() {
                reduction -= p::lmr_check();
            }

            if td.cutoff_count[ply + 1] > 2 {
                reduction += p::lmr_cutoff();
                reduction += p::lmr_cutoff_node() * (!NODE::PV && !cut_node) as i32;
            }

            if is_valid(tt_move_score) && is_valid(singular_score) {
                let margin = tt_move_score - singular_score;
                reduction +=
                    (p::lmr_singular() * (margin - p::lmr_singular_margin()) / 128).clamp(0, p::lmr_singular_max());
            }

            // Extra reduction when the parent was heavily reduced, gated on the
            // position also not having improved for us.
            //
            // The parent-reduction half is upstream's. The `!opponent_worsening`
            // half follows PlentyChess, which fires the same idea only when
            // `staticEval <= -(prev staticEval)` -- exactly the negation of the
            // `opponent_worsening` already computed above for RFP. Reducing
            // further because the parent was reduced is better evidence when the
            // position has not turned our way; when it has, the parent's
            // reduction says less and the extra cut is the likelier to miss
            // something. Free: the signal is already in scope.
            //
            // Untested. Note this and the `lmr_cutnode_null` correction both
            // move total reduction, so one SPRT covering both cannot attribute
            // a result to either.
            if !NODE::PV && !opponent_worsening && td.stack[ply - 1].reduction > reduction + 414 {
                reduction += p::lmr_prev_reduction();
            }

            // Lazy-SMP reduction jitter, restored from 0.1.2 (`4135b69`), where
            // it was dropped without a note in the README's "Removed, and why"
            // section -- the only removal in this codebase's history that never
            // got one, which is why it reads as collateral damage from a rework
            // rather than a tested decision.
            //
            // Seeding on `td.id` desynchronises the helper threads: without it
            // every thread reduces identically and re-explores the same lines,
            // which is exactly the kind of loss that stays invisible at
            // Threads=1 and only shows up in multi-threaded play. Note it also
            // perturbs single-threaded search, since `td.nodes()` varies on its
            // own, and the window is deliberately off-centre -- `(x % 128) - 59`
            // has mean +4.5, not 0 -- so it carries a small reduction bias that
            // the surrounding constants were tuned against. Restored verbatim
            // for that reason rather than re-centred.
            reduction += ((td.nodes() + td.id as u64 * 27) % 128) as i32 - 59;

            // PV bonus inside the clamp, as Stockfish has it. Applied after the
            // clamp (upstream's form) the ceiling became new_depth + 4, so a PV
            // scout could run *deeper* than the full-window re-search at
            // new_depth that follows it -- and `new_depth > reduced_depth`, the
            // test guarding that re-search, could then never be true at a PV
            // node. Inside the clamp the ceiling stays new_depth + 2.
            let pv_bonus = 2 * NODE::PV as i32;
            let reduced_depth = (new_depth - reduction / 1024 + pv_bonus).clamp(1, new_depth + 2);

            // Published across the re-search as well, matching the FDS branch.
            //
            // Clearing it before the `new_depth` re-search told that child
            // "your parent did not reduce you" -- when the only reason it exists
            // is that the parent DID reduce, and the reduced scout came back
            // above alpha. Three consumers read this field: both hindsight depth
            // adjustments and `lmr_prev_reduction`/`fds_prev_reduction`, and all
            // three were reasoning about the re-search as though it were an
            // ordinary full-depth visit.
            td.stack[ply].reduction = reduction;
            score = -search::<NonPV>(td, -alpha - 1, -alpha, reduced_depth, true, ply + 1);
            td.stack[ply].reduction = 0;
            current_search_count += 1;

            if score > alpha {
                if !NODE::ROOT {
                    new_depth += (score > best_score + p::lmr_research_up()) as i32;
                    new_depth -= (score < best_score + p::lmr_research_down()) as i32;
                }

                if new_depth > reduced_depth {
                    score = -search::<NonPV>(td, -alpha - 1, -alpha, new_depth, !cut_node, ply + 1);
                    current_search_count += 1;
                }
            }
        }
        // Full Depth Search (FDS)
        else if !NODE::PV || move_count >= 2 {
            let mut reduction = p::fds_ilog() * depth.ilog2() as i32;

            // Same move-count scaling as LMR above; FDS covers the
            // move_count == 1 non-PV / move_count >= 2 case and had the same
            // gap.
            // Same multiplicative form as the LMR twin above.
            reduction += p::fds_movecount_ilog() * depth.ilog2() as i32 * (move_count as u32).ilog2() as i32 / 16;

            reduction -= (p::fds_improvement() * improvement / 128).clamp(p::fds_improvement_lo(), p::fds_improvement_hi());
            reduction -= p::fds_corr() * correction_value.abs() / 1024;

            if is_quiet {
                reduction += p::fds_quiet_base();
                reduction -= p::fds_quiet_hist() * history / 1024;
            } else {
                reduction += p::fds_noisy_base();
                reduction -= p::fds_noisy_hist() * history / 1024;
            }

            if tt_pv {
                reduction -= p::fds_ttpv();
                reduction -= p::fds_ttpv_depth() * (is_valid(tt_score) && tt_depth >= depth) as i32;
            } else if cut_node {
                reduction += p::fds_cutnode();
                reduction += p::fds_cutnode_null() * tt_move.is_null() as i32;
                // Same conditional IIR compensation as the LMR twin above.
                reduction -= p::fds_iir_comp() * iir_applied as i32;
            }

            if td.cutoff_count[ply + 1] > 2 {
                reduction += p::fds_cutoff();
                reduction += p::fds_cutoff_node() * (!NODE::PV && !cut_node) as i32;
            }

            if is_valid(tt_move_score) && is_valid(singular_score) {
                let margin = tt_move_score - singular_score;
                reduction +=
                    (p::fds_singular() * (margin - p::fds_singular_margin()) / 128).clamp(0, p::fds_singular_max());
            }

            if mv == tt_move {
                reduction -= p::fds_ttmove();
            }

            // Same PlentyChess gating as the LMR twin above.
            if !opponent_worsening && td.stack[ply - 1].reduction > reduction + 590 {
                reduction += p::fds_prev_reduction();
            }

            // Same Lazy-SMP jitter as in the LMR block above, restored from
            // 0.1.2. Distinct multiplier and offset (26 / -56 against 27 / -59)
            // so the two reduction paths do not share a jitter pattern; mean
            // here is +7.5.
            reduction += ((td.nodes() + td.id as u64 * 26) % 128) as i32 - 56;

            let reduced_depth = new_depth - (reduction >= p::fds_reduction_t1()) as i32 - (reduction >= p::fds_reduction_t2()) as i32;

            // Published for the child, exactly as the LMR branch does. Without
            // this the FDS half of the tree left `stack[ply].reduction` at the
            // 0 it was reset to, so all three consumers -- the hindsight
            // depth adjustments, `lmr_prev_reduction` and `fds_prev_reduction`
            // -- silently saw "parent was not reduced" for every FDS child and
            // never fired there.
            td.stack[ply].reduction = reduction;
            score = -search::<NonPV>(td, -alpha - 1, -alpha, reduced_depth, !cut_node, ply + 1);
            td.stack[ply].reduction = 0;
            current_search_count += 1;
        }

        // Principal Variation Search (PVS)
        if NODE::PV && (move_count == 1 || score > alpha) {
            if mv == tt_move && tt_depth > 1 {
                new_depth = new_depth.max(1);
            }

            score = -search::<PV>(td, -beta, -alpha, new_depth, false, ply + 1);
            current_search_count += 1;
        }

        undo_move(td, mv);

        if td.shared.status.get() == Status::STOPPED {
            return Score::ZERO;
        }

        if NODE::ROOT {
            update_root_move(td, mv, score, alpha, beta, move_count, initial_nodes);
        }

        if mv == tt_move {
            tt_move_score = score;
        }

        if score > best_score {
            best_score = score;

            if score > alpha {
                bound = Bound::Exact;
                best_move = mv;

                if !NODE::ROOT && NODE::PV {
                    td.pv_table.update(ply as usize, mv);
                }

                if score >= beta {
                    bound = Bound::Lower;
                    td.cutoff_count[ply] += 1;
                    break;
                }

                alpha = score;
                alpha_raises += 1;

                // `!excluded` here as well as on the final write below. During
                // a singular verification search this fired on every alpha
                // raise, storing a `Bound::Lower` at full depth for the node
                // being tested -- from a search with the best move removed. The
                // `mv != tt_move` guard does not help: the TT move is precisely
                // what the excluded search never tries.
                if !excluded && !(NODE::ROOT && td.pv_index > 0) && mv != tt_move {
                    // `tt_pv`, not a literal `true`. Passing `true` made every
                    // ordinary cut node that raised alpha mark itself as a PV-line
                    // node in the table. `tt_pv` gates `lmr_ttpv`,
                    // `lmr_ttpv_score`, `lmr_ttpv_depth`, `fds_ttpv`,
                    // `fds_ttpv_depth`, RFP's `!tt_pv` entry condition, the
                    // singular margin's `tt_pv && !NODE::PV` term and
                    // `nmp_ttpv` -- so over-setting it disables RFP and
                    // under-reduces at a share of nodes that grows as the table
                    // fills with these writes.
                    td.shared.tt.write(hash, depth, raw_eval, score, Bound::Lower, mv, ply, tt_pv, false);
                }
            }
        }

        // Bounded by the buffers' own capacity, not a hand-copied literal. The two
        // must agree or `ArrayVec::push` silently drops moves (it no longer
        // corrupts memory, but a dropped move is still a missed history update).
        if mv != best_move && (move_count as usize) < ArrayVec::<Move, 32>::CAPACITY {
            if is_quiet {
                quiet_moves.push(mv);
            } else {
                noisy_moves.push(mv);
            }
        }
    }

    if move_count == 0 {
        // `alpha`, as Stockfish and Artemis both return here
        // (`bestValue = excludedMove ? alpha : ...`).
        //
        // This returned `-TB_WIN_IN_MAX + 1` -- a near-loss -- which reaches the
        // singular block as `singular_score` and then trivially satisfies BOTH
        // `singular_score < singular_beta - double_margin` and
        // `< singular_beta - triple_margin`. So "the TT move is the only legal
        // move" granted a TRIPLE extension every time, which is a node explosion
        // in exactly the forced lines where extending buys least.
        //
        // `move_count == 0` under exclusion means the TT move was the only move,
        // not that the position is lost: returning `alpha` says "nothing here
        // beat alpha", which is what the singular test is actually asking.
        if excluded {
            return alpha;
        }

        return if in_check { mated_in(ply) } else { draw(td) };
    }

    if best_move.is_present() {
        update_best_move_histories::<NODE>(
            td,
            HistoryUpdate {
                ply,
                depth,
                beta,
                best_move,
                best_score,
                tt_move,
                stm,
                cut_node,
                in_check,
                move_count,
                current_search_count,
            },
            &quiet_moves,
            &noisy_moves,
        );
    }

    if !NODE::ROOT && bound == Bound::Upper && (cut_node || NODE::PV) {
        update_prior_move_histories(td, ply, depth, eval, best_score, stm, in_check);
    }

    tt_pv |= !NODE::ROOT && bound == Bound::Upper && move_count > 2 && td.stack[ply - 1].tt_pv;

    #[cfg(feature = "syzygy")]
    if NODE::PV {
        best_score = best_score.min(max_score);
    }

    if !(excluded || NODE::ROOT && td.pv_index > 0) {
        td.shared.tt.write(hash, depth, raw_eval, best_score, bound, best_move, ply, tt_pv, NODE::PV);
    }

    if !(in_check
        || best_move.is_noisy()
        || (bound == Bound::Upper && best_score >= eval)
        || (bound == Bound::Lower && best_score <= eval))
    {
        update_correction_histories(td, depth, best_score - eval, ply);
    }

    debug_assert!(alpha < beta);
    debug_assert!(-Score::INFINITE < best_score && best_score < Score::INFINITE);

    best_score
}

fn qsearch<NODE: NodeType>(td: &mut ThreadData, mut alpha: i32, beta: i32, ply: isize) -> i32 {
    debug_assert!(!NODE::ROOT);
    debug_assert!(ply as usize <= MAX_PLY);
    debug_assert!(-Score::INFINITE <= alpha && alpha < beta && beta <= Score::INFINITE);
    debug_assert!(NODE::PV || alpha == beta - 1);

    let draw_score = draw(td);
    if alpha < draw_score && td.board.upcoming_repetition(ply as usize) {
        alpha = draw_score;
        if alpha >= beta {
            return alpha;
        }
    }

    let in_check = td.board.in_check();

    if NODE::PV {
        td.pv_table.clear(ply as usize);
        td.sel_depth = td.sel_depth.max(ply as i32);
    }

    // Thread 0 polls the clock; helpers back it up on a coarser interval.
    //
    // Gating this on `td.id == 0` alone meant that if the main thread was
    // descheduled -- routine under the concurrency an SPRT harness runs at --
    // NOTHING enforced the hard bound until it was scheduled again. The helpers
    // were searching happily with no way to end the move.
    //
    // `nodes & 16383` implies `nodes & 2047` (check_time's own mask), so helpers
    // reach the elapsed-time test roughly one eighth as often as thread 0: enough
    // to stop a forfeit, rare enough that N threads are not contending on the
    // clock mutex.
    if (td.id == 0 || td.nodes() & 16383 == 16383) && td.time_manager.check_time(td) {
        td.shared.status.set(Status::STOPPED);
        return Score::ZERO;
    }

    if td.board.is_draw(ply) {
        return draw(td);
    }

    if ply as usize >= MAX_PLY - 1 {
        return if in_check { draw(td) } else { td.nnue.evaluate(&td.board) };
    }

    // Computed before the TT probe so the work overlaps the prefetched cache
    // line's arrival instead of serializing after the lookup.
    let correction_value = eval_correction(td, ply);

    let hash = td.board.hash();
    let entry = td.shared.tt.read(hash, td.board.fiftymove_clock(), ply);

    let mut tt_score = Score::NONE;
    let mut tt_bound = Bound::None;
    let mut tt_pv = NODE::PV;
    let mut tt_move = Move::NULL;

    // QS early TT cutoff
    if let Some(entry) = &entry {
        tt_score = entry.score;
        tt_bound = entry.bound;
        tt_pv |= entry.tt_pv;
        tt_move = entry.mv;

        if is_valid(tt_score)
            && (!NODE::PV || !is_decisive(tt_score))
            && match tt_bound {
                Bound::Upper => tt_score <= alpha,
                Bound::Lower => tt_score >= beta,
                _ => true,
            }
        {
            return tt_score;
        }
    }

    let raw_eval;
    let eval;
    let mut best_score;

    // Evaluation
    if in_check {
        raw_eval = Score::NONE;
        eval = Score::NONE;
        best_score = -Score::INFINITE;
    } else {
        raw_eval = match &entry {
            Some(entry) if is_valid(entry.raw_eval) => entry.raw_eval,
            _ => td.nnue.evaluate(&td.board),
        };
        eval = correct_eval(td, raw_eval, correction_value);
        best_score = eval;

        if is_valid(tt_score)
            && (!NODE::PV || !is_decisive(tt_score))
            && match tt_bound {
                Bound::Upper => tt_score < best_score,
                Bound::Lower => tt_score > best_score,
                _ => true,
            }
        {
            best_score = tt_score;
        }
    }

    // Stand Pat
    if best_score >= beta {
        if entry.is_none() {
            td.shared.tt.write(hash, TtDepth::SOME, raw_eval, best_score, Bound::Lower, Move::NULL, ply, tt_pv, false);
        }

        return best_score;
    }

    if best_score > alpha {
        alpha = best_score;
    }

    let mut best_move = Move::NULL;

    let mut move_count = 0;

    // Separate from `move_count`, which counts generated moves for the checkmate
    // test. This one gates `qs_move_cap`; see the note at the break below.
    let mut searched_count = 0;

    // Set when the move cap cut the node short, so the TT write at the end can
    // tell "examined everything" from "stopped looking".
    let mut truncated = false;

    // The hash move, first. The TT is probed above and written below, but the
    // stored move was never read, so qsearch -- most of the tree -- ordered
    // every node without the one move most likely to be best.
    //
    // A quiet entry must be dropped, and legality is not the property that
    // decides it. `Stage::HashMove` emits the TT move before `skip_quiets` is
    // ever consulted, and nothing downstream catches a quiet: delta pruning is
    // gated on `!mv.is_quiet()`, SEE pruning uses a threshold any quiet passes,
    // and at `move_count == 1` the late-move break cannot fire. So a quiet TT
    // move was searched, and since a quiet does not reduce material there is
    // nothing driving the recursion toward a quiet position -- qsearch could
    // wander until MAX_PLY. Upstream sidesteps this by passing `Move::NULL`
    // here and giving up the ordering; the gate below is Stockfish's, keeping
    // the ordering win in the cases where it is sound.
    //
    // In check is the exception: every evasion is generated, quiet ones
    // included, so a quiet entry is a legitimate member of the move pool.
    let qs_tt_move = if in_check || !tt_move.is_quiet() { tt_move } else { Move::NULL };
    let mut move_picker = MovePicker::new(qs_tt_move, None);

    // Quiets are only generated to serve as check evasions, and only while no
    // evasion found so far has proven non-losing. best_score does move once
    // moves are searched below, so this has to be re-evaluated against its
    // live value on every iteration -- freezing it as `!in_check` before the
    // loop starts (equivalent to `!in_check || false`) let quiet evasions
    // keep being generated for the rest of the node instead of stopping as
    // soon as the first adequate evasion is found.
    while let Some(mv) = move_picker.next::<NODE>(td, !in_check || !is_loss(best_score), ply) {
        move_count += 1;

        if !is_loss(best_score) {
            let is_direct_check = td.board.is_direct_check(mv);

            // Late Move Pruning (LMP)
            // RESTORED to `break`. The `continue` form is sounder (a check
            // ordered behind index 3 still gets searched) but visits every
            // remaining move to test `is_direct_check`. qsearch is most of the
            // tree and in practice all checks are ordered first (score_noisy
            // gives them a 200000 bonus), so by move_count 3 no check remains.
            // The +62 build had `break` and searched 1.9 plies deeper.
            // Counts moves that were SEARCHED, not moves that were generated.
            //
            // `move_count` increments at the top of the loop, before delta and
            // SEE pruning, so a node whose first captures were all pruned could
            // reach this break having searched ZERO moves -- and still return a
            // stand-pat-derived bound as though it had looked at the position.
            // With a cap of 3 that is not a rare corner: delta pruning rejects
            // the cheap captures first, which are exactly the ones the picker
            // emits last.
            //
            // `move_count` itself is deliberately left alone: `in_check &&
            // move_count == 0` below is the checkmate test, and it must keep
            // counting generated evasions or a node whose evasions were all
            // pruned would be scored as mate.
            // A recapture on the contested square is exempt, like a check.
            // See `qs_recapture_exempt`; `!NODE::ROOT` is implicit here because
            // qsearch is never entered at the root, but `ply > 0` still has to
            // hold before reading the previous move.
            let is_recapture = p::qs_recapture_exempt() != 0
                && ply > 0
                && td.stack[ply - 1].mv.is_capture()
                && mv.to() == td.stack[ply - 1].mv.to();

            // `.max(0)` before the cast. A negative `qs_move_cap` would wrap to
            // ~65535 as u16 and silently disable the cap entirely -- qsearch would
            // then search every generated move at every node. Same reason
            // `good_noisy_cap` is clamped before its `as usize`.
            if searched_count >= p::qs_move_cap().max(0) as u16 && !is_direct_check && !is_recapture {
                truncated = true;
                break;
            }

            // Delta pruning: skip a capture that can't plausibly raise
            // alpha even crediting the full value of the captured piece,
            // before the pricier SEE call below. Standard qsearch technique.
            // A non-capture promotion has nothing on the target square (so
            // type_on(mv.capture_sq()) alone would credit zero material
            // gain), so its own value swing is credited separately. Uses
            // capture_sq() rather than to() so en passant (whose captured
            // pawn isn't on the destination square) is credited correctly.
            // `eval` and `PieceType::value()` are on different scales, so the
            // material gain has to be converted before it can be added to an
            // evaluation.
            //
            // `value()` calls a pawn 109; the search's own units put a pawn at
            // `normalization()`, which is 321 at the start and 361-382 through
            // the middlegame and endgame -- a stable ~3x across the whole
            // material range. Added raw, this credited a captured queen 1242
            // where the same queen is worth ~3700-5300 to `eval`, understating
            // every capture threefold and pruning ones that genuinely could
            // reach alpha. In qsearch, which is most of the tree.
            //
            // Upstream converts in the other direction for its qsearch SEE
            // threshold (`(alpha - eval) / qs_see_div`); this is the same
            // conversion applied the other way. Tunable so SPSA can refine the
            // factor rather than leaving 3 as another hand-picked constant.
            // The material lookup is deferred behind the four cheap guards
            // rather than computed for every move. It costs a board probe plus
            // a `PieceType::value` match, and none of it is used when the node
            // is in check -- where the prune is disabled outright, yet every
            // check evasion, quiets included, was paying for it. Those are the
            // widest move lists qsearch generates, and qsearch is most of the
            // tree. Pure reordering: `&&` short-circuits, so the guards and the
            // test are evaluated in the same order and the outcome is
            // unchanged.
            if !in_check && !is_direct_check && !mv.is_quiet() && is_valid(eval) {
                let captured = td.board.type_on(mv.capture_sq()).value();
                let promotion_gain =
                    if mv.is_promotion() { mv.promo_piece_type().value() - PieceType::Pawn.value() } else { 0 };
                let delta_value =
                    eval + (captured + promotion_gain) * p::qs_delta_piece_scale() / 64 + p::qs_delta_margin();

                if delta_value < alpha {
                    // Fail-soft: `delta_value` is this move's optimistic upper
                    // bound, and it is strictly above the standing pat that
                    // seeded `best_score` (the material term is >= 0 and the
                    // margin is positive). Skipping the move without raising
                    // `best_score` therefore returns an upper bound lower than
                    // the one actually proven, and that bound gets stored as
                    // Bound::Upper -- the parent negates it and sees a score
                    // *higher* than justified.
                    //
                    // Upstream raises best_score at both of its own futility
                    // prunes, so this block was the one place in the codebase
                    // breaking a convention it inherited. The error is capped
                    // by the margin, so it shows up as many small overestimates
                    // rather than occasional large ones -- which is what this
                    // fork measures against upstream: +0.0032 eval optimism on
                    // identical positions and 22% more half-pawn collapses,
                    // with no excess above 2 pawns.
                    best_score = best_score.max(delta_value);
                    continue;
                }
            }

            // Static Exchange Evaluation Pruning (SEE Pruning)
            //
            // No check exemption, as in 0.1.2 (`4135b69`). It matters more here
            // than in the main search: qsearch already generates checks and
            // recaptures, so waving every checking move past the SEE filter
            // leaves losing checks to be searched at every qsearch node, and
            // qsearch nodes dominate the tree.
            if is_valid(eval)
                && !td.board.see(
                    mv,
                    (alpha - eval) / p::qs_see_div().max(1) - correction_value.abs().min(p::qs_see_corr_cap()) - p::qs_see_base(),
                )
            {
                continue;
            }
        }

        searched_count += 1;

        make_move(td, ply, mv);
        let score = -qsearch::<NODE>(td, -beta, -alpha, ply + 1);
        undo_move(td, mv);

        if td.shared.status.get() == Status::STOPPED {
            return Score::ZERO;
        }

        if score > best_score {
            best_score = score;

            if score > alpha {
                best_move = mv;

                if NODE::PV {
                    td.pv_table.update(ply as usize, mv);
                }

                if score >= beta {
                    break;
                }

                alpha = score;
            }
        }
    }

    if in_check && move_count == 0 {
        return mated_in(ply);
    }

    if best_score >= beta && best_move.is_noisy() {
        let bonus = p::qs_noisy_bonus();

        td.noisy_history.update(
            td.board.all_threats(),
            td.board.moved_piece(best_move),
            best_move.to(),
            td.board.type_on(best_move.capture_sq()),
            bonus,
        );
    }

    if best_score >= beta && !is_decisive(best_score) && !is_decisive(beta) {
        best_score = lerp(best_score, beta, 0.5072);
    }

    let bound = if best_score >= beta { Bound::Lower } else { Bound::Upper };

    // A truncated node may not publish an UPPER bound.
    //
    // `Bound::Upper` asserts "no move here reached alpha", and a node that
    // stopped after `qs_move_cap` moves never established that -- it stopped
    // looking. Written anyway, the claim was indistinguishable from a complete
    // one, and the qsearch TT cutoff at the top of this function accepts it with
    // no depth condition at all, so a bound derived from two moves propagated
    // through the table unchallenged.
    //
    // A LOWER bound is unaffected: a move that beat beta beat beta, and not
    // having looked at the rest cannot undo that. Fail-highs -- the common and
    // useful qsearch result -- are still cached exactly as before.
    if !(truncated && bound == Bound::Upper) {
        td.shared.tt.write(hash, TtDepth::SOME, raw_eval, best_score, bound, best_move, ply, tt_pv, false);
    }

    debug_assert!(alpha < beta);
    debug_assert!(-Score::INFINITE < best_score && best_score < Score::INFINITE);

    best_score
}

/// Records a root move's search result: the nodes it consumed, the score and
/// bound UCI reporting will quote for it, and its principal variation.
///
/// A move that neither led the list nor beat alpha is marked `-INFINITE` so it
/// sorts to the back without being confused for a real evaluation.
fn update_root_move(
    td: &mut ThreadData, mv: Move, score: i32, alpha: i32, beta: i32, move_count: u16, initial_nodes: u64,
) {
    let current_nodes = td.nodes();
    let root_move = td.root_moves.iter_mut().find(|v| v.mv == mv).unwrap();

    root_move.nodes += current_nodes - initial_nodes;

    if !(move_count == 1 || score > alpha) {
        root_move.score = -Score::INFINITE;
        return;
    }

    root_move.upperbound = false;
    root_move.lowerbound = false;

    match score {
        v if v <= alpha => {
            root_move.display_score = alpha;
            root_move.upperbound = true;
        }
        v if v >= beta => {
            root_move.display_score = beta;
            root_move.lowerbound = true;
        }
        _ => {
            root_move.display_score = score;
        }
    }

    root_move.score = score;
    root_move.sel_depth = td.sel_depth;
    root_move.pv.commit_full_root_pv(&td.pv_table, 1);

    if move_count > 1 && td.pv_index == 0 {
        td.best_move_changes += 1;
    }
}

/// The node context the post-search history updates need. Bundled purely to
/// keep the two update helpers from taking a dozen positional arguments.
#[derive(Copy, Clone)]
struct HistoryUpdate {
    ply: isize,
    depth: i32,
    beta: i32,
    best_move: Move,
    best_score: i32,
    tt_move: Move,
    stm: Color,
    cut_node: bool,
    in_check: bool,
    move_count: u16,
    current_search_count: i32,
}

/// Credits the move that proved best at this node across every history table,
/// and debits the moves that were searched ahead of it.
fn update_best_move_histories<NODE: NodeType>(
    td: &mut ThreadData, ctx: HistoryUpdate, quiet_moves: &ArrayVec<Move, 32>, noisy_moves: &ArrayVec<Move, 32>,
) {
    let HistoryUpdate { ply, depth, best_move, stm, cut_node, in_check, move_count, .. } = ctx;

    let noisy_bonus = (p::hist_noisy_bonus_slope() * depth).min(p::hist_noisy_bonus_cap()) - 43 - p::hist_noisy_bonus_cut() * cut_node as i32;
    let noisy_malus = (p::hist_noisy_malus_slope() * depth).min(p::hist_noisy_malus_cap()) - 58 - p::hist_noisy_malus_decay() * noisy_moves.len() as i32;

    // At non-PV nodes, scale the bonus up by how many other moves were
    // searched before this one proved best (as in Stockfish).
    let quiet_bonus = (p::hist_quiet_bonus_slope() * depth).min(p::hist_quiet_bonus_cap()) - 72 - p::hist_quiet_bonus_cut() * cut_node as i32
        + (p::hist_quiet_late_scale() * (move_count as i32 - 1)).min(p::hist_quiet_late_cap()) * !NODE::PV as i32;
    let quiet_malus = (p::hist_quiet_malus_slope() * depth).min(p::hist_quiet_malus_cap()) - 46 - p::hist_quiet_malus_decay() * quiet_moves.len() as i32;

    let cont_bonus = (p::hist_cont_bonus_slope() * depth).min(p::hist_cont_bonus_cap()) - 74 - p::hist_cont_bonus_cut() * cut_node as i32;
    let cont_malus = (p::cont_malus_slope() * depth).min(p::cont_malus_cap()) - 49 - p::hist_cont_malus_decay() * quiet_moves.len() as i32;

    if best_move.is_noisy() {
        td.noisy_history.update(
            td.board.all_threats(),
            td.board.moved_piece(best_move),
            best_move.to(),
            td.board.type_on(best_move.capture_sq()),
            noisy_bonus,
        );
    } else {
        td.quiet_history.update(td.board.all_threats(), stm, best_move, quiet_bonus);
        td.corrhist().pawn_history.update(
            td.board.pawn_key(),
            td.board.moved_piece(best_move),
            best_move.to(),
            quiet_bonus,
        );
        update_continuation_histories_in_check(
            td,
            ply,
            td.board.moved_piece(best_move),
            best_move.to(),
            cont_bonus,
            in_check,
        );

        if (ply as usize) < LowPlyHistory::MAX_LOW_PLY {
            td.low_ply_history.update(ply as usize, best_move, quiet_bonus);
        }

        for (i, &mv) in quiet_moves.iter().enumerate() {
            let denom = 1024 + p::quiet_malus_decay() * i as i32;
            let scale = 1024_i32 * 1024 / (denom * denom / 1024);
            td.quiet_history.update(td.board.all_threats(), stm, mv, -quiet_malus * scale / 1024);

            if (ply as usize) < LowPlyHistory::MAX_LOW_PLY {
                td.low_ply_history.update(ply as usize, mv, -quiet_malus * scale / 1024);
            }
            td.corrhist().pawn_history.update(
                td.board.pawn_key(),
                td.board.moved_piece(mv),
                mv.to(),
                -quiet_malus * scale / 1024,
            );
            update_continuation_histories_in_check(
                td,
                ply,
                td.board.moved_piece(mv),
                mv.to(),
                -cont_malus * scale / 1024,
                in_check,
            );
        }
    }

    for &mv in noisy_moves.iter() {
        let captured_type = td.board.type_on(mv.capture_sq());
        td.noisy_history.update(td.board.all_threats(), td.board.moved_piece(mv), mv.to(), captured_type, -noisy_malus);
    }

    // Track how often the TT move turns out to be the best move; feeds back
    // into the singular double-extension margin (as in Stockfish).
    if !NODE::PV && ctx.tt_move.is_present() {
        update_tt_move_history(
            td,
            if best_move == ctx.tt_move { p::tt_move_history_best() } else { p::tt_move_history_not_best() },
        );
    }

    if !NODE::ROOT && td.stack[ply - 1].mv.is_quiet() && td.stack[ply - 1].move_count < 2 {
        let malus = (p::prior_malus_slope() * depth - 52).min(p::prior_malus_cap());
        update_continuation_histories(td, ply - 1, td.stack[ply - 1].piece, td.stack[ply - 1].mv.to(), -malus);
    }

    if ctx.current_search_count > 1 && best_move.is_quiet() && ctx.best_score >= ctx.beta {
        let bonus = (p::research_bonus_slope() * depth - 86).min(p::research_bonus_cap());
        update_continuation_histories_in_check(td, ply, td.stack[ply].piece, best_move.to(), bonus, in_check);
    }
}

/// Rewards the opponent's previous move when this node failed low: whatever it
/// was, it kept us from raising alpha here.
fn update_prior_move_histories(
    td: &mut ThreadData, ply: isize, depth: i32, eval: i32, best_score: i32, stm: Color, in_check: bool,
) {
    let prior_move = td.stack[ply - 1].mv;

    if prior_move.is_quiet() {
        let factor = 88
            + (17 * td.stack[ply - 1].move_count as i32).min(229)
            + p::prior_f_tt_move() * (prior_move == td.stack[ply - 1].tt_move) as i32
            + p::prior_f_fail_low() * (!in_check && best_score <= eval - 97) as i32
            + p::prior_f_worsening() * (is_valid(td.stack[ply - 1].eval) && best_score <= -td.stack[ply - 1].eval - 136) as i32;

        let scaled_bonus = factor * (p::prior_bonus_slope() * depth - 37).min(p::prior_bonus_cap()) / 128;

        td.quiet_history.update(td.board.prior_threats(), !stm, prior_move, scaled_bonus);

        let entry = &td.stack[ply - 2];
        if entry.mv.is_present() {
            let bonus = (p::prior_lag2_slope() * depth - 47).min(p::prior_lag2_cap());
            td.continuation_history.update(entry.conthist, td.stack[ply - 1].piece, prior_move.to(), bonus);
        }
    } else if prior_move.is_noisy() {
        let captured_type = td.board.captured_piece().piece_type();
        let bonus = (p::prior_noisy_slope() * depth).min(p::prior_noisy_cap());

        // Keyed by the piece that MOVED, not the piece now standing on the
        // destination. `prior_move` has already been played, so `piece_on(to)`
        // is the promoted piece after a promotion -- while every reader keys by
        // `piece_on(mv.from())` / `moved_piece(mv)`, i.e. the pawn
        // (movepick.rs:296, search.rs:1654). Promotion bonuses were therefore
        // written to a slot nothing ever reads, and the slot the readers use was
        // never written.
        //
        // `stack[ply - 1].piece` is captured in `make_move` before the board is
        // updated, so it is the pre-move piece -- the same source the
        // continuation-history update directly above already uses.
        td.noisy_history.update(
            td.board.prior_threats(),
            td.stack[ply - 1].piece,
            prior_move.to(),
            captured_type,
            bonus,
        );
    }
}

/// Per-term weights for the correction blend, in 1024ths.
///
/// ALL 1024 -- i.e. unweighted, reproducing the pre-1.1.0 blend exactly.
///
/// The Artemis-derived ratios (pawn 1301, non-pawn 1154, material 905,
/// continuation 815) shipped untested inside the batch measured at -17.7 Elo
/// against 1.0.0-ed3afcdd (304 pairs, p = 0.011). `correction_value` reaches
/// razoring, RFP, both singular margins, futility, LMR, FDS and qsearch SEE, and
/// through `eval` another six consumers -- the widest blast radius of anything
/// in that batch, and the only one that changes a value every single node reads.
///
/// The ratios come from an engine whose margins are tuned to them. Reckless's
/// are not, and every Artemis port attempted this session has measured negative.
/// The weighting MACHINERY is kept, so re-testing is a four-constant edit.
///
/// Every term used to be summed at full strength, so pawn correction -- keyed on
/// the most stable feature of a position -- counted exactly as much as a single
/// continuation-correction lag. Stockfish and its derivatives weight them. The
/// ratios here are Artemis's (a GPL-3.0 Stockfish derivative), normalised to
/// pawn = 1.00: non-pawn 0.887, minor/material 0.695, continuation 0.627.
///
/// **The total is load-bearing, and deliberately not SPSA-tunable.**
/// `correction_value` feeds razoring, RFP, both singular margins, futility, LMR,
/// FDS and qsearch SEE, and through `eval` it reaches null move, stand-pat,
/// improving, opponent-worsening, LMP and BNFP. Changing the blend's magnitude
/// silently rescales all of them -- the defect class this file documents
/// repeatedly, and the one that has cost this fork Elo more than once. Exposing
/// these to a tuner would let it wander the scale while appearing to tune the
/// ratios, so they are consts with a build-time check, exactly as
/// `CONTHIST_WEIGHTS` is in movepick.rs.
///
/// Redistribute freely; do not change the total.
const CORR_W_PAWN: i32 = 1024;
const CORR_W_NONPAWN: i32 = 1024;
const CORR_W_MATERIAL: i32 = 1024;
const CORR_W_CONT: i32 = 1024;

/// Six unweighted terms came to `6 * 1024`. Anything else rescales the blend.
const CORR_W_TOTAL: i32 = 6 * 1024;

const _: () = assert!(
    CORR_W_PAWN + 2 * CORR_W_NONPAWN + CORR_W_MATERIAL + 2 * CORR_W_CONT == CORR_W_TOTAL,
    "correction weights must sum to 6144, or every margin reading correction_value is rescaled"
);

fn eval_correction(td: &ThreadData, ply: isize) -> i32 {
    let stm = td.board.side_to_move();
    let bucket = td.board.fiftymove_clock_bucket();
    let corrhist = td.corrhist();

    // Per-term weights, in 1024ths. Previously every term was summed at full
    // strength, so pawn correction -- keyed on the most stable feature of a
    // position -- counted exactly as much as a single continuation-correction
    // lag. Stockfish and its derivatives weight them, and the ratios below are
    // taken from Artemis (a GPL-3.0 Stockfish derivative), normalised so that
    // pawn = 1.00: non-pawn 0.887, minor/material 0.695, continuation 0.627.
    //
    // THE WEIGHT TOTAL IS LOAD-BEARING. `correction_value` feeds razoring, RFP,
    // both singular margins, futility, LMR, FDS and qsearch SEE, and through
    // `eval` it reaches null move, stand-pat, improving, opponent-worsening,
    // LMP and BNFP. Changing the blend's magnitude silently rescales every one
    // of them -- the defect class this file documents repeatedly. The weights
    // therefore sum to 6 * 1024 = 6144, exactly what six unweighted terms came
    // to, and the divisor gains a matching factor of 1024. Redistribute freely;
    // do not change the total. The assertion below makes that a build error.
    (CORR_W_PAWN * corrhist.pawn.get(stm, td.board.pawn_key(), bucket)
        + CORR_W_NONPAWN
            * (corrhist.non_pawn[Color::White].get(stm, td.board.non_pawn_key(Color::White), bucket)
                + corrhist.non_pawn[Color::Black].get(stm, td.board.non_pawn_key(Color::Black), bucket))
        + CORR_W_MATERIAL * corrhist.material.get(stm, td.board.material_key(), bucket)
        + CORR_W_CONT
            * (td.continuation_corrhist.get(
                td.stack[ply - 2].contcorrhist,
                td.stack[ply - 1].piece,
                td.stack[ply - 1].mv.to(),
            ) + td.continuation_corrhist.get(
                td.stack[ply - 4].contcorrhist,
                td.stack[ply - 1].piece,
                td.stack[ply - 1].mv.to(),
            )))
        // `CORR_W_TOTAL / 6`, not a bare 1024. `corr_weight_div` was derived as
        // `64 * 6 / 5` for six UNWEIGHTED terms, so the divisor has to carry
        // whatever per-term scale the weights above use. Writing that scale as a
        // literal left two constants that must agree with nothing connecting
        // them: change `CORR_W_TOTAL` and this silently goes stale, rescaling
        // every margin that reads `correction_value`.
        / (p::corr_weight_div().max(1) * (CORR_W_TOTAL / 6))
}

fn update_correction_histories(td: &mut ThreadData, depth: i32, diff: i32, ply: isize) {
    let stm = td.board.side_to_move();
    let bucket = td.board.fiftymove_clock_bucket();
    let corrhist = td.corrhist();
    let bonus = (p::corr_bonus_scale() * depth * diff / 128).clamp(-p::corr_bonus_min(), p::corr_bonus_max());

    corrhist.pawn.update(stm, td.board.pawn_key(), bucket, bonus);

    corrhist.non_pawn[Color::White].update(stm, td.board.non_pawn_key(Color::White), bucket, bonus);
    corrhist.non_pawn[Color::Black].update(stm, td.board.non_pawn_key(Color::Black), bucket, bonus);
    corrhist.material.update(stm, td.board.material_key(), bucket, bonus);

    if td.stack[ply - 1].mv.is_present() && td.stack[ply - 2].mv.is_present() {
        td.continuation_corrhist.update(
            td.stack[ply - 2].contcorrhist,
            td.stack[ply - 1].piece,
            td.stack[ply - 1].mv.to(),
            bonus,
        );
    }

    if td.stack[ply - 1].mv.is_present() && td.stack[ply - 4].mv.is_present() {
        td.continuation_corrhist.update(
            td.stack[ply - 4].contcorrhist,
            td.stack[ply - 1].piece,
            td.stack[ply - 1].mv.to(),
            bonus,
        );
    }
}

fn update_continuation_histories(td: &mut ThreadData, ply: isize, piece: Piece, sq: Square, bonus: i32) {
    update_continuation_histories_in_check(td, ply, piece, sq, bonus, false);
}

fn update_continuation_histories_in_check(
    td: &mut ThreadData, ply: isize, piece: Piece, sq: Square, bonus: i32, in_check: bool,
) {
    // Per-lag weights and positive-consistency multipliers, as in Stockfish:
    // all six lags are updated, and the more continuation entries for this
    // move are already positive, the stronger the update. Lags 1/2/4/6 are
    // weighted equally (matching the already-tuned baseline, which treated those
    // four lags at full and equal strength); lags 3/5 are fork additions kept at
    // Stockfish's relative ratio to the primary weight. All SPSA-tunable.
    //
    // Dropping 3 and 5 was tried and reverted. Both tables do weight them far
    // below the rest -- 195 and 89 here against 700, and 277 and 126 in movepick
    // against ~1000-1600 -- and redistributing movepick's share of that weight
    // proportionally reproduces upstream's four-lag set to the unit. Suggestive,
    // but not a measurement, and it shipped untested inside a batch that cost
    // ~60 Elo. Retest it alone if at all: it changes move ordering at every
    // node.
    let conthist_bonuses: [(isize, i32); 6] = [
        (1, p::conthist_lag1()),
        (2, p::conthist_lag2()),
        (3, p::conthist_lag3()),
        (4, p::conthist_lag4()),
        (5, p::conthist_lag5()),
        (6, p::conthist_lag6()),
    ];
    let multipliers: [i32; 7] = [
        p::conthist_mult0(),
        p::conthist_mult1(),
        p::conthist_mult2(),
        p::conthist_mult3(),
        p::conthist_mult4(),
        p::conthist_mult5(),
        p::conthist_mult6(),
    ];

    // "How many of this move's continuation entries already favour it" is a
    // property of the move, so the count has to be complete before any bonus
    // is applied and every lag has to be scaled by the same multiplier.
    //
    // This used to increment and index `multipliers` inside the update loop,
    // making it a running prefix count instead: lag 1 could only ever reach
    // multipliers[1] and lag 6 could reach multipliers[6], so the nearest
    // lags were damped (94-103) relative to the distant ones (121-126) purely
    // by their position in the loop rather than by anything about the
    // position on the board -- and lags 1/2 are the ones move ordering leans
    // on hardest.
    //
    // Needing the total up front would normally mean walking the stack twice,
    // which this function -- called on every cutoff -- should not pay for. So
    // the first pass caches each eligible entry's subtable pointer and the
    // second reuses them, leaving exactly one stack traversal as before.
    let mut targets = [(std::ptr::null_mut::<[[i16; 64]; 13]>(), 0i32, 0isize); 6];
    let mut len = 0;
    let mut positive_count = 0;

    for (offset, weight) in conthist_bonuses {
        // Only update the nearest two continuation histories when in check.
        if in_check && offset > 2 {
            break;
        }

        let entry = &td.stack[ply - offset];
        if entry.mv.is_present() {
            if td.continuation_history.get(entry.conthist, piece, sq) > 0 {
                positive_count += 1;
            }

            targets[len] = (entry.conthist, weight, offset);
            len += 1;
        }
    }

    // Indexed by how many of the lags *considered* agreed, not by the raw
    // count. In check the loop stops after lag 2, so a raw count can never
    // exceed 2 and the multipliers tuned for indices 3-6 are unreachable there
    // -- an in-check node with both lags positive got index 2 where an
    // out-of-check node with all six positive got index 6, for the same
    // "everything agrees" state. Scaling by `len` puts both on the same
    // footing. `len` is 0 only when no lag had a move, and index 0 is the
    // no-agreement multiplier, which is the right answer for that case.
    let multiplier = multipliers[if len == 0 { 0 } else { positive_count * 6 / len }];

    for &(conthist, weight, offset) in &targets[..len] {
        // Overall scale stays SPSA-tunable.
        let scaled = bonus * weight * multiplier / p::conthist_div().max(1) + 73 * (offset < 2) as i32;
        td.continuation_history.update(conthist, piece, sq, scaled);
    }
}

/// Gravity-style update of the global TT-move reliability statistic, bounded
/// to roughly [-8192, 8192] like Stockfish's `TTMoveHistory`.
/// Gravity update for ProbCut's acceptance rate. Same form and bound as
/// `update_tt_move_history`.
/// Casts or retracts this thread's soft-stop vote.
///
/// Votes are level-triggered, not edge-triggered: a thread that has voted stays
/// voted until it retracts, and the search stops once 65% of threads agree.
/// That only works if every thread keeps reaching a point where it can answer,
/// which is why the Lazy SMP skip path calls this too.
fn soft_stop_vote(td: &mut ThreadData, thread_count: usize, voted: &mut bool, multiplier: f32) {
    if !td.time_manager.use_time_management() {
        return;
    }

    if td.time_manager.soft_limit(td, || multiplier) {
        if !*voted {
            *voted = true;

            let votes = td.shared.soft_stop_votes.fetch_add(1, Ordering::AcqRel) + 1;

            // Capped so at least one thread may lag without blocking the stop.
            //
            // `(n * 65).div_ceil(100)` alone returns 2 at n = 2 -- unanimity, not
            // a 65% majority. A vote is only cast at an iteration boundary, so a
            // single helper still mid-iteration held the whole search open and the
            // move ran to the HARD bound instead of the soft one. At Threads = 2,
            // the most common multi-threaded setting, that means spending the
            // emergency allowance on every move.
            //
            // The cap keeps the intended proportion everywhere it is already
            // achievable (3 of 4, 6 of 8) and only binds at n = 2, where 65% has
            // no sensible integer reading. `max(1)` keeps the single-threaded case
            // at 1 rather than 0.
            let majority = (thread_count * 65)
                .div_ceil(100)
                .min(thread_count.saturating_sub(1).max(1));
            if votes >= majority {
                td.shared.status.set(Status::STOPPED);
            }
        }
    } else if *voted {
        *voted = false;
        td.shared.soft_stop_votes.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Scales a pruning margin by threat density, as a proportion of the margin.
///
/// This exists because the additive version of the same idea cost roughly 60
/// Elo. `threat_density` contributed a flat `coefficient * density`, and the
/// margins it modified scale with depth: the RFP margin is 11 units at depth 1
/// and 727 at depth 8, so a flat +42 was **+382% at depth 1 and +6% at depth 8**.
/// RFP and futility stopped firing near the leaves, which is where most nodes
/// are, and the tree exploded.
///
/// No choice of constant fixes that, because the shape is wrong rather than the
/// size. Expressed as a proportion the term is the same percentage at every
/// depth by construction, and the failure mode cannot recur however the
/// coefficient is tuned -- which is the only kind of guard worth calling one.
///
/// The `.max(0)` on the base matters: a negative base (futility's depth term is
/// -48 at depth 1) would have its sign amplified rather than its magnitude, so
/// scaling is skipped there instead.
fn threat_scaled(base: i32, coefficient: i32, density: i32) -> i32 {
    if base <= 0 || coefficient == 0 {
        return base;
    }

    // Clamped at both ends, and each end is load-bearing.
    //
    // Upper (1536, +50%): a proportional term is safe in *shape* but still
    // unbounded in *size* if the coefficient and the density cap are both tuned
    // upward, and an RFP margin at twice its tuned value prunes almost nothing.
    //
    // Lower (1024, no change): the whole point of the signal is to prune *less*
    // when material is hanging. A negative coefficient -- which `set_parameter`
    // accepts, since it enforces no range -- would invert that and prune *more*
    // in exactly the tactical positions where the static eval is least
    // trustworthy. The floor makes the term one-directional by construction.
    //
    // `saturating_mul` because the same unchecked setter makes
    // `coefficient * density` an overflow away from wrapping negative, which
    // would defeat the floor by arriving underneath it.
    let scale = 1024i32.saturating_add(coefficient.saturating_mul(density)).clamp(1024, 1536);

    // Widened for the multiply. With `rfp_depth_quad` at the top of its range
    // and depth near MAX_PLY the base reaches ~808k, and multiplying by the
    // 1536 cap leaves only 1.7x of i32 headroom -- and `set_parameter` does not
    // enforce the range, so that headroom is not guaranteed at all. The 64-bit
    // multiply costs nothing here and removes the question; the result is back
    // in range by construction, since dividing by 1024 undoes the widening.
    // Clamped on the way back down. The i64 multiply removes overflow DURING the
    // product, but the result is up to 1.5x `base`, so a base near i32::MAX
    // would still truncate on the cast. Only reachable through an unchecked
    // `setoption`, and free to rule out.
    ((base as i64 * scale as i64) / 1024).clamp(i32::MIN as i64, i32::MAX as i64) as i32
}

fn update_probcut_history(td: &mut ThreadData, bonus: i32) {
    let bonus = bonus.clamp(-8192, 8192);
    let entry = td.probcut_history;
    td.probcut_history = entry + bonus - entry * bonus.abs() / 8192;
}

fn update_tt_move_history(td: &mut ThreadData, bonus: i32) {
    // Unlike every other gravity-style history update in the codebase, this
    // one was missing the initial clamp -- the multicut caller can pass
    // |bonus| > 8192 at high depth, letting the tracked value briefly
    // overshoot its documented [-8192, 8192] bound.
    let bonus = bonus.clamp(-8192, 8192);
    let entry = td.tt_move_history;
    td.tt_move_history = entry + bonus - entry * bonus.abs() / 8192;
}

/// Detects repetitive piece shuffling near the 50-move rule so that singular
/// extensions can be disabled there, limiting search explosions (Stockfish #6447).
fn is_shuffling(td: &ThreadData, tt_move: Move, ply: isize) -> bool {
    if !tt_move.is_quiet() || td.board.fiftymove_clock() < 10 || ply < 4 {
        return false;
    }

    // `plies_from_null < 6`, as Stockfish and Artemis both have it. The test
    // below walks back two and four plies expecting to land on OUR own moves,
    // and a null move passes the turn without a move being played -- so with one
    // inside the window, `stack[ply - 2]` can be the opponent's move and the
    // from/to chain is then comparing squares from two different sides.
    //
    // The existing `is_present()` checks catch a null move landing exactly on
    // ply-2 or ply-4 (those slots hold `Move::NULL`), but not one at ply-1 or
    // ply-3, which shifts the parity without leaving a null in either slot.
    if (td.board.plies_from_null() as i32) < p::shuffle_null_guard() {
        return false;
    }

    let prev2 = td.stack[ply - 2].mv;
    let prev4 = td.stack[ply - 4].mv;

    prev2.is_present() && prev4.is_present() && tt_move.from() == prev2.to() && prev2.from() == prev4.to()
}

fn make_move(td: &mut ThreadData, ply: isize, mv: Move) {
    td.shared.tt.prefetch(td.board.key_after(mv));

    // Hoisted. `moved_piece(mv)` is `piece_on(mv.from())` -- a mailbox read --
    // and it was performed three times, `in_check()` twice, all on a board that
    // does not change until `make_move` below. This runs at every node.
    //
    // Whether the compiler eliminated them was not obvious either way:
    // `subtable_ptr` takes `&mut self`, so there is a mutable borrow between the
    // reads, and CSE across it depends on LLVM disambiguating two fields of the
    // same `&mut ThreadData`. Hoisting removes the question rather than relying
    // on it, and the values are identical by construction.
    let moved = td.board.moved_piece(mv);
    let in_check = td.board.in_check();
    let noisy = mv.is_noisy();
    let to = mv.to();

    td.stack[ply + 1].follow_pv = td.stack[ply].follow_pv && td.previous_pv.get(ply as usize) == Some(&mv);
    td.stack[ply].mv = mv;
    td.stack[ply].piece = moved;
    td.stack[ply].conthist = td.continuation_history.subtable_ptr(in_check, noisy, moved, to);
    td.stack[ply].contcorrhist = td.continuation_corrhist.subtable_ptr(in_check, noisy, moved, to);

    td.shared.nodes.increment(td.id);

    td.nnue.push(mv, &td.board);
    td.board.make_move(mv, &mut td.nnue);

    td.shared.tt.prefetch(td.board.hash());
}

fn undo_move(td: &mut ThreadData, mv: Move) {
    td.nnue.pop();
    td.board.undo_move(mv);
}

fn lerp(a: i32, b: i32, t: f32) -> i32 {
    t.mul_add((b - a) as f32, a as f32) as i32
}