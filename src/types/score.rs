use super::{MAX_PLY, PieceType};
use crate::{board::Board, thread::ThreadData};

pub struct Score;

#[rustfmt::skip]
impl Score {
    pub const ZERO: i32 = 0;

    pub const NONE:     i32 = 32002;
    pub const INFINITE: i32 = 32001;
    pub const MATE:     i32 = 32000;

    pub const MATE_IN_MAX: i32 =  32000 - MAX_PLY as i32;

    pub const TB_WIN:        i32 = Self::MATE_IN_MAX - 1;
    pub const TB_WIN_IN_MAX: i32 = Self::TB_WIN - MAX_PLY as i32;
}

pub fn draw(td: &ThreadData) -> i32 {
    (td.nodes() % 5) as i32 - 2
}

pub const fn mated_in(ply: isize) -> i32 {
    -Score::MATE + ply as i32
}

pub const fn mate_in(ply: isize) -> i32 {
    Score::MATE - ply as i32
}

#[cfg(feature = "syzygy")]
pub const fn tb_loss_in(ply: isize) -> i32 {
    -Score::TB_WIN + ply as i32
}

#[cfg(feature = "syzygy")]
pub const fn tb_win_in(ply: isize) -> i32 {
    Score::TB_WIN - ply as i32
}

pub const fn is_win(score: i32) -> bool {
    score >= Score::TB_WIN_IN_MAX
}

pub const fn is_loss(score: i32) -> bool {
    score <= -Score::TB_WIN_IN_MAX
}

pub const fn is_decisive(score: i32) -> bool {
    is_win(score) || is_loss(score)
}

pub const fn is_valid(score: i32) -> bool {
    score != Score::NONE
}

fn normalization(board: &Board) -> f64 {
    let material = board.pieces(PieceType::Pawn).popcount()
        + 3 * board.pieces(PieceType::Knight).popcount()
        + 3 * board.pieces(PieceType::Bishop).popcount()
        + 5 * board.pieces(PieceType::Rook).popcount()
        + 9 * board.pieces(PieceType::Queen).popcount();

    let v = material.clamp(16, 78) as f64 / 58.0;

    -285.1 * v.powi(3) + 642.5 * v.powi(2) - 455.5 * v + 464.8
}

pub fn normalize_to_cp(score: i32, board: &Board) -> i32 {
    (100.0 * score as f64 / normalization(board)).round() as i32
}

/// Estimates (win, draw, loss) per mille from the side to move's perspective.
///
/// The 50% win probability is anchored on the same material-dependent
/// normalization used for centipawn reporting; the logistic slope is a rough
/// approximation and should be refit from Reckless self-play data.
pub fn wdl_model(score: i32, board: &Board) -> (i32, i32, i32) {
    if score >= Score::TB_WIN_IN_MAX {
        return (1000, 0, 0);
    }
    if score <= -Score::TB_WIN_IN_MAX {
        return (0, 0, 1000);
    }

    let a = normalization(board);
    let b = 0.35 * a;
    let v = f64::from(score);

    let win = (1000.0 / (1.0 + ((a - v) / b).exp())).round() as i32;
    let loss = (1000.0 / (1.0 + ((a + v) / b).exp())).round() as i32;

    (win, 1000 - win - loss, loss)
}
