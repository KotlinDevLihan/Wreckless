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
    let mut last_best_rootmove = RootMove::default();

    let mut eval_stability = 0;
    let mut pv_stability = 0;
    let mut soft_stop_voted = false;

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

    // Iterative Deepening
    for depth in 1..MAX_PLY as i32 {
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
            let mut delta = (23 - eval_stability.min(pv_stability).min(7)).max(10);
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
                        delta += 26 * delta / 128;
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
                        average[td.pv_index] = if average[td.pv_index] == Score::NONE {
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

        if td.id == 0
            && let Limits::Mate(moves) = td.time_manager.limits()
            && Score::MATE - td.root_moves[0].score.abs() <= moves as i32 * 2
        {
            td.shared.status.set(Status::STOPPED);
            break;
        }

        let iter_value = iter_values[(depth % 4) as usize];
        iter_values[(depth % 4) as usize] = td.root_moves[0].score;

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
                // pawn at ~208 where ours is ~321-382. Carried over unscaled,
                // the old 0.0480 saturated the ceiling after a 0.04-pawn drop
                // and pinned the result to the floor on any gain at all: a
                // bang-bang switch with no proportional band, which is also
                // why SPSA never found signal in these constants. Rescaled so
                // the ceiling is reached at ~0.37 pawns, matching SF's band in
                // pawn terms.
                //
                // Held in fixed point (1e-4) rather than f32 so the constants
                // can live in `parameters.rs` and actually be tuned now that
                // there is a gradient to tune against. The integer values are
                // exactly the old floats' digits, so this reintroduces no
                // rounding of its own.
                let trend = (p::tm_trend_base()
                    + p::tm_trend_diff() * difference
                    + p::tm_trend_recent() * recent)
                    .clamp(p::tm_trend_min(), p::tm_trend_max());

                trend as f32 / 10000.0
            };

            let pv_stability = (1.2881 - 0.0440 * pv_stability as f32).max(0.7160);

            let eval_stability = (1.2664 - 0.0416 * eval_stability as f32).max(0.8642);

            let best_move_stability = 1.1500 + (0.2526 * td.best_move_changes as f32).ln_1p();

            nodes * pv_stability * eval_stability * score_trend * best_move_stability
        };

        if td.time_manager.use_time_management() {
            if td.time_manager.soft_limit(td, multiplier) {
                if !soft_stop_voted {
                    soft_stop_voted = true;

                    let votes = td.shared.soft_stop_votes.fetch_add(1, Ordering::AcqRel) + 1;
                    let majority = (thread_count * 65).div_ceil(100);
                    if votes >= majority {
                        td.shared.status.set(Status::STOPPED);
                    }
                }
            } else if soft_stop_voted {
                soft_stop_voted = false;
                td.shared.soft_stop_votes.fetch_sub(1, Ordering::AcqRel);
            }
        }

        if td.shared.status.get() == Status::STOPPED {
            break;
        }
    }

    if matches!(td.time_manager.limits(), Limits::Infinite) {
        while td.shared.status.get() != Status::STOPPED {
            std::hint::spin_loop();
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

    if td.id == 0 && td.time_manager.check_time(td) {
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
            if tt_move.is_quiet() && tt_score >= beta && td.stack[ply - 1].move_count < 4 {
                let quiet_bonus = (190 * depth - 81).min(1691);
                let cont_bonus = (96 * depth - 73).min(1206);

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

            if td.board.fiftymove_clock() < 90 {
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

        if reduction >= 2249 && eval_delta < 0 {
            depth += 1;
        }

        if !tt_pv && depth >= 2 && reduction > 0 && eval_delta > 57 {
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
    };

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
    let complexity = if is_valid(eval) && is_valid(tt_score) && !is_decisive(tt_score) {
        (eval - tt_score).abs().min(p::complexity_cap())
    } else {
        0
    };

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
        && !tt_move.is_quiet()
        && tt_bound != Bound::Lower
    {
        return qsearch::<NonPV>(td, alpha, beta, ply);
    }

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
        && (!tt_move.is_quiet() || td.quiet_history.get(td.board.all_threats(), stm, tt_move) >= -2048)
        && estimated_score
            >= beta
                + (p::rfp_depth_quad() * depth * depth / 128 - p::rfp_improvement() * improvement / 1024
                    + p::rfp_depth_lin() * depth
                    + p::rfp_corr() * correction_value.abs() / 1024
                    + p::rfp_complexity() * complexity / 1024
                    - p::rfp_no_threats() * (td.board.all_threats() & td.board.colors(stm)).is_empty() as i32
                    - p::rfp_worsening() * opponent_worsening as i32
                    - p::rfp_base())
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
        // Zugzwang guard: material() sums every piece including pawns, so a
        // pawn-heavy, piece-empty endgame (the textbook zugzwang scenario
        // this check exists to catch) could pass a material()-based
        // threshold. non_pawn_material() is the correct signal here.
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
        && td.board.non_pawn_material() > p::nmp_material()
        && !is_loss(beta)
        && !is_win(estimated_score)
        && !(tt_bound == Bound::Lower
            && tt_move.is_capture()
            && td.board.piece_on(tt_move.to()).value() >= PieceType::Knight.value())
    {
        debug_assert_ne!(td.stack[ply - 1].mv, Move::NULL);

        let r = (p::nmp_r_base()
            + p::nmp_r_improving() * improving as i32
            + p::nmp_r_depth() * depth
            + p::nmp_r_beta() * (estimated_score - beta).clamp(0, p::nmp_r_beta_max()) / 128)
            / 1024;

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

        if td.shared.status.get() == Status::STOPPED {
            return Score::ZERO;
        }

        if score >= bound && !is_win(score) {
            if td.nmp_min_ply > 0 || depth < 16 {
                // Trusted immediately -- no verification search follows on
                // this path, so it's safe to feed correction history here.
                if score > eval {
                    update_correction_histories(td, depth, score - eval, ply);
                }
                return score;
            }

            let reduced_depth = depth - r;

            td.nmp_min_ply = ply as i32 + 3 * reduced_depth / 4;
            let verified_score = search::<NonPV>(td, bound - 1, bound, reduced_depth, false, ply);
            td.nmp_min_ply = 0;

            if td.shared.status.get() == Status::STOPPED {
                return Score::ZERO;
            }

            if verified_score >= bound {
                // Only now is the fail-high actually confirmed. Feeding
                // correction history before this point (as a previous version
                // of this code did) meant an update could go in from a score
                // the very next line then refused to trust as a cutoff --
                // the same "sub-search result isn't comparable to a genuine
                // full-search sample" problem documented for the
                // singular-multicut correction update this fork tried and
                // reverted.
                if score > eval {
                    update_correction_histories(td, depth, score - eval, ply);
                }
                return score;
            }
        }
    }

    // Internal Iterative Reductions (IIR): at sufficient depth, reduce PV and
    // expected cut nodes that have no TT move to anchor move ordering. Nodes
    // on the previous iteration's PV are exempt (as in Stockfish).
    let iir_applied =
        !NODE::ROOT && !td.stack[ply].follow_pv && (NODE::PV || cut_node) && depth >= 6 && tt_move.is_null();

    if iir_applied {
        depth -= 1;
    }

    // ProbCut
    let mut probcut_beta = beta + p::probcut_base() - p::probcut_improving() * improving as i32;

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
    if cut_node
        && !in_check
        && !is_win(beta)
        && if is_valid(tt_score) { tt_score >= probcut_beta && !is_decisive(tt_score) } else { eval >= beta }
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

            if score >= probcut_beta && probcut_depth > 0 {
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

            if score >= probcut_beta {
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
    let mut singular_score = Score::NONE;

    if !NODE::ROOT && !excluded && potential_singularity && !is_shuffling(td, tt_move, ply) {
        debug_assert!(is_valid(tt_score));

        let singular_margin = if tt_bound == Bound::Exact { (depth as u32).div_ceil(4) as i32 } else { depth }
            + depth * (tt_pv && !NODE::PV) as i32;
        let singular_beta = tt_score - singular_margin;
        let singular_depth = (depth - 1) / 2;

        td.excluded[ply] = tt_move;
        td.stack[ply].mv = Move::NULL;
        singular_score = search::<NonPV>(td, singular_beta - 1, singular_beta, singular_depth, cut_node, ply);
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

            extension = 1;
            extension += (singular_score < singular_beta - double_margin) as i32;
            extension += (singular_score < singular_beta - triple_margin) as i32;
        }
        // Multi-Cut
        else if singular_score >= beta && !is_decisive(singular_score) {
            update_tt_move_history(td, p::tt_move_history_multicut_base() - p::tt_move_history_multicut_depth() * depth);
            return lerp(singular_score, beta, 0.4027);
        } else if singular_score > tt_score && td.stack[ply].mv != Move::NULL {
            tt_move = Move::NULL;
        }
        // Negative Extensions
        else if tt_score >= beta || cut_node {
            extension = -3;
        }
    }
    // Low Depth Singular Extensions (LDSE)
    else if depth <= 7 && !in_check && cut_node && estimated_score <= alpha - 25 {
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

    // Continuation-history subtable pointers for the six lags read by the quiet
    // `history` sum below. They depend only on `ply`, so they are constant for
    // this whole node, but these used to go through a per-lag helper that
    // re-read `stack[ply - n].conthist` on every call -- six strided loads into
    // a large array per quiet move, where six for the entire node will do. The
    // raw-pointer read stops the optimiser from hoisting them itself.
    let conthist: [*mut PieceToHistory<i16>; 6] = std::array::from_fn(|i| td.stack[ply - 1 - i as isize].conthist);

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

        // Resolved once and reused by all six continuation lags below.
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
            // All six continuation lags, held at upstream's scale.
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
            td.quiet_history.get(td.board.all_threats(), stm, mv)
                + (td.continuation_history.get(conthist[0], moved, to)
                    + td.continuation_history.get(conthist[1], moved, to)
                    + td.continuation_history.get(conthist[2], moved, to) * 2 / 7
                    + td.continuation_history.get(conthist[3], moved, to)
                    + td.continuation_history.get(conthist[4], moved, to) / 8
                    + td.continuation_history.get(conthist[5], moved, to) / 2)
                    / 2
        } else {
            let captured_type = td.board.type_on(mv.capture_sq());
            // `moved_piece(mv)` is `mailbox[mv.from()]`, the same lookup as the
            // hoisted `moved` above.
            td.noisy_history.get(td.board.all_threats(), moved, to, captured_type)
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
                && move_count as i32
                    >= (p::lmp_base()
                        + p::lmp_improvement() * improvement / 16
                        + p::lmp_quad() * depth * depth
                        + p::lmp_history() * history / history_divisor)
                        / 1024
            {
                skip_quiets = true;
                continue;
            }

            // Futility Pruning (FP)
            let futility_value = eval
                + p::fp_depth() * depth
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
                + 96 * (!NODE::ROOT && td.stack[ply - 1].mv.is_present() && mv.to() == td.stack[ply - 1].mv.to())
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
                    + p::see_q_cutoff() * (td.cutoff_count[ply + 1] > 2) as i32
                    + p::see_q_base())
                .min(0)
            } else {
                (-p::see_n_quad() * depth * depth - p::see_n_lin() * depth - p::see_n_hist() * history / 1024
                    + p::see_n_cutoff() * (td.cutoff_count[ply + 1] > 2) as i32
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

        let mut new_depth = depth - 1 + if move_count == 1 { extension } else { 0 };

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
        let is_recapture = new_depth == 0
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
            reduction += p::lmr_movecount_ilog() * (move_count as u32).ilog2() as i32;

            reduction -= (p::lmr_improvement() * improvement / 128).clamp(-241, 1155);
            reduction -= p::lmr_corr() * correction_value.abs() / 1024;

            reduction += p::lmr_exact() * (bound == Bound::Exact) as i32;

            reduction += p::lmr_tt_alpha() * (is_valid(tt_score) && tt_score <= alpha) as i32;
            reduction += p::lmr_tt_depth() * (is_valid(tt_score) && tt_depth < depth) as i32;
            reduction += 1024 * is_win(beta) as i32;

            if is_quiet {
                reduction += p::lmr_quiet_base();
                reduction -= p::lmr_quiet_hist() * history / 1024;
                reduction += p::lmr_quiet_alpha() * ((alpha - estimated_score).clamp(-65, 91)) / 128;
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
                reduction -= p::lmr_iir_comp() * (tt_move.is_null() && iir_applied) as i32;
            }

            // Capped: `alpha_raises` is bounded only by the move count, so at a
            // node where many moves improve in turn this term could reach
            // double-digit plies on its own. The `reduced_depth` clamp stops
            // that being unsound, but it would mean every late move searched at
            // depth 1 because of one signal. The cap is also what the signal
            // means -- the first few raises say "this node is still improving";
            // the twentieth says nothing the third did not.
            reduction += p::lmr_alpha_raise() * alpha_raises.min(p::lmr_alpha_raise_cap());
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

            // Clamp first, then apply the PV bonus (as upstream): the bonus
            // raises both the floor and, effectively, the ceiling for PV
            // nodes to new_depth+4, rather than capping PV scout depth at
            // new_depth+2 the way clamping after the bonus would.
            let pv_bonus = 2 * NODE::PV as i32;
            let reduced_depth = (new_depth - reduction / 1024).clamp(1, new_depth + 2) + pv_bonus;

            td.stack[ply].reduction = reduction;
            score = -search::<NonPV>(td, -alpha - 1, -alpha, reduced_depth, true, ply + 1);
            td.stack[ply].reduction = 0;
            current_search_count += 1;

            if score > alpha {
                if !NODE::ROOT {
                    new_depth += (score > best_score + 57) as i32;
                    new_depth -= (score < best_score + 9) as i32;
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
            reduction += p::fds_movecount_ilog() * (move_count as u32).ilog2() as i32;

            reduction -= (p::fds_improvement() * improvement / 128).clamp(-206, 1370);
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
                reduction -= p::fds_iir_comp() * (tt_move.is_null() && iir_applied) as i32;
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

            let reduced_depth = new_depth - (reduction >= 2621) as i32 - (reduction >= 5579) as i32;

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

                if !(NODE::ROOT && td.pv_index > 0) && mv != tt_move {
                    td.shared.tt.write(hash, depth, raw_eval, score, Bound::Lower, mv, ply, true, false);
                }
            }
        }

        if mv != best_move && move_count < 32 {
            if is_quiet {
                quiet_moves.push(mv);
            } else {
                noisy_moves.push(mv);
            }
        }
    }

    if move_count == 0 {
        if excluded {
            return -Score::TB_WIN_IN_MAX + 1;
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

    if td.id == 0 && td.time_manager.check_time(td) {
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

    // QS early TT cutoff
    if let Some(entry) = &entry {
        tt_score = entry.score;
        tt_bound = entry.bound;
        tt_pv |= entry.tt_pv;

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
        if !is_decisive(best_score) && !is_decisive(beta) {
            best_score = lerp(best_score, beta, 0.8256);
        }

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
    let mut move_picker = MovePicker::new(Move::NULL, None);

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
            if move_count >= 3 && !is_direct_check {
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
        let bonus = 100;

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
    td.shared.tt.write(hash, TtDepth::SOME, raw_eval, best_score, bound, best_move, ply, tt_pv, false);

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

    let noisy_bonus = (96 * depth).min(885) - 43 - 87 * cut_node as i32;
    let noisy_malus = (175 * depth).min(1252) - 58 - 16 * noisy_moves.len() as i32;

    // At non-PV nodes, scale the bonus up by how many other moves were
    // searched before this one proved best (as in Stockfish).
    let quiet_bonus = (184 * depth).min(1742) - 72 - 42 * cut_node as i32
        + (18 * (move_count as i32 - 1)).min(180) * !NODE::PV as i32;
    let quiet_malus = (171 * depth).min(1099) - 46 - 31 * quiet_moves.len() as i32;

    let cont_bonus = (97 * depth).min(1098) - 74 - 48 * cut_node as i32;
    let cont_malus = (414 * depth).min(949) - 49 - 17 * quiet_moves.len() as i32;

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
            let denom = 1024 + 45 * i as i32;
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
        let malus = (93 * depth - 52).min(935);
        update_continuation_histories(td, ply - 1, td.stack[ply - 1].piece, td.stack[ply - 1].mv.to(), -malus);
    }

    if ctx.current_search_count > 1 && best_move.is_quiet() && ctx.best_score >= ctx.beta {
        let bonus = (233 * depth - 86).min(1550);
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
            + 110 * (prior_move == td.stack[ply - 1].tt_move) as i32
            + 144 * (!in_check && best_score <= eval - 97) as i32
            + 306 * (is_valid(td.stack[ply - 1].eval) && best_score <= -td.stack[ply - 1].eval - 136) as i32;

        let scaled_bonus = factor * (180 * depth - 37).min(2414) / 128;

        td.quiet_history.update(td.board.prior_threats(), !stm, prior_move, scaled_bonus);

        let entry = &td.stack[ply - 2];
        if entry.mv.is_present() {
            let bonus = (152 * depth - 47).min(1379);
            td.continuation_history.update(entry.conthist, td.stack[ply - 1].piece, prior_move.to(), bonus);
        }
    } else if prior_move.is_noisy() {
        let captured_type = td.board.captured_piece().piece_type();
        let bonus = (50 * depth).min(654);

        td.noisy_history.update(
            td.board.prior_threats(),
            td.board.piece_on(prior_move.to()),
            prior_move.to(),
            captured_type,
            bonus,
        );
    }
}

fn eval_correction(td: &ThreadData, ply: isize) -> i32 {
    let stm = td.board.side_to_move();
    let bucket = td.board.fiftymove_clock_bucket();
    let corrhist = td.corrhist();

    (corrhist.pawn.get(stm, td.board.pawn_key(), bucket)
        + corrhist.non_pawn[Color::White].get(stm, td.board.non_pawn_key(Color::White), bucket)
        + corrhist.non_pawn[Color::Black].get(stm, td.board.non_pawn_key(Color::Black), bucket)
        + corrhist.material.get(stm, td.board.material_key(), bucket)
        // A 6th term added to upstream's 5-term blend; corr_weight_div is
        // rescaled to match (see its definition in parameters.rs) rather than
        // left at upstream's value the way it silently was the first time
        // this table existed in this fork.
        + td.continuation_corrhist.get(
            td.stack[ply - 2].contcorrhist,
            td.stack[ply - 1].piece,
            td.stack[ply - 1].mv.to(),
        )
        + td.continuation_corrhist.get(
            td.stack[ply - 4].contcorrhist,
            td.stack[ply - 1].piece,
            td.stack[ply - 1].mv.to(),
        ))
        / p::corr_weight_div().max(1)
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
    // weighted equally (matching the original, already-tuned baseline, which
    // treated those four lags at full and equal strength); lags 3/5 are new
    // additions kept at Stockfish's relative ratio to the primary weight.
    // Both SPSA-tunable now (previously hardcoded consts).
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

    let multiplier = multipliers[positive_count];

    for &(conthist, weight, offset) in &targets[..len] {
        // Overall scale is SPSA-tunable since the right magnitude for this
        // 6-lag scheme relative to the original 4-lag baseline is an
        // empirical question, not one to guess at.
        let scaled = bonus * weight * multiplier / p::conthist_div().max(1) + 73 * (offset < 2) as i32;
        td.continuation_history.update(conthist, piece, sq, scaled);
    }
}

/// Gravity-style update of the global TT-move reliability statistic, bounded
/// to roughly [-8192, 8192] like Stockfish's `TTMoveHistory`.
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
    if !tt_move.is_quiet() || td.board.fiftymove_clock() < 10 || ply < 20 {
        return false;
    }

    let prev2 = td.stack[ply - 2].mv;
    let prev4 = td.stack[ply - 4].mv;

    prev2.is_present() && prev4.is_present() && tt_move.from() == prev2.to() && prev2.from() == prev4.to()
}

fn make_move(td: &mut ThreadData, ply: isize, mv: Move) {
    td.shared.tt.prefetch(td.board.key_after(mv));
    td.stack[ply + 1].follow_pv = td.stack[ply].follow_pv && td.previous_pv.get(ply as usize) == Some(&mv);
    td.stack[ply].mv = mv;
    td.stack[ply].piece = td.board.moved_piece(mv);
    td.stack[ply].conthist = td.continuation_history.subtable_ptr(
        td.board.in_check(),
        mv.is_noisy(),
        td.board.moved_piece(mv),
        mv.to(),
    );
    td.stack[ply].contcorrhist = td.continuation_corrhist.subtable_ptr(
        td.board.in_check(),
        mv.is_noisy(),
        td.board.moved_piece(mv),
        mv.to(),
    );

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