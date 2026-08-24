//! Throwaway baseline: picks a legal move from a PRNG seeded by the position
//! hash. Deterministic per position, varied across positions.
use hfc_rules::board::{Board, init_tables};

#[no_mangle]
pub extern "C" fn hfc_abi_version() -> u32 { hfc_abi::ABI_VERSION }
#[no_mangle]
pub extern "C" fn hfc_init() { init_tables(); }

/// # Safety
/// `pos` must be a valid pointer for the duration of the call.
#[no_mangle]
pub unsafe extern "C" fn hfc_play(pos: *const hfc_abi::Position, _b: *const hfc_abi::Budget) -> u16 {
    let mut b: Board = *(*pos).board;
    let mut buf = [0u16; 256];
    let n = b.gen_moves(&mut buf);
    let mut legal = [0u16; 256];
    let mut ln = 0usize;
    for i in 0..n {
        let m = buf[i];
        let u = b.make(m);
        if !b.in_check(b.stm ^ 1) { legal[ln] = m; ln += 1; }
        b.unmake(m, u);
    }
    if ln == 0 { return 0; }
    let mut h = b.hash();
    h ^= h >> 33;
    h = h.wrapping_mul(0xFF51AFD7ED558CCD);
    h ^= h >> 33;
    legal[(h as usize) % ln]
}
