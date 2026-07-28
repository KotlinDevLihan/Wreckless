//! Staged move generation and ordering.
//!
//! Moves are produced lazily, best-first within each stage, so that a node that
//! cuts off early never pays to generate or score the stages behind it. The
//! order is:
//!
//! 1. [`Stage::HashMove`]  — the transposition-table move, verified legal.
//! 2. [`Stage::GoodNoisy`] — captures and promotions passing a SEE threshold.
//! 3. [`Stage::Quiet`]     — quiets scoring above [`p::good_quiet_threshold`].
//! 4. [`Stage::BadNoisy`]  — captures that failed SEE, deferred from stage 2.
//! 5. [`Stage::BadQuiet`]  — quiets at or below the threshold, deferred from 3.
//!
//! Two invariants matter and have been broken before:
//!
//! * **Every legal move is yielded exactly once.** The TT move is removed from
//!   each generated list rather than filtered at the point of use, and a quiet
//!   that falls below the good/bad threshold is parked in `first_bad_quiet`
//!   instead of being dropped. A previous rewrite consumed that move without
//!   ever returning it, which silently removed one legal reply per node — and
//!   reported a false mate when it was the only reply.
//! * **[`MovePicker::stage`] describes the move just returned**, not the one
//!   coming next. `search` relies on `stage() == Stage::BadNoisy` to recognise
//!   losing captures for bad-noisy futility and history pruning, so each arm
//!   returns while its own stage is still current.
//!
//! Generation itself is delegated to the perft-validated generator, which
//! already restricts to legal moves when the side to move is in check. There is
//! deliberately no separate evasion generator: a hand-rolled one produced
//! illegal king escapes (it masked with the *current* attack set, which treats
//! the square behind the king along a checking ray as safe) and mislabelled
//! move kinds, corrupting the board.

use crate::{
    history::LowPlyHistory,
    lookup::king_attacks,
    parameters as p,
    search::NodeType,
    setwise::{bishop_attacks_setwise, knight_attacks_setwise, pawn_attacks_setwise, rook_attacks_setwise},
    thread::ThreadData,
    types::{ArrayVec, Bitboard, MAX_MOVES, Move, MoveEntry, MoveList, PieceType},
};

/// Which pool the most recently returned move came from.
#[derive(Copy, Clone, Eq, PartialEq, PartialOrd, Debug)]
pub enum Stage {
    HashMove,
    GenerateNoisy,
    GoodNoisy,
    Quiet,
    BadNoisy,
    BadQuiet,
}

pub struct MovePicker {
    /// Scratch pool for the stage currently being emitted: noisy moves first,
    /// then reused for quiets once the noisy pool is exhausted.
    list: MoveList,
    tt_move: Move,
    /// Fixed SEE threshold for the good/bad noisy split. `None` selects the
    /// depth-free dynamic threshold used in normal search; ProbCut passes an
    /// explicit value so it only ever sees captures beating its own margin.
    threshold: Option<i32>,
    stage: Stage,
    /// Captures that failed the SEE test, replayed after the good quiets.
    bad_noisy: ArrayVec<Move, MAX_MOVES>,
    bad_noisy_idx: usize,
    /// Good noisy moves emitted so far; feeds the dynamic SEE threshold.
    noisy_count: usize,
    /// The first quiet found at or below the good/bad threshold. Held here so
    /// it is replayed in `BadQuiet` rather than lost.
    first_bad_quiet: Move,
}

impl MovePicker {
    pub const fn new(tt_move: Move, threshold: Option<i32>) -> Self {
        Self {
            list: MoveList::new(),
            tt_move,
            threshold,
            stage: if tt_move.is_present() { Stage::HashMove } else { Stage::GenerateNoisy },
            bad_noisy: ArrayVec::new(),
            bad_noisy_idx: 0,
            noisy_count: 0,
            first_bad_quiet: Move::NULL,
        }
    }

    /// Abandons the remaining bad noisy moves while still allowing the deferred
    /// bad quiets to be searched. Used when bad-noisy futility pruning decides
    /// no remaining losing capture can reach alpha.
    pub fn skip_bad_noisy(&mut self) {
        self.bad_noisy_idx = self.bad_noisy.len();
    }

    /// The pool the last returned move came from. See the module note: this
    /// describes the previous move, not the next one.
    pub const fn stage(&self) -> Stage {
        self.stage
    }

    /// Yields the next move, or `None` once every stage is exhausted.
    ///
    /// `skip_quiets` may flip from false to true partway through a node once
    /// late-move or futility pruning fires. That is handled without regenerating
    /// anything: the quiet stages simply stop yielding, and any quiets already
    /// generated stay in `list` unused.
    pub fn next<NODE: NodeType>(&mut self, td: &ThreadData, skip_quiets: bool, ply: isize) -> Option<Move> {
        loop {
            match self.stage {
                Stage::HashMove => {
                    self.stage = Stage::GenerateNoisy;

                    if td.board.is_legal(self.tt_move) {
                        return Some(self.tt_move);
                    }
                }

                Stage::GenerateNoisy => {
                    self.stage = Stage::GoodNoisy;

                    td.board.append_noisy_moves(&mut self.list);
                    self.remove_tt();
                    self.score_noisy(td);
                }

                Stage::GoodNoisy => {
                    if let Some(mv) = self.next_good_noisy::<NODE>(td) {
                        return Some(mv);
                    }

                    // Noisy pool drained. In qsearch and ProbCut quiets are
                    // never wanted, so skip straight to the losing captures
                    // without paying for quiet generation and scoring.
                    if skip_quiets {
                        self.stage = Stage::BadNoisy;
                    } else {
                        self.stage = Stage::Quiet;
                        td.board.append_quiet_moves(&mut self.list);
                        self.remove_tt();
                        self.score_quiet(td, ply);
                    }
                }

                Stage::Quiet => {
                    if !skip_quiets && !self.list.is_empty() {
                        // At the root, history is updated between siblings, so
                        // rescore to pick up what the previous move learned.
                        if NODE::ROOT {
                            self.score_quiet(td, ply);
                        }

                        let entry = self.pop_best();
                        if entry.score > p::good_quiet_threshold() {
                            return Some(entry.mv);
                        }

                        // This was the best remaining quiet, so every other
                        // quiet is also at or below the threshold. Park it and
                        // let the whole tail wait behind the bad captures.
                        self.first_bad_quiet = entry.mv;
                    }

                    self.stage = Stage::BadNoisy;
                }

                Stage::BadNoisy => {
                    if self.bad_noisy_idx < self.bad_noisy.len() {
                        let mv = self.bad_noisy[self.bad_noisy_idx];
                        self.bad_noisy_idx += 1;
                        return Some(mv);
                    }

                    self.stage = Stage::BadQuiet;
                }

                Stage::BadQuiet => {
                    if skip_quiets {
                        return None;
                    }

                    if self.first_bad_quiet.is_present() {
                        let mv = self.first_bad_quiet;
                        self.first_bad_quiet = Move::NULL;
                        return Some(mv);
                    }

                    if !self.list.is_empty() {
                        return Some(self.pop_best().mv);
                    }

                    return None;
                }
            }
        }
    }

    /// Drains good noisy moves, diverting SEE failures into `bad_noisy`.
    fn next_good_noisy<NODE: NodeType>(&mut self, td: &ThreadData) -> Option<Move> {
        while !self.list.is_empty() {
            let entry = self.pop_best();

            // Without an explicit ProbCut threshold, demand more from a capture
            // the better its static score already is; once several good
            // captures have been tried behind a quiet TT move, demand instead
            // that it win material outright.
            //
            // Upstream diverges here: it sends *every* remaining noisy move to
            // `bad_noisy` under that condition, regardless of SEE, rather than
            // re-testing it against a fixed threshold. This fork is therefore
            // the more permissive of the two, and the difference has never been
            // measured either way.
            let threshold = self.threshold.unwrap_or_else(|| {
                if self.tt_move.is_quiet() && self.noisy_count > 2 { 1 } else { -entry.score / 47 + 116 }
            });

            if !td.board.see(entry.mv, threshold) {
                self.bad_noisy.push(entry.mv);
                continue;
            }

            if NODE::ROOT {
                self.score_noisy(td);
            }

            self.noisy_count += 1;
            return Some(entry.mv);
        }

        None
    }

    /// Removes and returns the highest-scoring entry.
    ///
    /// A linear scan beats sorting here: most nodes cut off after a handful of
    /// moves, so the moves after the cutoff are never compared at all.
    fn pop_best(&mut self) -> MoveEntry {
        let mut best_index = 0;
        let mut best_score = i32::MIN;

        for (index, entry) in self.list.iter().enumerate() {
            if entry.score >= best_score {
                best_index = index;
                best_score = entry.score;
            }
        }

        self.list.remove(best_index)
    }

    /// Drops the TT move from the freshly generated pool; it was already
    /// emitted by [`Stage::HashMove`].
    fn remove_tt(&mut self) {
        if let Some(pos) = self.list.iter().position(|&e| e.mv == self.tt_move) {
            self.list.remove(pos);
        }
    }

    fn score_noisy(&mut self, td: &ThreadData) {
        let threats = td.board.all_threats();
        let in_check = td.board.in_check();

        for entry in self.list.iter_mut() {
            let mv = entry.mv;
            let captured = td.board.type_on(mv.capture_sq());
            // Single mailbox read: `type_on` and `moved_piece` both resolve
            // `mv.from()`, and this loop wanted each of them once.
            let moved = td.board.piece_on(mv.from());
            let pt = moved.piece_type();

            entry.score = 14232 * captured.value() / 1024
                + td.noisy_history.get(threats, moved, mv.to(), captured)
                + 4558 * (mv.is_promotion() && mv.promo_piece_type() == PieceType::Queen) as i32
                // Evading check with the least valuable piece first dominates
                // every learned term, hence the deliberately huge constant.
                + (200000 - 20000 * pt as i32) * in_check as i32;
        }
    }

    fn score_quiet(&mut self, td: &ThreadData, ply: isize) {
        let ctx = QuietContext::new(td);
        let threats = td.board.all_threats();
        let side = td.board.side_to_move();
        let pawn_key = td.board.pawn_key();

        for entry in self.list.iter_mut() {
            let mv = entry.mv;
            // One mailbox read serves all three: `type_on(sq)` is
            // `piece_on(sq).piece_type()` and `moved_piece(mv)` is
            // `piece_on(mv.from())`. Spelled out separately, this square was
            // being re-read nine times per quiet move -- once here, once for
            // pawn history, and once inside each of the six `conthist` calls,
            // whose raw-pointer read blocks the optimiser from hoisting it.
            let from = mv.from();
            let to = mv.to();
            let moved = td.board.piece_on(from);
            let pt = moved.piece_type();

            entry.score = 1763 * td.quiet_history.get(threats, side, mv) / 1024
                + 1024 * td.corrhist().pawn_history.get(pawn_key, moved, to) / 1024
                + Self::low_ply_term(td, ply, mv)
                // All six continuation lags. The weights sum to the same total
                // as the four-lag set they replaced (4817), so the good/bad
                // quiet split still sees the same score distribution and
                // `good_quiet_threshold` keeps its meaning.
                + 1479 * td.conthist_at(ply, 1, moved, to) / 1024
                + 977 * td.conthist_at(ply, 2, moved, to) / 1024
                + 277 * td.conthist_at(ply, 3, moved, to) / 1024
                + 995 * td.conthist_at(ply, 4, moved, to) / 1024
                + 126 * td.conthist_at(ply, 5, moved, to) / 1024
                + 963 * td.conthist_at(ply, 6, moved, to) / 1024
                // Positional shaping: reward stepping a threatened piece to
                // safety, giving check, or attacking something; penalise moving
                // into a threat or breaking up the pawns shielding our king.
                + ctx.escape[pt] * ctx.threatened[pt].contains(from) as i32
                + 10723 * td.board.checking_squares(pt).contains(to) as i32
                - 8875 * ctx.threatened[pt].contains(to) as i32
                + 3446 * ctx.offense[pt].contains(to) as i32
                - 4494 * ctx.wall_pawns.contains(from) as i32;
        }
    }

    /// Root-relative history, covering only the first few plies.
    ///
    /// The divisor fades it out with depth. Its weight is anchored so the
    /// ply-0 ceiling matches continuation-history lag 1; left larger it
    /// outweighed every other ordering signal at the root by more than 2x,
    /// which is where ordering matters most.
    fn low_ply_term(td: &ThreadData, ply: isize, mv: Move) -> i32 {
        if (ply as usize) < LowPlyHistory::MAX_LOW_PLY {
            p::lowply_weight() * td.low_ply_history.get(ply as usize, mv) / (1024 * (1 + 2 * ply as i32))
        } else {
            0
        }
    }
}

/// Board-wide sets used by quiet scoring, computed once per rescore rather than
/// per move. Indexed by [`PieceType`]; king and `None` slots are inert.
struct QuietContext {
    /// Squares where a piece of this type is attacked by something cheaper.
    threatened: [Bitboard; 6],
    /// Bonus for moving a piece of this type off a threatened square.
    escape: [i32; 6],
    /// Safe squares from which a piece of this type attacks enemy material.
    offense: [Bitboard; 6],
    /// Pawns shielding our own castled king.
    wall_pawns: Bitboard,
}

impl QuietContext {
    fn new(td: &ThreadData) -> Self {
        let threats = td.board.all_threats();
        let side = td.board.side_to_move();
        let occupancies = td.board.occupancies();
        let pawn_threats = td.board.piece_threats(PieceType::Pawn);

        let non_pawn_threats = td.board.piece_threats(PieceType::Knight)
            | td.board.piece_threats(PieceType::Bishop)
            | td.board.piece_threats(PieceType::Rook)
            | td.board.piece_threats(PieceType::Queen)
            | td.board.piece_threats(PieceType::King);

        let threatened = {
            let minor_threats =
                pawn_threats | td.board.piece_threats(PieceType::Knight) | td.board.piece_threats(PieceType::Bishop);
            let rook_threats = minor_threats | td.board.piece_threats(PieceType::Rook);
            [Bitboard(0), pawn_threats, pawn_threats, minor_threats, rook_threats, Bitboard(0)]
        };

        let offense = {
            let knight_vulnerable = (td.board.colored_pieces(!side, PieceType::Bishop) & !threats)
                | td.board.colored_pieces(!side, PieceType::Rook)
                | td.board.colored_pieces(!side, PieceType::Queen);
            let bishop_vulnerable = td.board.colored_pieces(!side, PieceType::Rook);
            let queen_orth_vulnerable = td.board.colored_pieces(!side, PieceType::Bishop) & !threats;
            let queen_diag_vulnerable = td.board.colored_pieces(!side, PieceType::Rook) & !threats;

            let mut p = pawn_attacks_setwise(td.board.colors(!side), !side) & !threats;

            // Advanced pawns that already attack something count too.
            p |= pawn_threats & Bitboard::LEVER_RANKS[side] & !non_pawn_threats;

            let n = knight_attacks_setwise(knight_vulnerable) & !threats;
            let b = bishop_attacks_setwise(bishop_vulnerable, occupancies) & !threats;
            // Rooks aim at the enemy king's file rather than at a specific piece.
            let r = Bitboard::file(td.board.king_square(!side).file()) & !threats;
            let q = (rook_attacks_setwise(queen_orth_vulnerable, occupancies)
                | bishop_attacks_setwise(queen_diag_vulnerable, occupancies))
                & !threats;

            [p, n, b, r, q, Bitboard(0)]
        };

        let my_king = td.board.king_square(side);
        let wall_pawns = if Bitboard::HOME_ROWS[side].contains(my_king) {
            king_attacks(my_king) & td.board.pieces(PieceType::Pawn)
        } else {
            Bitboard(0)
        };

        Self { threatened, escape: [0, 8854, 8170, 14051, 20357, 0], offense, wall_pawns }
    }
}
