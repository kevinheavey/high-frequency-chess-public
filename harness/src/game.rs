use crate::native::{Engine, MoveOut};
use hfc_rules::board::{WHITE, mv_from, mv_is_promo, mv_promo_piece, mv_to};
use hfc_rules::{move_to_uci, Game, Outcome};

pub struct GameLog {
    pub outcome: Outcome,
    pub plies: usize,
    pub moves: Vec<u16>,
    pub opening: String,
    /// core cycles actually consumed per move, by colour
    pub cycles: [Vec<u64>; 2],
    /// the same, in ply order, aligned with `moves`
    pub ply_cycles: Vec<u64>,
    pub note: Option<String>,
}

pub struct Rules {
    /// Budget per colour: index 0 is White. Asymmetric budgets let a match
    /// price what extra compute is worth.
    pub move_cycles: [u64; 2],
    pub max_plies: usize,
}

/// Play one game. `engines[0]` has White.
/// `progress` is called twice per ply when present: once before an engine
/// thinks, with `played = None`, so an observer knows whose turn it is if the
/// process dies mid-move; and once after the move is applied, with the move.
pub type Progress<'a> = Option<&'a mut dyn FnMut(u32, usize, Option<u16>)>;

pub fn play_game(
    engines: &mut [&mut Engine; 2],
    opening: &str,
    rules: &Rules,
    mut progress: Progress,
) -> GameLog {
    let mut g = Game::from_fen(opening);
    let mut cycles = [Vec::new(), Vec::new()];
    let mut ply_cycles = Vec::new();
    let mut note = None;

    // History back to the most recent irreversible move (capture or pawn
    // move). Nothing before it can be repeated, so nothing before it is worth
    // an engine's cycles.
    let mut history: Vec<u64> = vec![g.board.hash()];

    // No hook, no round trip: the flag rides on each engine's first move of
    // the game, so clearing state costs that move's own budget.
    for e in engines.iter_mut() {
        e.begin_game();
    }

    // One legal-move generation per ply, reused for both the termination
    // check and validating the engine's answer.
    let mut legal_buf = [0u16; 256];
    let outcome = loop {
        let nlegal = g.board.legal_moves_into(&mut legal_buf);
        let o = g.outcome_with(nlegal);
        if o != Outcome::Ongoing {
            break o;
        }
        if g.moves.len() >= rules.max_plies {
            break Outcome::Draw("move limit");
        }

        let stm = g.board.stm;
        if let Some(p) = progress.as_deref_mut() {
            p(g.moves.len() as u32, stm, None);
        }
        let legal = &legal_buf[..nlegal];

        let result = engines[stm].play(&g.board, &history, rules.move_cycles[stm]);
        // Zero means the attempt died before a measurement existed; there
        // is no cost to tally.
        let spent = engines[stm].last_cycles;
        if spent > 0 {
            cycles[stm].push(spent);
            ply_cycles.push(spent);
        }

        let m = match result {
            MoveOut::Forfeit(reason) => {
                note = Some(format!("{}: {}", engines[stm].name, reason));
                break Outcome::Forfeit(stm, "engine failure");
            }
            MoveOut::Move(m) => m,
        };

        // Match on the parts that carry information -- the squares and the
        // promotion piece; the other flags are derivable from the board.
        // The arbiter then applies *its own* copy of the move, never the
        // engine's word for it.
        let canonical = legal.iter().copied().find(|&lm| {
            mv_from(lm) == mv_from(m)
                && mv_to(lm) == mv_to(m)
                && mv_is_promo(lm) == mv_is_promo(m)
                && (!mv_is_promo(lm) || mv_promo_piece(lm) == mv_promo_piece(m))
        });
        let m = match canonical {
            Some(c) => c,
            None => {
                note = Some(format!(
                    "{} played illegal move {} ({:#06x}) in position {}",
                    engines[stm].name,
                    move_to_uci(m),
                    m,
                    g.board.to_fen()
                ));
                break Outcome::Forfeit(stm, "illegal move");
            }
        };

        if let Some(p) = progress.as_deref_mut() {
            p(g.moves.len() as u32, stm, Some(m));
        }
        g.apply(m);
        // Positions before an irreversible move can never repeat one after it,
        // so the engine only needs the run since the clock last reset. Clearing
        // here is also what bounds the shared page's history buffer -- see the
        // assertion in `native.rs`, which depends on this staying in step with
        // the halfmove clock.
        if g.board.halfmove == 0 {
            history.clear();
        }
        history.push(g.board.hash());
    };

    GameLog {
        outcome,
        plies: g.moves.len(),
        moves: g.moves,
        opening: opening.to_string(),
        cycles,
        ply_cycles,
        note,
    }
}

/// Score from White's perspective: 1.0 win, 0.5 draw, 0.0 loss.
pub fn white_score(o: Outcome) -> f64 {
    match o {
        Outcome::Win(c) => {
            if c == WHITE {
                1.0
            } else {
                0.0
            }
        }
        Outcome::Forfeit(loser, _) => {
            if loser == WHITE {
                0.0
            } else {
                1.0
            }
        }
        Outcome::Draw(_) => 0.5,
        Outcome::Ongoing => 0.5,
    }
}
