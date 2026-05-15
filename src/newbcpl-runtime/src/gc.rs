// Ported wholesale from NewCP. A few `pub(crate)` types are exposed
// through `pub` items whose callers won't exist until the rest of the
// runtime catches up (driver `dump-heap`, list / class allocation,
// finaliser registration). Silence rather than diverge from upstream.
#![allow(dead_code, private_interfaces)]

//! NewBCPL Garbage Collector — v2.
//!
//! Ported from NewCP's `gc.rs` (see
//! `../../../NewCP/NewCP/docs/garbage_collection.md` for the architecture
//! contract this file implements). Symbols are renamed to the
//! `__newbcpl_*` C-ABI prefix; the algorithm and on-wire layout are
//! unchanged so improvements made on either side can be ported with a
//! mechanical diff.
//!
//! Properties (in priority order):
//!   1. Correctness — no use-after-free, no missed roots, no stale
//!      `TypeDesc` dereferences.
//!   2. Multi-threaded mutators — every BCPL-managed thread can allocate
//!      concurrently. NewBCPL itself is single-threaded for now; the
//!      multi-thread machinery is bonus capacity inherited from NewCP.
//!   3. Stop-the-world collection — mutators cooperatively park while
//!      the collector marks and sweeps. No write barriers, no
//!      concurrent marking, no compaction.
//!   4. Transparent — every state change is observable through
//!      `gc::snapshot()` / `dump-gc`. Tests can pin behaviour.
//!   5. Robust against module retirement — TypeDescs stay alive while
//!      any heap block tags them. NewBCPL has only one effective
//!      "module" (the program), so this property is mostly latent here.
//!
//! The on-wire data layout (`BlockHeader`, `TypeDesc`, `ModuleDesc`)
//! is identical to NewCP's so JIT-emitted code on both sides shares the
//! same struct offsets.

use std::alloc::Layout;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, RwLock};
use std::thread::ThreadId;
use std::time::{Duration, Instant};

// ─────────────────────────────────────────────────────────────────────────────
// On-wire layout (frozen — JIT codegen depends on these byte offsets)
// ─────────────────────────────────────────────────────────────────────────────

/// Header prefixed to every block in a cluster (allocated *or* free).
///
/// 16 bytes, 16-aligned. JIT-emitted code locates the header by
/// subtracting `size_of::<BlockHeader>()` (= 16) from the payload pointer
/// it received from `__newbcpl_new_rec`.
///
/// `tag` packs the TypeDesc address with a single mark-bit in the LSB.
/// A `tag` whose value (mark bit cleared) is 0 denotes a **free** block;
/// the payload then begins with a `FreeBlockLink`.
///
/// `block_size` is the total block size in bytes including this header.
/// Required to walk the cluster linearly during sweep.
#[repr(C)]
pub struct BlockHeader {
    pub tag: usize,
    pub block_size: usize,
}

impl BlockHeader {
    const MARK_BIT: usize = 1;

    #[inline]
    pub fn is_marked(&self) -> bool {
        self.tag & Self::MARK_BIT != 0
    }
    #[inline]
    pub fn set_mark(&mut self) {
        self.tag |= Self::MARK_BIT;
    }
    #[inline]
    pub fn clear_mark(&mut self) {
        self.tag &= !Self::MARK_BIT;
    }
    #[inline]
    pub fn type_desc(&self) -> *const TypeDesc {
        (self.tag & !Self::MARK_BIT) as *const TypeDesc
    }
    #[inline]
    pub fn is_free(&self) -> bool {
        (self.tag & !Self::MARK_BIT) == 0
    }
}

pub type Finalizer = unsafe extern "C-unwind" fn(*mut u8);

/// Runtime type descriptor emitted by `newbcpl-llvm` for every heap-
/// allocated record type.  Layout-frozen.
///
/// `vtable` / `vtable_len` are populated for forward compatibility
/// with TypeDesc-routed dispatch; the live dispatch path today uses
/// the *inline* vtable pointer the JIT installs at offset 0 of every
/// instance. Name-keyed dynamic dispatch
/// (`__newbcpl_lookup_method`) keys off that inline pointer through a
/// process-global registry — see `brk::register_jit_vtable_methods`.
#[repr(C)]
pub struct TypeDesc {
    pub size: isize,
    pub module: *const ModuleDesc,
    pub finalizer: Option<Finalizer>,
    pub base: *const TypeDesc,
    pub vtable: *const *const (),
    pub vtable_len: u64,
    pub name: *const u32,
    pub ptroffs: [isize; 0],
}

unsafe impl Sync for TypeDesc {}
unsafe impl Send for TypeDesc {}

impl TypeDesc {
    /// Iterator over the non-negative pointer offsets in `ptroffs`.
    /// The array is sentinel-terminated by the first negative entry.
    ///
    /// # Safety
    /// `ptroffs` must remain valid for the lifetime of the iterator.
    pub unsafe fn pointer_offsets(&self) -> impl Iterator<Item = isize> {
        let mut idx = 0usize;
        let base = self.ptroffs.as_ptr();
        std::iter::from_fn(move || unsafe {
            let offset = *base.add(idx);
            if offset < 0 {
                None
            } else {
                idx += 1;
                Some(offset)
            }
        })
    }
}

/// Module-level root metadata.  Layout-frozen.
#[repr(C)]
pub struct ModuleDesc {
    pub var_base: *const u8,
    pub ptrs: *const isize,
    pub next: *const ModuleDesc,
}

unsafe impl Sync for ModuleDesc {}
unsafe impl Send for ModuleDesc {}

// ─────────────────────────────────────────────────────────────────────────────
// Counters (process-global, atomic, always on)
// ─────────────────────────────────────────────────────────────────────────────

pub(crate) struct HeapCounters {
    pub(crate) alloc_blocks_lifetime: AtomicU64,
    pub(crate) alloc_bytes_lifetime: AtomicU64,
    pub(crate) free_blocks_lifetime: AtomicU64,
    pub(crate) free_bytes_lifetime: AtomicU64,
    pub(crate) bump_path_blocks: AtomicU64,
    pub(crate) free_list_path_blocks: AtomicU64,
    pub(crate) grow_events: AtomicU64,
    pub(crate) collect_cycles: AtomicU64,
    pub(crate) collect_total_nanos: AtomicU64,
    pub(crate) collect_last_nanos: AtomicU64,
    pub(crate) collect_last_reclaimed_bytes: AtomicU64,
    pub(crate) live_blocks: AtomicU64,
    pub(crate) live_bytes: AtomicU64,
    pub(crate) cluster_count: AtomicU64,
    pub(crate) module_root_count: AtomicU64,
    pub(crate) peak_live_bytes: AtomicU64,
    pub(crate) registered_threads: AtomicU64,
    pub(crate) safepoint_waits_total_nanos: AtomicU64,
    /// Bytes allocated since the last collect cycle. The
    /// allocator drops this to zero each time it triggers a
    /// collect; allocations between collects accumulate here.
    pub(crate) alloc_bytes_since_collect: AtomicU64,
    /// When `alloc_bytes_since_collect` would exceed this value
    /// on the next allocation, the allocator triggers a collect.
    /// Initialised to `INITIAL_COLLECT_THRESHOLD`; after each
    /// cycle it adapts to `max(INITIAL, live_bytes * GROWTH)` so
    /// cycle cost amortises to a fixed fraction of allocation
    /// cost. Threshold = 0 disables the auto-trigger entirely
    /// (used by `disable_auto_collect_for_test`).
    pub(crate) collect_threshold: AtomicU64,
}

/// Initial allocator-trigger threshold. Picked so that small
/// programs (Hello World, the corpus tests) never run a single
/// cycle, but a long-running mutator that allocates in a loop
/// will collect after a few MB. Tunable; production GCs typically
/// pick something in the 1–8 MB range as the floor.
const INITIAL_COLLECT_THRESHOLD: u64 = 4 * 1024 * 1024;

/// Threshold growth factor applied after each collect:
///   new_threshold = max(INITIAL, live_bytes * GROWTH_FACTOR).
/// Standard amortisation argument: every byte allocated pays for
/// at most `1/(GROWTH_FACTOR - 1)` bytes of marking, so collection
/// cost is a constant fraction of allocation cost. 2.0 is the
/// classical choice (HotSpot, Go, Boehm all sit around here).
const COLLECT_GROWTH_FACTOR: u64 = 2;

impl HeapCounters {
    const fn zeroed() -> Self {
        Self {
            alloc_blocks_lifetime: AtomicU64::new(0),
            alloc_bytes_lifetime: AtomicU64::new(0),
            free_blocks_lifetime: AtomicU64::new(0),
            free_bytes_lifetime: AtomicU64::new(0),
            bump_path_blocks: AtomicU64::new(0),
            free_list_path_blocks: AtomicU64::new(0),
            grow_events: AtomicU64::new(0),
            collect_cycles: AtomicU64::new(0),
            collect_total_nanos: AtomicU64::new(0),
            collect_last_nanos: AtomicU64::new(0),
            collect_last_reclaimed_bytes: AtomicU64::new(0),
            live_blocks: AtomicU64::new(0),
            live_bytes: AtomicU64::new(0),
            cluster_count: AtomicU64::new(0),
            module_root_count: AtomicU64::new(0),
            peak_live_bytes: AtomicU64::new(0),
            registered_threads: AtomicU64::new(0),
            safepoint_waits_total_nanos: AtomicU64::new(0),
            alloc_bytes_since_collect: AtomicU64::new(0),
            collect_threshold: AtomicU64::new(INITIAL_COLLECT_THRESHOLD),
        }
    }

    pub(crate) fn reset(&self) {
        for slot in [
            &self.alloc_blocks_lifetime,
            &self.alloc_bytes_lifetime,
            &self.free_blocks_lifetime,
            &self.free_bytes_lifetime,
            &self.bump_path_blocks,
            &self.free_list_path_blocks,
            &self.grow_events,
            &self.collect_cycles,
            &self.collect_total_nanos,
            &self.collect_last_nanos,
            &self.collect_last_reclaimed_bytes,
            &self.live_blocks,
            &self.live_bytes,
            &self.cluster_count,
            &self.module_root_count,
            &self.peak_live_bytes,
            &self.registered_threads,
            &self.safepoint_waits_total_nanos,
            &self.alloc_bytes_since_collect,
        ] {
            slot.store(0, Ordering::Relaxed);
        }
        // Reset the threshold to its initial value too — without
        // this, tests that reset counters and then run small
        // workloads would sit forever at whatever the previous
        // run grew the threshold to.
        self.collect_threshold
            .store(INITIAL_COLLECT_THRESHOLD, Ordering::Relaxed);
    }
}

pub(crate) static HEAP_COUNTERS: HeapCounters = HeapCounters::zeroed();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AllocPath {
    Bump,
    FreeList,
}

// ─────────────────────────────────────────────────────────────────────────────
// Cluster — one OS-allocation chunk subdivided into blocks
// ─────────────────────────────────────────────────────────────────────────────

const DEFAULT_CLUSTER_SIZE: usize = 1 << 20; // 1 MiB
const BLOCK_ALIGN: usize = 16;
const MIN_BLOCK: usize = 32;

/// Free-block payload prefix (singly-linked list within a cluster).
/// `next` points to the **header** of the next free block, or null.
#[repr(C)]
struct FreeBlockLink {
    next: *mut u8,
}

pub(crate) struct Cluster {
    pub(crate) base: *mut u8,
    pub(crate) size: usize,
    pub(crate) bump: usize,
    pub(crate) free_list: *mut u8,
    layout: Layout,
    /// Bit `i` set iff a block begins at `i * BLOCK_ALIGN` from `base`.
    pub(crate) block_starts: Vec<u64>,
}

unsafe impl Send for Cluster {}

impl Drop for Cluster {
    fn drop(&mut self) {
        // SAFETY: `base` was allocated with `layout`. v2 never frees a
        // cluster except at process exit, but Drop must be safe in
        // any case (including reset_for_test).
        unsafe { std::alloc::dealloc(self.base, self.layout) };
    }
}

impl Cluster {
    fn new(min_size: usize) -> Self {
        let size = min_size.max(DEFAULT_CLUSTER_SIZE);
        let layout = Layout::from_size_align(size, BLOCK_ALIGN).unwrap();
        let base = unsafe { std::alloc::alloc_zeroed(layout) };
        if base.is_null() {
            std::alloc::handle_alloc_error(layout);
        }
        let bits = size / BLOCK_ALIGN;
        let words = (bits + 63) / 64;
        Self {
            base,
            size,
            bump: 0,
            free_list: std::ptr::null_mut(),
            layout,
            block_starts: vec![0u64; words],
        }
    }

    #[inline]
    fn contains(&self, addr: usize) -> bool {
        let base = self.base as usize;
        addr >= base && addr < base + self.bump
    }

    #[inline]
    fn mark_block_start(&mut self, offset: usize) {
        debug_assert!(offset % BLOCK_ALIGN == 0);
        let bit = offset / BLOCK_ALIGN;
        self.block_starts[bit / 64] |= 1u64 << (bit % 64);
    }

    #[inline]
    fn clear_block_start(&mut self, offset: usize) {
        debug_assert!(offset % BLOCK_ALIGN == 0);
        let bit = offset / BLOCK_ALIGN;
        self.block_starts[bit / 64] &= !(1u64 << (bit % 64));
    }

    fn block_start_at_or_below(&self, offset: usize) -> Option<usize> {
        if offset >= self.bump {
            return self.block_start_at_or_below(self.bump.saturating_sub(BLOCK_ALIGN));
        }
        let bit = offset / BLOCK_ALIGN;
        let word_idx = bit / 64;
        let bit_in_word = bit % 64;
        let mask = if bit_in_word == 63 {
            !0u64
        } else {
            (1u64 << (bit_in_word + 1)) - 1
        };
        let first = self.block_starts[word_idx] & mask;
        if first != 0 {
            let top = 63 - first.leading_zeros() as usize;
            return Some((word_idx * 64 + top) * BLOCK_ALIGN);
        }
        for w in (0..word_idx).rev() {
            let word = self.block_starts[w];
            if word != 0 {
                let top = 63 - word.leading_zeros() as usize;
                return Some((w * 64 + top) * BLOCK_ALIGN);
            }
        }
        None
    }

    /// Try to satisfy a `total_size`-byte allocation.  Returns the
    /// **header** pointer plus the path.  Payload is zeroed before
    /// return.
    ///
    /// # Safety
    /// `total_size` must be a multiple of `BLOCK_ALIGN` and ≥ MIN_BLOCK.
    unsafe fn try_alloc(&mut self, total_size: usize) -> Option<(*mut u8, AllocPath)> {
        let header_size = std::mem::size_of::<BlockHeader>();

        // Free-list (first-fit).
        unsafe {
            let mut prev_link: *mut *mut u8 = &mut self.free_list;
            while !(*prev_link).is_null() {
                let block = *prev_link;
                let block_size = (*(block as *const BlockHeader)).block_size;
                let next_link = block.add(header_size) as *mut *mut u8;
                if block_size >= total_size {
                    *prev_link = *next_link;
                    let leftover = block_size - total_size;
                    if leftover >= MIN_BLOCK {
                        let split = block.add(total_size);
                        let split_offset = (split as usize) - (self.base as usize);
                        let split_hdr = split as *mut BlockHeader;
                        (*split_hdr).tag = 0;
                        (*split_hdr).block_size = leftover;
                        let split_link = split.add(header_size) as *mut FreeBlockLink;
                        split_link.write(FreeBlockLink { next: self.free_list });
                        self.free_list = split;
                        self.mark_block_start(split_offset);
                        (*(block as *mut BlockHeader)).block_size = total_size;
                    }
                    let final_size = (*(block as *const BlockHeader)).block_size;
                    let payload = block.add(header_size);
                    std::ptr::write_bytes(payload, 0, final_size - header_size);
                    return Some((block, AllocPath::FreeList));
                }
                prev_link = next_link;
            }
        }

        // Bump from cluster tail.
        if self.bump.checked_add(total_size)? <= self.size {
            let block_offset = self.bump;
            let block = unsafe { self.base.add(block_offset) };
            self.bump += total_size;
            unsafe {
                let hdr = block as *mut BlockHeader;
                (*hdr).block_size = total_size;
            }
            self.mark_block_start(block_offset);
            return Some((block, AllocPath::Bump));
        }

        None
    }

    /// Resolve an arbitrary address to the payload start of the block
    /// containing it, or `None`.  Used by the conservative stack scan.
    unsafe fn resolve(&self, addr: usize) -> Option<*const u8> {
        if !self.contains(addr) {
            return None;
        }
        let header_size = std::mem::size_of::<BlockHeader>();
        let base = self.base as usize;
        let offset_in_cluster = addr - base;
        let block_offset = self.block_start_at_or_below(offset_in_cluster)?;
        unsafe {
            let block = self.base.add(block_offset);
            let hdr = block as *const BlockHeader;
            let block_size = (*hdr).block_size;
            if block_size < MIN_BLOCK || block_offset + block_size > self.bump {
                return None;
            }
            let block_end = base + block_offset + block_size;
            if addr >= block_end {
                return None;
            }
            let type_bits = (*hdr).tag & !BlockHeader::MARK_BIT;
            if type_bits == 0 {
                return None;
            }
            let payload_start = base + block_offset + header_size;
            if addr < payload_start {
                return None;
            }
            Some(payload_start as *const u8)
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// TypeDesc registry — pinned-while-blocks-reference
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub(crate) struct TypeDescEntry {
    pub addr: usize,
    pub block_count: u64,
    pub owner_module: Option<String>,
    pub size_bytes: isize,
}

pub(crate) struct TypeDescRegistry {
    by_addr: HashMap<usize, TypeDescEntry>,
}

impl TypeDescRegistry {
    fn new() -> Self {
        Self { by_addr: HashMap::new() }
    }

    fn record(&mut self, td: usize, owner_module: Option<String>) {
        let size_bytes = if td == 0 {
            0
        } else {
            unsafe { (*(td as *const TypeDesc)).size }
        };
        self.by_addr.entry(td).or_insert(TypeDescEntry {
            addr: td,
            block_count: 0,
            owner_module,
            size_bytes,
        });
    }

    /// Increment `block_count` for `td`. Auto-records the entry if
    /// it's the first time we've seen this address (codegen paths
    /// that bypass `__newbcpl_register_type` still get accounted).
    fn inc(&mut self, td: usize) {
        let entry = self.by_addr.entry(td).or_insert_with(|| TypeDescEntry {
            addr: td,
            block_count: 0,
            owner_module: None,
            size_bytes: if td == 0 { 0 } else { unsafe { (*(td as *const TypeDesc)).size } },
        });
        entry.block_count = entry.block_count.saturating_add(1);
    }

    fn dec(&mut self, td: usize) {
        if let Some(entry) = self.by_addr.get_mut(&td) {
            if entry.block_count > 0 {
                entry.block_count -= 1;
            }
        }
    }

    pub(crate) fn snapshot(&self) -> Vec<TypeDescEntry> {
        let mut out: Vec<_> = self.by_addr.values().cloned().collect();
        out.sort_by_key(|e| e.addr);
        out
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Module roots
// ─────────────────────────────────────────────────────────────────────────────

pub(crate) struct ModuleRoots {
    pub(crate) name: String,
    pub(crate) var_base: *const u8,
    pub(crate) offsets: Vec<isize>,
}

unsafe impl Send for ModuleRoots {}

// ─────────────────────────────────────────────────────────────────────────────
// Mutator threads
// ─────────────────────────────────────────────────────────────────────────────

const STATE_RUNNING: u8 = 0;
const STATE_PARK_REQUESTED: u8 = 1;
const STATE_PARKED: u8 = 2;

pub(crate) struct Mutator {
    pub(crate) thread_id: ThreadId,
    pub(crate) stack_top: usize,
    pub(crate) state: AtomicU8,
    pub(crate) parked_sp: AtomicUsize,
    pub(crate) spill: std::sync::Mutex<[usize; 16]>,

    pub(crate) alloc_blocks_lifetime: AtomicU64,
    pub(crate) alloc_bytes_lifetime: AtomicU64,
    pub(crate) park_count: AtomicU64,
}

// ─────────────────────────────────────────────────────────────────────────────
// Allocation event log (ring buffer, optional)
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub(crate) struct CollectRecord {
    pub generation: u64,
    pub elapsed_nanos: u64,
    pub mutators_parked: u64,
    pub roots_marked: u64,
    pub blocks_freed: u64,
    pub bytes_freed: u64,
    pub bytes_live_after: u64,
}

pub(crate) struct CollectLog {
    capacity: usize,
    entries: std::collections::VecDeque<CollectRecord>,
}

impl CollectLog {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            entries: std::collections::VecDeque::with_capacity(capacity),
        }
    }
    fn push(&mut self, rec: CollectRecord) {
        if self.entries.len() == self.capacity {
            self.entries.pop_front();
        }
        self.entries.push_back(rec);
    }
    pub(crate) fn snapshot(&self) -> Vec<CollectRecord> {
        self.entries.iter().cloned().collect()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Heap state
// ─────────────────────────────────────────────────────────────────────────────

pub struct Heap {
    pub(crate) clusters: Vec<Cluster>,
    pub(crate) modules: Vec<ModuleRoots>,
    pub(crate) type_descs: TypeDescRegistry,
    pub(crate) collect_log: CollectLog,
    pub(crate) generation: u64,
    /// Pending finalizers queued by the most recent sweep.  Populated
    /// during `run_collect_cycle`, drained by `collect_stw` once
    /// SAFEPOINT_REQUESTED has been cleared.
    pub(crate) pending_finalizers: Vec<(Finalizer, *mut u8)>,
}

// Heap is moved between threads only inside the global Mutex; the
// raw pointers in `pending_finalizers` reference cluster-owned
// payload memory and are only consumed on the same thread that
// queued them.  Marking the struct Send is sound under those rules.
unsafe impl Send for Heap {}

static HEAP_CELL: OnceLock<Mutex<Heap>> = OnceLock::new();

fn heap_lock() -> std::sync::MutexGuard<'static, Heap> {
    HEAP_CELL
        .get_or_init(|| {
            Mutex::new(Heap {
                clusters: Vec::new(),
                modules: Vec::new(),
                type_descs: TypeDescRegistry::new(),
                collect_log: CollectLog::new(16),
                generation: 0,
                pending_finalizers: Vec::new(),
            })
        })
        .lock()
        .unwrap()
}

static MUTATORS: RwLock<Vec<Arc<Mutator>>> = RwLock::new(Vec::new());

/// Global cooperative-park flag.  Mutators check this at safepoints
/// (currently: implicit at every allocation slow-path).  Once the
/// collector is done, it clears the flag and notifies the condvar.
static SAFEPOINT_REQUESTED: AtomicU8 = AtomicU8::new(0);
static SAFEPOINT_CONDVAR: Condvar = Condvar::new();
static SAFEPOINT_LOCK: Mutex<()> = Mutex::new(());

/// Initial-thread bootstrap stack base.  Set by `__newbcpl_init_gc`.
/// The bootstrap thread is auto-registered as a mutator if it later
/// allocates without explicit `__newbcpl_register_thread`.
static BOOTSTRAP_STACK_BASE: AtomicUsize = AtomicUsize::new(0);

/// TLS guard whose `Drop` removes the calling thread's `Mutator`
/// from the global registry.  Without this, a dead thread leaves
/// a `Mutator` entry behind whose state never updates, and
/// `collect_stw` would wait the full safepoint timeout for it
/// to "park" before aborting.
struct MutatorTls {
    handle: std::cell::RefCell<Option<Arc<Mutator>>>,
}

impl Drop for MutatorTls {
    fn drop(&mut self) {
        if let Some(m) = self.handle.borrow().clone() {
            let id = m.thread_id;
            // SAFEPOINT_LOCK: notify in case a collector is actively
            // waiting for this thread to park; an exiting thread is
            // effectively parked (it can't run any more user code).
            m.state.store(STATE_PARKED, Ordering::SeqCst);
            let mut threads = MUTATORS.write().unwrap();
            threads.retain(|x| x.thread_id != id);
            HEAP_COUNTERS
                .registered_threads
                .store(threads.len() as u64, Ordering::Relaxed);
            drop(threads);
            // Wake any collector that might be waiting.
            let _g = SAFEPOINT_LOCK.lock().unwrap();
            SAFEPOINT_CONDVAR.notify_all();
        }
    }
}

thread_local! {
    static MUTATOR_HANDLE: MutatorTls = MutatorTls {
        handle: std::cell::RefCell::new(None),
    };
}

fn ensure_mutator_for_current_thread() -> Arc<Mutator> {
    if let Some(m) = MUTATOR_HANDLE.with(|tls| tls.handle.borrow().clone()) {
        return m;
    }
    // Auto-register using the bootstrap stack base.  The exact
    // top-of-stack matters less than that the scan covers every
    // currently-live frame; bootstrap is set high enough to
    // include any frame on this thread.
    let stack_top = BOOTSTRAP_STACK_BASE.load(Ordering::Acquire);
    register_thread_inner(stack_top, /* explicit */ false)
}

fn register_thread_inner(stack_top: usize, _explicit: bool) -> Arc<Mutator> {
    let mutator = Arc::new(Mutator {
        thread_id: std::thread::current().id(),
        stack_top,
        state: AtomicU8::new(STATE_RUNNING),
        parked_sp: AtomicUsize::new(0),
        spill: Mutex::new([0usize; 16]),
        alloc_blocks_lifetime: AtomicU64::new(0),
        alloc_bytes_lifetime: AtomicU64::new(0),
        park_count: AtomicU64::new(0),
    });
    {
        let mut threads = MUTATORS.write().unwrap();
        threads.push(mutator.clone());
        HEAP_COUNTERS
            .registered_threads
            .store(threads.len() as u64, Ordering::Relaxed);
    }
    MUTATOR_HANDLE.with(|tls| {
        *tls.handle.borrow_mut() = Some(mutator.clone());
    });
    mutator
}

// ─────────────────────────────────────────────────────────────────────────────
// JIT-callable exports
// ─────────────────────────────────────────────────────────────────────────────

/// Initialise the GC and record the *bootstrap* stack base.  Idempotent;
/// only the first call has effect.  Called once from runtime startup
/// on the bootstrap thread.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn __newbcpl_init_gc(base_stack: *const u8) {
    let prev = BOOTSTRAP_STACK_BASE.load(Ordering::Acquire);
    if prev == 0 {
        BOOTSTRAP_STACK_BASE.store(base_stack as usize, Ordering::Release);
    }
    // Auto-register the bootstrap thread.
    ensure_mutator_for_current_thread();
}

/// Explicitly register the calling thread as a CP mutator.  Required
/// by any thread that wants to run CP code; auto-called by the alloc
/// path if the caller didn't, using the bootstrap stack base as a
/// fallback (suitable for single-thread cases).
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn __newbcpl_register_thread(stack_top: *const u8) {
    let _ = register_thread_inner(stack_top as usize, true);
}

/// Unregister the calling thread.  Safe to call multiple times.
/// The TLS guard's `Drop` runs the same cleanup at thread exit, so
/// explicitly calling this is optional.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn __newbcpl_unregister_thread() {
    let id = std::thread::current().id();
    let mut threads = MUTATORS.write().unwrap();
    threads.retain(|m| m.thread_id != id);
    HEAP_COUNTERS
        .registered_threads
        .store(threads.len() as u64, Ordering::Relaxed);
    MUTATOR_HANDLE.with(|tls| {
        *tls.handle.borrow_mut() = None;
    });
}

/// Register a module's global roots.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn __newbcpl_register_module(
    var_base: *const u8,
    offsets_ptr: *const isize,
    count: usize,
) {
    let offsets = unsafe { std::slice::from_raw_parts(offsets_ptr, count).to_vec() };
    let mut heap = heap_lock();
    heap.modules.push(ModuleRoots {
        name: "<unnamed>".to_string(),
        var_base,
        offsets,
    });
    HEAP_COUNTERS
        .module_root_count
        .store(heap.modules.len() as u64, Ordering::Relaxed);
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn __newbcpl_register_module_named(
    name_utf8: *const u8,
    name_len: usize,
    var_base: *const u8,
    offsets_ptr: *const isize,
    count: usize,
) {
    let name = if name_utf8.is_null() || name_len == 0 {
        "<unnamed>".to_string()
    } else {
        let bytes = unsafe { std::slice::from_raw_parts(name_utf8, name_len) };
        std::str::from_utf8(bytes)
            .map(str::to_string)
            .unwrap_or_else(|_| "<invalid-utf8>".to_string())
    };
    let offsets = unsafe { std::slice::from_raw_parts(offsets_ptr, count).to_vec() };
    let mut heap = heap_lock();
    heap.modules.push(ModuleRoots {
        name,
        var_base,
        offsets,
    });
    HEAP_COUNTERS
        .module_root_count
        .store(heap.modules.len() as u64, Ordering::Relaxed);
}

/// Allocate and zero-initialise a heap-tracked record.  The single
/// allocation entry point for managed CP code.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn __newbcpl_new_rec(tag: *const TypeDesc) -> *mut u8 {
    // Allocation is implicitly a safepoint: poll before doing the work
    // so a concurrent collector can pause us cleanly.  Without this
    // poll, a tight loop of allocations that all hit the TLAB / cluster
    // bump fast-path could prevent the collector from making progress.
    if SAFEPOINT_REQUESTED.load(Ordering::Acquire) != 0 {
        park_self_for_safepoint();
    }

    let mut spill_buf = [0usize; 16];
    let sp = capture_sp(&mut spill_buf);

    let mutator = ensure_mutator_for_current_thread();
    let payload_size = unsafe { (*tag).size as usize };
    let total_size = total_block_size(payload_size);
    let header_size = std::mem::size_of::<BlockHeader>();

    // Threshold-based auto-collect. We collect *before* the
    // allocation so the new block can't be reclaimed by the
    // sweep that fires (no root references it yet — the JIT'd
    // caller hasn't received the pointer). After the cycle we
    // re-baseline the threshold to `live_bytes * GROWTH` so
    // collection cost amortises.
    let threshold = HEAP_COUNTERS.collect_threshold.load(Ordering::Acquire);
    if threshold != 0 {
        let bytes_since_load =
            HEAP_COUNTERS.alloc_bytes_since_collect.load(Ordering::Acquire);
        if bytes_since_load.saturating_add(total_size as u64) >= threshold {
            collect_stw(&mutator, sp, &spill_buf);
            HEAP_COUNTERS
                .alloc_bytes_since_collect
                .store(0, Ordering::Release);
            let live = HEAP_COUNTERS.live_bytes.load(Ordering::Acquire);
            let new_threshold = std::cmp::max(
                INITIAL_COLLECT_THRESHOLD,
                live.saturating_mul(COLLECT_GROWTH_FACTOR),
            );
            HEAP_COUNTERS
                .collect_threshold
                .store(new_threshold, Ordering::Release);
        }
    }

    let block = alloc_under_lock(total_size, &mutator, sp, &spill_buf);

    unsafe {
        let hdr = block as *mut BlockHeader;
        (*hdr).tag = tag as usize;
        (*hdr).block_size = total_size;
        let mut heap = heap_lock();
        heap.type_descs.inc(tag as usize);
    }

    HEAP_COUNTERS.alloc_blocks_lifetime.fetch_add(1, Ordering::Relaxed);
    HEAP_COUNTERS
        .alloc_bytes_lifetime
        .fetch_add(total_size as u64, Ordering::Relaxed);
    HEAP_COUNTERS
        .alloc_bytes_since_collect
        .fetch_add(total_size as u64, Ordering::Relaxed);
    mutator.alloc_blocks_lifetime.fetch_add(1, Ordering::Relaxed);
    mutator
        .alloc_bytes_lifetime
        .fetch_add(total_size as u64, Ordering::Relaxed);

    unsafe { block.add(header_size) }
}

/// `__newbcpl_collect()` — extern wrapper around `collect()` so
/// JIT'd code (and the `GC()` builtin) can request a sweep
/// directly. Useful as an escape hatch when the threshold
/// heuristic doesn't fit the workload (benchmarks, tests, "I
/// just freed a huge thing").
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn __newbcpl_collect() -> i64 {
    collect();
    0
}

/// Allocate untracked, untraced bytes (`SYSTEM.NEW`).  These live
/// outside the cluster heap and are never reclaimed.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn __newbcpl_sys_new(n: usize) -> *mut u8 {
    if n == 0 {
        return std::ptr::NonNull::<u8>::dangling().as_ptr();
    }
    let layout = Layout::from_size_align(n, BLOCK_ALIGN)
        .expect("__newbcpl_sys_new: invalid allocation layout");
    let ptr = unsafe { std::alloc::alloc(layout) };
    if ptr.is_null() {
        std::alloc::handle_alloc_error(layout);
    }
    ptr
}

/// Cooperative safepoint poll — JIT-emitted code calls this at every
/// CP procedure entry.  Fast-path (no GC requested): a single atomic
/// load and a branch, ~ns.  Slow-path (collect requested): the calling
/// thread spills callee-saved registers, captures sp, marks itself
/// `Parked`, and waits on the safepoint condvar until the collector
/// clears the request.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn __newbcpl_safepoint() {
    if SAFEPOINT_REQUESTED.load(Ordering::Acquire) == 0 {
        return;
    }
    park_self_for_safepoint();
}

/// Park the current mutator at a safepoint.  Captures sp + callee-saved
/// registers, transitions state to `Parked`, then waits on the
/// safepoint condvar until the collector clears `SAFEPOINT_REQUESTED`.
/// On wake, transitions back to `Running` and returns.
#[inline(never)]
fn park_self_for_safepoint() {
    let mutator = ensure_mutator_for_current_thread();
    let mut spill_buf = [0usize; 16];
    let sp = capture_sp(&mut spill_buf);

    if let Ok(mut buf) = mutator.spill.lock() {
        *buf = spill_buf;
    }
    mutator.parked_sp.store(sp, Ordering::Release);
    mutator.state.store(STATE_PARKED, Ordering::SeqCst);
    mutator.park_count.fetch_add(1, Ordering::Relaxed);

    // Wait for the collector to clear the flag.
    let mut guard = SAFEPOINT_LOCK.lock().unwrap();
    while SAFEPOINT_REQUESTED.load(Ordering::Acquire) != 0 {
        guard = SAFEPOINT_CONDVAR.wait(guard).unwrap();
    }
    drop(guard);

    mutator.state.store(STATE_RUNNING, Ordering::SeqCst);
}

// ─────────────────────────────────────────────────────────────────────────────
// Allocation slow-path (handles refill, collect, grow)
// ─────────────────────────────────────────────────────────────────────────────

fn alloc_under_lock(
    total_size: usize,
    mutator: &Arc<Mutator>,
    caller_sp: usize,
    caller_spill: &[usize; 16],
) -> *mut u8 {
    // Step 1: try every existing cluster.
    {
        let mut heap = heap_lock();
        if let Some((block, _path)) = try_alloc_in_clusters(&mut heap, total_size) {
            return block;
        }
    }

    // Step 2: pressure → run a STW collection.
    collect_stw(mutator, caller_sp, caller_spill);

    // Step 3: retry after collection.
    {
        let mut heap = heap_lock();
        if let Some((block, _path)) = try_alloc_in_clusters(&mut heap, total_size) {
            return block;
        }
        // Step 4: grow.
        heap.clusters.push(Cluster::new(total_size));
        HEAP_COUNTERS.grow_events.fetch_add(1, Ordering::Relaxed);
        HEAP_COUNTERS
            .cluster_count
            .store(heap.clusters.len() as u64, Ordering::Relaxed);
        let last = heap.clusters.last_mut().expect("just pushed");
        unsafe { last.try_alloc(total_size) }
            .map(|(block, _)| block)
            .expect("fresh cluster must satisfy the request that drove its creation")
    }
}

fn try_alloc_in_clusters(heap: &mut Heap, total_size: usize) -> Option<(*mut u8, AllocPath)> {
    for cluster in &mut heap.clusters {
        if let Some(block) = unsafe { cluster.try_alloc(total_size) } {
            match block.1 {
                AllocPath::Bump => HEAP_COUNTERS.bump_path_blocks.fetch_add(1, Ordering::Relaxed),
                AllocPath::FreeList => HEAP_COUNTERS
                    .free_list_path_blocks
                    .fetch_add(1, Ordering::Relaxed),
            };
            return Some(block);
        }
    }
    None
}

#[inline]
fn align_up(n: usize) -> usize {
    (n + BLOCK_ALIGN - 1) & !(BLOCK_ALIGN - 1)
}

#[inline]
fn total_block_size(payload_size: usize) -> usize {
    let raw = std::mem::size_of::<BlockHeader>() + payload_size;
    align_up(raw).max(MIN_BLOCK)
}

// ─────────────────────────────────────────────────────────────────────────────
// Collection
// ─────────────────────────────────────────────────────────────────────────────

/// Run a stop-the-world collection cycle.  May be called from any
/// registered mutator thread; coordinates parking with every other
/// registered thread, then marks and sweeps under both `THREADS` (read)
/// and `HEAP` (write) locks.
fn collect_stw(initiator: &Arc<Mutator>, sp: usize, spill: &[usize; 16]) {
    let cycle_start = Instant::now();

    // Snapshot mutator handles outside the heap lock.
    let mutators: Vec<Arc<Mutator>> = MUTATORS.read().unwrap().clone();

    // Park self up-front so we don't deadlock with a concurrent collector.
    initiator.parked_sp.store(sp, Ordering::Release);
    if let Ok(mut buf) = initiator.spill.lock() {
        *buf = *spill;
    }
    initiator.state.store(STATE_PARKED, Ordering::SeqCst);

    // Request safepoint and wait for every other mutator to park.
    SAFEPOINT_REQUESTED.store(1, Ordering::SeqCst);
    let park_deadline = Instant::now() + Duration::from_secs(2);
    let mut all_parked = false;
    while Instant::now() < park_deadline {
        let still_running = mutators
            .iter()
            .filter(|m| {
                m.thread_id != initiator.thread_id
                    && m.state.load(Ordering::Acquire) != STATE_PARKED
            })
            .count();
        if still_running == 0 {
            all_parked = true;
            break;
        }
        // Briefly wait, then poll again.
        std::thread::sleep(Duration::from_micros(50));
    }
    if !all_parked {
        // Couldn't park everyone — abort cleanly rather than risk a
        // half-scanned mutator's stack.
        initiator.state.store(STATE_RUNNING, Ordering::SeqCst);
        SAFEPOINT_REQUESTED.store(0, Ordering::SeqCst);
        SAFEPOINT_CONDVAR.notify_all();
        eprintln!(
            "[newbcpl-gc] WARN: collect aborted; not all mutators parked within 2s ({} threads)",
            mutators.len()
        );
        return;
    }

    // Run the cycle under the heap lock.  The cycle returns the
    // pending-finalizer list so we can run them OUTSIDE the
    // safepoint-requested window.  Finalizers are JIT-compiled CP
    // code whose entry instruction is a safepoint poll; if we ran
    // them while SAFEPOINT_REQUESTED is still 1 they'd deadlock
    // waiting for the collector that's calling them.
    let parked_count = mutators.len() as u64;
    let (collect_summary, pending_finalizers) = {
        let mut heap = heap_lock();
        let s = run_collect_cycle(&mut heap, &mutators);
        let pending = std::mem::take(&mut heap.pending_finalizers);
        (s, pending)
    };

    // Release safepoint, wake other mutators.  Held the heap lock
    // through sweep, so the heap is consistent before they wake.
    SAFEPOINT_REQUESTED.store(0, Ordering::SeqCst);
    initiator.state.store(STATE_RUNNING, Ordering::SeqCst);
    {
        let _g = SAFEPOINT_LOCK.lock().unwrap();
        SAFEPOINT_CONDVAR.notify_all();
    }

    // Run pending finalizers outside both heap lock and safepoint
    // window.  Finalizers MUST NOT allocate (we don't enforce this
    // yet); a finalizer that allocates re-enters the GC, which is
    // re-entrancy we haven't designed for.
    let trace = std::env::var("NEWBCPL_GC_TRACE_FINALIZERS").is_ok();
    if trace {
        eprintln!("[gc] running {} pending finalizers", pending_finalizers.len());
    }
    for (i, (fin, payload)) in pending_finalizers.into_iter().enumerate() {
        if trace {
            eprintln!("[gc]   fin[{i}] @ {:p} on payload {:p}", fin as *const (), payload);
        }
        unsafe { fin(payload) };
        if trace {
            eprintln!("[gc]   fin[{i}] returned");
        }
    }

    let elapsed = cycle_start.elapsed().as_nanos() as u64;
    HEAP_COUNTERS.collect_cycles.fetch_add(1, Ordering::Relaxed);
    HEAP_COUNTERS
        .collect_total_nanos
        .fetch_add(elapsed, Ordering::AcqRel);
    HEAP_COUNTERS.collect_last_nanos.store(elapsed, Ordering::Relaxed);
    HEAP_COUNTERS
        .collect_last_reclaimed_bytes
        .store(collect_summary.bytes_freed, Ordering::Relaxed);

    // Push a record for `dump-gc --collect-log`.
    let mut heap = heap_lock();
    heap.generation += 1;
    let generation = heap.generation;
    heap.collect_log.push(CollectRecord {
        generation,
        elapsed_nanos: elapsed,
        mutators_parked: parked_count,
        roots_marked: collect_summary.roots_marked,
        blocks_freed: collect_summary.blocks_freed,
        bytes_freed: collect_summary.bytes_freed,
        bytes_live_after: collect_summary.bytes_live,
    });
}

#[derive(Default)]
struct CollectSummary {
    roots_marked: u64,
    blocks_freed: u64,
    bytes_freed: u64,
    live_blocks: u64,
    bytes_live: u64,
}

fn run_collect_cycle(heap: &mut Heap, mutators: &[Arc<Mutator>]) -> CollectSummary {
    // Phase 1a: clear marks across every cluster.
    for cluster in &mut heap.clusters {
        let mut offset: usize = 0;
        while offset < cluster.bump {
            unsafe {
                let hdr = cluster.base.add(offset) as *mut BlockHeader;
                let block_size = (*hdr).block_size;
                if block_size < MIN_BLOCK || offset + block_size > cluster.bump {
                    break;
                }
                (*hdr).clear_mark();
                offset += block_size;
            }
        }
    }

    // Phase 1b: scan each parked thread's stack range.
    let mut summary = CollectSummary::default();
    for m in mutators {
        let sp = m.parked_sp.load(Ordering::Acquire);
        let top = m.stack_top;
        if sp == 0 || top == 0 || sp >= top {
            continue;
        }
        let word = std::mem::size_of::<usize>();
        let mut cursor = sp;
        while cursor < top {
            let val = unsafe { *(cursor as *const usize) };
            if let Some(payload) = resolve_heap_ptr(val, &heap.clusters) {
                unsafe { mark_object(payload, &heap.type_descs) };
                summary.roots_marked += 1;
            }
            cursor += word;
        }
        // Spill buffer (if a recent park captured callee-saved regs).
        if let Ok(buf) = m.spill.lock() {
            for &val in buf.iter() {
                if let Some(payload) = resolve_heap_ptr(val, &heap.clusters) {
                    unsafe { mark_object(payload, &heap.type_descs) };
                    summary.roots_marked += 1;
                }
            }
        }
    }

    // Phase 1c: precise module roots.
    for module in &heap.modules {
        for &offset in &module.offsets {
            unsafe {
                let field = module.var_base.add(offset as usize) as *const *const u8;
                let ptr = *field;
                if !ptr.is_null() {
                    mark_object(ptr, &heap.type_descs);
                    summary.roots_marked += 1;
                }
            }
        }
    }

    // Phase 2: sweep.  Finalizers are queued (not run) here; the
    // caller drains them after clearing SAFEPOINT_REQUESTED.
    let mut pending: Vec<(Finalizer, *mut u8)> = Vec::new();
    for cluster in &mut heap.clusters {
        let s = unsafe { cluster_sweep(cluster, &mut heap.type_descs, &mut pending) };
        summary.blocks_freed += s.blocks_freed;
        summary.bytes_freed += s.bytes_freed;
        summary.live_blocks += s.live_blocks;
        summary.bytes_live += s.bytes_live;
    }
    heap.pending_finalizers.extend(pending);

    HEAP_COUNTERS
        .free_blocks_lifetime
        .fetch_add(summary.blocks_freed, Ordering::Relaxed);
    HEAP_COUNTERS
        .free_bytes_lifetime
        .fetch_add(summary.bytes_freed, Ordering::Relaxed);
    HEAP_COUNTERS
        .live_blocks
        .store(summary.live_blocks, Ordering::Relaxed);
    HEAP_COUNTERS
        .live_bytes
        .store(summary.bytes_live, Ordering::Relaxed);
    HEAP_COUNTERS
        .cluster_count
        .store(heap.clusters.len() as u64, Ordering::Relaxed);
    let mut peak = HEAP_COUNTERS.peak_live_bytes.load(Ordering::Acquire);
    while summary.bytes_live > peak {
        match HEAP_COUNTERS.peak_live_bytes.compare_exchange_weak(
            peak,
            summary.bytes_live,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => break,
            Err(observed) => peak = observed,
        }
    }

    summary
}

fn resolve_heap_ptr(addr: usize, clusters: &[Cluster]) -> Option<*const u8> {
    if addr == 0 {
        return None;
    }
    for cluster in clusters {
        if let Some(p) = unsafe { cluster.resolve(addr) } {
            return Some(p);
        }
    }
    None
}

unsafe fn mark_object(start: *const u8, registry: &TypeDescRegistry) {
    let mut work: Vec<*const u8> = Vec::with_capacity(64);
    work.push(start);
    while let Some(payload) = work.pop() {
        if payload.is_null() {
            continue;
        }
        unsafe {
            let hdr = header_of(payload);
            if (*hdr).is_marked() {
                continue;
            }
            if (*hdr).is_free() {
                continue;
            }
            (*hdr).set_mark();

            let td = (*hdr).type_desc();
            if td.is_null() {
                continue;
            }
            // Safety net: refuse to dereference a TypeDesc address
            // we've never seen.  In v2 every TD is supposed to land
            // in the registry via `__newbcpl_register_type` or the
            // first allocation that uses it; an unknown address is
            // either codegen-emitted (still valid) or stale.
            let known = registry.by_addr.contains_key(&(td as usize));
            if !known {
                continue;
            }
            // Cross-check size against the block's payload range.
            let payload_bytes = (*hdr)
                .block_size
                .saturating_sub(std::mem::size_of::<BlockHeader>());
            let claimed = (*td).size;
            if claimed <= 0 || (claimed as usize) > payload_bytes {
                continue;
            }
            for offset in (*td).pointer_offsets() {
                if (offset as usize) + std::mem::size_of::<*const u8>() > payload_bytes {
                    break;
                }
                let field = payload.add(offset as usize) as *const *const u8;
                let child = *field;
                if !child.is_null() {
                    work.push(child);
                }
            }
        }
    }
}

#[derive(Default)]
struct SweepStats {
    blocks_freed: u64,
    bytes_freed: u64,
    live_blocks: u64,
    bytes_live: u64,
}

unsafe fn cluster_sweep(
    cluster: &mut Cluster,
    registry: &mut TypeDescRegistry,
    pending_finalizers: &mut Vec<(Finalizer, *mut u8)>,
) -> SweepStats {
    let mut stats = SweepStats::default();
    let header_size = std::mem::size_of::<BlockHeader>();
    cluster.free_list = std::ptr::null_mut();
    let mut offset: usize = 0;
    let mut prev_free: *mut u8 = std::ptr::null_mut();

    unsafe {
        while offset < cluster.bump {
            let block = cluster.base.add(offset);
            let hdr = block as *mut BlockHeader;
            let block_size = (*hdr).block_size;
            if block_size < MIN_BLOCK || offset + block_size > cluster.bump {
                break;
            }
            let raw_tag = (*hdr).tag;
            let type_bits = raw_tag & !BlockHeader::MARK_BIT;
            let is_marked = raw_tag & BlockHeader::MARK_BIT != 0;
            let was_free = type_bits == 0;
            let is_dead = !was_free && !is_marked;

            if !was_free && is_marked {
                (*hdr).clear_mark();
                stats.live_blocks += 1;
                stats.bytes_live += block_size as u64;
                prev_free = std::ptr::null_mut();
            } else {
                if is_dead {
                    stats.blocks_freed += 1;
                    stats.bytes_freed += block_size as u64;
                    // Decrement the TypeDesc refcount only for newly-
                    // dead blocks.  Already-free blocks were never
                    // counted.
                    registry.dec(type_bits);

                    // Queue the finalizer (if any).  We do NOT call it
                    // here: finalizers are CP code that calls
                    // `__newbcpl_safepoint` on entry, and SAFEPOINT_REQUESTED
                    // is still set during sweep.  Calling synchronously
                    // would either deadlock the safepoint condvar or
                    // re-enter the GC.  Defer to a post-resume drain.
                    if registry.by_addr.contains_key(&type_bits) {
                        let td = type_bits as *const TypeDesc;
                        let payload_bytes = block_size.saturating_sub(header_size);
                        let claimed = (*td).size;
                        if claimed > 0
                            && (claimed as usize) <= payload_bytes
                        {
                            if let Some(fin) = (*td).finalizer {
                                let payload = block.add(header_size);
                                pending_finalizers.push((fin, payload));
                            }
                        }
                    }

                    // Wipe the payload only AFTER the finalizer has had
                    // a chance to read it.  We do that wipe in the
                    // post-resume drain, not here.  For now, leave the
                    // payload bytes intact; they'll be zeroed when the
                    // free-list path next reuses this block.
                }

                if !prev_free.is_null() {
                    let prev_hdr = prev_free as *mut BlockHeader;
                    (*prev_hdr).block_size += block_size;
                    cluster.clear_block_start(offset);
                } else {
                    (*hdr).tag = 0;
                    (*hdr).block_size = block_size;
                    let link = block.add(header_size) as *mut FreeBlockLink;
                    link.write(FreeBlockLink {
                        next: cluster.free_list,
                    });
                    cluster.free_list = block;
                    prev_free = block;
                }
            }
            offset += block_size;
        }
    }

    stats
}

// ─────────────────────────────────────────────────────────────────────────────
// Public entry points (collect, snapshot, registry interaction)
// ─────────────────────────────────────────────────────────────────────────────

pub fn collect() {
    let mut spill_buf = [0usize; 16];
    let sp = capture_sp(&mut spill_buf);
    let mutator = ensure_mutator_for_current_thread();
    collect_stw(&mutator, sp, &spill_buf);
}

/// Record a TypeDesc with the registry.  Called by codegen-emitted
/// `__init_types` and by the kernel module surface.
pub(crate) fn register_typedesc(td: usize, owner_module: Option<String>) {
    let mut heap = heap_lock();
    heap.type_descs.record(td, owner_module);
}

/// True iff every TypeDesc owned by `module_name` currently has a
/// `block_count` of 0 — i.e. no live heap block tags any of those
/// TypeDescs.  Used by the loader's `RetiredImageDropPredicate`
/// to decide whether a retired JIT image's memory can be freed
/// without dangling tags.  A module with no TypeDescs registered
/// returns `true` (vacuously safe).
pub fn module_has_no_live_blocks(module_name: &str) -> bool {
    let heap = heap_lock();
    !heap.type_descs.by_addr.values().any(|entry| {
        entry.owner_module.as_deref() == Some(module_name) && entry.block_count > 0
    })
}

#[inline]
unsafe fn header_of(payload: *const u8) -> *mut BlockHeader {
    unsafe { payload.sub(std::mem::size_of::<BlockHeader>()) as *mut BlockHeader }
}

// ─────────────────────────────────────────────────────────────────────────────
// Register spill + stack-pointer capture (architecture-dependent)
// ─────────────────────────────────────────────────────────────────────────────

#[inline(never)]
fn capture_sp(spill_buf: &mut [usize; 16]) -> usize {
    let sp: usize;
    unsafe {
        #[cfg(all(target_arch = "x86_64", not(target_os = "windows")))]
        std::arch::asm!(
            "mov [{buf}     ], rbx",
            "mov [{buf} +  8], rbp",
            "mov [{buf} + 16], r12",
            "mov [{buf} + 24], r13",
            "mov [{buf} + 32], r14",
            "mov [{buf} + 40], r15",
            "mov {sp}, rsp",
            buf = in(reg) spill_buf.as_mut_ptr(),
            sp  = out(reg) sp,
        );

        #[cfg(all(target_arch = "x86_64", target_os = "windows"))]
        std::arch::asm!(
            "mov [{buf}     ], rbx",
            "mov [{buf} +  8], rbp",
            "mov [{buf} + 16], rdi",
            "mov [{buf} + 24], rsi",
            "mov [{buf} + 32], r12",
            "mov [{buf} + 40], r13",
            "mov [{buf} + 48], r14",
            "mov [{buf} + 56], r15",
            "mov {sp}, rsp",
            buf = in(reg) spill_buf.as_mut_ptr(),
            sp  = out(reg) sp,
        );

        #[cfg(target_arch = "aarch64")]
        std::arch::asm!(
            "stp x19, x20, [{buf}]",
            "stp x21, x22, [{buf}, #16]",
            "stp x23, x24, [{buf}, #32]",
            "stp x25, x26, [{buf}, #48]",
            "mov {sp}, sp",
            buf = in(reg) spill_buf.as_mut_ptr(),
            sp  = out(reg) sp,
        );

        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        {
            sp = spill_buf.as_ptr() as usize;
        }
    }
    sp
}

// ─────────────────────────────────────────────────────────────────────────────
// Introspection — public read-only access for `dump-gc` and tests
// ─────────────────────────────────────────────────────────────────────────────

/// Locked-state shim for v1 callers.  v2 keeps the same shape so
/// `heap_introspect.rs` migrates without semantic surprise.
#[derive(Clone)]
pub struct GcState {
    pub clusters: Vec<ClusterView>,
    pub modules: Vec<ModuleView>,
    pub type_descs: Vec<TypeDescEntry>,
    pub mutators: Vec<MutatorView>,
}

#[derive(Clone)]
pub struct ClusterView {
    pub base: usize,
    pub size: usize,
    pub bump: usize,
    pub free_blocks: u64,
    pub free_bytes: u64,
}

#[derive(Clone)]
pub struct ModuleView {
    pub name: String,
    pub var_base: usize,
    pub offset_count: usize,
}

#[derive(Clone)]
pub struct MutatorView {
    pub thread_id: ThreadId,
    pub stack_top: usize,
    pub state: u8,
    pub parked_sp: usize,
    pub alloc_blocks_lifetime: u64,
    pub alloc_bytes_lifetime: u64,
    pub park_count: u64,
}

pub fn snapshot() -> GcState {
    let heap = heap_lock();
    let mutators_guard = MUTATORS.read().unwrap();

    let clusters = heap
        .clusters
        .iter()
        .map(|c| {
            let (free_blocks, free_bytes) = walk_free_list(c);
            ClusterView {
                base: c.base as usize,
                size: c.size,
                bump: c.bump,
                free_blocks,
                free_bytes,
            }
        })
        .collect();
    let modules = heap
        .modules
        .iter()
        .map(|m| ModuleView {
            name: m.name.clone(),
            var_base: m.var_base as usize,
            offset_count: m.offsets.len(),
        })
        .collect();
    let type_descs = heap.type_descs.snapshot();
    let mutators = mutators_guard
        .iter()
        .map(|m| MutatorView {
            thread_id: m.thread_id,
            stack_top: m.stack_top,
            state: m.state.load(Ordering::Relaxed),
            parked_sp: m.parked_sp.load(Ordering::Relaxed),
            alloc_blocks_lifetime: m.alloc_blocks_lifetime.load(Ordering::Relaxed),
            alloc_bytes_lifetime: m.alloc_bytes_lifetime.load(Ordering::Relaxed),
            park_count: m.park_count.load(Ordering::Relaxed),
        })
        .collect();

    GcState {
        clusters,
        modules,
        type_descs,
        mutators,
    }
}

fn walk_free_list(cluster: &Cluster) -> (u64, u64) {
    let header_size = std::mem::size_of::<BlockHeader>();
    let mut count = 0u64;
    let mut bytes = 0u64;
    let mut node = cluster.free_list;
    while !node.is_null() {
        unsafe {
            let hdr = node as *const BlockHeader;
            count += 1;
            bytes += (*hdr).block_size as u64;
            let next_link = node.add(header_size) as *const *mut u8;
            node = *next_link;
        }
    }
    (count, bytes)
}

pub fn collect_log_snapshot() -> Vec<CollectRecord> {
    let heap = heap_lock();
    heap.collect_log.snapshot()
}

/// Compatibility shim for tests that used to call
/// `gc::with_locked_state(|s| ...)`.  The closure receives the same
/// snapshot type, just produced from the new layout.
pub(crate) fn with_locked_state<R>(f: impl FnOnce(&GcState) -> R) -> R {
    let snap = snapshot();
    f(&snap)
}

/// Run `f` against the live, locked `Heap`.  Used by `heap_introspect`
/// to walk every cluster's blocks under one consistent lock-held
/// window.  The closure must not call back into the GC (no
/// allocations, no `collect()`).
pub(crate) fn with_heap_locked<R>(f: impl FnOnce(&Heap) -> R) -> R {
    let heap = heap_lock();
    f(&heap)
}

// ─────────────────────────────────────────────────────────────────────────────
// Indirect method dispatch
// ─────────────────────────────────────────────────────────────────────────────
//
// `__newbcpl_lookup_method(obj, name)` resolves an untyped
// `obj.method()` call. The IR emits this when the receiver's class
// can't be determined statically — usually a routine parameter
// without an `AS Class` annotation. The helper returns the function
// pointer for the receiver's actual class's method of that name, or
// null when the method isn't defined.
//
// The instance points at its inline vtable header at offset 0
// (installed by `emit_new`). We use that vtable address as the
// dispatch key into a process-global registry of class metadata
// populated by the LLVM crate at JIT-finalize time. Each entry
// pairs a vtable pointer with the matching `method_names` array
// (the parallel `[N x ptr]` of name strings emitted alongside the
// vtable).
//
// Safety contract: `obj` is expected to be a managed heap pointer
// whose first word is a vtable pointer. A null receiver or an
// unregistered vtable returns null — the caller will then indirect-
// call through null and the resulting fault surfaces the issue
// cleanly rather than silently dispatching to the wrong method.

/// One entry in the JIT-side class registry.
struct VtableMethodTable {
    vtable_addr: u64,
    names_addr: u64,
    count: u64,
}

static VTABLE_METHOD_REGISTRY: RwLock<Vec<VtableMethodTable>> =
    RwLock::new(Vec::new());

/// Register a class's `(vtable_addr, method_names_addr, count)`
/// triple for name-keyed dispatch. Called from the LLVM crate once
/// per class layout right after `LLVMGetFunctionAddress(START)`
/// triggers MCJIT finalize.
///
/// `count` is the vtable length (number of methods); `names_addr`
/// points at a `[count x *const u8]` array of null-terminated UTF-8
/// method names parallel to the vtable. Both addresses are stable
/// for the rest of the process's life (the JIT never relocates
/// finalised code or globals).
pub fn register_jit_vtable_methods(
    vtable_addr: u64,
    names_addr: u64,
    count: u64,
) {
    if vtable_addr == 0 || names_addr == 0 || count == 0 {
        return;
    }
    let mut guard = VTABLE_METHOD_REGISTRY
        .write()
        .expect("VTABLE_METHOD_REGISTRY poisoned");
    guard.push(VtableMethodTable {
        vtable_addr,
        names_addr,
        count,
    });
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn __newbcpl_lookup_method(
    obj: *const u8,
    name: *const u8,
) -> *const () {
    if obj.is_null() || name.is_null() {
        return core::ptr::null();
    }
    // Instance offset 0 holds the inline vtable pointer.
    let vtable_addr = unsafe { *(obj as *const u64) };
    if vtable_addr == 0 {
        return core::ptr::null();
    }
    let guard = match VTABLE_METHOD_REGISTRY.read() {
        Ok(g) => g,
        Err(_) => return core::ptr::null(),
    };
    let entry = match guard.iter().find(|e| e.vtable_addr == vtable_addr) {
        Some(e) => e,
        None => return core::ptr::null(),
    };
    let names_base = entry.names_addr as *const *const u8;
    let vtable_base = vtable_addr as *const *const ();
    for i in 0..entry.count {
        let entry_name = unsafe { *names_base.add(i as usize) };
        if entry_name.is_null() {
            continue;
        }
        if cstr_eq(entry_name, name) {
            return unsafe { *vtable_base.add(i as usize) };
        }
    }
    core::ptr::null()
}

/// Compare two null-terminated byte strings for equality. Bounded
/// at 256 bytes per side so a malformed pointer doesn't cause us to
/// walk off the end of the world.
unsafe fn cstr_eq(a: *const u8, b: *const u8) -> bool {
    if a.is_null() || b.is_null() {
        return false;
    }
    for i in 0..256 {
        let ba = unsafe { *a.add(i) };
        let bb = unsafe { *b.add(i) };
        if ba != bb {
            return false;
        }
        if ba == 0 {
            return true;
        }
    }
    false
}

// ─────────────────────────────────────────────────────────────────────────────
// Test helpers
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
pub(crate) fn reset_for_test() {
    let mut heap = heap_lock();
    heap.clusters.clear();
    heap.modules.clear();
    heap.type_descs = TypeDescRegistry::new();
    heap.collect_log = CollectLog::new(16);
    heap.generation = 0;
    heap.pending_finalizers.clear();
    let mut threads = MUTATORS.write().unwrap();
    threads.clear();
    BOOTSTRAP_STACK_BASE.store(0, Ordering::Release);
    SAFEPOINT_REQUESTED.store(0, Ordering::Release);
    HEAP_COUNTERS.reset();
}

#[cfg(test)]
pub(crate) static GLOBAL_TEST_LOCK: Mutex<()> = Mutex::new(());

#[cfg(test)]
pub(crate) fn lock_tests_global() -> std::sync::MutexGuard<'static, ()> {
    match GLOBAL_TEST_LOCK.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(unused_mut, unused_unsafe)]
mod tests {
    use super::*;
    use std::sync::Arc as TestArc;
    use std::sync::Barrier;

    fn lock_tests() -> std::sync::MutexGuard<'static, ()> {
        super::lock_tests_global()
    }

    /// Simplest path: register, alloc, collect, repeat.
    #[test]
    fn alloc_collect_alloc() {
        let _lock = lock_tests();
        reset_for_test();

        // Bake a TypeDesc with a 64-byte payload, no pointers.
        static mut TD: TypeDesc = TypeDesc {
            size: 64,
            module: std::ptr::null(),
            finalizer: None,
            base: std::ptr::null(),
            vtable: std::ptr::null(),
            vtable_len: 0,
            name: std::ptr::null(),
            ptroffs: [],
        };
        let td_ptr = unsafe { (&raw const TD) as *const TypeDesc };

        let mut local: usize = 0;
        unsafe { __newbcpl_init_gc(&raw const local as *const u8) };
        let _ = local;

        for _ in 0..100 {
            let ptr = unsafe { __newbcpl_new_rec(td_ptr) };
            assert!(!ptr.is_null());
        }
        collect();
        for _ in 0..100 {
            let ptr = unsafe { __newbcpl_new_rec(td_ptr) };
            assert!(!ptr.is_null());
        }
    }

    /// Two threads allocating concurrently.  No assertions on
    /// retention (each thread's allocations are immediately
    /// unreachable), but the test must not crash and the counters
    /// must add up.
    #[test]
    fn multi_thread_alloc_no_crash() {
        let _lock = lock_tests();
        reset_for_test();

        static mut TD: TypeDesc = TypeDesc {
            size: 128,
            module: std::ptr::null(),
            finalizer: None,
            base: std::ptr::null(),
            vtable: std::ptr::null(),
            vtable_len: 0,
            name: std::ptr::null(),
            ptroffs: [],
        };
        let td_addr = unsafe { (&raw const TD) as *const TypeDesc as usize };

        let mut local: usize = 0;
        unsafe { __newbcpl_init_gc(&raw const local as *const u8) };
        let _ = local;

        const N_THREADS: usize = 4;
        const ALLOCS_PER_THREAD: usize = 200;
        let barrier = TestArc::new(Barrier::new(N_THREADS));
        let mut handles = Vec::new();
        for _ in 0..N_THREADS {
            let b = barrier.clone();
            handles.push(std::thread::spawn(move || {
                // Each thread registers its own stack-top.
                let mut local_root: usize = 0;
                unsafe { __newbcpl_register_thread(&raw const local_root as *const u8) };
                let td_ptr = td_addr as *const TypeDesc;
                b.wait();
                for _ in 0..ALLOCS_PER_THREAD {
                    unsafe {
                        let p = __newbcpl_new_rec(td_ptr);
                        assert!(!p.is_null());
                        local_root = p as usize; // make alive on stack
                        std::hint::black_box(local_root);
                    }
                }
                unsafe { __newbcpl_unregister_thread() };
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        let total_threads = N_THREADS as u64;
        let expected_blocks = total_threads * ALLOCS_PER_THREAD as u64;
        assert!(
            HEAP_COUNTERS.alloc_blocks_lifetime.load(Ordering::Relaxed) >= expected_blocks,
            "fewer allocations counted than threads performed"
        );
    }

    /// Safepoint test: a worker thread that polls in a tight loop
    /// must park promptly when another thread requests collection.
    /// The collector's wait timeout (2 s) failing here would indicate
    /// the safepoint mechanism isn't working.
    #[test]
    fn safepoint_pauses_polling_worker() {
        let _lock = lock_tests();
        reset_for_test();

        let mut local: usize = 0;
        unsafe { __newbcpl_init_gc(&raw const local as *const u8) };
        let _ = local;

        let stop = TestArc::new(std::sync::atomic::AtomicBool::new(false));
        let park_count_seen = TestArc::new(AtomicU64::new(0));

        // Spawn a worker that polls __newbcpl_safepoint in a hot loop.
        let stop_clone = stop.clone();
        let park_clone = park_count_seen.clone();
        let worker = std::thread::spawn(move || {
            let mut local_root: usize = 0;
            unsafe { __newbcpl_register_thread(&raw const local_root as *const u8) };
            let _ = local_root;
            while !stop_clone.load(Ordering::Acquire) {
                unsafe { __newbcpl_safepoint() };
                // Make sure the loop isn't optimised away.
                std::hint::black_box(&local_root);
            }
            // Snapshot the worker's park count after the loop ends.
            let m = ensure_mutator_for_current_thread();
            park_clone.store(m.park_count.load(Ordering::Relaxed), Ordering::Release);
            unsafe { __newbcpl_unregister_thread() };
        });

        // Give the worker a moment to actually start polling.
        std::thread::sleep(Duration::from_millis(20));

        // Trigger a collect from the main thread; without working
        // safepoints the worker would never park and this would
        // either deadlock or hit the 2 s abort path.
        let collect_start = Instant::now();
        collect();
        let collect_elapsed = collect_start.elapsed();
        assert!(
            collect_elapsed < Duration::from_millis(500),
            "collect() took {collect_elapsed:?} — safepoint mechanism not pausing worker"
        );

        // Trigger a second one to confirm the worker resumes and re-parks.
        std::thread::sleep(Duration::from_millis(10));
        collect();

        stop.store(true, Ordering::Release);
        worker.join().unwrap();

        let parks = park_count_seen.load(Ordering::Acquire);
        assert!(
            parks >= 2,
            "worker should have parked at least twice (saw {parks})"
        );
    }

    /// TypeDesc refcount must zero out after a collect that finds
    /// every block dead.
    #[test]
    fn type_desc_refcount_zeroes_on_collect() {
        let _lock = lock_tests();
        reset_for_test();

        static mut TD: TypeDesc = TypeDesc {
            size: 32,
            module: std::ptr::null(),
            finalizer: None,
            base: std::ptr::null(),
            vtable: std::ptr::null(),
            vtable_len: 0,
            name: std::ptr::null(),
            ptroffs: [],
        };
        let td_ptr = unsafe { (&raw const TD) as *const TypeDesc };
        let td_addr = td_ptr as usize;

        let mut local: usize = 0;
        unsafe { __newbcpl_init_gc(&raw const local as *const u8) };
        let _ = local;

        for _ in 0..50 {
            let _ = unsafe { __newbcpl_new_rec(td_ptr) }; // immediately unreachable
        }
        // The pointers were thrown away — collect should reclaim them all.
        collect();

        let snap = snapshot();
        let entry = snap.type_descs.iter().find(|e| e.addr == td_addr);
        let count = entry.map(|e| e.block_count).unwrap_or(0);
        assert_eq!(count, 0, "expected 0 live blocks for this TypeDesc, got {count}");
    }
}
