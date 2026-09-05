use std::ops::{Index, IndexMut};

use crate::types::{MAX_PLY, Move, Piece, Score};

pub struct Stack {
    data: [StackEntry; MAX_PLY + 16],
    sentinel: [[i16; 64]; 13],
}

impl Stack {
    pub fn sentinel(&mut self) -> &mut StackEntry {
        unsafe { self.data.get_unchecked_mut(0) }
    }

    pub fn new() -> Box<Self> {
        let mut stack = Box::new(Self::default());
        stack.reset();
        stack
    }

    /// Resets an existing stack in place (same content as a freshly built
    /// one), reusing its heap allocation instead of allocating a new `Box`.
    /// Called every aspiration-window retry and every iterative-deepening
    /// depth, so avoiding the realloc there is a real, if small, NPS win with
    /// no change to search behavior.
    pub fn reset(&mut self) {
        self.data = [StackEntry::default(); MAX_PLY + 16];
        self.sentinel = [[0; 64]; 13];

        let ptr = &raw mut self.sentinel;
        for entry in &mut self.data {
            entry.conthist = ptr;
            entry.contcorrhist = ptr;
        }
    }
}

impl Default for Stack {
    fn default() -> Self {
        Self { data: [StackEntry::default(); MAX_PLY + 16], sentinel: [[0; 64]; 13] }
    }
}

#[derive(Copy, Clone)]
pub struct StackEntry {
    pub mv: Move,
    pub piece: Piece,
    pub eval: i32,
    pub tt_move: Move,
    pub tt_pv: bool,
    pub move_count: u16,
    /// Reduction applied to this node's child, in LMR units (1024 = one ply).
    ///
    /// Read by the child's hindsight depth adjustments and by
    /// `lmr_prev_reduction`. Both were tuned against upstream's LMR scale, so
    /// only values on that scale may be published here -- the FDS branch,
    /// whose raw reduction lives on a different scale entirely, publishes the
    /// plies it actually applied (`1024 * plies`) rather than its raw counter.
    /// See `fds_reduction` for why the raw value needs a field of its own.
    pub reduction: i32,
    /// Raw FDS reduction counter for this node's child, on the FDS scale.
    ///
    /// Kept separate from [`Self::reduction`] because the two are not
    /// comparable: `fds_quiet_base` (1468) / `fds_ilog` (207) run 700-1300
    /// below `lmr_quiet_base` (2171) / `lmr_ilog` (269) for the same position,
    /// so a single shared field made `fds_prev_reduction` and
    /// `lmr_prev_reduction` fire on which branch the parent happened to take
    /// rather than on how much it reduced. `fds_prev_reduction` reads this one;
    /// `lmr_prev_reduction` reads `reduction`.
    pub fds_reduction: i32,
    pub follow_pv: bool,
    /// Double/triple singular extensions accumulated along this line.
    ///
    /// Each singular node can grant up to +3 plies and nothing tracked how often
    /// that had already happened above it, so a tactical line could keep
    /// extending. `MAX_PLY` bounds the damage but not the tree. Stockfish and
    /// Berserk both carry an equivalent counter (`ss->doubleExtensions`,
    /// `ss->de`) and gate their upper extension tiers on it.
    pub double_extensions: i32,
    /// Accumulated "how many wide, late-move branches has this line taken"
    /// counter, ported from upstream Reckless (`stack.rs`/`search.rs`
    /// there), which this fork had silently dropped -- no field, no
    /// consumer, no note in the README's "Removed, and why" section, unlike
    /// every other deliberate removal this codebase documents. Restored
    /// rather than left missing.
    ///
    /// Propagated from the parent and bumped in `make_move` by
    /// `(move_count.ilog2() - 1).max(0)`, so it only grows once move_count
    /// passes 4 and grows slowly (one unit per doubling) after that; reset to
    /// 0 across a null move, matching upstream, since a null move restarts
    /// the "how many alternatives did we already reject to get here" count.
    /// Read by the non-PV branch of the LMR reduction formula (see
    /// `lmr_nonpv_base`/`lmr_nonpv_laterality` in parameters.rs): a line that
    /// is cumulatively "later" -- has already passed through many
    /// higher-move-count nodes to get here -- gets reduced somewhat harder,
    /// on the same logic a single node's own move_count already reduces
    /// harder for.
    pub laterality: i32,
    pub conthist: *mut [[i16; 64]; 13],
    pub contcorrhist: *mut [[i16; 64]; 13],
}

unsafe impl Send for StackEntry {}

impl Default for StackEntry {
    fn default() -> Self {
        Self {
            mv: Move::NULL,
            piece: Piece::None,
            eval: Score::NONE,
            tt_move: Move::NULL,
            tt_pv: false,
            move_count: 0,
            reduction: 0,
            fds_reduction: 0,
            follow_pv: false,
            double_extensions: 0,
            laterality: 0,
            conthist: std::ptr::null_mut(),
            contcorrhist: std::ptr::null_mut(),
        }
    }
}

impl Index<isize> for Stack {
    type Output = StackEntry;

    fn index(&self, index: isize) -> &Self::Output {
        // The assertion must bound the *shifted* index, which is what actually
        // indexes `data`. Bounding the raw `index` against the array length
        // instead left the top 8 slots of the permitted range past the end of
        // `data` -- so the one guard backing this `get_unchecked` did not
        // actually cover it. Matches the (already correct) `PlyArray` form.
        debug_assert!(index + 8 >= 0 && ((index + 8) as usize) < MAX_PLY + 16);
        // SAFETY: the debug_assert above proves the index is in bounds.
        unsafe { self.data.get_unchecked((index + 8) as usize) }
    }
}

impl IndexMut<isize> for Stack {
    fn index_mut(&mut self, index: isize) -> &mut Self::Output {
        debug_assert!(index + 8 >= 0 && ((index + 8) as usize) < MAX_PLY + 16);
        // SAFETY: see Index::index above.
        unsafe { self.data.get_unchecked_mut((index + 8) as usize) }
    }
}
