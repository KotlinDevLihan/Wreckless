use std::sync::atomic::{AtomicPtr, AtomicU8, AtomicU64, AtomicUsize, Ordering};

use crate::types::{Move, Score, is_decisive, is_loss, is_valid, is_win};

pub const DEFAULT_TT_SIZE: usize = 16;

const MEGABYTE: usize = 1024 * 1024;
const CLUSTER_SIZE: usize = std::mem::size_of::<Cluster>();

const ENTRIES_PER_CLUSTER: usize = 3;

const AGE_CYCLE: u8 = 1 << 5;
const AGE_MASK: u8 = AGE_CYCLE - 1;

const _: () = assert!(std::mem::size_of::<Cluster>() == 32);
const _: () = assert!(std::mem::size_of::<InternalEntry>() == 8);

#[derive(Copy, Clone)]
pub struct Entry {
    pub mv: Move,
    pub score: i32,
    pub raw_eval: i32,
    pub depth: i32,
    pub bound: Bound,
    pub tt_pv: bool,
}

#[derive(Copy, Clone)]
pub struct Flags {
    data: u8,
}

impl Flags {
    pub const fn new(bound: Bound, tt_pv: bool, age: u8) -> Self {
        debug_assert!(age <= AGE_MASK);

        Self { data: (bound as u8) | ((tt_pv as u8) << 2) | (age << 3) }
    }

    pub const fn bound(self) -> Bound {
        match self.data & 0b11 {
            0 => Bound::None,
            1 => Bound::Exact,
            2 => Bound::Lower,
            3 => Bound::Upper,
            _ => unreachable!(),
        }
    }

    pub const fn tt_pv(self) -> bool {
        (self.data & (1 << 2)) != 0
    }

    pub const fn age(self) -> u8 {
        self.data >> 3
    }
}

/// Type of the score returned by the search.
#[derive(Copy, Clone, Eq, PartialEq)]
pub enum Bound {
    None,
    Exact,
    Lower,
    Upper,
}

/// Internal representation of a transposition table entry (8 bytes).
#[derive(Clone, Copy)]
#[repr(C)]
pub struct InternalEntry {
    mv: Move,         // 2 bytes
    score: i16,       // 2 bytes
    raw_eval: i16,    // 2 bytes
    offset_depth: u8, // 1 byte
    flags: Flags,     // 1 byte
}

impl InternalEntry {
    const fn depth(&self) -> i32 {
        TtDepth::from_tt(self.offset_depth)
    }
}

pub enum TtDepth {}

impl TtDepth {
    pub const NONE: i32 = -2;
    pub const SOME: i32 = -1;

    const fn from_tt(offset_depth: u8) -> i32 {
        offset_depth as i32 - 2
    }

    fn to_tt(depth: i32) -> u8 {
        (depth + 2).clamp(u8::MIN as i32, u8::MAX as i32) as u8
    }
}

impl InternalEntry {
    pub const fn relative_age(&self, tt_age: u8) -> i32 {
        ((AGE_CYCLE + tt_age - self.flags.age()) & AGE_MASK) as i32
    }
}

#[repr(align(32))]
struct Cluster {
    // Atomic entries to prevent data tearing during concurrent write/probe
    entries: [AtomicU64; ENTRIES_PER_CLUSTER],
    // Packs all 3 entries' 16-bit verification keys into one word.
    keys: AtomicU64,
}

impl Cluster {
    fn key(&self, index: usize) -> u16 {
        verification_key(self.keys.load(Ordering::Acquire) >> (index * 16))
    }

    fn set_key(&self, index: usize, key: u16) {
        let mask = 0xFFFFu64 << (index * 16);
        let bits = (key as u64) << (index * 16);
        // Plain load/store, not `fetch_update`. The three verification keys share
        // one u64, so updating one is a read-modify-write -- but doing it as a CAS
        // put a `lock cmpxchg` LOOP on the TT write path, which runs at
        // essentially every node.
        //
        // What the CAS bought: if two threads write different entries of the same
        // cluster simultaneously, one key update can be lost, leaving an entry
        // whose key belongs to a previous occupant. What that costs: a later probe
        // matches the stale key and reads the new payload -- an entry from another
        // position accepted as a hit.
        //
        // That race already exists regardless, because key and payload are
        // separate atomics and a probe can interleave between them, so the CAS was
        // paying full price at every node to narrow one window of a race it could
        // not close. Every serious engine accepts this class of TT race; the
        // downstream guards are what make it safe -- a hit's move is checked with
        // `is_legal` before it is played, and the score only feeds pruning.
        let old = self.keys.load(Ordering::Relaxed);
        self.keys.store((old & !mask) | bits, Ordering::Release);
    }

    fn lookup_key(&self, key: u16) -> usize {
        let bits = 0x0001_0001_0001_0001;
        let needle = key as u64 * bits;
        let zeros = self.keys.load(Ordering::Acquire) ^ needle;
        let matches = zeros.wrapping_sub(bits) & !zeros & (bits << 15);
        (matches.trailing_zeros() / 16) as usize
    }

    fn read_entry(&self, index: usize) -> InternalEntry {
        unsafe { std::mem::transmute(self.entries[index].load(Ordering::Acquire)) }
    }

    fn write_entry(&self, index: usize, entry: InternalEntry) {
        self.entries[index].store(unsafe { std::mem::transmute(entry) }, Ordering::Release);
    }

    /// Updates only the 2-byte `mv` field of an entry in place, leaving
    /// score/raw_eval/depth/flags untouched. Unlike `write_entry`, this can't
    /// clobber those fields with a stale snapshot if another thread claims
    /// the slot for a different position between our read and this write.
    fn write_move(&self, index: usize, entry: InternalEntry) {
        let bits: u64 = unsafe { std::mem::transmute(entry) };
        let mv_bits = bits & 0xFFFF;
        // CAS, restored. The plain load/store form was not merely a narrower
        // window on an existing race -- it was a LOST UPDATE with a much worse
        // failure mode.
        //
        // `write_entry` may land between our load and our store. The plain form
        // then writes back the OLD entry with the new move patched in,
        // resurrecting a stale score, eval, depth and bound into a slot another
        // thread had just claimed for a different position. That is not a torn
        // read a legality check catches; it is a fully-formed entry that is wrong.
        //
        // The whole point of `write_move` is that it must not clobber the fields
        // it is not updating, which is precisely what a read-modify-write cannot
        // promise without the compare-exchange.
        let _ = self.entries[index]
            .fetch_update(Ordering::Release, Ordering::Acquire, |old| Some((old & !0xFFFFu64) | mv_bits));
    }
}

/// The transposition table is used to cache previously performed search results.
pub struct TranspositionTable {
    ptr: AtomicPtr<Cluster>,
    len: AtomicUsize,
    age: AtomicU8,
}

unsafe impl Sync for TranspositionTable {}

impl TranspositionTable {
    pub fn clear(&self, threads: usize) {
        unsafe { parallel_clear(threads, self.ptr(), self.len()) };
        self.age.store(0, Ordering::Relaxed);
    }

    pub fn resize(&self, threads: usize, megabytes: usize) {
        unsafe { deallocate(self.ptr(), self.len()) };

        let (new_ptr, new_len) = unsafe { allocate(threads, megabytes) };

        self.ptr.store(new_ptr, Ordering::Relaxed);
        self.len.store(new_len, Ordering::Relaxed);
        self.age.store(0, Ordering::Relaxed);
    }

    pub fn hashfull(&self) -> usize {
        let age = self.age();
        let clusters = unsafe { std::slice::from_raw_parts(self.ptr(), self.len()) };

        let mut count = 0;
        for cluster in clusters.iter().take(1000) {
            for i in 0..ENTRIES_PER_CLUSTER {
                let entry = cluster.read_entry(i);
                count += (entry.flags.bound() != Bound::None && entry.flags.age() == age) as usize;
            }
        }

        count / ENTRIES_PER_CLUSTER
    }

    pub fn increment_age(&self) {
        self.age.store((self.age() + 1) & AGE_MASK, Ordering::Relaxed);
    }

    pub fn read(&self, hash: u64, halfmove_clock: u8, ply: isize) -> Option<Entry> {
        let cluster = {
            let index = index(hash, self.len());
            unsafe { &*self.ptr().add(index) }
        };

        let key = verification_key(hash);
        let index = cluster.lookup_key(key);

        if index < ENTRIES_PER_CLUSTER {
            let entry = cluster.read_entry(index);

            // A never-written slot has key 0, so `lookup_key` matches it for any
            // position whose verification key is also 0 -- roughly 1 probe in
            // 65536 -- and returns an all-zero payload as a hit. That is not
            // harmless: `raw_eval == 0` passes `is_valid`, so the node skips the
            // network and evaluates the position as dead level, and in qsearch
            // `Bound::None` falls through to the permissive arm and returns 0 as
            // a cutoff.
            //
            // Distinguishable only because `TtDepth::NONE` is -2 rather than 0:
            // `to_tt` offsets by +2, so an untouched `offset_depth` of 0 decodes
            // to NONE and cannot be confused with the `TtDepth::SOME` (-1)
            // static-eval writes. The replacement scan in `write` already relies
            // on exactly this test; the probe path simply never used it.
            if entry.depth() == TtDepth::NONE {
                return None;
            }

            let hit = Entry {
                depth: entry.depth(),
                score: score_from_tt(entry.score as i32, ply, halfmove_clock),
                raw_eval: entry.raw_eval as i32,
                bound: entry.flags.bound(),
                tt_pv: entry.flags.tt_pv(),
                mv: entry.mv,
            };

            Some(hit)
        } else {
            None
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn write(
        &self, hash: u64, depth: i32, raw_eval: i32, mut score: i32, bound: Bound, mv: Move, ply: isize, tt_pv: bool,
        force: bool,
    ) {
        debug_assert!(depth != TtDepth::NONE);

        let cluster = {
            let index = index(hash, self.len());
            unsafe { &mut *self.ptr().add(index) }
        };

        let key = verification_key(hash);
        let tt_age = self.age();

        let replacement_index = {
            let lookup_index = cluster.lookup_key(key);
            if lookup_index < ENTRIES_PER_CLUSTER {
                lookup_index
            } else {
                let mut replacement_index = None;
                let mut lowest_quality = i32::MAX;

                for index in 0..ENTRIES_PER_CLUSTER {
                    let candidate = cluster.read_entry(index);
                    if candidate.depth() == TtDepth::NONE {
                        replacement_index = Some(index);
                        break;
                    }

                    let quality = candidate.depth() - 4 * candidate.relative_age(tt_age);
                    if quality < lowest_quality {
                        replacement_index = Some(index);
                        lowest_quality = quality;
                    }
                }

                replacement_index.unwrap()
            }
        };

        let entry_key = cluster.key(replacement_index);
        let mut entry = cluster.read_entry(replacement_index);

        let refreshed_move = !(entry_key == key && mv.is_null());
        if refreshed_move {
            entry.mv = mv;
        }

        // The age condition is load-bearing in BOTH directions.
        //
        // Dropping it looked like a fix -- `increment_age()` runs at the top of
        // every search, so on the first write of a new search every entry is the
        // "wrong" age and a deep entry from the previous move could lose its slot
        // to a shallow write. But the age term is also what REFRESHES an entry:
        // when the age differs, this guard falls through and the entry is
        // rewritten, stamping the current age on it.
        //
        // Without it, a deep entry blocks all writes indefinitely AND keeps its
        // stale age forever -- so `quality = depth - 4 * relative_age` decays on
        // every `increment_age()` until the replacement scan evicts it, even
        // though it is hot on the current search path. The entry is preserved
        // right up until it is thrown away.
        //
        // Rewriting once per age is the intended cost, and it is what keeps a
        // useful entry's age current.
        if !force
            && key == entry_key
            && depth + 4 + 2 * tt_pv as i32 <= entry.depth()
            && entry.flags.age() == tt_age
        {
            // Keep the existing deeper entry's score/depth/flags, but persist
            // the refreshed best move.
            //
            // Before the entries became atomic, `entry` was a `&mut` directly
            // into the cluster, so the `entry.mv = mv` above had already landed
            // in the table by the time this early return fired. It is now a
            // local copy, so returning here would silently drop the move
            // update. This path is taken whenever we re-reach a position we
            // already hold a deeper entry for -- common -- and the TT move is
            // tried first at every node, so losing those refreshes quietly
            // degrades move ordering everywhere.
            //
            // No `set_key` needed: this branch only runs when `key ==
            // entry_key`, so the verification key is already correct.
            if refreshed_move {
                cluster.write_move(replacement_index, entry);
            }
            return;
        }

        if is_decisive(score) && is_valid(score) {
            score += score.signum() * ply as i32;
        }

        entry.offset_depth = TtDepth::to_tt(depth);
        entry.score = score as i16;
        entry.raw_eval = raw_eval as i16;
        entry.flags = Flags::new(bound, tt_pv, tt_age);
        
        cluster.write_entry(replacement_index, entry);
        cluster.set_key(replacement_index, key);
    }

    pub fn prefetch(&self, hash: u64) {
        #[cfg(target_arch = "x86_64")]
        unsafe {
            use std::arch::x86_64::{_MM_HINT_T0, _mm_prefetch};

            let index = index(hash, self.len());
            let ptr = self.ptr().add(index).cast();
            _mm_prefetch::<_MM_HINT_T0>(ptr);
        }

        #[cfg(not(target_arch = "x86_64"))]
        let _ = hash;
    }

    fn age(&self) -> u8 {
        self.age.load(Ordering::Relaxed)
    }

    fn ptr(&self) -> *mut Cluster {
        self.ptr.load(Ordering::Relaxed)
    }

    fn len(&self) -> usize {
        self.len.load(Ordering::Relaxed)
    }
}

const fn index(hash: u64, len: usize) -> usize {
    (((hash as u128) * (len as u128)) >> 64) as usize
}

const fn verification_key(hash: u64) -> u16 {
    hash as u16
}

const fn score_from_tt(score: i32, ply: isize, halfmove_clock: u8) -> i32 {
    if score == Score::NONE {
        return Score::NONE;
    }

    if is_win(score) {
        if score >= Score::MATE_IN_MAX && Score::MATE - score > 100 - halfmove_clock as i32 {
            return Score::TB_WIN_IN_MAX - 1;
        }
        if Score::TB_WIN - score > 100 - halfmove_clock as i32 {
            return Score::TB_WIN_IN_MAX - 1;
        }
        return score - ply as i32;
    }

    if is_loss(score) {
        if score <= -Score::MATE_IN_MAX && Score::MATE + score > 100 - halfmove_clock as i32 {
            return -Score::TB_WIN_IN_MAX + 1;
        }
        if Score::TB_WIN + score > 100 - halfmove_clock as i32 {
            return -Score::TB_WIN_IN_MAX + 1;
        }
        return score + ply as i32;
    }

    score
}

impl Default for TranspositionTable {
    fn default() -> Self {
        let (ptr, len) = unsafe { allocate(1, DEFAULT_TT_SIZE) };
        Self {
            ptr: AtomicPtr::new(ptr),
            len: AtomicUsize::new(len),
            age: AtomicU8::new(0),
        }
    }
}

impl Drop for TranspositionTable {
    fn drop(&mut self) {
        unsafe { deallocate(self.ptr(), self.len()) };
    }
}

unsafe fn allocate(threads: usize, size_mb: usize) -> (*mut Cluster, usize) {
    #[cfg(target_os = "linux")]
    use libc::{MADV_HUGEPAGE, MAP_ANONYMOUS, MAP_PRIVATE, PROT_READ, PROT_WRITE, madvise, mmap};

    let size = size_mb * MEGABYTE;
    let len = size / CLUSTER_SIZE;

    // `Hash` accepts up to 262144 MB (256 GB), so a request the system cannot
    // satisfy is reachable from an ordinary `setoption` -- and it is usually the
    // GUI, not the user, that picks the number. Every branch here must report
    // failure rather than propagate a bad pointer into `parallel_clear`, which
    // writes across the whole allocation immediately.
    //
    // The Windows branch already asserted; the other two did not, so there an
    // over-large `Hash` crashed with no diagnostic. Note `mmap` reports failure
    // as MAP_FAILED (-1), not null, so a null check would not have caught it:
    // `madvise` was called on -1 and then `parallel_clear` wrote to it.
    #[cfg(target_os = "linux")]
    let ptr = {
        let ptr = mmap(std::ptr::null_mut(), size, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        assert!(ptr != libc::MAP_FAILED, "Failed to allocate {size_mb} MB of table memory");
        madvise(ptr, size, MADV_HUGEPAGE);
        ptr.cast()
    };

    #[cfg(target_os = "windows")]
    let ptr = windows::allocate(size).cast();

    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    let ptr = {
        let layout = std::alloc::Layout::from_size_align(size, std::mem::align_of::<Cluster>()).unwrap();
        let ptr = std::alloc::alloc_zeroed(layout);
        assert!(!ptr.is_null(), "Failed to allocate {size_mb} MB of table memory");
        ptr.cast()
    };

    unsafe { parallel_clear(threads, ptr, len) };
    (ptr, len)
}

unsafe fn deallocate(ptr: *mut Cluster, len: usize) {
    let size = len * CLUSTER_SIZE;

    #[cfg(target_os = "linux")]
    let _ = libc::munmap(ptr.cast(), size);

    #[cfg(target_os = "windows")]
    {
        let _ = size;
        windows::deallocate(ptr.cast());
    }

    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        let layout = std::alloc::Layout::from_size_align(size, std::mem::align_of::<Cluster>()).unwrap();
        std::alloc::dealloc(ptr.cast(), layout);
    }
}

#[cfg(target_os = "windows")]
pub(crate) mod windows {
    type Handle = *mut std::ffi::c_void;

    const MEM_COMMIT: u32 = 0x1000;
    const MEM_RESERVE: u32 = 0x2000;
    const MEM_RELEASE: u32 = 0x8000;
    const MEM_LARGE_PAGES: u32 = 0x20000000;
    const PAGE_READWRITE: u32 = 0x04;

    const TOKEN_QUERY: u32 = 0x0008;
    const TOKEN_ADJUST_PRIVILEGES: u32 = 0x0020;
    const SE_PRIVILEGE_ENABLED: u32 = 0x0002;

    #[repr(C)]
    struct Luid {
        low: u32,
        high: i32,
    }

    #[repr(C)]
    struct TokenPrivileges {
        count: u32,
        luid: Luid,
        attributes: u32,
    }

    unsafe extern "system" {
        fn GetCurrentProcess() -> Handle;
        fn GetLargePageMinimum() -> usize;
        fn GetLastError() -> u32;
        fn CloseHandle(handle: Handle) -> i32;
        fn VirtualAlloc(address: *mut std::ffi::c_void, size: usize, kind: u32, protect: u32) -> *mut std::ffi::c_void;
        fn VirtualFree(address: *mut std::ffi::c_void, size: usize, kind: u32) -> i32;
    }

    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn OpenProcessToken(process: Handle, access: u32, token: *mut Handle) -> i32;
        fn LookupPrivilegeValueW(system: *const u16, name: *const u16, luid: *mut Luid) -> i32;
        fn AdjustTokenPrivileges(
            token: Handle, disable_all: i32, state: *const TokenPrivileges, length: u32,
            previous: *mut TokenPrivileges, returned: *mut u32,
        ) -> i32;
    }

    /// Large pages or nothing. `None` means the privilege was unavailable or
    /// the reservation failed, and the caller should decide what to do rather
    /// than silently accept regular pages.
    pub fn allocate_large_only(size: usize) -> Option<*mut std::ffi::c_void> {
        allocate_large_pages(size)
    }

    pub fn allocate(size: usize) -> *mut std::ffi::c_void {
        if let Some(ptr) = allocate_large_pages(size) {
            return ptr;
        }

        let ptr = unsafe { VirtualAlloc(std::ptr::null_mut(), size, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE) };
        assert!(!ptr.is_null(), "Failed to allocate {} MB of table memory", size / (1024 * 1024));
        ptr
    }

    pub fn deallocate(ptr: *mut std::ffi::c_void) {
        unsafe { VirtualFree(ptr, 0, MEM_RELEASE) };
    }

    fn allocate_large_pages(size: usize) -> Option<*mut std::ffi::c_void> {
        unsafe {
            let page = GetLargePageMinimum();
            if page == 0 || !enable_lock_memory_privilege() {
                return None;
            }

            let size = size.div_ceil(page) * page;
            let flags = MEM_COMMIT | MEM_RESERVE | MEM_LARGE_PAGES;
            let ptr = VirtualAlloc(std::ptr::null_mut(), size, flags, PAGE_READWRITE);
            (!ptr.is_null()).then_some(ptr)
        }
    }

    unsafe fn enable_lock_memory_privilege() -> bool {
        unsafe {
            let mut token = std::ptr::null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY, &mut token) == 0 {
                return false;
            }

            let name: Vec<u16> = "SeLockMemoryPrivilege\0".encode_utf16().collect();
            let mut privileges = TokenPrivileges {
                count: 1,
                luid: Luid { low: 0, high: 0 },
                attributes: SE_PRIVILEGE_ENABLED,
            };

            let enabled = LookupPrivilegeValueW(std::ptr::null(), name.as_ptr(), &mut privileges.luid) != 0
                && AdjustTokenPrivileges(token, 0, &privileges, 0, std::ptr::null_mut(), std::ptr::null_mut()) != 0
                && GetLastError() == 0;

            CloseHandle(token);
            enabled
        }
    }
}

unsafe fn parallel_clear<T: std::marker::Send>(threads: usize, ptr: *mut T, len: usize) {
    #[cfg(not(target_arch = "wasm32"))]
    std::thread::scope(|scope| {
        let slice = std::slice::from_raw_parts_mut(ptr, len);

        let chunk_size = len.div_ceil(threads);
        for chunk in slice.chunks_mut(chunk_size) {
            scope.spawn(|| chunk.as_mut_ptr().write_bytes(0, chunk.len()));
        }
    });

    #[cfg(target_arch = "wasm32")]
    {
        let _ = threads;
        ptr.write_bytes(0, len);
    }
}