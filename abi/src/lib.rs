//! The High-Frequency Chess engine ABI, and the cycle counter both sides read.
//!
//! An engine is a shared library exporting three C symbols:
//!
//! ```c
//! uint32_t hfc_abi_version(void);
//! void     hfc_init(void);
//! uint16_t hfc_play(const hfc_position *pos, const hfc_budget *b);
//! ```
//!
//! `hfc_play` answers with a move word; only its from-square, to-square
//! and promotion piece are read, so the other bits need not be encoded.
//!
//! It is loaded with `dlopen` by `engine-host`, a separate process, and
//! driven over a pipe. There is no `hfc_new_game`: a new game is a flag on
//! the first `hfc_play` of that game, so clearing state costs that move's
//! budget.
//!
//! There is no trap. **The engine is responsible for returning before it
//! exhausts its budget** — the harness measures what was spent and forfeits
//! the game on an overrun.

/// The host compares for equality and refuses anything else: the board is a
/// `repr(C)` struct read out of shared memory, so a stale copy of `chess` or
/// `hfc-abi` would not fail, it would silently read the wrong bytes. Bumped
/// whenever a shape in here changes.
pub const ABI_VERSION: u32 = 1;

/// The board handed to `hfc_play`. This is `hfc_rules::board::Board` verbatim --
/// bitboards, a mailbox, and the small state -- laid out `repr(C)`. No parsing.
pub use hfc_rules::board::Board as WireBoard;

/// A position plus the history needed to see repetitions.
///
/// `history` holds Zobrist hashes from the last irreversible move up to and
/// including the current position, oldest first. The key schedule is published
/// (`hfc_rules::ZOBRIST`, seeded from a fixed constant) so both sides agree.
#[repr(C)]
pub struct Position {
    pub board: *const WireBoard,
    pub history: *const u64,
    pub history_len: u32,
    /// Non-zero on the first move an engine is asked for in a new game.
    /// Clear anything that must not carry over; it costs this move's budget.
    pub new_game: u32,
}

impl Position {
    /// # Safety
    /// `self.history` must point to `history_len` readable u64s.
    pub unsafe fn history(&self) -> &[u64] {
        if self.history.is_null() || self.history_len == 0 {
            &[]
        } else {
            core::slice::from_raw_parts(self.history, self.history_len as usize)
        }
    }
}

/// Passed to `hfc_play`. Read the counter through `now()`.
#[repr(C)]
pub struct Budget {
    /// Pointer to an mmap'd `perf_event_mmap_page`, for `rdpmc`. Never null.
    pub perf_page: *const u8,
    /// Core cycles this call may consume.
    pub cycles: u64,
}

use std::sync::atomic::{compiler_fence, Ordering};

#[inline(always)]
unsafe fn rdpmc(idx: u32) -> u64 {
    let lo: u32;
    let hi: u32;
    core::arch::asm!("rdpmc", in("ecx") idx, out("eax") lo, out("edx") hi,
                     options(nostack, nomem, preserves_flags));
    ((hi as u64) << 32) | (lo as u64)
}

/// Current core-cycle count for this thread. Seqlock read, per the
/// `perf_event_mmap_page` contract. Costs roughly 30-40 cycles.
///
/// This counts only cycles this thread actually executed — being descheduled
/// does not spend your budget.
#[inline(always)]
pub fn now(perf_page: *const u8) -> u64 {
    if perf_page.is_null() {
        return unsafe { core::arch::x86_64::_rdtsc() };
    }
    unsafe {
        let pc = perf_page as *const perf_event_open_sys::bindings::perf_event_mmap_page;
        loop {
            let seq = std::ptr::read_volatile(&(*pc).lock);
            compiler_fence(Ordering::SeqCst);

            let idx = std::ptr::read_volatile(&(*pc).index);
            let offset = std::ptr::read_volatile(&(*pc).offset);
            let width = std::ptr::read_volatile(&(*pc).pmc_width);

            let mut count = offset as u64;
            if idx != 0 {
                let raw = rdpmc(idx - 1);
                let shift = 64 - width as u32;
                count = count.wrapping_add((((raw << shift) as i64) >> shift) as u64);
            }

            compiler_fence(Ordering::SeqCst);
            if std::ptr::read_volatile(&(*pc).lock) == seq {
                return count;
            }
        }
    }
}

// ------------------------------------------------------- harness-side setup

/// Two counters on the same task, because one cannot do both jobs.
///
/// * `audit_fd` has `inherit` set, so it follows threads and child processes.
///   That is what stops an engine doing unbounded work on a helper thread for
///   free. `inherit` is incompatible with the mmap'd `rdpmc` page, so it is
///   read with a syscall.
/// * `page` is a plain per-thread counter the engine reads with `rdpmc` to
///   pace itself. Cheap, advisory, and never used to decide an overrun.
/// The engine's own pacing counter: per-thread, mmap'd, read with `rdpmc`.
/// Advisory only — the arbiter measures the engine process from outside.
pub struct Counter {
    page: *mut libc::c_void,
    _self_fd: i32,
}

impl Counter {
    pub fn new() -> Result<Counter, String> {
        use perf_event_open_sys as sys;
        let base = || {
            let mut a = sys::bindings::perf_event_attr {
                size: std::mem::size_of::<sys::bindings::perf_event_attr>() as u32,
                type_: sys::bindings::PERF_TYPE_HARDWARE,
                config: sys::bindings::PERF_COUNT_HW_CPU_CYCLES as u64,
                ..Default::default()
            };
            a.set_exclude_kernel(1);
            a.set_exclude_hv(1);
            a
        };

        // Authoritative counter: follows threads and children.
        // read_format carries the enabled/running times so multiplexing can be
        // detected rather than silently turning the count into an estimate.
        let mut me = base();
        let self_fd = unsafe { sys::perf_event_open(&mut me, 0, -1, -1, 0) };
        if self_fd < 0 {
            return Err(format!(
                "perf_event_open failed: {}\n  need: sudo sysctl kernel.perf_event_paranoid=2",
                std::io::Error::last_os_error()
            ));
        }
        let page = unsafe {
            libc::mmap(std::ptr::null_mut(), 4096, libc::PROT_READ, libc::MAP_SHARED, self_fd, 0)
        };
        if page == libc::MAP_FAILED {
            return Err("mmap of perf page failed".into());
        }

        let c = Counter { page, _self_fd: self_fd };
        if now(c.page()) == 0 && now(c.page()) == 0 {
            return Err("rdpmc returned no counter; try:\n  \
                        echo 2 | sudo tee /sys/bus/event_source/devices/cpu_core/rdpmc"
                .into());
        }
        Ok(c)
    }

    pub fn page(&self) -> *const u8 {
        self.page as *const u8
    }

    /// The engine's view, for self-limiting. Cheap (`rdpmc`, ~35 cycles) but
    /// only its own thread, and it reads a page the engine could in principle
    /// remap. Advisory only.
    #[inline(always)]
    pub fn read(&self) -> u64 {
        now(self.page())
    }

}

/// Pin the calling thread to one logical CPU. Mandatory: core-cycle budgets
/// are only comparable within a single core, and on a hybrid CPU (Intel P/E)
/// they are not comparable across core types at all.
pub fn pin_to(cpu: usize) -> bool {
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        libc::CPU_ZERO(&mut set);
        libc::CPU_SET(cpu, &mut set);
        libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &set) == 0
    }
}

/// Warn if the chosen CPU shares a physical core with another online CPU:
/// a busy sibling hyperthread inflates identical work, which invalidates
/// the budget.
pub fn smt_sibling_warning(cpu: usize) -> Option<String> {
    let path = format!("/sys/devices/system/cpu/cpu{cpu}/topology/thread_siblings_list");
    let s = std::fs::read_to_string(path).ok()?;
    let sibs: Vec<&str> = s.trim().split(&[',', '-'][..]).collect();
    if sibs.len() > 1 {
        return Some(format!(
            "CPU {cpu} shares a physical core with CPU(s) {}. \
             Identical work costs ~52% more cycles when a sibling is busy.\n  \
             fix: echo off | sudo tee /sys/devices/system/cpu/smt/control",
            sibs.iter().filter(|x| **x != cpu.to_string()).cloned().collect::<Vec<_>>().join(", ")
        ));
    }
    None
}

// ------------------------------------------------- out-of-process protocol

/// The shared page an engine host and the arbiter exchange positions through.
///
/// The engine runs in its own process. Data moves through this page with no
/// copying; a pipe carries the wakeup. Since the budget is measured in cycles,
/// a blocked process costs nothing, so blocking is free where spinning would
/// have cost a core.
/// Positions the shared page carries for repetition detection.
///
/// Not an arbitrary cap: the arbiter clears the history when the halfmove
/// clock resets and the fifty-move rule ends the game at 100, so the real
/// bound is 100. The headroom is deliberate, and `native.rs` copies the whole
/// history rather than truncating it, so overflowing this would be a panic
/// rather than a silently shortened history.
pub const MAX_HISTORY: usize = 256;

#[repr(C)]
pub struct Shared {
    /// Position to search.
    pub board: WireBoard,
    pub history_len: u32,
    pub history: [u64; MAX_HISTORY],
    /// Cycles the engine may spend on this move.
    pub cycles: u64,
    /// What the engine chose. The arbiter clears this before every request,
    /// so a stale reply cannot be mistaken for an answer: an acknowledgement
    /// without a fresh `hfc_play` leaves a 0 here, and 0 is not a move.
    pub answer: u16,
    /// Passed through to `Position::new_game` for the next `hfc_play`.
    pub new_game: u32,
    /// Set by the host once `hfc_init` has completed. The arbiter spins on it
    /// after `load`, so an engine is never asked to move before it is built.
    pub ready: u32,
    /// Reference-clock ticks spent inside `hfc_init`, timed by the host: the
    /// arbiter can only poll, and the cycle counter is not installed on a
    /// task until its next context switch -- exactly the window in question.
    pub init_ticks: u64,
    /// Set by the host once it is running but *before* it calls `hfc_init`.
    ///
    /// Two jobs. It separates process startup -- exec, dynamic linking,
    /// `dlopen` -- from the engine's own setup, so only the latter is charged.
    /// And waiting for it gives the arbiter's counter a context switch to be
    /// installed on, without which the early cycles of a task simply are not
    /// counted.
    pub started: u32,
}

impl Shared {
    /// # Safety
    /// `p` must point to a mapping of at least `size_of::<Shared>()` bytes.
    pub unsafe fn from_ptr<'a>(p: *mut u8) -> &'a mut Shared {
        &mut *(p as *mut Shared)
    }
}

/// Create the shared page, backed by a descriptor so it survives `exec`.
/// Returns the mapping and the fd to hand the engine host.
pub fn map_shared() -> Result<(*mut u8, i32), String> {
    let len = std::mem::size_of::<Shared>().next_multiple_of(4096);
    let name = b"hfc-shared\0";
    let fd = unsafe { libc::memfd_create(name.as_ptr() as *const libc::c_char, 0) };
    if fd < 0 {
        return Err(format!("memfd_create failed: {}", std::io::Error::last_os_error()));
    }
    if unsafe { libc::ftruncate(fd, len as libc::off_t) } != 0 {
        return Err("could not size the shared page".into());
    }
    let p = unsafe {
        libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ | libc::PROT_WRITE,
                   libc::MAP_SHARED, fd, 0)
    };
    if p == libc::MAP_FAILED {
        return Err("could not map the shared page".into());
    }
    unsafe { std::ptr::write_bytes(p as *mut u8, 0, len) };
    Ok((p as *mut u8, fd))
}

pub fn shared_len() -> usize {
    std::mem::size_of::<Shared>().next_multiple_of(4096)
}

/// A cycle counter for *another* process.
///
/// Opened by the arbiter on the engine's pid after forking it, so the
/// engine never holds the descriptor and cannot close, reset or remap it.
pub struct RemoteCounter {
    fd: i32,
}

impl RemoteCounter {
    pub fn open(pid: i32) -> Result<RemoteCounter, String> {
        use perf_event_open_sys as sys;
        let mut attr = sys::bindings::perf_event_attr {
            size: std::mem::size_of::<sys::bindings::perf_event_attr>() as u32,
            type_: sys::bindings::PERF_TYPE_HARDWARE,
            config: sys::bindings::PERF_COUNT_HW_CPU_CYCLES as u64,
            ..Default::default()
        };
        attr.set_exclude_kernel(1);
        attr.set_exclude_hv(1);
        // Not a control in its own right: seccomp already denies clone, so
        // there are no threads or children for this to follow, and `harness
        // attacks` measures the claim -- with seccomp on, turning inherit
        // off changes no probe's outcome. Kept because it is one attribute
        // flag and costs nothing.
        attr.set_inherit(1);
        attr.read_format = (sys::bindings::PERF_FORMAT_TOTAL_TIME_ENABLED
            | sys::bindings::PERF_FORMAT_TOTAL_TIME_RUNNING) as u64;

        let fd = unsafe { sys::perf_event_open(&mut attr, pid, -1, -1, 0) };
        if fd < 0 {
            return Err(format!(
                "perf_event_open on pid {pid} failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(RemoteCounter { fd })
    }

    /// Cycles the engine process has burned, or `u64::MAX` if the reading is
    /// not trustworthy.
    pub fn read(&self) -> u64 {
        let mut buf = [0u64; 3];
        let want = std::mem::size_of::<[u64; 3]>();
        let n = unsafe { libc::read(self.fd, buf.as_mut_ptr() as *mut libc::c_void, want) };
        if n != want as isize || buf[2] < buf[1] {
            return u64::MAX;
        }
        buf[0]
    }
}

impl Drop for RemoteCounter {
    fn drop(&mut self) {
        unsafe { libc::close(self.fd) };
    }
}
