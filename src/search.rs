use std::sync::atomic::Ordering;

use crate::{
    evaluation::correct_eval,
    history::LowPlyHistory,
    movepick::{MovePicker, Stage},
    parameters as p,
    stack::Stack,
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

    td.pv_table.clear(0);
    td.nnue.full_refresh(&td.board);

    td.multi_pv = td.multi_pv.min(td.root_moves.len());

    let mut average = vec![td.previous_best_score; td.multi_pv];
    let mut last_best_rootmove = RootMove::default();

    let mut eval_stability = 0;
    let mut pv_stability = 0;
    let mut soft_stop_voted = false;

    // Ring buffer of best scores from recent iterations, so the time manager
    // can compare against the score four iterations ago (as in Stockfish).
    let mut iter_values = [td.previous_best_score; 4];

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

        // Aspiration window floor: keep a minimum delta so very stable
        // positions still search wide enough to catch a sudden tactical
        // shift, rather than triggering hairline-window re-searches.
        let mut delta = (23 - eval_stability.min(pv_stability).min(7)).max(10);
        let mut reduction = 0;

        for index in 0..td.multi_pv {
            td.pv_index = index;

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
                td.stack = Stack::new();
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
                        // Collapse beta to the failed window's floor (as in
                        // Stockfish) so the re-search stays narrow.
                        beta = alpha;
                        alpha = (score - delta).max(-Score::INFINITE);
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
            td.previous_pv = td.pv_table.line().to_vec();
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
                let difference = (td.previous_best_score - td.root_moves[0].score) as f32;
                let recent = (iter_value - td.root_moves[0].score) as f32;
                (0.7426 + 0.0480 * difference + 0.0230 * recent).clamp(0.7214, 1.4031)
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
    } else {
        0
    };

    let improving = improvement > 0;

    // True when our static evaluation swung further in our favor than the
    // opponent's symmetric expectation from one ply ago (as in Stockfish).
    let opponent_worsening = !in_check && is_valid(td.stack[ply - 1].eval) && eval > -td.stack[ply - 1].eval;

    // Razoring
    if !NODE::PV
        && !in_check
        && estimated_score
            < alpha - p::razor_base() - p::razor_quad() * depth * depth + 65 * (td.cutoff_count[ply + 1] > 3) as i32
        && alpha < 2048
        && !tt_move.is_quiet()
        && tt_bound != Bound::Lower
    {
        return qsearch::<NonPV>(td, alpha, beta, ply);
    }

    // Reverse Futility Pruning (RFP)
    if !tt_pv
        && !in_check
        && !excluded
        && (!tt_move.is_quiet() || td.quiet_history.get(td.board.all_threats(), stm, tt_move) >= -2048)
        && estimated_score
            >= beta
                + (p::rfp_depth_quad() * depth * depth / 128 - p::rfp_improvement() * improvement / 1024
                    + p::rfp_depth_lin() * depth
                    + p::rfp_corr() * correction_value.abs() / 1024
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
        && td.board.material() > 491
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
            if (td.nmp_min_ply > 0 || depth < 16) && score >= beta {
                // A confirmed null-move fail high is evidence the static eval
                // undershoots this position: feed it to the correction histories.
                if score > eval {
                    update_correction_histories(td, depth, score - eval, ply);
                }
                return score;
            }

            let reduced_depth = if score < beta { depth / 2 } else { depth - r };

            td.nmp_min_ply = ply as i32 + 3 * reduced_depth / 4;
            let verified_score = search::<NonPV>(td, beta - 1, beta, reduced_depth, false, ply);
            td.nmp_min_ply = 0;

            if td.shared.status.get() == Status::STOPPED {
                return Score::ZERO;
            }

            if verified_score >= beta {
                return score;
            }
        }
    }

    // Also treat the node as improving when the static evaluation already
    // clears beta (as in Stockfish); affects heuristics from here on.
    let improving = improving || (!in_check && is_valid(eval) && eval >= beta);

    // Internal Iterative Reductions (IIR): at sufficient depth, reduce PV and
    // expected cut nodes that have no TT move to anchor move ordering. Nodes
    // on the previous iteration's PV are exempt (as in Stockfish).
    if !NODE::ROOT && !td.stack[ply].follow_pv && (NODE::PV || cut_node) && depth >= 6 && tt_move.is_null() {
        depth -= 1;
    }

    // ProbCut
    let mut probcut_beta = beta + p::probcut_base() - p::probcut_improving() * improving as i32;

    if cut_node
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
            let mut probcut_depth = (base_depth - (score - probcut_beta) / p::probcut_score_div()).clamp(0, base_depth);

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
    if matches!(tt_bound, Bound::Lower | Bound::Exact)
        && tt_depth >= depth - 4
        && is_valid(tt_score)
        && tt_score >= probcut_beta_tt
        && !is_decisive(beta)
        && !is_decisive(tt_score)
    {
        return probcut_beta_tt;
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
            let double_margin = 195 * NODE::PV as i32 + 48 * (NODE::PV && !tt_was_pv) as i32
                - 16 * tt_move.is_quiet() as i32
                - 16 * correction_value.abs() / 128
                - 1175 * td.tt_move_history / 114178
                - 38 * (ply as i32 > td.root_depth) as i32;
            let triple_margin = 230 * NODE::PV as i32 + 56 * (NODE::PV && !tt_was_pv) as i32
                - 19 * tt_move.is_quiet() as i32
                - 15 * correction_value.abs() / 128
                - 43 * (ply as i32 > td.root_depth) as i32
                + 36;

            extension = 1;
            extension += (singular_score < singular_beta - double_margin) as i32;
            extension += (singular_score < singular_beta - triple_margin) as i32;
        }
        // Multi-Cut
        else if singular_score >= beta && !is_decisive(singular_score) {
            update_tt_move_history(td, -421 - 110 * depth);
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

    let mut move_count = 0;
    let mut move_picker = MovePicker::new(tt_move, None);
    let mut skip_quiets = false;
    let mut current_search_count = 0;
    let mut tt_move_score = Score::NONE;

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
        let is_direct_check = td.board.is_direct_check(mv);

        // For noisy moves, reductions additionally credit the captured piece's
        // value (as in Stockfish's statScore for captures).
        let mut capture_stat = 0;

        let history = if is_quiet {
            td.quiet_history.get(td.board.all_threats(), stm, mv)
                + td.conthist(ply, 1, mv)
                + td.conthist(ply, 2, mv)
                + td.conthist(ply, 6, mv) / 2
        } else {
            let captured_type = td.board.type_on(mv.to());
            capture_stat = p::lmr_capture_stat() * captured_type.value() / 64;
            td.noisy_history.get(td.board.all_threats(), td.board.moved_piece(mv), mv.to(), captured_type)
        };

        // Depth-indexed divisor for history contributions to quiet pruning
        // (Stockfish's lmrDivisor table, renormalized around a 1024 base) so
        // history weighs in non-uniformly per depth.
        const HISTORY_DIVISORS: [i32; 16] =
            [1221, 936, 927, 987, 1065, 1124, 1057, 927, 931, 1043, 1043, 1027, 1045, 1004, 1037, 1189];
        let history_divisor = HISTORY_DIVISORS[(depth.min(16) - 1) as usize];

        if !NODE::ROOT && !is_loss(best_score) {
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
            if !in_check && is_quiet && depth < 5 && history < -p::hp_margin() * depth {
                continue;
            }

            // Static Exchange Evaluation Pruning (SEE Pruning)
            let threshold = if is_quiet {
                (-p::see_q_quad() * depth * depth + p::see_q_lin() * depth - p::see_q_hist() * history / 1024
                    + p::see_q_base())
                .min(0)
            } else {
                (-p::see_n_quad() * depth * depth - p::see_n_lin() * depth - p::see_n_hist() * history / 1024
                    + p::see_n_base())
                .min(0)
            };

            if !in_check && !td.board.see(mv, threshold) {
                continue;
            }
        }

        let initial_nodes = td.nodes();

        make_move(td, ply, mv);

        let mut new_depth = depth - 1 + if move_count == 1 { extension } else { 0 };

        // Pre-qsearch TT-move extension: at PV nodes, give a well-established
        // TT move one full ply instead of dropping it straight into qsearch.
        // Never override a negative (singular) extension decision.
        if NODE::PV && mv == tt_move && new_depth == 0 && extension >= 0 && tt_depth >= depth {
            new_depth = 1;
        }

        let mut score = Score::ZERO;

        // Late Move Reductions (LMR)
        if depth >= 2 && move_count >= 2 {
            let mut reduction = p::lmr_ilog() * depth.ilog2() as i32;

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
                reduction -= p::lmr_noisy_hist() * (history + capture_stat) / 1024;
            }

            if NODE::PV {
                reduction -= p::lmr_pv_base() + p::lmr_pv_delta() * (beta - alpha) / td.root_delta;
            }

            if tt_pv {
                reduction -= p::lmr_ttpv();
                reduction -= p::lmr_ttpv_score() * (is_valid(tt_score) && tt_score > alpha) as i32;
                reduction -= p::lmr_ttpv_depth() * (is_valid(tt_score) && tt_depth >= depth) as i32;
            } else if cut_node {
                reduction += p::lmr_cutnode();
                reduction += p::lmr_cutnode_null() * tt_move.is_null() as i32;
            }

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

            if !NODE::PV && td.stack[ply - 1].reduction > reduction + 414 {
                reduction += p::lmr_prev_reduction();
            }

            reduction += ((td.nodes() + td.id as u64 * 27) % 128) as i32 - 59;

            let reduced_depth = (new_depth - reduction / 1024).clamp(1, new_depth + 2) + 2 * NODE::PV as i32;

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

            reduction -= (p::fds_improvement() * improvement / 128).clamp(-206, 1370);
            reduction -= p::fds_corr() * correction_value.abs() / 1024;

            if is_quiet {
                reduction += p::fds_quiet_base();
                reduction -= p::fds_quiet_hist() * history / 1024;
            } else {
                reduction += p::fds_noisy_base();
                reduction -= p::fds_noisy_hist() * (history + capture_stat) / 1024;
            }

            if tt_pv {
                reduction -= p::fds_ttpv();
                reduction -= p::fds_ttpv_depth() * (is_valid(tt_score) && tt_depth >= depth) as i32;
            } else if cut_node {
                reduction += p::fds_cutnode();
                reduction += p::fds_cutnode_null() * tt_move.is_null() as i32;
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

            if td.stack[ply - 1].reduction > reduction + 590 {
                reduction += p::fds_prev_reduction();
            }

            reduction += ((td.nodes() + td.id as u64 * 26) % 128) as i32 - 56;

            let reduced_depth = new_depth - (reduction >= 2621) as i32 - (reduction >= 5579) as i32;

            score = -search::<NonPV>(td, -alpha - 1, -alpha, reduced_depth, !cut_node, ply + 1);
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
            let current_nodes = td.nodes();
            let root_move = td.root_moves.iter_mut().find(|v| v.mv == mv).unwrap();

            root_move.nodes += current_nodes - initial_nodes;

            if move_count == 1 || score > alpha {
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
            } else {
                root_move.score = -Score::INFINITE;
            }
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
                td.board.type_on(best_move.to()),
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
            let captured_type = td.board.type_on(mv.to());
            td.noisy_history.update(
                td.board.all_threats(),
                td.board.moved_piece(mv),
                mv.to(),
                captured_type,
                -noisy_malus,
            );
        }

        // Track how often the TT move turns out to be the best move; feeds back
        // into the singular double-extension margin (as in Stockfish).
        if !NODE::PV && tt_move.is_present() {
            update_tt_move_history(td, if best_move == tt_move { 918 } else { -747 });
        }

        if !NODE::ROOT && td.stack[ply - 1].mv.is_quiet() && td.stack[ply - 1].move_count < 2 {
            let malus = (93 * depth - 52).min(935);
            update_continuation_histories(td, ply - 1, td.stack[ply - 1].piece, td.stack[ply - 1].mv.to(), -malus);
        }

        if current_search_count > 1 && best_move.is_quiet() && best_score >= beta {
            let bonus = (233 * depth - 86).min(1550);
            update_continuation_histories_in_check(td, ply, td.stack[ply].piece, best_move.to(), bonus, in_check);
        }
    }

    if !NODE::ROOT && bound == Bound::Upper && (cut_node || NODE::PV) {
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
                td.continuation_history.update(
                    entry.conthist,
                    td.stack[ply - 1].piece,
                    prior_move.to(),
                    bonus,
                );
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

    let skip_quiets = !in_check || !is_loss(best_score);

    while let Some(mv) = move_picker.next::<NODE>(td, skip_quiets, ply) {
        move_count += 1;

        if !is_loss(best_score) {
            // Late Move Pruning (LMP)
            if move_count >= 3 && !td.board.is_direct_check(mv) {
                break;
            }

            // Static Exchange Evaluation Pruning (SEE Pruning)
            if is_valid(eval) && !td.board.see(mv, (alpha - eval) / 8 - correction_value.abs().min(68) - 74) {
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
            td.board.type_on(best_move.to()),
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

fn eval_correction(td: &ThreadData, ply: isize) -> i32 {
    let stm = td.board.side_to_move();
    let bucket = td.board.fiftymove_clock_bucket();
    let corrhist = td.corrhist();

    (corrhist.pawn.get(stm, td.board.pawn_key(), bucket)
        + corrhist.non_pawn[Color::White].get(stm, td.board.non_pawn_key(Color::White), bucket)
        + corrhist.non_pawn[Color::Black].get(stm, td.board.non_pawn_key(Color::Black), bucket)
        // Upstream's blend summed 5 terms over corr_weight_div; material,
        // minor, and major are all additions since, so they're grouped under
        // one tunable weight rather than each inflating the sum at full,
        // un-normalized strength against a divisor tuned for 5 terms.
        + (corrhist.material.get(stm, td.board.material_key(), bucket)
            + corrhist.minor.get(stm, td.board.minor_key(), bucket)
            + corrhist.major.get(stm, td.board.major_key(), bucket))
            * p::corr_minor_major()
            / 128
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
        / p::corr_weight_div()
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

    corrhist.minor.update(stm, td.board.minor_key(), bucket, bonus);

    corrhist.major.update(stm, td.board.major_key(), bucket, bonus);

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
    const CONTHIST_BONUSES: [(isize, i32); 6] = [(1, 700), (2, 700), (3, 195), (4, 700), (5, 89), (6, 700)];
    const MULTIPLIERS: [i32; 7] = [94, 103, 110, 106, 119, 126, 121];

    let mut positive_count = 0;

    for (offset, weight) in CONTHIST_BONUSES {
        // Only update the nearest two continuation histories when in check.
        if in_check && offset > 2 {
            break;
        }

        let entry = &td.stack[ply - offset];
        if entry.mv.is_present() {
            if td.continuation_history.get(entry.conthist, piece, sq) > 0 {
                positive_count += 1;
            }

            // Overall scale is SPSA-tunable since the right magnitude for this
            // 6-lag scheme relative to the original 4-lag baseline is an
            // empirical question, not one to guess at.
            let scaled = bonus * weight * MULTIPLIERS[positive_count] / p::conthist_div() + 73 * (offset < 2) as i32;
            td.continuation_history.update(entry.conthist, piece, sq, scaled);
        }
    }
}

/// Gravity-style update of the global TT-move reliability statistic, bounded
/// to roughly [-8192, 8192] like Stockfish's `TTMoveHistory`.
fn update_tt_move_history(td: &mut ThreadData, bonus: i32) {
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
