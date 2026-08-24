// Bitboard move generation. Classical ray attacks for sliders.
// Validated against standard perft positions.

pub const WHITE: usize = 0;
pub const BLACK: usize = 1;

pub const PAWN: usize = 0;
pub const KNIGHT: usize = 1;
pub const BISHOP: usize = 2;
pub const ROOK: usize = 3;
pub const QUEEN: usize = 4;
pub const KING: usize = 5;

// move flags
pub const F_QUIET: u16 = 0;
pub const F_DOUBLE: u16 = 1;
pub const F_KCASTLE: u16 = 2;
pub const F_QCASTLE: u16 = 3;
pub const F_CAPTURE: u16 = 4;
pub const F_EP: u16 = 5;
pub const F_PROMO_N: u16 = 8;
pub const F_PROMO_Q: u16 = 11;

#[inline(always)]
pub fn mv(from: u32, to: u32, flag: u16) -> u16 {
    (from as u16) | ((to as u16) << 6) | (flag << 12)
}
#[inline(always)]
pub fn mv_from(m: u16) -> u32 {
    (m & 63) as u32
}
#[inline(always)]
pub fn mv_to(m: u16) -> u32 {
    ((m >> 6) & 63) as u32
}
#[inline(always)]
pub fn mv_flag(m: u16) -> u16 {
    m >> 12
}
#[inline(always)]
pub fn mv_is_cap(m: u16) -> bool {
    (m >> 12) & 4 != 0
}
#[inline(always)]
pub fn mv_is_promo(m: u16) -> bool {
    (m >> 12) & 8 != 0
}
#[inline(always)]
pub fn mv_promo_piece(m: u16) -> usize {
    (((m >> 12) & 3) + 1) as usize // N,B,R,Q
}

pub struct Tables {
    pub knight: [u64; 64],
    pub king: [u64; 64],
    pub pawn: [[u64; 64]; 2],
    pub ray: [[u64; 64]; 8], // N NE E SE S SW W NW
    pub between: [[u64; 64]; 64],
}

static mut TABLES: Tables = Tables {
    knight: [0; 64],
    king: [0; 64],
    pawn: [[0; 64]; 2],
    ray: [[0; 64]; 8],
    between: [[0; 64]; 64],
};

#[inline(always)]
pub fn tb() -> &'static Tables {
    unsafe { &*core::ptr::addr_of!(TABLES) }
}

const DIRS: [(i32, i32); 8] = [
    (0, 1),
    (1, 1),
    (1, 0),
    (1, -1),
    (0, -1),
    (-1, -1),
    (-1, 0),
    (-1, 1),
];

pub fn init_tables() {
    // Callers cannot easily know whether someone else has already done
    // this, so they all just call it -- possibly from several threads at
    // once. `Once` makes that sound, and costs one relaxed load once warm.
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(init_tables_uncond);
}

fn init_tables_uncond() {
    let t = unsafe { &mut *core::ptr::addr_of_mut!(TABLES) };

    for sq in 0..64i32 {
        let (f, r) = (sq % 8, sq / 8);
        // knight
        let mut k = 0u64;
        for (df, dr) in [
            (1, 2),
            (2, 1),
            (2, -1),
            (1, -2),
            (-1, -2),
            (-2, -1),
            (-2, 1),
            (-1, 2),
        ] {
            let (nf, nr) = (f + df, r + dr);
            if (0..8).contains(&nf) && (0..8).contains(&nr) {
                k |= 1u64 << (nr * 8 + nf);
            }
        }
        t.knight[sq as usize] = k;

        // king
        let mut kg = 0u64;
        for (df, dr) in DIRS {
            let (nf, nr) = (f + df, r + dr);
            if (0..8).contains(&nf) && (0..8).contains(&nr) {
                kg |= 1u64 << (nr * 8 + nf);
            }
        }
        t.king[sq as usize] = kg;

        // pawn attacks
        for (c, dr) in [(WHITE, 1i32), (BLACK, -1i32)] {
            let mut p = 0u64;
            for df in [-1i32, 1] {
                let (nf, nr) = (f + df, r + dr);
                if (0..8).contains(&nf) && (0..8).contains(&nr) {
                    p |= 1u64 << (nr * 8 + nf);
                }
            }
            t.pawn[c][sq as usize] = p;
        }

        // rays
        for (d, (df, dr)) in DIRS.iter().enumerate() {
            let mut b = 0u64;
            let (mut nf, mut nr) = (f + df, r + dr);
            while (0..8).contains(&nf) && (0..8).contains(&nr) {
                b |= 1u64 << (nr * 8 + nf);
                nf += df;
                nr += dr;
            }
            t.ray[d][sq as usize] = b;
        }
    }

    // between[a][b] = squares strictly between a and b on a shared line
    for a in 0..64usize {
        for d in 0..8usize {
            let mut path = 0u64;
            let (df, dr) = DIRS[d];
            let (mut nf, mut nr) = ((a % 8) as i32 + df, (a / 8) as i32 + dr);
            while (0..8).contains(&nf) && (0..8).contains(&nr) {
                let b = (nr * 8 + nf) as usize;
                t.between[a][b] = path;
                path |= 1u64 << b;
                nf += df;
                nr += dr;
            }
        }
    }
}

#[inline(always)]
fn ray_att(d: usize, sq: u32, occ: u64) -> u64 {
    let t = tb();
    let att = t.ray[d][sq as usize];
    let blockers = att & occ;
    if blockers == 0 {
        return att;
    }
    // dirs 0,1,2,7 are "positive" (increasing square index)
    let first = if d <= 2 || d == 7 {
        blockers.trailing_zeros()
    } else {
        63 - blockers.leading_zeros()
    };
    att ^ t.ray[d][first as usize]
}

#[inline(always)]
pub fn rook_att(sq: u32, occ: u64) -> u64 {
    ray_att(0, sq, occ) | ray_att(2, sq, occ) | ray_att(4, sq, occ) | ray_att(6, sq, occ)
}
#[inline(always)]
pub fn bishop_att(sq: u32, occ: u64) -> u64 {
    ray_att(1, sq, occ) | ray_att(3, sq, occ) | ray_att(5, sq, occ) | ray_att(7, sq, occ)
}
#[inline(always)]
pub fn queen_att(sq: u32, occ: u64) -> u64 {
    rook_att(sq, occ) | bishop_att(sq, occ)
}

/// The wire format. `repr(C)` and stable: the harness hands engines a pointer
/// to exactly this, so an engine using this layout copies 88 bytes and is done.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Board {
    pub pieces: [u64; 6],
    pub colors: [u64; 2],
    pub mailbox: [u8; 64], // 6 = empty
    pub stm: usize,
    pub castle: u8, // 1=WK 2=WQ 4=BK 8=BQ
    pub ep: u8,     // 64 = none
    pub halfmove: u8,
}

#[derive(Clone, Copy)]
pub struct Undo {
    pub captured: u8, // 6 = none
    pub castle: u8,
    pub ep: u8,
    pub halfmove: u8,
}

const CASTLE_MASK: [u8; 64] = {
    let mut m = [15u8; 64];
    m[0] = 13; // a1 -> clear WQ
    m[4] = 12; // e1 -> clear WK|WQ
    m[7] = 14; // h1 -> clear WK
    m[56] = 7; // a8 -> clear BQ
    m[60] = 3; // e8 -> clear BK|BQ
    m[63] = 11; // h8 -> clear BK
    m
};

impl Board {
    pub fn empty() -> Board {
        Board {
            pieces: [0; 6],
            colors: [0; 2],
            mailbox: [6; 64],
            stm: WHITE,
            castle: 0,
            ep: 64,
            halfmove: 0,
        }
    }

    #[inline(always)]
    pub fn occ(&self) -> u64 {
        self.colors[0] | self.colors[1]
    }

    #[inline(always)]
    pub fn piece_at(&self, sq: u32) -> usize {
        self.mailbox[sq as usize] as usize
    }

    /// Parse a FEN, rejecting anything that does not describe a real
    /// position. These checks stop a bad string becoming a bad `Board`, not
    /// a full legality audit: material counts, for instance, are not
    /// policed, because an odd but playable position is not this function's
    /// business.
    pub fn try_from_fen(fen: &str) -> Result<Board, String> {
        init_tables();
        let mut b = Board::empty();
        let mut parts = fen.split_whitespace();
        let placement = parts.next().unwrap_or("");

        let ranks: Vec<&str> = placement.split('/').collect();
        if ranks.len() != 8 {
            return Err(format!("placement has {} ranks, expected 8", ranks.len()));
        }
        for (i, rank) in ranks.iter().enumerate() {
            let r = 7 - i as i32;
            let mut f = 0i32;
            for ch in rank.chars() {
                if let Some(n) = ch.to_digit(10) {
                    if n == 0 || n > 8 {
                        return Err(format!("rank {}: '{ch}' is not a run of 1-8", r + 1));
                    }
                    f += n as i32;
                    continue;
                }
                let p = match ch.to_ascii_lowercase() {
                    'p' => PAWN, 'n' => KNIGHT, 'b' => BISHOP,
                    'r' => ROOK, 'q' => QUEEN, 'k' => KING,
                    _ => return Err(format!("rank {}: '{ch}' is not a piece", r + 1)),
                };
                if f > 7 {
                    return Err(format!("rank {} describes more than 8 squares", r + 1));
                }
                if p == PAWN && (r == 0 || r == 7) {
                    return Err(format!("a pawn on rank {}", r + 1));
                }
                let color = if ch.is_ascii_uppercase() { WHITE } else { BLACK };
                let sq = (r * 8 + f) as u32;
                b.pieces[p] |= 1u64 << sq;
                b.colors[color] |= 1u64 << sq;
                b.mailbox[sq as usize] = p as u8;
                f += 1;
            }
            if f != 8 {
                return Err(format!("rank {} describes {f} squares, expected 8", r + 1));
            }
        }
        for (c, who) in [(WHITE, "white"), (BLACK, "black")] {
            let n = (b.pieces[KING] & b.colors[c]).count_ones();
            if n != 1 {
                return Err(format!("{who} has {n} kings, expected 1"));
            }
        }

        b.stm = match parts.next() {
            Some("w") | None => WHITE,
            Some("b") => BLACK,
            Some(x) => return Err(format!("side to move is '{x}', expected w or b")),
        };

        if let Some(c) = parts.next() {
            if c != "-" {
                for ch in c.chars() {
                    // Each right needs the king at home and the rook still on
                    // its corner; without that the castle is generated and
                    // `make` walks into an empty square.
                    let (bit, king, rook, name) = match ch {
                        'K' => (1u8, 4u32, 7u32, "white kingside"),
                        'Q' => (2, 4, 0, "white queenside"),
                        'k' => (4, 60, 63, "black kingside"),
                        'q' => (8, 60, 56, "black queenside"),
                        _ => return Err(format!("'{ch}' is not a castling right")),
                    };
                    let side = if ch.is_ascii_uppercase() { WHITE } else { BLACK };
                    let has = |p: usize, sq: u32| b.pieces[p] & b.colors[side] & (1u64 << sq) != 0;
                    if !has(KING, king) || !has(ROOK, rook) {
                        return Err(format!(
                            "claims {name} castling, but the king or rook has moved"));
                    }
                    b.castle |= bit;
                }
            }
        }

        b.ep = match parts.next() {
            Some(s) if s != "-" => {
                let bs = s.as_bytes();
                if bs.len() != 2 || !(b'a'..=b'h').contains(&bs[0]) || !(b'1'..=b'8').contains(&bs[1]) {
                    return Err(format!("en passant square '{s}' is not a square"));
                }
                let sq = (bs[1] - b'1') * 8 + (bs[0] - b'a');
                let want = if b.stm == WHITE { 5 } else { 2 };
                if sq / 8 != want {
                    return Err(format!(
                        "en passant square '{s}' is not on rank {}", want + 1));
                }
                // The pawn that just double-pushed must actually be there.
                let pawn = if b.stm == WHITE { sq - 8 } else { sq + 8 };
                if b.pieces[PAWN] & b.colors[b.stm ^ 1] & (1u64 << pawn) == 0 {
                    return Err(format!("en passant square '{s}' with no pawn to capture"));
                }
                // Normalised, not rejected, the same way `make` records it:
                // a square nobody can capture into describes a position
                // identical to one without it, and keeping it would give the
                // two different Zobrist keys.
                let file = pawn % 8;
                let mut beside = 0u64;
                if file > 0 { beside |= 1u64 << (pawn - 1) }
                if file < 7 { beside |= 1u64 << (pawn + 1) }
                if b.pieces[PAWN] & b.colors[b.stm] & beside == 0 {
                    64
                } else {
                    sq
                }
            }
            _ => 64,
        };

        b.halfmove = match parts.next() {
            Some(s) => s.parse().map_err(|_| format!("halfmove clock '{s}' is not a number"))?,
            None => 0,
        };

        // The side that just moved must not have left its own king attacked --
        // that position is not reachable, and the side to move could capture a
        // king, which movegen does not expect to be possible.
        if b.in_check(b.stm ^ 1) {
            return Err("the side not to move is in check".into());
        }
        Ok(b)
    }

    /// Parse a FEN, panicking on a bad one. For literals and for input already
    /// validated on the way in; anything from a file or a request should use
    /// [`Board::try_from_fen`] and report the error.
    pub fn from_fen(fen: &str) -> Board {
        match Board::try_from_fen(fen) {
            Ok(b) => b,
            Err(e) => panic!("bad FEN {fen:?}: {e}"),
        }
    }


    pub fn attacked(&self, sq: u32, by: usize) -> bool {
        let t = tb();
        let occ = self.occ();
        let them = self.colors[by];
        if t.pawn[by ^ 1][sq as usize] & self.pieces[PAWN] & them != 0 {
            return true;
        }
        if t.knight[sq as usize] & self.pieces[KNIGHT] & them != 0 {
            return true;
        }
        if t.king[sq as usize] & self.pieces[KING] & them != 0 {
            return true;
        }
        let bq = (self.pieces[BISHOP] | self.pieces[QUEEN]) & them;
        if bq != 0 && bishop_att(sq, occ) & bq != 0 {
            return true;
        }
        let rq = (self.pieces[ROOK] | self.pieces[QUEEN]) & them;
        if rq != 0 && rook_att(sq, occ) & rq != 0 {
            return true;
        }
        false
    }

    #[inline(always)]
    pub fn king_sq(&self, c: usize) -> u32 {
        (self.pieces[KING] & self.colors[c]).trailing_zeros()
    }

    pub fn in_check(&self, c: usize) -> bool {
        self.attacked(self.king_sq(c), c ^ 1)
    }

    #[inline(always)]
    pub fn gen_moves(&self, out: &mut [u16; 256]) -> usize {
        self.gen::<false>(out)
    }
    #[inline(always)]
    pub fn gen_caps(&self, out: &mut [u16; 256]) -> usize {
        self.gen::<true>(out)
    }

    pub fn gen<const CAPS: bool>(&self, out: &mut [u16; 256]) -> usize {
        let mut n = 0usize;
        let t = tb();
        let us = self.stm;
        let them = us ^ 1;
        let occ = self.occ();
        let own = self.colors[us];
        let opp = self.colors[them];

        // pawns
        let pawns = self.pieces[PAWN] & own;
        let (push, rank2, rank7) = if us == WHITE {
            (8i32, 0x0000_0000_0000_FF00u64, 0x00FF_0000_0000_0000u64)
        } else {
            (-8i32, 0x00FF_0000_0000_0000u64, 0x0000_0000_0000_FF00u64)
        };
        let mut p = pawns;
        while p != 0 {
            let from = p.trailing_zeros();
            p &= p - 1;
            let fb = 1u64 << from;
            let to = (from as i32 + push) as u32;
            if occ & (1u64 << to) == 0 && (!CAPS || fb & rank7 != 0) {
                if fb & rank7 != 0 {
                    for fl in [F_PROMO_N, F_PROMO_N + 1, F_PROMO_N + 2, F_PROMO_Q] {
                        out[n] = mv(from, to, fl);
                        n += 1;
                    }
                } else {
                    out[n] = mv(from, to, F_QUIET);
                    n += 1;
                    if fb & rank2 != 0 {
                        let to2 = (from as i32 + 2 * push) as u32;
                        if occ & (1u64 << to2) == 0 {
                            out[n] = mv(from, to2, F_DOUBLE);
                            n += 1;
                        }
                    }
                }
            }
            let mut caps = t.pawn[us][from as usize] & opp;
            while caps != 0 {
                let c = caps.trailing_zeros();
                caps &= caps - 1;
                if fb & rank7 != 0 {
                    for fl in [
                        F_PROMO_N | 4,
                        (F_PROMO_N + 1) | 4,
                        (F_PROMO_N + 2) | 4,
                        F_PROMO_Q | 4,
                    ] {
                        out[n] = mv(from, c, fl);
                        n += 1;
                    }
                } else {
                    out[n] = mv(from, c, F_CAPTURE);
                    n += 1;
                }
            }
            if self.ep < 64 && t.pawn[us][from as usize] & (1u64 << self.ep) != 0 {
                out[n] = mv(from, self.ep as u32, F_EP);
                n += 1;
            }
        }

        // knights / king
        for (pc, tab) in [(KNIGHT, &t.knight), (KING, &t.king)] {
            let mut b = self.pieces[pc] & own;
            while b != 0 {
                let from = b.trailing_zeros();
                b &= b - 1;
                let mut a = tab[from as usize] & !own;
                if CAPS { a &= opp; }
                while a != 0 {
                    let to = a.trailing_zeros();
                    a &= a - 1;
                    let fl = if opp & (1u64 << to) != 0 {
                        F_CAPTURE
                    } else {
                        F_QUIET
                    };
                    out[n] = mv(from, to, fl);
                    n += 1;
                }
            }
        }

        // sliders
        for pc in [BISHOP, ROOK, QUEEN] {
            let mut b = self.pieces[pc] & own;
            while b != 0 {
                let from = b.trailing_zeros();
                b &= b - 1;
                let mut a = match pc {
                    BISHOP => bishop_att(from, occ),
                    ROOK => rook_att(from, occ),
                    _ => queen_att(from, occ),
                } & !own;
                if CAPS { a &= opp; }
                while a != 0 {
                    let to = a.trailing_zeros();
                    a &= a - 1;
                    let fl = if opp & (1u64 << to) != 0 {
                        F_CAPTURE
                    } else {
                        F_QUIET
                    };
                    out[n] = mv(from, to, fl);
                    n += 1;
                }
            }
        }

        // castling
        if CAPS { return n; }
        let (ks_bit, qs_bit, e, f1, g1, d1, c1, b1) = if us == WHITE {
            (1u8, 2u8, 4u32, 5u32, 6u32, 3u32, 2u32, 1u32)
        } else {
            (4u8, 8u8, 60u32, 61u32, 62u32, 59u32, 58u32, 57u32)
        };
        if self.castle & ks_bit != 0
            && occ & ((1 << f1) | (1 << g1)) == 0
            && !self.attacked(e, them)
            && !self.attacked(f1, them)
            && !self.attacked(g1, them)
        {
            out[n] = mv(e, g1, F_KCASTLE);
            n += 1;
        }
        if self.castle & qs_bit != 0
            && occ & ((1 << d1) | (1 << c1) | (1 << b1)) == 0
            && !self.attacked(e, them)
            && !self.attacked(d1, them)
            && !self.attacked(c1, them)
        {
            out[n] = mv(e, c1, F_QCASTLE);
            n += 1;
        }

        n
    }

    pub fn make(&mut self, m: u16) -> Undo {
        let us = self.stm;
        let them = us ^ 1;
        let from = mv_from(m);
        let to = mv_to(m);
        let flag = mv_flag(m);
        let fb = 1u64 << from;
        let tbit = 1u64 << to;

        let u = Undo {
            captured: 6,
            castle: self.castle,
            ep: self.ep,
            halfmove: self.halfmove,
        };
        let mut u = u;

        let moving = self.piece_at(from);

        // remove captured
        if flag == F_EP {
            let cap_sq = if us == WHITE { to - 8 } else { to + 8 };
            self.pieces[PAWN] &= !(1u64 << cap_sq);
            self.colors[them] &= !(1u64 << cap_sq);
            self.mailbox[cap_sq as usize] = 6;
            u.captured = PAWN as u8;
        } else if self.colors[them] & tbit != 0 {
            let cp = self.piece_at(to);
            self.pieces[cp] &= !tbit;
            self.colors[them] &= !tbit;
            u.captured = cp as u8;
        }

        // move the piece
        self.pieces[moving] &= !fb;
        self.colors[us] &= !fb;
        let landed = if mv_is_promo(m) {
            mv_promo_piece(m)
        } else {
            moving
        };
        self.pieces[landed] |= tbit;
        self.colors[us] |= tbit;
        self.mailbox[from as usize] = 6;
        self.mailbox[to as usize] = landed as u8;

        // rook for castling
        if flag == F_KCASTLE {
            let (rf, rt) = if us == WHITE { (7u32, 5u32) } else { (63, 61) };
            self.pieces[ROOK] ^= (1u64 << rf) | (1u64 << rt);
            self.colors[us] ^= (1u64 << rf) | (1u64 << rt);
            self.mailbox[rf as usize] = 6;
            self.mailbox[rt as usize] = ROOK as u8;
        } else if flag == F_QCASTLE {
            let (rf, rt) = if us == WHITE { (0u32, 3u32) } else { (56, 59) };
            self.pieces[ROOK] ^= (1u64 << rf) | (1u64 << rt);
            self.colors[us] ^= (1u64 << rf) | (1u64 << rt);
            self.mailbox[rf as usize] = 6;
            self.mailbox[rt as usize] = ROOK as u8;
        }

        self.castle &= CASTLE_MASK[from as usize] & CASTLE_MASK[to as usize];
        // Only record an en passant square that someone can actually capture
        // into: FIDE calls two positions the same when the possible moves of
        // both players are the same, and a square no pawn can take on would
        // change the Zobrist key without changing the position. A pawn
        // merely standing alongside is enough; pins are not checked, which
        // is the conservative direction -- it cannot remove a move that
        // exists.
        self.ep = if flag == F_DOUBLE {
            let file = to % 8;
            let mut beside = 0u64;
            if file > 0 { beside |= 1u64 << (to - 1) }
            if file < 7 { beside |= 1u64 << (to + 1) }
            if self.pieces[PAWN] & self.colors[them] & beside != 0 {
                ((from as i32 + if us == WHITE { 8 } else { -8 }) as u8).min(64)
            } else {
                64
            }
        } else {
            64
        };
        self.halfmove = if moving == PAWN || u.captured != 6 {
            0
        } else {
            self.halfmove.saturating_add(1)
        };
        self.stm = them;
        u
    }

    pub fn unmake(&mut self, m: u16, u: Undo) {
        let them = self.stm;
        let us = them ^ 1;
        self.stm = us;
        let from = mv_from(m);
        let to = mv_to(m);
        let flag = mv_flag(m);
        let fb = 1u64 << from;
        let tbit = 1u64 << to;

        let landed = self.piece_at(to);
        let orig = if mv_is_promo(m) { PAWN } else { landed };

        self.pieces[landed] &= !tbit;
        self.colors[us] &= !tbit;
        self.pieces[orig] |= fb;
        self.colors[us] |= fb;
        self.mailbox[to as usize] = 6;
        self.mailbox[from as usize] = orig as u8;

        if flag == F_KCASTLE {
            let (rf, rt) = if us == WHITE { (7u32, 5u32) } else { (63, 61) };
            self.pieces[ROOK] ^= (1u64 << rf) | (1u64 << rt);
            self.colors[us] ^= (1u64 << rf) | (1u64 << rt);
            self.mailbox[rt as usize] = 6;
            self.mailbox[rf as usize] = ROOK as u8;
        } else if flag == F_QCASTLE {
            let (rf, rt) = if us == WHITE { (0u32, 3u32) } else { (56, 59) };
            self.pieces[ROOK] ^= (1u64 << rf) | (1u64 << rt);
            self.colors[us] ^= (1u64 << rf) | (1u64 << rt);
            self.mailbox[rt as usize] = 6;
            self.mailbox[rf as usize] = ROOK as u8;
        }

        if u.captured != 6 {
            if flag == F_EP {
                let cap_sq = if us == WHITE { to - 8 } else { to + 8 };
                self.pieces[PAWN] |= 1u64 << cap_sq;
                self.colors[them] |= 1u64 << cap_sq;
                self.mailbox[cap_sq as usize] = PAWN as u8;
            } else {
                self.pieces[u.captured as usize] |= tbit;
                self.colors[them] |= tbit;
                self.mailbox[to as usize] = u.captured;
            }
        }

        self.castle = u.castle;
        self.ep = u.ep;
        self.halfmove = u.halfmove;
    }
}
