//! Runs one engine, in its own process.
//!
//! The arbiter forks and execs this with three inherited descriptors: a shared
//! page carrying positions and answers, and a pipe pair for wakeups. It loads
//! the engine, reports ready, then answers move requests until the pipe closes.
//!
//! Everything here exists so that a submitted engine cannot touch the
//! arbiter's memory. In-process, an engine could rewrite the board, the
//! results, or the cycle counter's bookkeeping, and nothing would notice.
//! Across a process boundary none of that is expressible.
//!
//! usage: engine-host <engine.so> <shm-fd> <req-fd> <resp-fd>

use libloading::{Library, Symbol};
use hfc_abi::{Budget, Position, Shared};

type FnVoid = extern "C" fn();
type FnVersion = extern "C" fn() -> u32;
type FnPlay = unsafe extern "C" fn(*const Position, *const Budget) -> u16;

fn die(msg: &str) -> ! {
    eprintln!("engine-host: {msg}");
    unsafe { libc::_exit(2) }
}

/// Refuse to create processes or threads.
///
/// This is what stops pondering, and nothing else does. The arbiter reads the
/// cycle counter as a delta around each move, so cycles a helper burns
/// *between* moves fall outside every measured window -- `inherit` on the
/// counter cannot see them. Denying process and thread creation removes the
/// helper instead.
///
/// Failure to install is fatal. An engine running unfiltered would be
/// measurable but not bounded.
fn forbid_process_creation() {
    // A minimal classic-BPF seccomp filter over the syscall number.
    #[repr(C)]
    struct SockFilter { code: u16, jt: u8, jf: u8, k: u32 }
    #[repr(C)]
    struct SockFprog { len: u16, filter: *const SockFilter }

    const LD_W_ABS: u16 = 0x20;
    const JMP_JEQ_K: u16 = 0x15;
    const RET_K: u16 = 0x06;
    const NR_OFFSET: u32 = 0; // offsetof(seccomp_data, nr)
    const KILL: u32 = 0x0000_0000; // SECCOMP_RET_KILL_THREAD
    const ALLOW: u32 = 0x7fff_0000;

    // openat2, which the libc crate does not name on every version.
    const SYS_OPENAT2: u32 = 437;
    let blocked: [u32; 11] = [
        libc::SYS_clone as u32,
        libc::SYS_clone3 as u32,
        libc::SYS_fork as u32,
        libc::SYS_vfork as u32,
        libc::SYS_execve as u32,
        libc::SYS_ptrace as u32,
        // No new file descriptors. An engine has nothing to open: the board
        // arrives in shared memory and the answer goes back down a pipe that
        // is already open. Without this it ran as the harness user with the
        // harness's whole filesystem -- it could read every other entry's
        // source out of entries/ and archive/, read the database, and write
        // results/<anything>.txt, which the server's sweep would then take in
        // as a match that never happened.
        libc::SYS_open as u32,
        libc::SYS_openat as u32,
        SYS_OPENAT2,
        // And nothing off the machine.
        libc::SYS_socket as u32,
        libc::SYS_connect as u32,
    ];

    let mut prog: Vec<SockFilter> = Vec::new();
    prog.push(SockFilter { code: LD_W_ABS, jt: 0, jf: 0, k: NR_OFFSET });
    for (i, nr) in blocked.iter().enumerate() {
        // On a match, jump to the final KILL. From this instruction at index
        // i+1, the KILL sits at index blocked.len()+2, and a jump of `jt`
        // lands at (i+1)+1+jt -- so jt is blocked.len() - i.
        let to_kill = (blocked.len() - i) as u8;
        prog.push(SockFilter { code: JMP_JEQ_K, jt: to_kill, jf: 0, k: *nr });
    }
    prog.push(SockFilter { code: RET_K, jt: 0, jf: 0, k: ALLOW });
    prog.push(SockFilter { code: RET_K, jt: 0, jf: 0, k: KILL });

    let fprog = SockFprog { len: prog.len() as u16, filter: prog.as_ptr() };
    unsafe {
        if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
            die("could not set no_new_privs; refusing to run an engine unfiltered");
        }
        if libc::syscall(libc::SYS_seccomp, 1 /* SECCOMP_SET_MODE_FILTER */, 0, &fprog) != 0 {
            die("could not install the seccomp filter; refusing to run an engine unfiltered");
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 5 {
        die("usage: engine-host <engine.so> <shm-fd> <req-fd> <resp-fd>");
    }
    let path = args[1].clone();
    let shm_fd: i32 = args[2].parse().unwrap_or_else(|_| die("bad shm fd"));
    let req_fd: i32 = args[3].parse().unwrap_or_else(|_| die("bad req fd"));
    let resp_fd: i32 = args[4].parse().unwrap_or_else(|_| die("bad resp fd"));

    let len = std::mem::size_of::<Shared>().next_multiple_of(4096);
    let p = unsafe {
        libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ | libc::PROT_WRITE,
                   libc::MAP_SHARED, shm_fd, 0)
    };
    if p == libc::MAP_FAILED {
        die("could not map the shared page");
    }
    let sh = unsafe { Shared::from_ptr(p as *mut u8) };

    let lib = match unsafe { Library::new(&path) } {
        Ok(l) => l,
        Err(e) => die(&format!("could not load {path}: {e}")),
    };
    let play: FnPlay = unsafe {
        let v: Symbol<FnVersion> = lib.get(b"hfc_abi_version").unwrap_or_else(|_| die("no hfc_abi_version"));
        if v() != hfc_abi::ABI_VERSION {
            die("ABI version mismatch");
        }
        let init: Symbol<FnVoid> = lib.get(b"hfc_init").unwrap_or_else(|_| die("no hfc_init"));
        // Everything above is the host's own startup and is not the engine's
        // to pay for. Say so before arming the filter and calling init.
        sh.started = 1;
        // Armed before init, not after: hfc_init is the one place an engine
        // could start a worker that outlives every measurement window.
        forbid_process_creation();
        let t0 = core::arch::x86_64::_rdtsc();
        init();
        sh.init_ticks = core::arch::x86_64::_rdtsc().saturating_sub(t0);
        *lib.get::<FnPlay>(b"hfc_play").unwrap_or_else(|_| die("no hfc_play"))
    };
    // The engine's own pacing counter. Advisory: the arbiter measures this
    // process from outside and does not trust anything reported from in here.
    let pacing = hfc_abi::Counter::new().ok();
    let page = pacing.as_ref().map(|c| c.page()).unwrap_or(std::ptr::null());

    sh.ready = 1;
    let mut cmd = [0u8; 1];
    loop {
        let n = unsafe { libc::read(req_fd, cmd.as_mut_ptr() as *mut libc::c_void, 1) };
        if n != 1 {
            break; // arbiter closed the pipe: the match is over
        }
        match cmd[0] {
            b'p' => {
                let pos = Position {
                    board: &sh.board as *const _,
                    history: sh.history.as_ptr(),
                    history_len: sh.history_len.min(hfc_abi::MAX_HISTORY as u32),
                    new_game: sh.new_game,
                };
                let b = Budget { perf_page: page, cycles: sh.cycles };
                sh.answer = unsafe { play(&pos, &b) };
            }
            _ => {}
        }
        let ack = [1u8; 1];
        if unsafe { libc::write(resp_fd, ack.as_ptr() as *const libc::c_void, 1) } != 1 {
            break;
        }
    }
    unsafe { libc::_exit(0) }
}
