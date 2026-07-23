use crate::{
    classical_eval::{classical_bonus, king_safety_bonus},
    parameters as p,
    thread::ThreadData,
    types::Score,
};

// Combined material (both sides, all pieces) at the game's start. Board::material
// sums every piece's value including pawns (see board/parser.rs), so this is
// 2*(8*109 + 2*403 + 2*435 + 2*679 + 1242) = 2*5148, not just non-pawn
// material -- verified against the actual field, not assumed. Used only to
// interpolate the classical-eval phase weight below, not as an authoritative
// game-phase constant elsewhere.
const STARTING_MATERIAL: i32 = 10296;

pub fn correct_eval(td: &ThreadData, raw_eval: i32, correction_value: i32) -> i32 {
    let mut eval = (raw_eval * (21032 + td.board.material())
        + td.optimism[td.board.side_to_move()] * (1548 + td.board.material()))
        / 27015;

    eval = eval * (200 - td.board.fiftymove_clock() as i32) / 200;

    eval += correction_value;
    eval += classical_eval_contribution(td);
    eval += king_safety_bonus(td) * p::king_safety_weight() / 128;

    eval.clamp(-Score::TB_WIN_IN_MAX + 1, Score::TB_WIN_IN_MAX - 1)
}

// The classical eval is gated behind one overall weight rather than added at
// full strength: a hand-crafted eval layered on an already-trained NNUE
// risks double-counting signal the network already learned from real game
// data, so this defaults low (a gentle nudge, not a competing parallel eval)
// and is phase-scaled -- stronger in material-light endgames, where
// classical rules (passed pawns, king activity) are well-established and
// NNUE training data is typically sparser, weaker in full-material
// middlegames where the network is most reliable and redundancy risk is
// highest. SPSA can discover the right operating point, including
// effectively zero if the whole approach doesn't pay off.
fn classical_eval_contribution(td: &ThreadData) -> i32 {
    let material = td.board.material().clamp(0, STARTING_MATERIAL);

    let weight = p::classical_eval_endgame_weight()
        - (p::classical_eval_endgame_weight() - p::classical_eval_weight()) * material / STARTING_MATERIAL;

    classical_bonus(td) * weight / 128
}
