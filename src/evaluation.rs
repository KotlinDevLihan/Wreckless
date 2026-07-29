use crate::{thread::ThreadData, types::Score};

pub fn correct_eval(td: &ThreadData, raw_eval: i32, correction_value: i32) -> i32 {
    let mut eval = (raw_eval * (21032 + td.board.material())
        + td.optimism[td.board.side_to_move()] * (1548 + td.board.material()))
        / 27015;

    // Damp toward zero as the fiftymove clock runs up. The clock is a `u8`
    // parsed straight from the FEN, so 201..=255 is representable: without the
    // clamp the multiplier goes negative and the evaluation comes back with
    // its **sign flipped**. `is_draw` gates every node except the root, so the
    // reachable case is a root FEN with a clock above 200 feeding a negated
    // static eval into aspiration and every margin that reads it.
    //
    // Unreachable from legal play -- the fiftymove rule ends the game at 100 --
    // so this only fires on a hand-written or corrupt FEN, and is inert for
    // normal games.
    eval = eval * (200 - td.board.fiftymove_clock().min(200) as i32) / 200;

    eval += correction_value;

    eval.clamp(-Score::TB_WIN_IN_MAX + 1, Score::TB_WIN_IN_MAX - 1)
}
