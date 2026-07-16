use crate::{attacks::*, magics::*};

pub fn generate_king_map() -> [u64; 64] {
    generate_map(king_attacks)
}

pub fn generate_knight_map() -> [u64; 64] {
    generate_map(knight_attacks)
}

fn generate_map<F: Fn(u8) -> u64>(f: F) -> [u64; 64] {
    let mut map = [0; 64];
    for square in 0..64 {
        map[square as usize] = f(square as u8);
    }

    map
}

pub fn generate_between_map() -> [[u64; 64]; 64] {
    std::array::from_fn(|i| generate_map(|square| generate_ray(square, i as u8, true)))
}

pub fn generate_rays_map() -> [[u64; 64]; 64] {
    std::array::from_fn(|i| generate_map(|square| generate_ray(square, i as u8, false)))
}

pub fn generate_pawn_map() -> [[u64; 64]; 2] {
    [
        generate_map(|square| pawn_attacks(square, Color::White)),
        generate_map(|square| pawn_attacks(square, Color::Black)),
    ]
}

pub fn generate_diagonal_tables() -> [[u64; 64]; 2] {
    [
        generate_map(|square| sliding_attacks(square, 0, &[9, -9])),
        generate_map(|square| sliding_attacks(square, 0, &[7, -7])),
    ]
}

pub fn generate_rook_map(use_pext: bool) -> Vec<u64> {
    generate_sliding_map(ROOK_MAP_SIZE, &ROOK_MAGICS, &[8, -8, 1, -1], use_pext)
}

pub fn generate_bishop_map(use_pext: bool) -> Vec<u64> {
    generate_sliding_map(BISHOP_MAP_SIZE, &BISHOP_MAGICS, &[9, 7, -7, -9], use_pext)
}

fn generate_sliding_map(size: usize, magics: &[MagicEntry], directions: &[i8], use_pext: bool) -> Vec<u64> {
    let mut map = vec![0; size];

    for square in 0..64 {
        let entry = &magics[square as usize];

        let mut occupancies = 0u64;
        for _ in 0..get_permutation_count(entry.mask) {
            let hash = if use_pext { pext_index(occupancies, entry) } else { magic_index(occupancies, entry) };
            map[hash] = sliding_attacks(square, occupancies, directions);

            occupancies = occupancies.wrapping_sub(entry.mask) & entry.mask;
        }
    }

    map
}

const fn get_permutation_count(mask: u64) -> u64 {
    1 << mask.count_ones()
}

const fn magic_index(occupancies: u64, entry: &MagicEntry) -> usize {
    let mut hash = occupancies & entry.mask;
    hash = hash.wrapping_mul(entry.magic) >> entry.shift;
    hash as usize + entry.offset
}

/// Matches the indexing of the BMI2 `pext` instruction: each per-square table
/// segment has exactly `2^popcount(mask)` slots, so the magic offsets carry
/// over unchanged. Computed in software so table generation does not depend
/// on the build host supporting BMI2.
const fn pext_index(occupancies: u64, entry: &MagicEntry) -> usize {
    let mut mask = entry.mask;
    let mut result = 0u64;
    let mut bit = 1u64;

    while mask != 0 {
        if occupancies & mask & mask.wrapping_neg() != 0 {
            result |= bit;
        }
        mask &= mask - 1;
        bit <<= 1;
    }

    result as usize + entry.offset
}
