use crate::{parameters as p, thread::ThreadData, types::Score};

pub fn correct_eval(td: &ThreadData, raw_eval: i32, correction_value: i32) -> i32 {
    // Stockfish's `evaluate()` structure: blend the raw network output with
    // `optimism`, both scaled by material so the same centipawn means less in a
    // bare endgame than in a full middlegame.
    //
    // These were hardcoded, and `spsa.config` had no eval-side entry at all --
    // so the function every single evaluation passes through was the one part
    // of the engine tuning could not reach, while 148 search constants
    // downstream of it were tuned repeatedly.
    //
    // NOTE the divisor is deliberately NOT exposed. It sets the engine's
    // overall evaluation scale, and every margin in the search -- RFP, futility,
    // razoring, singular, SEE, the history normalisation -- is tuned against
    // that scale. Letting SPSA move it would silently rescale all of them at
    // once: the exact scale-drift failure this codebase has paid for repeatedly.
    // The numerators change the blend; the divisor would change the units.
    let mut eval = (raw_eval * (p::eval_material_base() + td.board.material())
        + td.optimism[td.board.side_to_move()] * (p::eval_optimism_base() + td.board.material()))
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
    // Stockfish uses `(200 - rule50) / 214` here, which damps to ~0.93 even at a
    // clock of zero; this damps to exactly 1.0. Both are defensible and the
    // difference has never been tested, so the offset and the divisor are now
    // separate parameters whose defaults reproduce the shipped behaviour
    // exactly. Setting `eval_fifty_div` to 214 reproduces Stockfish's.
    //
    // The `.min()` tracks the offset so the sign-flip guard above holds for any
    // tuned value; the divisor is `.max(1)` because SPSA writes it.
    // Compared in i32. `horizon as u8` wrapped: `eval_fifty_offset` is declared
    // in spsa.config with an upper bound of 260, and at 260 the cast gives 4 --
    // damping would stop at a clock of 4 and the multiplier would sit at
    // 260/200 = 1.3, inflating every static eval by 30% and rescaling every
    // search margin that reads it. That is precisely the scale drift the clamp
    // above exists to prevent, reintroduced by the clamp itself.
    // The divisor is floored at the offset so this term can only ever DAMP.
    //
    // At a clock of zero the multiplier is `offset / divisor`, and the two are
    // independently tunable over [150, 260] -- so a tuner could reach 260/150 =
    // 1.73x and silently amplify every static eval in the engine by 73%. That
    // rescales every margin downstream of `eval`, which is the exact drift the
    // divisor was deliberately left out of `correct_eval`'s blend to prevent;
    // splitting this term into two free parameters reintroduced it by the back
    // door.
    //
    // Stockfish's shape is offset 200 / divisor 214, i.e. divisor >= offset. The
    // floor keeps the whole tunable range on that side of 1.0.
    let horizon = p::eval_fifty_offset();
    let divisor = p::eval_fifty_div().max(horizon).max(1);
    let clock = (td.board.fiftymove_clock() as i32).min(horizon);
    eval = eval * (horizon - clock) / divisor;

    eval += correction_value;

    eval.clamp(-Score::TB_WIN_IN_MAX + 1, Score::TB_WIN_IN_MAX - 1)
}
