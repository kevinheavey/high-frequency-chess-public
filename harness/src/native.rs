//! An engine, running in its own process.
//!
//! Each engine is a separate process. Positions move through a shared page
//! with no copying, a pipe carries the wakeup, and the cycle counter is
//! opened by the arbiter *on the engine's pid* — the engine never holds the
//! descriptor and cannot close, reset or remap it.

use hfc_abi::{RemoteCounter, Shared, WireBoard};

/// Off by default: this is the hot loop and even a branch here is measurable.

pub struct Engine {
    pub name: String,
    pub last_cycles: u64,
    /// Set by [`Engine::begin_game`], carried on the next move.
    pending_new_game: bool,
    /// What loading cost: exec, linking, dlopen and `hfc_init`. Paid once per
    /// match, and currently bounded only by a 30-second deadline.
    pub load_cycles: u64,
    shm: *mut u8,
    req: i32,
    resp: i32,
    pid: i32,
    counter: RemoteCounter,
    dead: Option<String>,
}

#[derive(Debug)]
pub enum MoveOut {
    Move(u16),
    Forfeit(String),
}

fn wait_ms(pid: i32, ms: u64) -> Option<i32> {
    for _ in 0..(ms / 2).max(1) {
        let mut st = 0i32;
        let r = unsafe { libc::waitpid(pid, &mut st, libc::WNOHANG) };
        if r == pid {
            return Some(st);
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    None
}

fn describe_exit(st: i32) -> String {
    if libc::WIFSIGNALED(st) {
        let s = libc::WTERMSIG(st);
        let d = match s {
            libc::SIGSEGV => "SIGSEGV - bad pointer",
            libc::SIGBUS => "SIGBUS",
            libc::SIGILL => "SIGILL",
            libc::SIGFPE => "SIGFPE",
            libc::SIGABRT => "SIGABRT - panic or assertion",
            libc::SIGSYS => "SIGSYS - blocked syscall (tried to fork or spawn a thread?)",
            libc::SIGKILL => "SIGKILL - killed, out of memory?",
            _ => "unknown signal",
        };
        format!("crashed with signal {s} ({d})")
    } else {
        format!("exited with status {}", libc::WEXITSTATUS(st))
    }
}

impl Engine {
    pub fn load(path: &str) -> Result<Engine, String> {
        let host = "target/release/engine-host";
        if !std::path::Path::new(host).exists() {
            return Err(format!("{host} is missing; run ./build.sh"));
        }
        if !std::path::Path::new(path).exists() {
            return Err(format!("{path}: no such file"));
        }

        let (shm, shm_fd) = hfc_abi::map_shared()?;
        let mut rq = [0i32; 2];
        let mut rs = [0i32; 2];
        if unsafe { libc::pipe(rq.as_mut_ptr()) } != 0 || unsafe { libc::pipe(rs.as_mut_ptr()) } != 0
        {
            return Err("could not create pipes".into());
        }

        let pid = unsafe { libc::fork() };
        if pid < 0 {
            return Err("fork failed".into());
        }
        if pid == 0 {
            unsafe {
                libc::close(rq[1]);
                libc::close(rs[0]);
                // Memory ceiling, so a submission cannot exhaust the machine.
                let lim = libc::rlimit { rlim_cur: 256 << 20, rlim_max: 256 << 20 };
                libc::setrlimit(libc::RLIMIT_AS, &lim);

                let exe = std::ffi::CString::new(host).unwrap();
                let a1 = std::ffi::CString::new(path).unwrap();
                let a2 = std::ffi::CString::new(shm_fd.to_string()).unwrap();
                let a3 = std::ffi::CString::new(rq[0].to_string()).unwrap();
                let a4 = std::ffi::CString::new(rs[1].to_string()).unwrap();
                let argv = [
                    exe.as_ptr(), a1.as_ptr(), a2.as_ptr(), a3.as_ptr(), a4.as_ptr(),
                    std::ptr::null(),
                ];
                libc::execv(exe.as_ptr(), argv.as_ptr());
                libc::_exit(127);
            }
        }

        unsafe {
            libc::close(rq[0]);
            libc::close(rs[1]);
            // The child inherited its own copy at the fork, and the mapping
            // made above outlives the descriptor. Unclosed, this was one
            // leaked fd per load -- invisible in the one-match harness
            // process, fatal in the long-lived web server.
            libc::close(shm_fd);
        }

        // Opened here, after the fork, so the engine never sees this fd.
        // On failure the child is already alive and must not be stranded.
        let counter = match RemoteCounter::open(pid) {
            Ok(c) => c,
            Err(e) => {
                unsafe {
                    libc::kill(pid, libc::SIGKILL);
                    libc::waitpid(pid, std::ptr::null_mut(), 0);
                    libc::close(rq[1]);
                    libc::close(rs[0]);
                    libc::munmap(shm as *mut libc::c_void, hfc_abi::shared_len());
                }
                return Err(e);
            }
        };

        let sh = unsafe { Shared::from_ptr(shm) };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        // Wait for the host to be up before starting the clock: process
        // startup is not the engine's cost, and the counter is not installed
        // on a task until its next context switch anyway.
        while sh.started == 0 {
            if let Some(st) = wait_ms(pid, 2) {
                return Err(format!("{path}: engine host {}", describe_exit(st)));
            }
            if std::time::Instant::now() > deadline {
                unsafe { libc::kill(pid, libc::SIGKILL) };
                return Err(format!("{path}: engine host never started"));
            }
        }
        let load_t0 = counter.read();
        while sh.ready == 0 {
            if let Some(st) = wait_ms(pid, 2) {
                return Err(format!("{path}: engine host {}", describe_exit(st)));
            }
            if std::time::Instant::now() > deadline {
                unsafe { libc::kill(pid, libc::SIGKILL) };
                return Err(format!("{path}: hfc_init did not finish within 30s"));
            }
        }

        // Cap `hfc_init` at ten moves' worth of work. It runs once per match
        // before any measurement window exists, and the book is public and
        // frozen -- unbounded setup would be unbounded offline analysis of
        // the positions every game starts from. Ticks, not core cycles: the
        // host cannot read the arbiter's counter, and a tick is worth more
        // than a cycle here, so the cap errs loose.
        const INIT_TICKS: u64 = 10 * crate::MOVE_CYCLES;
        if sh.init_ticks > INIT_TICKS {
            unsafe { libc::kill(pid, libc::SIGKILL) };
            let _ = wait_ms(pid, 60);
            return Err(format!(
                "{path}: hfc_init spent {} reference ticks, limit is {INIT_TICKS} \
                 ({}x a move budget). Setup is for tables and weights, not for \
                 precomputing against the book.",
                sh.init_ticks,
                sh.init_ticks / crate::MOVE_CYCLES
            ));
        }

        let load_t1 = counter.read();
        let _ = (load_t0, load_t1);
        // Taken from the host, which timed the call itself.
        let load_cycles = sh.init_ticks;

        let name = std::path::Path::new(path)
            .file_stem()
            .map(|s| s.to_string_lossy().trim_start_matches("lib").to_string())
            .unwrap_or_else(|| path.to_string());

        Ok(Engine { name, last_cycles: 0, pending_new_game: false, load_cycles, shm, req: rq[1], resp: rs[0], pid, counter, dead: None })
    }

    /// Wake the host and wait for its acknowledgement.
    fn round_trip(&mut self, cmd: u8) -> Result<(), String> {
        if let Some(d) = &self.dead {
            return Err(d.clone());
        }
        let c = [cmd];
        if unsafe { libc::write(self.req, c.as_ptr() as *const libc::c_void, 1) } != 1 {
            return Err(self.diagnose());
        }
        // Bounded, not blocking.
        // The bound is wall clock because a hung engine's whole point is that
        // its cycle count never arrives. It is enormously generous -- a
        // 150,000-cycle move is about 30 microseconds, so this is roughly five
        // orders of magnitude of slack, and cannot fire on a slow machine, a
        // loaded machine, or a large budget. Hanging is sticky, so the rest of
        // the match forfeits immediately rather than waiting again per move.
        const REPLY_TIMEOUT_MS: i32 = 5_000;
        let mut ack = [0u8; 1];
        loop {
            let mut pfd = libc::pollfd { fd: self.resp, events: libc::POLLIN, revents: 0 };
            let r = unsafe { libc::poll(&mut pfd, 1, REPLY_TIMEOUT_MS) };
            if r < 0 {
                if unsafe { *libc::__errno_location() } == libc::EINTR {
                    continue;
                }
                return Err(self.diagnose());
            }
            if r == 0 {
                unsafe { libc::kill(self.pid, libc::SIGKILL) };
                let _ = wait_ms(self.pid, 60);
                let why = format!(
                    "{} hung: no reply within {REPLY_TIMEOUT_MS}ms", self.name);
                self.dead = Some(why.clone());
                return Err(why);
            }
            break;
        }
        if unsafe { libc::read(self.resp, ack.as_mut_ptr() as *mut libc::c_void, 1) } != 1 {
            return Err(self.diagnose());
        }
        Ok(())
    }

    /// The pipe broke: find out why, and remember it.
    fn diagnose(&mut self) -> String {
        let why = match wait_ms(self.pid, 60) {
            Some(st) => format!("{} {}", self.name, describe_exit(st)),
            None => {
                unsafe { libc::kill(self.pid, libc::SIGKILL) };
                let _ = wait_ms(self.pid, 60);
                format!("{} stopped responding", self.name)
            }
        };
        self.dead = Some(why.clone());
        why
    }

    pub fn is_dead(&self) -> bool {
        self.dead.is_some()
    }

    /// Mark that the next move this engine is asked for begins a new game.
    /// There is no round trip and nothing to meter: the flag rides along on
    /// the next `hfc_play`, and whatever the engine does about it comes out
    /// of that move's budget.
    pub fn begin_game(&mut self) {
        self.pending_new_game = true;
    }

    pub fn play(&mut self, board: &WireBoard, history: &[u64], budget: u64) -> MoveOut {
        let sh = unsafe { Shared::from_ptr(self.shm) };
        sh.board = *board;
        // No bound check here: `sh.history` is a fixed [u64; MAX_HISTORY], so
        // the slice below already panics if this ever overflows, in release,
        // on this line. An assert would only reword the message.
        //
        // It cannot overflow: the arbiter clears the history when the
        // halfmove clock resets, and `outcome_with` draws the game at
        // halfmove 100 before `play` is called, so at most 100 of the 256
        // slots are ever used.
        //
        sh.history[..history.len()].copy_from_slice(history);
        sh.history_len = history.len() as u32;

        sh.cycles = budget;
        sh.answer = 0;
        sh.new_game = u32::from(self.pending_new_game);
        self.pending_new_game = false;

        // An attempt that dies before a measurement exists must not report
        // the previous move's cost as its own.
        self.last_cycles = 0;
        let t0 = self.counter.read();
        if let Err(why) = self.round_trip(b'p') {
            return MoveOut::Forfeit(why);
        }
        let t1 = self.counter.read();

        if t0 == u64::MAX || t1 == u64::MAX {
            return MoveOut::Forfeit(
                "cycle counter unreadable across the move (the PMU was time-shared)".into(),
            );
        }
        if t1 < t0 {
            return MoveOut::Forfeit(format!("cycle counter went backwards ({t0} -> {t1})"));
        }
        let used = t1 - t0;
        self.last_cycles = used;

        // The counter reads bracket a pipe round trip, so `used` includes
        // wakeup the engine cannot observe, let alone control; the grace
        // covers that overhead and nothing more.
        const GRACE: u64 = 2_000;
        if used > budget + GRACE {
            return MoveOut::Forfeit(format!(
                "budget overrun: spent {used} cycles of {budget} ({:+.1}%, grace {GRACE})",
                (used as f64 / budget as f64 - 1.0) * 100.0
            ));
        }
        let m = sh.answer;
        if m == 0 {
            return MoveOut::Forfeit("returned move 0 (no move chosen)".into());
        }
        MoveOut::Move(m)
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.req);
            libc::close(self.resp);
            libc::kill(self.pid, libc::SIGKILL);
        }
        let _ = wait_ms(self.pid, 60);
        unsafe { libc::munmap(self.shm as *mut libc::c_void, hfc_abi::shared_len()) };
    }
}
