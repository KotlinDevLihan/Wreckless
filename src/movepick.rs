use crate::{
    search::NodeType,
    thread::ThreadData,
    types::{MAX_MOVES, Move, MoveList},
};

/// Search stages for MovePicker
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    HashMove,
    GenerateCaptures,
    GoodCaptures,
    GenerateQuiets,
    GoodQuiets,
    BadNoisy,
    BadQuiets,
    QSearchCaptures,
    Evasions,
    Done,
}

/// A move paired with its move-ordering score
#[derive(Debug, Clone, Copy, Default)]
pub struct ScoredMove {
    pub mv: Move,
    pub score: i32,
}

pub struct MovePicker {
    stage: Stage,
    tt_move: Move,
    /// SEE threshold used to split captures into "good" (>= threshold, tried
    /// first) and "bad noisy" (below threshold, tried last / prunable) pools.
    /// `None` means the default threshold of 0.
    threshold: Option<i32>,
    moves: [ScoredMove; MAX_MOVES],
    bad_captures: [ScoredMove; MAX_MOVES],
    cur: usize,
    end: usize,
    bad_cur: usize,
    bad_end: usize,
    skip_quiets: bool,
}

impl MovePicker {
    /// Threshold score to distinguish good quiet moves from bad quiet moves
    const GOOD_QUIET_THRESHOLD: i32 = -14000;

    /// Create a new MovePicker for main search
    pub fn new(tt_move: Move, threshold: Option<i32>) -> Self {
        Self {
            stage: Stage::HashMove,
            tt_move,
            threshold,
            moves: [ScoredMove::default(); MAX_MOVES],
            bad_captures: [ScoredMove::default(); MAX_MOVES],
            cur: 0,
            end: 0,
            bad_cur: 0,
            bad_end: 0,
            skip_quiets: false,
        }
    }

    pub fn stage(&self) -> Stage {
        self.stage
    }

    /// Skip whatever bad noisy moves remain (e.g. once BNFP/HP-noisy decides
    /// the rest aren't worth trying), while still allowing any deferred bad
    /// quiets to be searched afterwards.
    pub fn skip_bad_noisy(&mut self) {
        self.bad_cur = self.bad_end;
    }

    /// Select and return the next best pseudo-legal move
    pub fn next<NODE: NodeType>(&mut self, td: &ThreadData, skip_quiets: bool, ply: isize) -> Option<Move> {
        self.skip_quiets = skip_quiets;
        while self.stage != Stage::Done {
            match self.stage {
                Stage::HashMove => {
                    // In-check nodes always go through the dedicated evasion
                    // generator (king retreats, blocks, and captures), even
                    // when quiets are being skipped (qsearch/ProbCut) --
                    // otherwise a node in check can be left with no legal
                    // reply generated at all.
                    self.stage = if td.board.in_check() {
                        Stage::Evasions
                    } else if self.skip_quiets {
                        Stage::QSearchCaptures
                    } else {
                        Stage::GenerateCaptures
                    };

                    if self.tt_move != Move::NULL && td.board.is_legal(self.tt_move) {
                        return Some(self.tt_move);
                    }
                }

                Stage::GenerateCaptures => {
                    self.generate_and_score_captures(td);
                    self.stage = Stage::GoodCaptures;
                }

                Stage::GoodCaptures => {
                    if let Some(mv) = self.pick_best() {
                        if mv == self.tt_move {
                            continue;
                        }
                        return Some(mv);
                    }
                    self.stage = Stage::GenerateQuiets;
                }

                Stage::GenerateQuiets => {
                    if self.skip_quiets {
                        self.stage = Stage::BadNoisy;
                    } else {
                        self.generate_and_score_quiets(td, ply);
                        self.stage = Stage::GoodQuiets;
                    }
                }

                Stage::GoodQuiets => {
                    if let Some(mv) = self.pick_best() {
                        if mv == self.tt_move {
                            continue;
                        }
                        // Defer quiets with bad history scores until after bad noisy moves
                        if self.moves[self.cur - 1].score < Self::GOOD_QUIET_THRESHOLD {
                            self.stage = Stage::BadNoisy;
                            continue;
                        }
                        return Some(mv);
                    }
                    self.stage = Stage::BadNoisy;
                }

                Stage::BadNoisy => {
                    if self.bad_cur < self.bad_end {
                        let mv = self.bad_captures[self.bad_cur].mv;
                        self.bad_cur += 1;
                        if mv == self.tt_move {
                            continue;
                        }
                        return Some(mv);
                    }
                    self.stage = Stage::BadQuiets;
                }

                Stage::BadQuiets => {
                    if let Some(mv) = self.pick_best() {
                        if mv == self.tt_move {
                            continue;
                        }
                        return Some(mv);
                    }
                    self.stage = Stage::Done;
                }

                Stage::QSearchCaptures => {
                    if self.cur == 0 && self.end == 0 {
                        self.generate_and_score_captures(td);
                    }
                    if let Some(mv) = self.pick_best() {
                        if mv == self.tt_move {
                            continue;
                        }
                        return Some(mv);
                    }
                    self.stage = Stage::BadNoisy;
                }

                Stage::Evasions => {
                    if self.cur == 0 && self.end == 0 {
                        self.generate_evasions(td, ply);
                    }
                    if let Some(mv) = self.pick_best() {
                        if mv == self.tt_move {
                            continue;
                        }
                        return Some(mv);
                    }
                    self.stage = Stage::Done;
                }

                Stage::Done => break,
            }
        }
        None
    }

    fn pick_best(&mut self) -> Option<Move> {
        if self.cur >= self.end {
            return None;
        }

        let mut best_idx = self.cur;
        for i in (self.cur + 1)..self.end {
            if self.moves[i].score > self.moves[best_idx].score {
                best_idx = i;
            }
        }

        self.moves.swap(self.cur, best_idx);
        let mv = self.moves[self.cur].mv;
        self.cur += 1;
        Some(mv)
    }

    fn score_capture(&self, td: &ThreadData, mv: Move) -> (bool, i32) {
        let threshold = self.threshold.unwrap_or(0);
        let is_good = td.board.see(mv, threshold);
        let score = (if is_good { 1000 } else { -1000 })
            + td.noisy_history.get(td.board.all_threats(), td.board.moved_piece(mv), mv.to(), td.board.type_on(mv.capture_sq()));
        (is_good, score)
    }

    fn score_quiet(&self, td: &ThreadData, ply: isize, mv: Move) -> i32 {
        td.quiet_history.get(td.board.all_threats(), td.board.side_to_move(), mv)
            + 1479 * td.conthist(ply, 1, mv) / 1024
            + 977 * td.conthist(ply, 2, mv) / 1024
            + 277 * td.conthist(ply, 3, mv) / 1024
            + 995 * td.conthist(ply, 4, mv) / 1024
            + 126 * td.conthist(ply, 5, mv) / 1024
            + 963 * td.conthist(ply, 6, mv) / 1024
    }

    fn generate_and_score_captures(&mut self, td: &ThreadData) {
        let mut move_list = MoveList::new();
        td.board.append_noisy_moves(&mut move_list);

        self.cur = 0;
        self.end = 0;
        self.bad_cur = 0;
        self.bad_end = 0;

        for entry in move_list.iter() {
            let mv = entry.mv;
            let (is_good, score) = self.score_capture(td, mv);

            if is_good {
                self.moves[self.end] = ScoredMove { mv, score };
                self.end += 1;
            } else {
                self.bad_captures[self.bad_end] = ScoredMove { mv, score };
                self.bad_end += 1;
            }
        }
    }

    fn generate_and_score_quiets(&mut self, td: &ThreadData, ply: isize) {
        let mut move_list = MoveList::new();
        td.board.append_quiet_moves(&mut move_list);

        self.cur = 0;
        self.end = 0;

        for entry in move_list.iter() {
            let mv = entry.mv;
            let score = self.score_quiet(td, ply, mv);
            self.moves[self.end] = ScoredMove { mv, score };
            self.end += 1;
        }
    }

    fn generate_evasions(&mut self, td: &ThreadData, ply: isize) {
        let mut move_list = MoveList::new();
        td.board.append_evasions(&mut move_list);

        self.cur = 0;
        self.end = 0;

        for entry in move_list.iter() {
            let mv = entry.mv;
            let score = if mv.is_capture() {
                let (_, capture_score) = self.score_capture(td, mv);
                10000 + capture_score
            } else {
                self.score_quiet(td, ply, mv)
            };
            self.moves[self.end] = ScoredMove { mv, score };
            self.end += 1;
        }
    }
}