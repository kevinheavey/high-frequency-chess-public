pub mod board;

use board::{BISHOP, BLACK, Board, F_KCASTLE, F_QCASTLE, KNIGHT, PAWN, QUEEN, ROOK, WHITE, mv_flag, mv_from, mv_is_cap, mv_is_promo, mv_promo_piece, mv_to};

// ------------------------------------------------------------------- zobrist

pub struct Zobrist {
    pieces: [[[u64; 64]; 6]; 2],
    castle: [u64; 16],
    ep: [u64; 8],
    stm: u64,
}

const fn splitmix64(x: &mut u64) -> u64 {
    *x = x.wrapping_add(0x9E3779B97F4A7C15);
    let mut z = *x;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
    z ^ (z >> 31)
}

pub static ZOBRIST: Zobrist = {
    let mut s = 0x1234_5678_9ABC_DEF0u64;
    let mut pieces = [[[0u64; 64]; 6]; 2];
    let mut c = 0;
    while c < 2 {
        let mut p = 0;
        while p < 6 {
            let mut sq = 0;
            while sq < 64 {
                pieces[c][p][sq] = splitmix64(&mut s);
                sq += 1;
            }
            p += 1;
        }
        c += 1;
    }
    let mut castle = [0u64; 16];
    let mut i = 0;
    while i < 16 {
        castle[i] = splitmix64(&mut s);
        i += 1;
    }
    let mut ep = [0u64; 8];
    let mut i = 0;
    while i < 8 {
        ep[i] = splitmix64(&mut s);
        i += 1;
    }
    Zobrist { pieces, castle, ep, stm: splitmix64(&mut s) }
};

impl Board {
    pub fn hash(&self) -> u64 {
        let mut h = 0u64;
        for c in 0..2 {
            for p in 0..6 {
                let mut bb = self.pieces[p] & self.colors[c];
                while bb != 0 {
                    let sq = bb.trailing_zeros() as usize;
                    bb &= bb - 1;
                    h ^= ZOBRIST.pieces[c][p][sq];
                }
            }
        }
        h ^= ZOBRIST.castle[(self.castle & 15) as usize];
        if self.ep < 64 {
            h ^= ZOBRIST.ep[(self.ep % 8) as usize];
        }
        if self.stm == BLACK {
            h ^= ZOBRIST.stm;
        }
        h
    }

    /// Fully legal moves into a caller-owned buffer. No allocation: the
    /// arbiter runs this on every ply of every game.
    pub fn legal_moves_into(&self, out: &mut [u16; 256]) -> usize {
        let mut buf = [0u16; 256];
        let n = self.gen_moves(&mut buf);
        let mut k = 0usize;
        let mut b = *self;
        for i in 0..n {
            let m = buf[i];
            let u = b.make(m);
            if !b.in_check(b.stm ^ 1) {
                out[k] = m;
                k += 1;
            }
            b.unmake(m, u);
        }
        k
    }

    /// Fully legal moves (pseudo-legal filtered by king safety).
    pub fn legal_moves(&self) -> Vec<u16> {
        let mut buf = [0u16; 256];
        let n = self.gen_moves(&mut buf);
        let mut out = Vec::with_capacity(n);
        let mut b = *self;
        for i in 0..n {
            let m = buf[i];
            let u = b.make(m);
            if !b.in_check(b.stm ^ 1) {
                out.push(m);
            }
            b.unmake(m, u);
        }
        out
    }

    pub fn to_fen(&self) -> String {
        let mut s = String::new();
        for r in (0..8).rev() {
            let mut empty = 0;
            for f in 0..8 {
                let sq = r * 8 + f;
                let p = self.mailbox[sq] as usize;
                if p == 6 {
                    empty += 1;
                    continue;
                }
                if empty > 0 {
                    s.push_str(&empty.to_string());
                    empty = 0;
                }
                let ch = b"pnbrqk"[p] as char;
                let white = self.colors[WHITE] & (1u64 << sq) != 0;
                s.push(if white { ch.to_ascii_uppercase() } else { ch });
            }
            if empty > 0 {
                s.push_str(&empty.to_string());
            }
            if r > 0 {
                s.push('/');
            }
        }
        s.push(' ');
        s.push(if self.stm == WHITE { 'w' } else { 'b' });
        s.push(' ');
        if self.castle == 0 {
            s.push('-');
        } else {
            for (bit, ch) in [(1u8, 'K'), (2, 'Q'), (4, 'k'), (8, 'q')] {
                if self.castle & bit != 0 {
                    s.push(ch);
                }
            }
        }
        s.push(' ');
        if self.ep < 64 {
            s.push((b'a' + (self.ep % 8)) as char);
            s.push((b'1' + (self.ep / 8)) as char);
        } else {
            s.push('-');
        }
        s.push_str(&format!(" {} 1", self.halfmove));
        s
    }

    pub fn insufficient_material(&self) -> bool {
        if self.pieces[PAWN] | self.pieces[ROOK] | self.pieces[QUEEN] != 0 {
            return false;
        }
        if (self.pieces[KNIGHT] | self.pieces[BISHOP]).count_ones() <= 1 {
            return true; // bare kings, or a lone minor
        }
        // Kings and bishops all on one colour complex is dead no matter how
        // many bishops or whose they are: every check falls on that complex,
        // and the escape squares on the other can never all be covered.
        // Promotion can put two same-complex bishops on one side, so the
        // count does not stop at two.
        if self.pieces[KNIGHT] == 0 {
            const DARK: u64 = 0xAA55_AA55_AA55_AA55;
            let b = self.pieces[BISHOP];
            return b & DARK == 0 || b & !DARK == 0;
        }
        false
    }
}

pub fn move_to_uci(m: u16) -> String {
    let s = |n: u32| {
        format!(
            "{}{}",
            (b'a' + (n % 8) as u8) as char,
            (b'1' + (n / 8) as u8) as char
        )
    };
    let mut out = format!("{}{}", s(mv_from(m)), s(mv_to(m)));
    if mv_is_promo(m) {
        out.push(b"nbrq"[mv_promo_piece(m) - 1] as char);
    }
    out
}

// ---------------------------------------------------------------- game state

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Outcome {
    Ongoing,
    Win(usize),  // colour that won
    Draw(&'static str),
    Forfeit(usize, &'static str), // colour that lost, reason
}

pub struct Game {
    pub board: Board,
    pub history: Vec<u64>,
    pub moves: Vec<u16>,
}

impl Game {
    pub fn from_fen(fen: &str) -> Game {
        let board = Board::from_fen(fen);
        let h = board.hash();
        Game { board, history: vec![h], moves: vec![] }
    }

    pub fn apply(&mut self, m: u16) {
        self.board.make(m);
        self.moves.push(m);
        self.history.push(self.board.hash());
    }

    pub fn outcome(&self) -> Outcome {
        let mut buf = [0u16; 256];
        self.outcome_with(self.board.legal_moves_into(&mut buf))
    }

    /// Outcome, given a legal-move count the caller has already computed.
    /// Saves regenerating them: the arbiter needs the list anyway, to check
    /// the move the engine returns.
    pub fn outcome_with(&self, legal: usize) -> Outcome {
        if legal == 0 {
            return if self.board.in_check(self.board.stm) {
                Outcome::Win(self.board.stm ^ 1)
            } else {
                Outcome::Draw("stalemate")
            };
        }
        if self.board.halfmove >= 100 {
            return Outcome::Draw("fifty-move");
        }
        if self.board.insufficient_material() {
            return Outcome::Draw("insufficient material");
        }
        let last = *self.history.last().unwrap();
        if self.history.iter().filter(|&&h| h == last).count() >= 3 {
            return Outcome::Draw("threefold repetition");
        }
        Outcome::Ongoing
    }
}

// ------------------------------------------------------------ SAN / PGN

fn file_ch(sq: u32) -> char { (b'a' + (sq % 8) as u8) as char }
fn rank_ch(sq: u32) -> char { (b'1' + (sq / 8) as u8) as char }
fn sq_str(sq: u32) -> String { format!("{}{}", file_ch(sq), rank_ch(sq)) }

/// Standard algebraic notation, with disambiguation and check/mate suffixes.
pub fn move_to_san(b: &Board, m: u16) -> String {
    let flag = mv_flag(m);
    let mut s = if flag == F_KCASTLE {
        "O-O".to_string()
    } else if flag == F_QCASTLE {
        "O-O-O".to_string()
    } else {
        let (from, to) = (mv_from(m), mv_to(m));
        let piece = b.piece_at(from);
        let mut s = String::new();
        if piece == PAWN {
            if mv_is_cap(m) {
                s.push(file_ch(from));
                s.push('x');
            }
            s.push_str(&sq_str(to));
            if mv_is_promo(m) {
                s.push('=');
                s.push(b"NBRQ"[mv_promo_piece(m) - 1] as char);
            }
        } else {
            s.push(b" NBRQK"[piece] as char);
            let rivals: Vec<u16> = b
                .legal_moves()
                .into_iter()
                .filter(|&o| o != m && mv_to(o) == to && b.piece_at(mv_from(o)) == piece)
                .collect();
            if !rivals.is_empty() {
                let same_file = rivals.iter().any(|&o| mv_from(o) % 8 == from % 8);
                let same_rank = rivals.iter().any(|&o| mv_from(o) / 8 == from / 8);
                if !same_file {
                    s.push(file_ch(from));
                } else if !same_rank {
                    s.push(rank_ch(from));
                } else {
                    s.push(file_ch(from));
                    s.push(rank_ch(from));
                }
            }
            if mv_is_cap(m) {
                s.push('x');
            }
            s.push_str(&sq_str(to));
        }
        s
    };

    let mut after = *b;
    after.make(m);
    if after.in_check(after.stm) {
        s.push(if after.legal_moves().is_empty() { '#' } else { '+' });
    }
    s
}

/// A PGN game. `moves` must be legal from `opening` in order.
pub fn to_pgn(
    opening: &str,
    moves: &[u16],
    white: &str,
    black: &str,
    result: Outcome,
    round: usize,
) -> String {
    let (res, term) = match result {
        Outcome::Win(c) => (if c == WHITE { "1-0" } else { "0-1" }, "normal".to_string()),
        Outcome::Draw(why) => ("1/2-1/2", why.to_string()),
        Outcome::Forfeit(loser, why) => (
            if loser == WHITE { "0-1" } else { "1-0" },
            format!("forfeit: {why}"),
        ),
        Outcome::Ongoing => ("*", "unfinished".to_string()),
    };

    let mut s = String::new();
    s.push_str(&format!("[Event \"High-Frequency Chess\"]\n[Round \"{round}\"]\n"));
    s.push_str(&format!("[White \"{white}\"]\n[Black \"{black}\"]\n"));
    s.push_str(&format!("[Result \"{res}\"]\n"));
    s.push_str(&format!("[FEN \"{opening}\"]\n[SetUp \"1\"]\n"));
    s.push_str(&format!("[Termination \"{term}\"]\n\n"));

    let mut b = Board::from_fen(opening);
    let mut n = 1;
    let mut line = String::new();
    if b.stm == BLACK {
        line.push_str(&format!("{n}... "));
    }
    for (i, &m) in moves.iter().enumerate() {
        if b.stm == WHITE {
            line.push_str(&format!("{n}. "));
        }
        line.push_str(&move_to_san(&b, m));
        line.push(' ');
        if b.stm == BLACK {
            n += 1;
        }
        b.make(m);
        if i % 8 == 7 {
            line.push('\n');
        }
    }
    line.push_str(res);
    s.push_str(&line);
    s.push_str("\n\n");
    s
}
