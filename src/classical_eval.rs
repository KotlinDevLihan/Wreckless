//! Hand-crafted (not learned) evaluation terms, added on top of the NNUE
//! output in plain centipawn space. See the comment on `correct_eval` in
//! `evaluation.rs` for why these live here instead of as new NNUE inputs:
//! a brand-new *learned* feature has no meaningful weights without
//! training data, but classical hand-tuned evaluation is a real, working
//! technique on its own, and every constant below is SPSA-tunable (see
//! `parameters.rs`) so it can be refined by testing instead of guessing.

use crate::{
    board::Board,
    lookup,
    parameters as p,
    types::{Bitboard, Color, Piece, PieceType, Square},
};

pub fn classical_bonus(board: &Board) -> i32 {
    let stm = board.side_to_move();
    classical_score(board, stm) - classical_score(board, !stm)
}

fn file_mask(file_index: u8) -> Bitboard {
    Bitboard(0x0101_0101_0101_0101u64 << file_index)
}

fn rank_mask(rank_index: u8) -> Bitboard {
    Bitboard(0xFFu64 << (rank_index * 8))
}

fn adjacent_files_mask(file_index: u8) -> Bitboard {
    let mut mask = Bitboard(0);
    if file_index > 0 {
        mask |= file_mask(file_index - 1);
    }
    if file_index < 7 {
        mask |= file_mask(file_index + 1);
    }
    mask
}

// All squares strictly ahead of `rank_index`, from `color`'s point of view.
fn ahead_mask(rank_index: u8, color: Color) -> Bitboard {
    match color {
        Color::White => {
            if rank_index == 7 { Bitboard(0) } else { Bitboard(!0u64 << ((rank_index as u32 + 1) * 8)) }
        }
        Color::Black => {
            if rank_index == 0 { Bitboard(0) } else { Bitboard(!0u64 >> ((8 - rank_index) as u32 * 8)) }
        }
    }
}

fn pawn_attacks(pawns: Bitboard, color: Color) -> Bitboard {
    let forward = Square::UP[color as usize];
    let not_a = !file_mask(0);
    let not_h = !file_mask(7);

    Bitboard(pawns.0 & not_a.0).shift(forward - 1) | Bitboard(pawns.0 & not_h.0).shift(forward + 1)
}

fn relative_rank(square: Square, color: Color) -> u8 {
    let rank = square as u8 >> 3;
    if color == Color::White { rank } else { 7 - rank }
}

fn passed_bonus(relative_rank: u8) -> i32 {
    match relative_rank {
        1 => p::passed_r1(),
        2 => p::passed_r2(),
        3 => p::passed_r3(),
        4 => p::passed_r4(),
        5 => p::passed_r5(),
        6 => p::passed_r6(),
        _ => 0,
    }
}

fn classical_score(board: &Board, color: Color) -> i32 {
    let mut score = pawn_score(board, color);
    score += bishop_pair_score(board, color);
    score += rook_file_score(board, color);
    score += outpost_score(board, color);
    score += mobility_score(board, color);
    score += king_safety_score(board, color);
    score
}

fn pawn_score(board: &Board, color: Color) -> i32 {
    let pawns = board.pieces(PieceType::Pawn) & board.colors(color);
    let their_pawns = board.pieces(PieceType::Pawn) & board.colors(!color);

    if pawns.is_empty() {
        return 0;
    }

    let our_attacks = pawn_attacks(pawns, color);
    let forward = Square::UP[color as usize];

    let mut score = 0;

    for file_index in 0..8u8 {
        let count = (pawns & file_mask(file_index)).popcount() as i32;
        if count >= 2 {
            score -= p::doubled_penalty() * (count - 1);
        }
    }

    for sq in pawns {
        let file_index = sq as u8 & 7;
        let rank_index = sq as u8 >> 3;
        let neighbor_files = adjacent_files_mask(file_index);
        let mut neighbor_pawns = pawns & neighbor_files;

        if neighbor_pawns.is_empty() {
            score -= p::isolated_penalty();
        } else {
            let our_rank = relative_rank(sq, color);

            if neighbor_pawns.all(|nb| relative_rank(nb, color) > our_rank) {
                let stop_square = sq.shift(forward);
                if pawn_attacks(their_pawns, !color).contains(stop_square) {
                    score -= p::backward_penalty();
                }
            }
        }

        if !(pawns & neighbor_files & rank_mask(rank_index)).is_empty() {
            score += p::phalanx_bonus();
        }

        if our_attacks.contains(sq) {
            score += p::chain_bonus();
        }

        let ahead_zone = ahead_mask(rank_index, color) & (neighbor_files | file_mask(file_index));
        if (their_pawns & ahead_zone).is_empty() {
            score += passed_bonus(relative_rank(sq, color));
        }
    }

    score
}

fn bishop_pair_score(board: &Board, color: Color) -> i32 {
    let bishops = board.pieces(PieceType::Bishop) & board.colors(color);
    if bishops.popcount() >= 2 { p::bishop_pair_bonus() } else { 0 }
}

fn rook_file_score(board: &Board, color: Color) -> i32 {
    let own_pawns = board.pieces(PieceType::Pawn) & board.colors(color);
    let their_pawns = board.pieces(PieceType::Pawn) & board.colors(!color);
    let rooks = board.pieces(PieceType::Rook) & board.colors(color);

    let mut score = 0;
    for sq in rooks {
        let file_index = sq as u8 & 7;
        let file = file_mask(file_index);

        if (own_pawns & file).is_empty() {
            score += if (their_pawns & file).is_empty() { p::rook_open_file_bonus() } else { p::rook_semi_open_file_bonus() };
        }
    }
    score
}

// A minor piece is an outpost when it sits on a square in enemy territory,
// defended by one of our pawns, that no enemy pawn can ever attack (no
// enemy pawn on an adjacent file that is still behind or level with it).
fn outpost_score(board: &Board, color: Color) -> i32 {
    let own_pawns = board.pieces(PieceType::Pawn) & board.colors(color);
    let their_pawns = board.pieces(PieceType::Pawn) & board.colors(!color);
    let our_pawn_attacks = pawn_attacks(own_pawns, color);

    let mut score = 0;

    for (piece_type, bonus) in [(PieceType::Knight, p::knight_outpost_bonus()), (PieceType::Bishop, p::bishop_outpost_bonus())]
    {
        let pieces = board.pieces(piece_type) & board.colors(color);

        for sq in pieces {
            let rel_rank = relative_rank(sq, color);
            if rel_rank < 4 {
                continue;
            }

            if !our_pawn_attacks.contains(sq) {
                continue;
            }

            let file_index = sq as u8 & 7;
            let rank_index = sq as u8 >> 3;
            let neighbor_files = adjacent_files_mask(file_index);
            let attackable_zone = ahead_mask(rank_index, !color) & neighbor_files;

            if (their_pawns & attackable_zone).is_empty() {
                score += bonus;
            }
        }
    }

    score
}

fn mobility_score(board: &Board, color: Color) -> i32 {
    let occ = board.occupancies();
    let own = board.colors(color);

    let mut score = 0;

    for (piece_type, weight) in [
        (PieceType::Knight, p::knight_mobility()),
        (PieceType::Bishop, p::bishop_mobility()),
        (PieceType::Rook, p::rook_mobility()),
        (PieceType::Queen, p::queen_mobility()),
    ] {
        let pieces = board.pieces(piece_type) & own;
        for sq in pieces {
            let attacks = lookup::attacks(Piece::new(color, piece_type), sq, occ);
            score += weight * (attacks & !own).popcount() as i32;
        }
    }

    score
}

fn king_safety_score(board: &Board, color: Color) -> i32 {
    let king_sq = board.king_square(color);
    let file_index = king_sq as u8 & 7;
    let rank_index = king_sq as u8 >> 3;
    let own_pawns = board.pieces(PieceType::Pawn) & board.colors(color);

    let mut score = 0;

    let shield_rank = match color {
        Color::White => rank_index + 1,
        Color::Black => rank_index.wrapping_sub(1),
    };

    if shield_rank < 8 {
        let shield_files = file_mask(file_index) | adjacent_files_mask(file_index);
        let shield_zone = shield_files & rank_mask(shield_rank);
        let missing = shield_zone.popcount() as i32 - (own_pawns & shield_zone).popcount() as i32;
        score -= p::king_shield_penalty() * missing;
    }

    if (own_pawns & file_mask(file_index)).is_empty() {
        score -= p::king_open_file_penalty();
    }

    score
}
