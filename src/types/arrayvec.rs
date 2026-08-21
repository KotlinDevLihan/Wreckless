use std::{mem::MaybeUninit, ops::Index};

use crate::types::MoveEntry;

#[derive(Clone)]
pub struct ArrayVec<T: Copy, const N: usize> {
    data: [MaybeUninit<T>; N],
    len: usize,
}

impl<T: Copy, const N: usize> ArrayVec<T, N> {
    pub const fn new() -> Self {
        Self { data: [const { MaybeUninit::uninit() }; N], len: 0 }
    }

    pub const fn len(&self) -> usize {
        self.len
    }

    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn get(&self, index: usize) -> &T {
        debug_assert!(index < self.len);

        unsafe { self.data.get_unchecked(index).assume_init_ref() }
    }

    /// Capacity, so callers can bound themselves against the real number rather
    /// than a literal that has to be kept in sync by hand.
    pub const CAPACITY: usize = N;

    pub fn push(&mut self, value: T) {
        debug_assert!(self.len < N);

        // Checked in release too, not just debug.
        //
        // The write below is `get_unchecked_mut`, so overflowing this is not a
        // panic -- it is an out-of-bounds write into whatever follows the array,
        // and `ArrayVec` is a stack local inside `search()`. The callers are
        // correct today (`move_count < 32` against a capacity of 32, with one
        // slot of margin), but that is two independent literals in two files
        // agreeing by hand; change either and the failure is silent memory
        // corruption rather than a dropped move.
        //
        // The branch costs a compare per pushed move -- once per move at a node,
        // against a `MovePicker` call and a `make_move` -- and buys the guarantee
        // that being wrong about the bound is survivable.
        if self.len >= N {
            return;
        }

        unsafe { self.data.get_unchecked_mut(self.len).write(value) };
        self.len += 1;
    }

    pub fn maybe_push(&mut self, mask: bool, value: T) {
        debug_assert!(self.len < N);

        // Same guard as `push`. Note this one writes even when `mask` is false --
        // only the length advance is conditional -- so a full buffer overflows
        // here on a push that was not even meant to be kept.
        if self.len >= N {
            return;
        }

        unsafe { self.data.get_unchecked_mut(self.len).write(value) };
        self.len += mask as usize;
    }

    pub const fn clear(&mut self) {
        self.len = 0;
    }

    pub fn swap_remove(&mut self, index: usize) -> T {
        // SAFETY: index < len is assumed by the caller, len - 1 < N always holds.
        unsafe {
            let value = self.data.get_unchecked(index).assume_init();
            self.len -= 1;
            std::ptr::copy(
                self.data.get_unchecked(self.len).as_ptr(),
                self.data.get_unchecked_mut(index).as_mut_ptr(),
                1,
            );

            value
        }
    }

    pub fn iter(&self) -> std::slice::Iter<'_, T> {
        // SAFETY: data[..len] is fully initialized
        unsafe { std::slice::from_raw_parts(self.data.as_ptr().cast(), self.len) }.iter()
    }

    pub fn iter_mut(&mut self) -> std::slice::IterMut<'_, T> {
        // SAFETY: data[..len] is fully initialized
        unsafe { std::slice::from_raw_parts_mut(self.data.as_mut_ptr().cast(), self.len) }.iter_mut()
    }

    #[allow(dead_code)]
    pub unsafe fn unchecked_write<F>(&mut self, op: F)
    where
        F: FnOnce(*mut T) -> usize,
    {
        self.len += op(self.data.get_unchecked_mut(self.len).as_mut_ptr());
    }
}

impl<const N: usize> ArrayVec<MoveEntry, N> {
    #[cfg(target_feature = "avx512vbmi2")]
    pub unsafe fn splat8(&mut self, mask: u32, vector: std::arch::x86_64::__m512i) {
        use std::arch::x86_64::*;

        let count = mask.count_ones() as usize;
        let to_write = _mm512_maskz_compress_epi16(mask, vector);
        let to_write0 = _mm512_cvtepi16_epi64(_mm512_castsi512_si128(to_write));
        _mm512_storeu_si512(self.data.get_unchecked_mut(self.len).as_mut_ptr().cast(), to_write0);
        self.len += count;
    }

    #[cfg(target_feature = "avx512vbmi2")]
    pub unsafe fn splat16(&mut self, mask: u32, vector: std::arch::x86_64::__m512i) {
        use std::arch::x86_64::*;

        let count = mask.count_ones() as usize;
        let to_write = _mm512_maskz_compress_epi16(mask, vector);
        let to_write0 = _mm512_cvtepi16_epi64(_mm512_castsi512_si128(to_write));
        let to_write1 = _mm512_cvtepi16_epi64(_mm512_extracti32x4_epi32::<1>(to_write));
        _mm512_storeu_si512(self.data.get_unchecked_mut(self.len).as_mut_ptr().cast(), to_write0);
        _mm512_storeu_si512(self.data.get_unchecked_mut(self.len + 8).as_mut_ptr().cast(), to_write1);
        self.len += count;
    }
}

impl<const N: usize, T: Copy> Index<usize> for ArrayVec<T, N> {
    type Output = T;

    fn index(&self, index: usize) -> &Self::Output {
        unsafe { &*self.data.get_unchecked(index).as_ptr() }
    }
}
