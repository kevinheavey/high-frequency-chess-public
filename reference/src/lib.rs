use hfc_rules::board::{BLACK, Board, F_EP, PAWN, WHITE, init_tables, mv_flag, mv_from, mv_is_cap, mv_is_promo, mv_to};

// ---------------------------------------------------------------- evaluation

const PIECE_VAL: [i32; 6] = [100, 320, 330, 500, 900, 0];

#[rustfmt::skip]
const PST: [[i32; 64]; 6] = [
    // pawn
    [ 0,  0,  0,  0,  0,  0,  0,  0,
      5, 10, 10,-20,-20, 10, 10,  5,
      5, -5,-10,  0,  0,-10, -5,  5,
      0,  0,  0, 20, 20,  0,  0,  0,
      5,  5, 10, 25, 25, 10,  5,  5,
     10, 10, 20, 30, 30, 20, 10, 10,
     50, 50, 50, 50, 50, 50, 50, 50,
      0,  0,  0,  0,  0,  0,  0,  0],
    // knight
    [-50,-40,-30,-30,-30,-30,-40,-50,
     -40,-20,  0,  5,  5,  0,-20,-40,
     -30,  5, 10, 15, 15, 10,  5,-30,
     -30,  0, 15, 20, 20, 15,  0,-30,
     -30,  5, 15, 20, 20, 15,  5,-30,
     -30,  0, 10, 15, 15, 10,  0,-30,
     -40,-20,  0,  0,  0,  0,-20,-40,
     -50,-40,-30,-30,-30,-30,-40,-50],
    // bishop
    [-20,-10,-10,-10,-10,-10,-10,-20,
     -10,  5,  0,  0,  0,  0,  5,-10,
     -10, 10, 10, 10, 10, 10, 10,-10,
     -10,  0, 10, 10, 10, 10,  0,-10,
     -10,  5,  5, 10, 10,  5,  5,-10,
     -10,  0,  5, 10, 10,  5,  0,-10,
     -10,  0,  0,  0,  0,  0,  0,-10,
     -20,-10,-10,-10,-10,-10,-10,-20],
    // rook
    [  0,  0,  0,  5,  5,  0,  0,  0,
      -5,  0,  0,  0,  0,  0,  0, -5,
      -5,  0,  0,  0,  0,  0,  0, -5,
      -5,  0,  0,  0,  0,  0,  0, -5,
      -5,  0,  0,  0,  0,  0,  0, -5,
      -5,  0,  0,  0,  0,  0,  0, -5,
       5, 10, 10, 10, 10, 10, 10,  5,
       0,  0,  0,  0,  0,  0,  0,  0],
    // queen
    [-20,-10,-10, -5, -5,-10,-10,-20,
     -10,  0,  5,  0,  0,  0,  0,-10,
     -10,  5,  5,  5,  5,  5,  0,-10,
       0,  0,  5,  5,  5,  5,  0, -5,
      -5,  0,  5,  5,  5,  5,  0, -5,
     -10,  0,  5,  5,  5,  5,  0,-10,
     -10,  0,  0,  0,  0,  0,  0,-10,
     -20,-10,-10, -5, -5,-10,-10,-20],
    // king (midgame)
    [ 20, 30, 10,  0,  0, 10, 30, 20,
      20, 20,  0,  0,  0,  0, 20, 20,
     -10,-20,-20,-20,-20,-20,-20,-10,
     -20,-30,-30,-40,-40,-30,-30,-20,
     -30,-40,-40,-50,-50,-40,-40,-30,
     -30,-40,-40,-50,-50,-40,-40,-30,
     -30,-40,-40,-50,-50,-40,-40,-30,
     -30,-40,-40,-50,-50,-40,-40,-30],
];

pub fn eval(b: &Board) -> i32 {
    let mut score = 0i32;
    for p in 0..6 {
        let mut w = b.pieces[p] & b.colors[WHITE];
        while w != 0 {
            let sq = w.trailing_zeros() as usize;
            w &= w - 1;
            score += PIECE_VAL[p] + PST[p][sq];
        }
        let mut bl = b.pieces[p] & b.colors[BLACK];
        while bl != 0 {
            let sq = bl.trailing_zeros() as usize;
            bl &= bl - 1;
            score -= PIECE_VAL[p] + PST[p][sq ^ 56];
        }
    }
    if b.stm == WHITE {
        score
    } else {
        -score
    }
}

// -------------------------------------------------------------------- search

const MAX_PLY: usize = 96;

struct Arena {
    moves: [[u16; 256]; MAX_PLY],
    scores: [[i32; 256]; MAX_PLY],
}
static mut ARENA: Arena = Arena {
    moves: [[0; 256]; MAX_PLY],
    scores: [[0; 256]; MAX_PLY],
};

#[inline(always)]
fn ml(ply: usize) -> &'static mut [u16; 256] {
    unsafe { &mut (*core::ptr::addr_of_mut!(ARENA)).moves[ply.min(MAX_PLY - 1)] }
}
#[inline(always)]
fn sl(ply: usize) -> &'static mut [i32; 256] {
    unsafe { &mut (*core::ptr::addr_of_mut!(ARENA)).scores[ply.min(MAX_PLY - 1)] }
}

/// How many nodes between budget checks: up to this many nodes can elapse
/// past the deadline, so the interval scales with how much budget there is.
#[inline(always)]
fn check_interval(budget: u64) -> u32 {
    match budget {
        0..=200_000 => 1,
        200_001..=1_000_000 => 4,
        _ => 8,
    }
}

pub struct Searcher {
    pub nodes: u64,
    pub deadline: u64,
    pub page: *const u8,
    pub aborted: bool,
    check: u32,
    interval: u32,
}

const MATE: i32 = 30000;

impl Searcher {
    pub fn new() -> Searcher {
        Searcher {
            nodes: 0,
            deadline: u64::MAX,
            page: core::ptr::null(),
            aborted: false,
            check: 0,
            interval: 8,
        }
    }

    #[inline(always)]
    fn out_of_budget(&mut self) -> bool {
        if self.aborted {
            return true;
        }
        self.check += 1;
        if self.check >= self.interval {
            self.check = 0;
            if hfc_abi::now(self.page) >= self.deadline {
                self.aborted = true;
            }
        }
        self.aborted
    }

    fn order(&self, b: &Board, moves: &mut [u16; 256], scores: &mut [i32; 256], n: usize) {
        for i in 0..n {
            let m = moves[i];
            let mut s = 0;
            if mv_is_cap(m) {
                let victim = if mv_flag(m) == F_EP { PAWN } else { b.piece_at(mv_to(m)) };
                let attacker = b.piece_at(mv_from(m));
                s = 10_000 + PIECE_VAL[victim.min(5)] * 10 - PIECE_VAL[attacker.min(5)];
            }
            if mv_is_promo(m) {
                s += 9_000;
            }
            scores[i] = s;
        }
        for i in 1..n {
            let (ks, km) = (scores[i], moves[i]);
            let mut j = i;
            while j > 0 && scores[j - 1] < ks {
                scores[j] = scores[j - 1];
                moves[j] = moves[j - 1];
                j -= 1;
            }
            scores[j] = ks;
            moves[j] = km;
        }
    }

    pub fn qsearch(&mut self, b: &mut Board, mut alpha: i32, beta: i32, ply: usize) -> i32 {
        self.nodes += 1;
        if self.out_of_budget() {
            return alpha;
        }
        let stand = eval(b);
        if stand >= beta {
            return beta;
        }
        if stand > alpha {
            alpha = stand;
        }

        let moves = ml(ply);
        let n = b.gen_caps(moves);
        self.order(b, moves, sl(ply), n);

        for i in 0..n {
            let m = ml(ply)[i];
            let u = b.make(m);
            if b.in_check(b.stm ^ 1) {
                b.unmake(m, u);
                continue;
            }
            let s = -self.qsearch(b, -beta, -alpha, ply + 1);
            b.unmake(m, u);
            if s >= beta {
                return beta;
            }
            if s > alpha {
                alpha = s;
            }
        }
        alpha
    }

    pub fn alphabeta(&mut self, b: &mut Board, depth: i32, mut alpha: i32, beta: i32, ply: usize) -> i32 {
        if depth <= 0 {
            return self.qsearch(b, alpha, beta, ply);
        }
        self.nodes += 1;
        if self.out_of_budget() {
            return alpha;
        }

        let moves = ml(ply);
        let n = b.gen_moves(moves);
        self.order(b, moves, sl(ply), n);

        let mut legal = 0;
        for i in 0..n {
            let m = ml(ply)[i];
            let u = b.make(m);
            if b.in_check(b.stm ^ 1) {
                b.unmake(m, u);
                continue;
            }
            legal += 1;
            let s = -self.alphabeta(b, depth - 1, -beta, -alpha, ply + 1);
            b.unmake(m, u);
            if s >= beta {
                return beta;
            }
            if s > alpha {
                alpha = s;
            }
        }

        if legal == 0 {
            return if b.in_check(b.stm) { -MATE + ply as i32 } else { 0 };
        }
        alpha
    }

    /// The root moves arrive already legal, already ordered best-static-
    /// first, each with how many times the position after it has occurred.
    /// A move that would be the third occurrence is a draw, so it is scored
    /// 0 instead of searched — the engine then avoids it when winning and
    /// steers into it when losing.
    pub fn search_root(&mut self, b: &mut Board, depth: i32, reps: &[(u16, usize)]) -> (u16, i32) {
        let mut best = 0u16;
        let mut alpha = -MATE * 2;
        for &(m, count) in reps {
            // Once aborted, stop: every further make/unmake is overshoot.
            if self.aborted {
                break;
            }
            let u = b.make(m);
            let s = if count >= 2 {
                0
            } else {
                -self.alphabeta(b, depth - 1, -MATE * 2, -alpha, 1)
            };
            b.unmake(m, u);
            if best == 0 || s > alpha {
                alpha = s;
                best = m;
            }
        }
        (best, alpha)
    }
}

// ------------------------------------------------------------------- C ABI

/// Reserve a slice of the budget so that overshoot between checks cannot push
/// us past the hard limit the harness enforces. Overrunning forfeits the game,
/// so the asymmetry is worth a few percent of search.
const SAFETY_NUM: u64 = 94;
const SAFETY_DEN: u64 = 100;

/// The first move after loading runs on cold caches and costs more, so it
/// alone gets a wider margin.
const FIRST_MOVE_SAFETY_NUM: u64 = 88;
static mut MOVES_PLAYED: u32 = 0;

#[no_mangle]
pub extern "C" fn hfc_abi_version() -> u32 {
    hfc_abi::ABI_VERSION
}

#[no_mangle]
pub extern "C" fn hfc_init() {
    init_tables();
}

/// Choose a move for `pos` within the budget. Returns the encoded move, or 0
/// if the position has no legal moves.
///
/// # Safety
/// `pos` and `budget` must be valid pointers for the duration of the call.
#[no_mangle]
pub unsafe extern "C" fn hfc_play(pos: *const hfc_abi::Position, budget: *const hfc_abi::Budget) -> u16 {
    let b = &*budget;
    let start = hfc_abi::now(b.perf_page);

    let p = &*pos;
    // The wire format is our board layout, so this is a copy, not a parse.
    let mut board: Board = *p.board;
    let hist = p.history();
    let hn = hist.len();

    let mut s = Searcher::new();
    s.page = b.perf_page;
    let margin = unsafe {
        let first = MOVES_PLAYED == 0;
        MOVES_PLAYED = MOVES_PLAYED.saturating_add(1);
        if first { FIRST_MOVE_SAFETY_NUM } else { SAFETY_NUM }
    };
    s.deadline = start + b.cycles * margin / SAFETY_DEN;
    s.interval = check_interval(b.cycles);

    // Cheapest useful thing first: one legal move, so that from here on the
    // engine can always answer, whatever the budget turns out to be.
    let buf = ml(0);
    let n = board.gen_moves(buf);
    let mut reps: [(u16, usize); 256] = [(0, 0); 256];
    let mut scores: [i32; 256] = [0; 256];
    let mut rn = 0usize;

    for i in 0..n {
        let m = buf[i];
        let u = board.make(m);
        let legal = !board.in_check(board.stm ^ 1);
        board.unmake(m, u);
        if legal {
            reps[0] = (m, 0);
            rn = 1;
            buf.swap(0, i);
            break;
        }
    }
    if rn == 0 {
        return 0;
    }

    // The rest of the root moves, each with its repetition count and a
    // one-ply static score. The best static score is the move in hand -- on
    // sharp positions the search may complete no full iteration, and the
    // move in hand is what gets played -- and the same scores order the
    // root for earlier cutoffs.
    let need_reps = hn > 2;
    scores[0] = {
        let m = reps[0].0;
        let u = board.make(m);
        let sc = -eval(&board);
        board.unmake(m, u);
        sc
    };
    for i in 1..n {
        if i % (s.interval as usize) == 0 && hfc_abi::now(b.perf_page) >= s.deadline {
            return reps[0].0;
        }
        let m = buf[i];
        let u = board.make(m);
        let legal = !board.in_check(board.stm ^ 1);
        let sc = if legal { -eval(&board) } else { 0 };
        let h = if legal && need_reps { board.hash() } else { 0 };
        board.unmake(m, u);
        if !legal {
            continue;
        }
        let mut c = 0usize;
        if need_reps {
            for &x in hist.iter() {
                if x == h {
                    c += 1;
                }
            }
        }
        reps[rn] = (m, c);
        scores[rn] = sc;
        rn += 1;
        if rn == 256 {
            break;
        }
    }
    // Order by static score, best first. A repetition that would draw on
    // the spot is worth 0, not its board score -- exactly the value the
    // search will give it.
    for i in 0..rn {
        if reps[i].1 >= 2 {
            scores[i] = 0;
        }
    }
    for i in 1..rn {
        let (ks, kr) = (scores[i], reps[i]);
        let mut j = i;
        while j > 0 && scores[j - 1] < ks {
            scores[j] = scores[j - 1];
            reps[j] = reps[j - 1];
            j -= 1;
        }
        scores[j] = ks;
        reps[j] = kr;
    }

    // The greedy choice is the floor; iterative deepening improves on it
    // for as long as the budget lets whole iterations complete.
    let mut best = reps[0].0;
    if hfc_abi::now(b.perf_page) >= s.deadline {
        return best;
    }

    for depth in 1..64 {
        let (m, _) = s.search_root(&mut board, depth, &reps[..rn]);
        // An aborted iteration searched only part of the root moves, so its
        // result is not comparable with the completed one before it.
        if s.aborted {
            break;
        }
        if m != 0 {
            best = m;
        }
    }
    best
}
