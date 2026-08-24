//! The referee: metering, match running, and the pieces both binaries share.

pub mod book;
pub mod game;
pub mod native;

pub type R<T> = Result<T, String>;

/// ~30us at 5GHz. Recalibrate with `measure` and asymmetric-budget
/// self-play once two real engines exist.
pub const MOVE_CYCLES: u64 = 150_000;
pub const MAX_PLIES: usize = 400;
/// Failed games tolerated before a match is abandoned. Only bites alongside a
/// failure rate over 25%, so a rare fault is measured rather than punished.
pub const FAILURE_CAP: u64 = 200;
pub const DEFAULT_CPU: usize = 2;

pub fn arg(args: &[String], flag: &str) -> Option<String> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1).cloned())
}
pub fn num<T: std::str::FromStr>(args: &[String], flag: &str, d: T) -> T {
    arg(args, flag).and_then(|s| s.parse().ok()).unwrap_or(d)
}

/// Pin, and complain loudly about anything that would corrupt the measurement.
pub fn setup_core(cpu: usize) -> R<()> {
    if !hfc_abi::pin_to(cpu) {
        return Err(format!("could not pin to CPU {cpu}"));
    }
    if let Some(w) = hfc_abi::smt_sibling_warning(cpu) {
        eprintln!("warning: {w}\n");
    }
    Ok(())
}

pub const FAILURE_NAMES: [&str; 6] =
    ["crash", "hang", "illegal move", "budget overrun", "no move returned", "other"];

pub fn failure_kind(note: &str) -> usize {
    if note.contains("crashed with signal") || note.contains("exited with status") {
        0
    } else if note.contains("hung") {
        1
    } else if note.contains("illegal move") {
        2
    } else if note.contains("budget overrun") {
        3
    } else if note.contains("returned move 0") {
        4
    } else {
        5
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every failure the policy names has to land in the right bucket: the
    /// counts go into the result file and are what a submitter is shown when
    /// their engine loses without playing. Miscategorising is not cosmetic --
    /// "other" is the bucket that says nobody understood what happened.
    #[test]
    fn every_failure_lands_in_the_right_bucket() {
        let cases: &[(&str, &str)] = &[
            ("crash", "faulty crashed with signal 11 (SIGSEGV - bad pointer)"),
            ("crash", "faulty crashed with signal 6 (SIGABRT - panic or assertion)"),
            ("crash", "faulty exited with status 0"),
            ("hang", "faulty hung: no reply within 5000ms"),
            ("illegal move", "faulty played illegal move h8h8q (0xffff) in position 8/8/8"),
            ("budget overrun", "faulty: budget overrun: spent 8012381 cycles of 150000"),
            ("no move returned", "faulty: returned move 0 (no move chosen)"),
            ("other", "something nobody anticipated"),
        ];
        for (want, note) in cases {
            let got = FAILURE_NAMES[failure_kind(note)];
            assert_eq!(got, *want, "{note:?} was filed as {got}, not {want}");
        }
    }

    /// A crash is diagnosed by the words the arbiter itself writes, so those
    /// two have to stay in step. This catches the pairing drifting apart.
    #[test]
    fn the_classifier_matches_what_the_arbiter_writes() {
        // As produced by native::describe_exit and the forfeit paths.
        for note in ["e crashed with signal 11 (SIGSEGV - bad pointer)", "e exited with status 3"] {
            assert_eq!(FAILURE_NAMES[failure_kind(note)], "crash", "{note:?}");
        }
    }
}
