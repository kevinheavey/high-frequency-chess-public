use harness::game::{play_game, white_score, GameLog, Rules};
use harness::native::{self, Engine};
use harness::{arg, book, failure_kind, num, setup_core, FAILURE_NAMES, R};
use harness::{DEFAULT_CPU, FAILURE_CAP, MAX_PLIES, MOVE_CYCLES};
use stats::{sprt_bounds, Penta, Results};

fn usage() -> ! {
    eprintln!(
        "usage:
  harness match <a.so> <b.so> [--games N] [--cycles C] [--cycles-b C] [--cpu N]
                              [--elo0 E] [--elo1 E] [--pgn FILE]
                              [--book-offset N] [--out FILE]
  harness verify <engine.so>            smoke test: ABI, legality, budget
  harness merge <results...> [--out F]   sum results from parallel runs

Matches run serially on one pinned core: engines are dlopen'd into this
process, so their static state cannot be shared between threads. To use more
cores, run several harness processes pinned to different physical cores."
    );
    std::process::exit(2)
}

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn run() -> R<()> {
    hfc_rules::board::init_tables();
    let args: Vec<String> = std::env::args().skip(1).collect();

    match args.first().map(|s| s.as_str()) {
        Some("match") if args.len() >= 3 => {
            setup_core(num(&args, "--cpu", DEFAULT_CPU))?;
            run_match(
                &args[1],
                &args[2],
                num(&args, "--games", 1000usize),
                num(&args, "--cycles", MOVE_CYCLES),
                arg(&args, "--cycles-b").and_then(|s| s.parse().ok()),
                num(&args, "--elo0", 0.0f64),
                num(&args, "--elo1", 10.0f64),
                arg(&args, "--pgn"),
                num(&args, "--book-offset", 0usize),
                arg(&args, "--out"),
                args.iter().any(|a| a == "--moves"),
            )
        }
        Some("verify") if args.len() >= 2 => {
            setup_core(num(&args, "--cpu", DEFAULT_CPU))?;
            verify(&args[1])
        }
        Some("merge") if args.len() >= 2 => {
            let mut acc = Results::default();
            let mut skip_next = false;
            for p in &args[1..] {
                // Skip flags and the value that follows them, or --out's path
                // gets read back as an input file.
                if skip_next { skip_next = false; continue; }
                if p.starts_with("--") { skip_next = true; continue; }
                let t = std::fs::read_to_string(p).map_err(|e| format!("{p}: {e}"))?;
                let r = Results::from_text(&t).map_err(|e| format!("{p}: {e}"))?;
                acc.merge(&r).map_err(|e| format!("{p}: {e}"))?;
            }
            if let Some(p) = arg(&args, "--out") {
                if let Some(d) = std::path::Path::new(&p).parent() {
                    std::fs::create_dir_all(d).ok();
                }
                std::fs::write(&p, acc.to_text()).map_err(|e| format!("writing {p}: {e}"))?;
                println!("  {:<24} {p}", "merged results");
            }
            report(&acc, num(&args, "--elo0", 0.0f64), num(&args, "--elo1", 10.0f64));
            Ok(())
        }
        _ => usage(),
    }
}

// -------------------------------------------------------------------- ladder



/// Run the matches the ladder picks. Small matches, many of them: a rating
/// comes from accumulating results, not from one long match. It also keeps the
/// opening book from running out -- 2,000 games is 1,000 openings, so a pair
/// can meet fifty times before repeating a start.

// -------------------------------------------------------------------- verify

/// Smoke test a freshly built entry before it is allowed into a match: the
/// ABI resolves, it returns legal moves across a range of positions, and it
/// respects its budget.
fn verify(path: &str) -> R<()> {
    let mut e = Engine::load(path)?;
    println!("loaded {} (ABI ok)", e.name);
    println!(
        "hfc_init: {} reference ticks ({:.2}ms at 5GHz)",
        e.load_cycles,
        e.load_cycles as f64 / 5e6
    );
    e.begin_game();

    // Terminal positions are included deliberately: returning 0 there is
    // correct, and returning anything else is a bug the harness must catch.
    let cases: &[(&str, &str)] = &[
        ("start", "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1"),
        ("midgame", "r2q1rk1/pp2ppbp/2np1np1/2p5/4P3/2NPBN1P/PPPQ1PP1/R3KB1R w KQ - 0 9"),
        ("tactical", "rnbq1k1r/pp1Pbppp/2p5/8/2B5/8/PPP1NnPP/RNBQK2R w KQ - 1 8"),
        ("endgame", "8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 0 1"),
        ("in check", "4k3/8/8/8/8/8/4r3/4K3 w - - 0 1"),
        ("one legal move", "7k/8/8/8/8/8/5Q2/6RK b - - 0 1"),
        ("promotion", "8/P6k/8/8/8/8/7K/8 w - - 0 1"),
        ("checkmate", "7k/6Q1/6K1/8/8/8/8/8 b - - 0 1"),
        ("stalemate", "7k/5Q2/6K1/8/8/8/8/8 b - - 0 1"),
    ];

    let mut bad = 0;
    for &budget in &[MOVE_CYCLES, MOVE_CYCLES / 4, MOVE_CYCLES * 4] {
        for (name, fen) in cases {
            // No wrapper needed any more: the engine is already in its own
            // process, so a crash here kills only its host and comes back as
            // a diagnosis rather than taking the arbiter with it. A dead host
            // stays dead, though, so restart it before continuing.
            if e.is_dead() {
                e = Engine::load(path)?;
            }
            e.begin_game();
            let board = hfc_rules::board::Board::from_fen(fen);
            let legal = board.legal_moves();
            let hist = [board.hash()];
            let _ = e.play(&board, &hist, budget);
            let out = e.play(&board, &hist, budget);
            if !check_case(name, budget, &legal, out) {
                bad += 1;
            }
        }
    }
    if bad == 0 {
        println!("{} positions x 3 budgets: all legal, all within budget", cases.len());
        Ok(())
    } else {
        Err(format!("{bad} failure(s)"))
    }
}

/// Returns true if the engine's answer was acceptable for this position.
fn check_case(name: &str, budget: u64, legal: &[u16], out: native::MoveOut) -> bool {
    if legal.is_empty() {
        // No legal moves: 0 is the only correct answer.
        return match out {
            native::MoveOut::Forfeit(ref r) if r.contains("returned move 0") => true,
            native::MoveOut::Move(m) => {
                println!("  FAIL {name} @{budget}: returned {} in a terminal position",
                    hfc_rules::move_to_uci(m));
                false
            }
            native::MoveOut::Forfeit(r) => {
                println!("  FAIL {name} @{budget}: {r}");
                false
            }
        };
    }
    match out {
        native::MoveOut::Move(m) if legal.contains(&m) => true,
        native::MoveOut::Move(m) => {
            println!("  FAIL {name} @{budget}: illegal move {} ({m:#06x})",
                hfc_rules::move_to_uci(m));
            false
        }
        native::MoveOut::Forfeit(r) => {
            println!("  FAIL {name} @{budget}: {r}");
            false
        }
    }
}

// --------------------------------------------------------------------- match

#[derive(Default)]
struct Tally {
    penta: Penta,
    wins_a: u64,
    draws: u64,
    wins_b: u64,
    plies: u64,
    games: u64,
    cycles: u64,
    moves: u64,
    notes: Vec<String>,
    pgn: Vec<String>,
    /// "<result>|<opening fen>|<uci moves>" per game -- what the web UI
    /// replays. PGN is for humans and chess GUIs; this is for machines.
    game_lines: Vec<String>,
    /// crash, hang, illegal move, budget overrun, no move, other
    failures: [u64; 6],
}

fn results(t: &Tally, a: &str, b: &str, ca: u64, cb: u64, book: u64) -> Results {
    Results {
        a: a.to_string(),
        b: b.to_string(),
        cycles_a: ca,
        cycles_b: cb,
        book,
        penta: t.penta.clone(),
        wins_a: t.wins_a,
        draws: t.draws,
        wins_b: t.wins_b,
        plies: t.plies,
        games: t.games,
        cycles_sum: t.cycles,
        cycles_n: t.moves,
        failures: t.failures,
    }
}

fn absorb(t: &mut Tally, log: &GameLog, a_is_white: bool, pgn: Option<(&str, &str, usize)>) -> f64 {
    if let Some((w, b, round)) = pgn {
        t.pgn.push(hfc_rules::to_pgn(&log.opening, &log.moves, w, b, log.outcome, round));
        let res = match log.outcome {
            hfc_rules::Outcome::Win(c) => if c == hfc_rules::board::WHITE { "1-0" } else { "0-1" },
            hfc_rules::Outcome::Forfeit(l, _) => if l == hfc_rules::board::WHITE { "0-1" } else { "1-0" },
            hfc_rules::Outcome::Draw(_) => "1/2-1/2",
            hfc_rules::Outcome::Ongoing => "*",
        };
        let ended = match log.outcome {
            hfc_rules::Outcome::Win(_) => "checkmate",
            hfc_rules::Outcome::Draw(why) => why,
            hfc_rules::Outcome::Forfeit(_, why) => why,
            hfc_rules::Outcome::Ongoing => "",
        };
        t.game_lines.push(format!("{res}|{}|{}|{}|{ended}\n", log.opening,
            log.moves.iter().map(|&m| hfc_rules::move_to_uci(m)).collect::<Vec<_>>().join(" "),
            // One entry per move, plus one more when a measured attempt
            // ended the game -- the bar that shows why it forfeited.
            log.ply_cycles.iter()
                .map(|c| c.to_string()).collect::<Vec<_>>().join(" ")));
    }
    let w = white_score(log.outcome);
    let a = if a_is_white { w } else { 1.0 - w };
    if a > 0.75 {
        t.wins_a += 1;
    } else if a < 0.25 {
        t.wins_b += 1;
    } else {
        t.draws += 1;
    }
    t.plies += log.plies as u64;
    t.games += 1;
    for side in 0..2 {
        t.cycles += log.cycles[side].iter().sum::<u64>();
        t.moves += log.cycles[side].len() as u64;
    }
    if let Some(n) = &log.note {
        t.failures[failure_kind(n)] += 1;
        if t.notes.len() < 10 {
            t.notes.push(n.clone());
        }
    }
    a
}

#[allow(clippy::too_many_arguments)]
fn run_match(
    a_path: &str,
    b_path: &str,
    games: usize,
    cycles: u64,
    cycles_b: Option<u64>,
    elo0: f64,
    elo1: f64,
    pgn_path: Option<String>,
    book_offset: usize,
    out_path: Option<String>,
    write_moves: bool,
) -> R<()> {
    // dlopen keys on the file, so loading the same path twice returns one
    // shared instance -- the two "engines" would share static state. Copy it
    // so self-play really is two independent engines.
    let mut tmp_b = None;
    let b_load = if std::fs::canonicalize(a_path).ok() == std::fs::canonicalize(b_path).ok() {
        let t = std::env::temp_dir().join(format!("mc-selfplay-{}.so", std::process::id()));
        std::fs::copy(b_path, &t).map_err(|e| format!("copying {b_path}: {e}"))?;
        let p = t.to_string_lossy().to_string();
        tmp_b = Some(t);
        p
    } else {
        b_path.to_string()
    };
    let mut ea = Engine::load(a_path)?;
    let mut eb = Engine::load(&b_load)?;
    // Report B under its real name, not the temp copy's. Otherwise parallel
    // self-play records a different opponent name per worker (the copy is
    // named after the pid) and the results refuse to merge.
    if tmp_b.is_some() {
        eb.name = std::path::Path::new(b_path)
            .file_stem()
            .map(|s| s.to_string_lossy().trim_start_matches("lib").to_string())
            .unwrap_or_else(|| eb.name.clone());
    }
    let (na, nb) = (ea.name.clone(), eb.name.clone());

    let mut pairs = games.div_ceil(2);
    let all = book::load()?;
    let book_sum = book::checksum(&all);
    if book_offset >= all.len() {
        return Err(format!("--book-offset {book_offset} is past the end of a {}-opening book", all.len()));
    }
    let openings = &all[book_offset..];
    if openings.is_empty() {
        return Err("book generation produced no positions".into());
    }
    if openings.len() < pairs {
        eprintln!(
            "note: {} openings available from offset {book_offset}, {pairs} pairs requested; \
             capping.\n      Openings are not reused: two games from the same \
             start are correlated, which inflates the SPRT.",
            openings.len()
        );
        pairs = openings.len();
    }

    println!("A: {na}   B: {nb}");
    println!("{} games ({pairs} pairs) | {cycles} cycles/move | book {book_sum:016x}\
              {}", pairs * 2,
        if book_offset > 0 { format!(" offset {book_offset}") } else { String::new() });


    // Asymmetric budgets: prices what a given amount of extra compute is
    // worth in Elo, which is the same question as "what is an efficiency
    // improvement worth".
    let cb = cycles_b.unwrap_or(cycles);
    let rules_ab = Rules { move_cycles: [cycles, cb], max_plies: MAX_PLIES };
    let rules_ba = Rules { move_cycles: [cb, cycles], max_plies: MAX_PLIES };
    if cb != cycles {
        println!("  (A gets {cycles} cycles/move, B gets {cb})");
    }
    let want_pgn = pgn_path.is_some();
    let mut t = Tally::default();
    let t0 = std::time::Instant::now();

    for i in 0..pairs {
        let opening = &openings[i];
        let g1 = play_game(&mut [&mut ea, &mut eb], opening, &rules_ab, None);
        let s1 = absorb(&mut t, &g1, true, want_pgn.then_some((na.as_str(), nb.as_str(), i + 1)));
        let g2 = play_game(&mut [&mut eb, &mut ea], opening, &rules_ba, None);
        let s2 = absorb(&mut t, &g2, false, want_pgn.then_some((nb.as_str(), na.as_str(), i + 1)));
        t.penta.add(s1 + s2);

        // Restart whichever engine died, so the next pair starts fresh:
        // death is sticky within a game -- once the host is gone there is
        // nothing to ask -- but a failure costs the game it happened in,
        // not the rest of the match.
        for (e, path) in [(&mut ea, a_path), (&mut eb, b_load.as_str())] {
            if e.is_dead() {
                let name = e.name.clone();
                match Engine::load(path) {
                    Ok(mut fresh) => {
                        // play_game calls new_game itself at the start of
                        // every game, so there is nothing to set up here.
                        fresh.name = name;
                        *e = fresh;
                    }
                    // Nothing more can be measured; keep what we have.
                    Err(why) => {
                        eprintln!("\n  {name}: could not restart after failure: {why}");
                        break;
                    }
                }
            }
        }

        // An engine that fails constantly should not spend hours proving it.
        // The cap is on failed *games*, so a rare fault runs to completion and
        // a broken submission stops early with its record intact.
        let failed: u64 = t.failures.iter().sum();
        if failed > FAILURE_CAP && failed * 4 > t.games {
            eprintln!(
                "\n  stopping after {failed} failed games of {}: this engine is not \
                 playable, and the rest of the match would measure nothing.",
                t.games
            );
            break;
        }

        if (i + 1) % 25 == 0 || i + 1 == pairs {
            let (e, err) = t.penta.elo();
            let llr = t.penta.llr(elo0, elo1);
            eprint!("\r  {}/{pairs} pairs   {:+.0} +/- {:.0} Elo   llr {llr:+.2}      ",
                i + 1, e, if err.is_finite() { err } else { 999.0 });
            // Write through periodically: a long run that dies should not
            // take its results with it. To a side name, because anything
            // ingesting FILE must never see a mid-run snapshot: a snapshot
            // stored under the final name would shadow the real result.
            if let Some(p) = &out_path {
                let _ =
                    std::fs::write(format!("{p}.partial"), results(&t, &na, &nb, cycles, cb, book_sum).to_text());
            }
        }
    }
    eprintln!();
    if let Some(p) = &out_path {
        if let Some(d) = std::path::Path::new(p).parent() {
            std::fs::create_dir_all(d).map_err(|e| format!("creating {}: {e}", d.display()))?;
        }
        let _ = std::fs::remove_file(format!("{p}.partial"));
        std::fs::write(p, results(&t, &na, &nb, cycles, cb, book_sum).to_text())
            .map_err(|e| format!("writing {p}: {e}"))?;
        println!("  {:<24} {p}", "results");
    }

    if t.games == 0 {
        return Err("no games were played".into());
    }

    report(&results(&t, &na, &nb, cycles, cb, book_sum), elo0, elo1);

    // Wall time was lost when the reporter was factored out; it is the number
    // you watch when tuning throughput, so it belongs here.
    let secs = t0.elapsed().as_secs_f64();
    println!("  {:<24} {secs:.2}s  ({:.0} games/s)", "wall time", t.games as f64 / secs);

    if let Some(p) = &pgn_path {
        if let Some(d) = std::path::Path::new(p).parent() {
            std::fs::create_dir_all(d).map_err(|e| format!("creating {}: {e}", d.display()))?;
        }
        std::fs::write(p, t.pgn.concat()).map_err(|e| format!("writing {p}: {e}"))?;
        let mut wrote = p.clone();
        if write_moves {
            let mv = p.strip_suffix(".pgn").unwrap_or(p.as_str()).to_string() + ".moves";
            std::fs::write(&mv, t.game_lines.concat()).map_err(|e| format!("writing {mv}: {e}"))?;
            wrote = format!("{p}, {mv}");
        }
        println!("  {:<24} {} games -> {wrote}", "games", t.pgn.len());
    }
    if let Some(t) = tmp_b {
        let _ = std::fs::remove_file(t);
    }
    if !t.notes.is_empty() {
        println!("\n  first few failures:");
        for n in &t.notes {
            println!("    - {n}");
        }
    }
    Ok(())
}

/// Single place that turns results into the summary, so a merged run and a
/// direct run print identically.
fn report(r: &Results, elo0: f64, elo1: f64) {

    let (elo, err) = r.penta.elo();
    let llr = r.penta.llr(elo0, elo1);
    let (lo, hi) = sprt_bounds(0.05, 0.05);

    println!("\n  {:<24} {} - {} - {}  (W-D-L for A)", "result", r.wins_a, r.draws, r.wins_b);
    println!("  {:<24} {:.1}%", "score",
        100.0 * (r.wins_a as f64 + 0.5 * r.draws as f64) / r.games as f64);
    if !err.is_finite() {
        println!("  {:<24} {:+.1}  (no variance)", "elo", elo);
    } else if err > 400.0 {
        println!("  {:<24} {:+.1} +/- unbounded  (score too lopsided)", "elo", elo);
    } else {
        println!("  {:<24} {:+.1} +/- {:.1}", "elo", elo, err);
    }
    println!("  {:<24} {:.2}   bounds [{lo:.2}, {hi:.2}]   {}", "SPRT llr", llr,
        if llr >= hi { "H1 accepted" } else if llr <= lo { "H0 accepted" } else { "inconclusive" });
    println!("  {:<24} [{}]", "pentanomial",
        r.penta.counts.iter().map(|c| c.to_string()).collect::<Vec<_>>().join(", "));
    println!("  {:<24} {}", "games", r.games);
    println!("  {:<24} {:.1}", "avg game length (plies)", r.plies as f64 / r.games as f64);
    if r.cycles_n > 0 {
        let per = r.cycles_sum as f64 / r.cycles_n as f64;
        println!("  {:<24} {:.0} ({:.1}% of budget)", "avg cycles/move", per,
            100.0 * per / r.cycles_a.max(1) as f64);
    }
    if r.failures.iter().any(|&c| c > 0) {
        println!("\n  failures:");
        for (i, &c) in r.failures.iter().enumerate() {
            if c > 0 {
                println!("    {:<20} {c}", FAILURE_NAMES[i]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use harness::{arg, num};

    #[test]
    fn flags_are_read_off_the_command_line() {
        let args: Vec<String> = ["match", "a.so", "b.so", "--games", "40", "--cycles", "150000"]
            .iter().map(|s| s.to_string()).collect();
        assert_eq!(arg(&args, "--games").as_deref(), Some("40"));
        assert_eq!(num(&args, "--games", 0u64), 40);
        assert_eq!(num(&args, "--cycles", 0u64), 150_000);
        // Absent, and present-but-unparseable, both fall back to the default.
        assert_eq!(num(&args, "--missing", 7u64), 7);
        let bad: Vec<String> = ["x", "--games", "lots"].iter().map(|s| s.to_string()).collect();
        assert_eq!(num(&bad, "--games", 7u64), 7);
        // A flag in final position has no value to take.
        let trailing: Vec<String> = ["x", "--games"].iter().map(|s| s.to_string()).collect();
        assert_eq!(arg(&trailing, "--games"), None);
    }
}
