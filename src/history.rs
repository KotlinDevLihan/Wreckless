use std::sync::atomic::{AtomicI16, Ordering};

use crate::types::{Bitboard, Color, Move, Piece, PieceType, Square};

type FromToHistory<T> = [[T; 64]; 64];
type PieceToHistory<T> = [[T; 64]; 13];
type ContinuationHistoryType = [[[[PieceToHistory<AtomicI16>; 64]; 13]; 2]; 2];

struct HugeBox<T> {
    ptr: std::ptr::NonNull<T>,
}

unsafe impl<T: Send> Send for HugeBox<T> {}
unsafe impl<T: Sync> Sync for HugeBox<T> {}

impl<T> HugeBox<T> {
    fn new_zeroed() -> Self {
        #[cfg(target_os = "linux")]
        let ptr = unsafe {
            use libc::{MADV_HUGEPAGE, MAP_ANONYMOUS, MAP_FAILED, MAP_PRIVATE, PROT_READ, PROT_WRITE, madvise, mmap};
            let size = std::mem::size_of::<T>();
            assert!(size > 0, "HugeBox requires a non-zero-sized type");
            let p = mmap(std::ptr::null_mut(), size, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
            if p == MAP_FAILED {
                std::alloc::handle_alloc_error(std::alloc::Layout::new::<T>());
            }
            madvise(p, size, MADV_HUGEPAGE);
            std::ptr::NonNull::new_unchecked(p.cast::<T>())
        };

        // Windows: VirtualAlloc with 2 MB pages when the privilege allows it
        // (falls back to regular pages); memory is zeroed by the OS.
        #[cfg(target_os = "windows")]
        let ptr = unsafe {
            let size = std::mem::size_of::<T>();
            assert!(size > 0, "HugeBox requires a non-zero-sized type");
            std::ptr::NonNull::new_unchecked(crate::transposition::windows::allocate(size).cast::<T>())
        };

        #[cfg(not(any(target_os = "linux", target_os = "windows")))]
        let ptr = unsafe {
            let layout = std::alloc::Layout::new::<T>();
            let p = std::alloc::alloc_zeroed(layout);
            if p.is_null() {
                std::alloc::handle_alloc_error(layout);
            }
            std::ptr::NonNull::new_unchecked(p.cast::<T>())
        };

        HugeBox { ptr }
    }
}

impl<T> std::ops::Deref for HugeBox<T> {
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { self.ptr.as_ref() }
    }
}

impl<T> std::ops::DerefMut for HugeBox<T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { self.ptr.as_mut() }
    }
}

impl<T> Drop for HugeBox<T> {
    fn drop(&mut self) {
        #[cfg(target_os = "linux")]
        {
            let size = std::mem::size_of::<T>();
            assert!(size > 0, "HugeBox requires a non-zero-sized type");
            unsafe {
                libc::munmap(self.ptr.as_ptr().cast(), size);
            }
        }

        #[cfg(target_os = "windows")]
        crate::transposition::windows::deallocate(self.ptr.as_ptr().cast());

        #[cfg(not(any(target_os = "linux", target_os = "windows")))]
        unsafe {
            let layout = std::alloc::Layout::new::<T>();
            std::alloc::dealloc(self.ptr.as_ptr().cast(), layout);
        }
    }
}

fn apply_bonus<const MAX: i32>(entry: &mut i16, bonus: i32) {
    let bonus = bonus.clamp(-MAX, MAX);
    *entry += (bonus - bonus.abs() * (*entry) as i32 / MAX) as i16;
}

pub struct QuietHistory {
    // [side_to_move][from_threatened][to_threatened][from][to]
    entries: Box<[[[FromToHistory<i16>; 2]; 2]; 2]>,
}

impl QuietHistory {
    const MAX_HISTORY: i32 = 8192;

    pub fn get(&self, threats: Bitboard, stm: Color, mv: Move) -> i32 {
        self.entries[stm][threats.contains(mv.from()) as usize][threats.contains(mv.to()) as usize][mv.from()][mv.to()]
            as i32
    }

    pub fn update(&mut self, threats: Bitboard, stm: Color, mv: Move, bonus: i32) {
        let entry = &mut self.entries[stm][threats.contains(mv.from()) as usize][threats.contains(mv.to()) as usize]
            [mv.from()][mv.to()];
        apply_bonus::<{ Self::MAX_HISTORY }>(entry, bonus);
    }

    pub fn halve(&mut self) {
        for a in self.entries.iter_mut() {
            for b in a.iter_mut() {
                for c in b.iter_mut() {
                    for row in c.iter_mut() {
                        for entry in row.iter_mut() {
                            *entry /= 2;
                        }
                    }
                }
            }
        }
    }
}

impl Default for QuietHistory {
    fn default() -> Self {
        Self { entries: zeroed_box() }
    }
}

pub struct LowPlyHistory {
    // [ply][from][to]
    entries: Box<[FromToHistory<i16>; Self::MAX_LOW_PLY]>,
}

impl LowPlyHistory {
    pub const MAX_LOW_PLY: usize = 5;

    const MAX_HISTORY: i32 = 8192;

    pub fn get(&self, ply: usize, mv: Move) -> i32 {
        self.entries[ply][mv.from()][mv.to()] as i32
    }

    pub fn update(&mut self, ply: usize, mv: Move, bonus: i32) {
        let entry = &mut self.entries[ply][mv.from()][mv.to()];
        apply_bonus::<{ Self::MAX_HISTORY }>(entry, bonus);
    }

    /// Shifts entries down by 2 plies so that knowledge about moves near the
    /// previous root carries over into the next `go` command.
    pub fn shift(&mut self) {
        self.entries.rotate_left(2);
        for table in &mut self.entries[Self::MAX_LOW_PLY - 2..] {
            *table = [[0; 64]; 64];
        }
    }
}

impl Default for LowPlyHistory {
    fn default() -> Self {
        Self { entries: zeroed_box() }
    }
}

pub struct PawnHistory {
    // [pawn_key_bucket][piece][to]
    // Atomic so the table can be shared across search threads (racy-lossy
    // updates are fine for history data, as with the correction histories).
    entries: Box<[PieceToHistory<AtomicI16>; Self::SIZE]>,
}

impl PawnHistory {
    const MAX_HISTORY: i32 = 8192;

    const SIZE: usize = 512;
    const MASK: usize = Self::SIZE - 1;

    pub fn get(&self, pawn_key: u64, piece: Piece, to: Square) -> i32 {
        self.entries[pawn_key as usize & Self::MASK][piece][to].load(Ordering::Relaxed) as i32
    }

    pub fn update(&self, pawn_key: u64, piece: Piece, to: Square, bonus: i32) {
        let entry = &self.entries[pawn_key as usize & Self::MASK][piece][to];
        let bonus = bonus.clamp(-Self::MAX_HISTORY, Self::MAX_HISTORY);
        let current = entry.load(Ordering::Relaxed) as i32;
        entry.store((current + bonus - bonus.abs() * current / Self::MAX_HISTORY) as i16, Ordering::Relaxed);
    }

    pub fn clear(&self) {
        for bucket in self.entries.iter() {
            for entries in bucket.iter() {
                for entry in entries {
                    entry.store(0, Ordering::Relaxed);
                }
            }
        }
    }

    pub fn halve(&self) {
        for bucket in self.entries.iter() {
            for entries in bucket.iter() {
                for entry in entries {
                    let v = entry.load(Ordering::Relaxed);
                    entry.store(v / 2, Ordering::Relaxed);
                }
            }
        }
    }
}

impl Default for PawnHistory {
    fn default() -> Self {
        Self { entries: zeroed_box() }
    }
}

pub struct NoisyHistory {
    // [piece][to][captured_piece_type][to_threatened]
    entries: Box<PieceToHistory<[[i16; 2]; 7]>>,
}

impl NoisyHistory {
    const MAX_HISTORY: i32 = 12800;

    pub fn get(&self, threats: Bitboard, piece: Piece, sq: Square, captured: PieceType) -> i32 {
        self.entries[piece][sq][captured][threats.contains(sq) as usize] as i32
    }

    pub fn update(&mut self, threats: Bitboard, piece: Piece, sq: Square, captured: PieceType, bonus: i32) {
        let entry = &mut self.entries[piece][sq][captured][threats.contains(sq) as usize];
        apply_bonus::<{ Self::MAX_HISTORY }>(entry, bonus);
    }

    pub fn halve(&mut self) {
        for a in self.entries.iter_mut() {
            for b in a.iter_mut() {
                for row in b.iter_mut() {
                    for entry in row.iter_mut() {
                        *entry /= 2;
                    }
                }
            }
        }
    }
}

impl Default for NoisyHistory {
    fn default() -> Self {
        Self { entries: zeroed_box() }
    }
}

pub struct CorrectionHistory {
    // [bucket][side_to_move][key]
    entries: Box<[[[AtomicI16; Self::SIZE]; 2]; 16]>,
}

impl CorrectionHistory {
    const MAX_HISTORY: i32 = 14605;

    const SIZE: usize = 65536;
    const MASK: usize = Self::SIZE - 1;

    pub fn get(&self, stm: Color, key: u64, bucket: usize) -> i32 {
        self.entries[bucket][stm][key as usize & Self::MASK].load(Ordering::Relaxed) as i32
    }

    pub fn update(&self, stm: Color, key: u64, bucket: usize, bonus: i32) {
        let current = self.entries[bucket][stm][key as usize & Self::MASK].load(Ordering::Relaxed) as i32;
        let new = current + bonus - bonus.abs() * current / Self::MAX_HISTORY;
        self.entries[bucket][stm][key as usize & Self::MASK].store(new as i16, Ordering::Relaxed);
    }

    pub fn clear(&self) {
        for bucket in self.entries.iter() {
            for entries in bucket.iter() {
                for entry in entries {
                    entry.store(0, Ordering::Relaxed);
                }
            }
        }
    }
}

impl Default for CorrectionHistory {
    fn default() -> Self {
        Self { entries: zeroed_box() }
    }
}

pub struct ContinuationCorrectionHistory {
    // [in_check][capture][piece][to][piece][to]
    entries: HugeBox<ContinuationHistoryType>,
}

impl ContinuationCorrectionHistory {
    const MAX_HISTORY: i32 = 16418;

    /// Shared across search threads (atomic, racy-lossy updates), so `&self`
    /// suffices to hand out a subtable pointer.
    pub fn subtable_ptr(
        &self, in_check: bool, capture: bool, piece: Piece, to: Square,
    ) -> *mut PieceToHistory<AtomicI16> {
        (&raw const self.entries[in_check as usize][capture as usize][piece][to]).cast_mut()
    }

    pub fn get(&self, subtable_ptr: *mut PieceToHistory<AtomicI16>, piece: Piece, to: Square) -> i32 {
        unsafe { (&*subtable_ptr)[piece][to].load(Ordering::Relaxed) as i32 }
    }

    pub fn update(&self, subtable_ptr: *mut PieceToHistory<AtomicI16>, piece: Piece, to: Square, bonus: i32) {
        let entry = unsafe { &(&*subtable_ptr)[piece][to] };
        let bonus = bonus.clamp(-Self::MAX_HISTORY, Self::MAX_HISTORY);
        let current = entry.load(Ordering::Relaxed) as i32;
        entry.store((current + bonus - bonus.abs() * current / Self::MAX_HISTORY) as i16, Ordering::Relaxed);
    }

    pub fn clear(&self) {
        for a in self.entries.iter() {
            for b in a.iter() {
                for c in b.iter() {
                    for d in c.iter() {
                        for row in d.iter() {
                            for entry in row.iter() {
                                entry.store(0, Ordering::Relaxed);
                            }
                        }
                    }
                }
            }
        }
    }
}

impl Default for ContinuationCorrectionHistory {
    fn default() -> Self {
        Self { entries: HugeBox::new_zeroed() }
    }
}

pub struct ContinuationHistory {
    // [in_check][capture][piece][to][piece][to]
    entries: HugeBox<ContinuationHistoryType>,
}

impl ContinuationHistory {
    const MAX_HISTORY: i32 = 15320;

    /// Shared across search threads (atomic, racy-lossy updates), so `&self`
    /// suffices to hand out a subtable pointer.
    pub fn subtable_ptr(
        &self, in_check: bool, capture: bool, piece: Piece, to: Square,
    ) -> *mut PieceToHistory<AtomicI16> {
        (&raw const self.entries[in_check as usize][capture as usize][piece][to]).cast_mut()
    }

    pub fn get(&self, subtable_ptr: *mut PieceToHistory<AtomicI16>, piece: Piece, to: Square) -> i32 {
        (unsafe { &*subtable_ptr }[piece][to]).load(Ordering::Relaxed) as i32
    }

    pub fn update(&self, subtable_ptr: *mut PieceToHistory<AtomicI16>, piece: Piece, to: Square, bonus: i32) {
        let entry = unsafe { &(&*subtable_ptr)[piece][to] };
        let bonus = bonus.clamp(-Self::MAX_HISTORY, Self::MAX_HISTORY);
        let current = entry.load(Ordering::Relaxed) as i32;
        entry.store((current + bonus - bonus.abs() * current / Self::MAX_HISTORY) as i16, Ordering::Relaxed);
    }

    pub fn clear(&self) {
        for a in self.entries.iter() {
            for b in a.iter() {
                for c in b.iter() {
                    for d in c.iter() {
                        for row in d.iter() {
                            for entry in row.iter() {
                                entry.store(0, Ordering::Relaxed);
                            }
                        }
                    }
                }
            }
        }
    }
}

impl Default for ContinuationHistory {
    fn default() -> Self {
        Self { entries: HugeBox::new_zeroed() }
    }
}

fn zeroed_box<T>() -> Box<T> {
    unsafe {
        let layout = std::alloc::Layout::new::<T>();
        let ptr = std::alloc::alloc_zeroed(layout);
        if ptr.is_null() {
            std::alloc::handle_alloc_error(layout);
        }
        Box::<T>::from_raw(ptr.cast())
    }
}
