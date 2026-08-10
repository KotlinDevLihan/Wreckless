use crate::{
    lookup::{
        between, bishop_attacks, king_attacks, knight_attacks, queen_attacks, ray_pass, relative_diagonal, rook_attacks,
    },
    types::{Bitboard, CastlingKind, File, MoveKind, MoveList, PieceType, Square},
};

#[derive(Copy, Clone, Eq, PartialEq)]
enum MovegenKind {
    Quiet,
    Noisy,
}

impl super::Board {
    pub fn has_legal_moves(&self) -> bool {
        let mut list = MoveList::new();
        self.append_all_moves(&mut list);
        !list.is_empty()
    }

    pub fn generate_all_moves(&self) -> MoveList {
        let mut list = MoveList::new();
        self.append_all_moves(&mut list);
        list
    }

    pub fn append_all_moves(&self, list: &mut MoveList) {
        self.append_noisy_moves(list);
        self.append_quiet_moves(list);
    }

    pub fn append_quiet_moves(&self, list: &mut MoveList) {
        self.generate_moves(list, MovegenKind::Quiet);
    }

    pub fn append_noisy_moves(&self, list: &mut MoveList) {
        self.generate_moves(list, MovegenKind::Noisy);
    }

    /// Generates moves of `mgkind`, restricted to legal ones whenever
    /// `checkers()` is non-empty.
    ///
    /// **There is deliberately no separate evasion generator.** Check nodes go
    /// through this same path, which is the one perft validates. A bespoke
    /// evasion generator lived here once and caused two crashes:
    ///
    ///   - King escapes were masked with `!all_threats()`, the attack set of
    ///     the *current* position. That set still treats the square behind the
    ///     king along a checking ray as safe, because the king itself blocks
    ///     it, so stepping straight back down a rook/bishop check was emitted
    ///     as legal. The resulting position lets the opponent capture the king,
    ///     after which `king_square()` returns `Square::None` (64) and
    ///     `INPUT_BUCKETS_LAYOUT[64 ^ 56]` indexes 120 into a 64-entry table.
    ///   - Move kinds were wrong in both directions: king captures were tagged
    ///     `Normal`, so `make_move` never removed the captured piece and left
    ///     two pieces on one square; quiet blocks by knights and sliders were
    ///     tagged `Capture`, so `make_move` tried to remove a piece from an
    ///     empty square. Either corrupts the board, and a corrupt board loses
    ///     the king the same way.
    ///
    /// Splitting evasions out is only worth doing with a generator that carries
    /// its own perft coverage. Until then the shared path is both correct and
    /// no slower in practice, since check nodes are a small fraction of all
    /// nodes. (An `append_evasions` wrapper that merely re-called this function
    /// was removed: nothing called it, and perft drives `generate_all_moves`,
    /// so it would have shipped unvalidated the moment anyone wired it up.)
    fn generate_moves(&self, list: &mut MoveList, mgkind: MovegenKind) {
        let stm = self.side_to_move();
        let occupancies = self.occupancies();
        let kind_target = if mgkind == MovegenKind::Quiet { !occupancies } else { self.colors(!stm) };
        let move_kind = if mgkind == MovegenKind::Quiet { MoveKind::Normal } else { MoveKind::Capture };

        let king_sq = self.king_square(stm);
        list.push_setwise(king_sq, king_attacks(king_sq) & !self.all_threats() & kind_target, move_kind);

        if self.checkers().is_multiple() {
            return;
        }

        let mut target =
            if self.in_check() { between(king_sq, self.checkers().lsb()) | self.checkers() } else { Bitboard::ALL };
        let pinned = self.pinned(stm);

        // Deliberately passed the *unmasked* target: pawn moves do their own
        // noisy/quiet split internally, and it does not line up with
        // `kind_target`. A queen promotion by push is noisy but lands on an
        // empty square, so masking with `kind_target` (= `colors(!stm)` for
        // noisy) before the call would silently drop every push-promotion from
        // noisy generation. Inside, quiets are masked by `empty` and captures
        // by `colors(!stm)`, which is the same restriction applied where the
        // distinction is actually known.
        self.collect_pawn_moves(list, target, pinned, mgkind);

        // Everything below is a piece move, where the split does line up.
        target &= kind_target;

        for knight in self.colored_pieces(stm, PieceType::Knight) & !pinned {
            list.push_setwise(knight, knight_attacks(knight) & target, move_kind);
        }

        let bishops = self.colored_pieces(stm, PieceType::Bishop);
        let rooks = self.colored_pieces(stm, PieceType::Rook);
        let queens = self.colored_pieces(stm, PieceType::Queen);

        self.collect::<_>(list, target, bishops, move_kind, pinned, |sq| bishop_attacks(sq, occupancies));
        self.collect::<_>(list, target, rooks, move_kind, pinned, |sq| rook_attacks(sq, occupancies));
        self.collect::<_>(list, target, queens, move_kind, pinned, |sq| queen_attacks(sq, occupancies));

        if mgkind == MovegenKind::Quiet {
            self.collect_castling(list);
        }
    }

    fn collect<F: Fn(Square) -> Bitboard>(
        &self, list: &mut MoveList, target: Bitboard, pieces: Bitboard, move_kind: MoveKind, pinned: Bitboard,
        attacks: F,
    ) {
        for from in pieces & !pinned {
            list.push_setwise(from, attacks(from) & target, move_kind);
        }

        let king_sq = self.king_square(self.side_to_move());
        for from in pieces & pinned {
            let pin_mask = ray_pass(king_sq, from);
            list.push_setwise(from, attacks(from) & target & pin_mask, move_kind);
        }
    }

    fn collect_castling(&self, list: &mut MoveList) {
        let stm = self.side_to_move();
        for kind in [CastlingKind::KINDS[stm][0], CastlingKind::KINDS[stm][1]] {
            if self.castling().is_allowed(kind)
                && (self.castling_path[kind] & self.occupancies()).is_empty()
                && (self.castling_threat[kind] & self.all_threats()).is_empty()
                && !self.pinned(stm).contains(self.castling_rooks[kind])
            {
                list.push(self.king_square(stm), kind.landing_square(), MoveKind::Castling);
            }
        }
    }

    /// Whether this specific en-passant capture leaves our own king safe.
    ///
    /// Both the capturing pawn and the captured pawn leave the board, so two
    /// squares are vacated at once. An ordinary pin scan cannot see that: each
    /// pawn individually may be unpinned while their joint departure opens a
    /// rank onto the king. Tested against an occupancy with both removed.
    fn en_passant_discovers_no_check(&self, from: Square, ep: Square) -> bool {
        let stm = self.side_to_move();
        let king = self.king_square(stm);
        let occupancies = self.occupancies() ^ from.to_bb() ^ (ep ^ 8).to_bb() | ep.to_bb();

        let diagonal = self.pieces2(PieceType::Bishop, PieceType::Queen) & self.colors(!stm);
        let orthogonal = self.pieces2(PieceType::Rook, PieceType::Queen) & self.colors(!stm);

        (bishop_attacks(king, occupancies) & diagonal).is_empty()
            && (rook_attacks(king, occupancies) & orthogonal).is_empty()
    }

    fn collect_pawn_captures(&self, list: &mut MoveList, pawns: Bitboard, dir: i8, target: Bitboard) {
        let captures = pawns.shift(dir) & target;
        let promos = captures & Bitboard::BOTH_HOME_ROWS;
        list.push_promotion_capture_setwise(dir, promos);
        list.push_pawns_setwise(dir, captures ^ promos, MoveKind::Capture);

        // En passant is generated by hand rather than through `target`, and both
        // of the checks below are load-bearing. This is a legal move generator --
        // nothing filters its output, `is_legal` is called only on the TT move --
        // so anything emitted here is played.
        //
        // 1. `target` masks a move by its DESTINATION, and en passant captures a
        //    pawn that is not on its destination square. Masking on `ep` alone
        //    would drop the legitimate capture of a checking pawn (which sits on
        //    `ep ^ 8`); not masking at all admits en passant when the checker is
        //    something else entirely -- a double push delivering a DISCOVERED
        //    check, which en passant does not resolve. The move is legal in check
        //    only if it captures the checker (`ep ^ 8`) or blocks it (`ep`).
        //
        // 2. `validate_en_passant` clears the ep square only when NO taker is
        //    legal. With two capturers and one of them horizontally pinned
        //    through the double-vacated rank, it leaves the square set and both
        //    moves get generated. That pin is invisible to the ordinary `pinned`
        //    set, because it only appears once BOTH pawns leave the rank, so it
        //    has to be tested per capturer here.
        let ep = self.en_passant();
        if ep != Square::None
            && pawns.contains(ep.shift(-dir))
            && (target.contains(ep) || target.contains(ep ^ 8))
            && self.en_passant_discovers_no_check(ep.shift(-dir), ep)
        {
            list.push(ep.shift(-dir), self.en_passant(), MoveKind::EnPassant);
        }
    }

    fn collect_pawn_moves(&self, list: &mut MoveList, target: Bitboard, pinned: Bitboard, mgkind: MovegenKind) {
        let stm = self.side_to_move();
        let up = Square::UP[stm];
        let pawns = self.colored_pieces(stm, PieceType::Pawn);
        let third_rank = Bitboard::THIRD_RANK[stm];
        let empty = !self.occupancies();
        let king_sq = self.king_square(stm);

        let pushed_pawns = (pawns & (!pinned | Bitboard::file(king_sq.file()))).shift(up) & empty;
        let promotions = pushed_pawns & Bitboard::BOTH_HOME_ROWS & target;

        if mgkind == MovegenKind::Quiet {
            let single_pushes = pushed_pawns ^ promotions;
            let double_pushes = (single_pushes & third_rank).shift(up) & empty;

            list.push_pawns_setwise(up, single_pushes & target, MoveKind::Normal);
            list.push_pawns_setwise(up * 2, double_pushes & target, MoveKind::DoublePush);
            list.push_pawns_setwise(up, promotions, MoveKind::PromotionR);
            list.push_pawns_setwise(up, promotions, MoveKind::PromotionB);
            list.push_pawns_setwise(up, promotions, MoveKind::PromotionN);
        }

        if mgkind == MovegenKind::Noisy {
            list.push_pawns_setwise(up, promotions & target, MoveKind::PromotionQ);

            let target = target & self.colors(!stm);

            let dirs = [up + Square::RIGHT, up + Square::LEFT];
            let pin_masks = [relative_diagonal(stm, king_sq), relative_diagonal(!stm, king_sq)];
            let shift_masks = [!Bitboard::file(File::H), !Bitboard::file(File::A)];

            for i in 0..2 {
                let the_pawns = pawns & (!pinned | pin_masks[i]) & shift_masks[i];
                self.collect_pawn_captures(list, the_pawns, dirs[i], target);
            }
        }
    }
}
