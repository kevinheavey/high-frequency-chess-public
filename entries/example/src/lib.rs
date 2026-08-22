//! A minimal but complete entry: greedy 1-ply search on material.
//!
//! Deliberately short, so the whole ABI fits on one screen. It is not
//! strong, but everything here is correct, and the three things that are
//! easy to get wrong are done properly and marked. SPEC.md says how to
//! submit.

use hfc_rules::board::{BLACK, Board, WHITE, init_tables};

const VALUE: [i32; 6] = [100, 320, 330, 500, 900, 0];

#[no_mangle]
pub extern "C" fn hfc_abi_version() -> u32 {
    hfc_abi::ABI_VERSION
}

/// Called once at load, before the first game of a match. Capped, but
/// generous: enough for tables and unpacking weights.
#[no_mangle]
pub extern "C" fn hfc_init() {
    init_tables();
}

fn material(b: &Board) -> i32 {
    let mut s = 0;
    for p in 0..5 {
        s += (b.pieces[p] & b.colors[WHITE]).count_ones() as i32 * VALUE[p];
        s -= (b.pieces[p] & b.colors[BLACK]).count_ones() as i32 * VALUE[p];
    }
    if b.stm == WHITE { s } else { -s }
}

/// # Safety
/// `pos` and `budget` must be valid for the duration of the call.
/// `pos.new_game` is non-zero on the first move of each game. Clear anything
/// that must not carry over -- a transposition table, killers, history --
/// there rather than in a separate hook. It costs this move's budget, which
/// is the point: state you cannot afford to clear is state you cannot afford
/// to fill at 150,000 cycles a move. This engine keeps none, so it ignores it.
#[no_mangle]
pub unsafe extern "C" fn hfc_play(
    pos: *const hfc_abi::Position,
    budget: *const hfc_abi::Budget,
) -> u16 {
    let bud = &*budget;
    let start = hfc_abi::now(bud.perf_page);

    // GOTCHA 1: nothing stops you. There is no trap -- spend more than your
    // budget and you forfeit the game. Leave a margin, because you can only
    // check between units of work.
    let deadline = start + bud.cycles * 94 / 100;

    // No parsing: the board arrives as a struct in our own layout.
    let mut b: Board = *(*pos).board;

    let mut moves = [0u16; 256];
    let n = b.gen_moves(&mut moves);

    let mut best = 0u16;
    let mut best_score = i32::MIN;

    for i in 0..n {
        let m = moves[i];
        let u = b.make(m);
        let legal = !b.in_check(b.stm ^ 1);
        let score = if legal { -material(&b) } else { 0 };
        b.unmake(m, u);
        if !legal {
            continue;
        }

        // GOTCHA 2: get a legal move in hand as early as possible. Then running
        // out of budget costs you a worse move rather than the whole game.
        if best == 0 {
            best = m;
            best_score = score;
            continue;
        }
        if score > best_score {
            best_score = score;
            best = m;
        }

        // GOTCHA 3: check often enough that overshoot stays inside the margin.
        // `hfc_abi::now` costs about 35 cycles, so checking is cheap; the real
        // cost of checking rarely is overshooting and forfeiting.
        if hfc_abi::now(bud.perf_page) >= deadline {
            break;
        }
    }

    // 0 means "no legal moves" -- checkmate or stalemate. Returning it in any
    // other situation is a forfeit.
    best
}
