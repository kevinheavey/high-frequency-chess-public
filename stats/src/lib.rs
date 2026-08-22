/// Pentanomial statistics. Each opening is played twice with colours reversed;
/// the pair result (0, 0.5, 1, 1.5, 2) is the unit of observation. Pairing
/// removes most of the variance that comes from the opening itself rather than
/// from the engines.
#[derive(Default, Clone)]
pub struct Penta {
    /// counts of pair results: [0, 0.5, 1, 1.5, 2]
    pub counts: [u64; 5],
}

impl Penta {
    pub fn add(&mut self, pair_score: f64) {
        let idx = (pair_score * 2.0).round() as usize;
        self.counts[idx.min(4)] += 1;
    }

    pub fn pairs(&self) -> u64 {
        self.counts.iter().sum()
    }

    /// Mean pair score normalised to [0, 1], and its variance.
    /// The five counts from their stored comma-separated form.
    pub fn from_csv(text: &str) -> Penta {
        let mut p = Penta::default();
        for (i, v) in text.split(',').enumerate().take(5) {
            p.counts[i] = v.trim().parse().unwrap_or(0);
        }
        p
    }

    fn mean_var(&self) -> (f64, f64) {
        let n = self.pairs() as f64;
        if n == 0.0 {
            return (0.5, 0.0);
        }
        let vals = [0.0, 0.25, 0.5, 0.75, 1.0];
        let mean: f64 = vals
            .iter()
            .zip(self.counts.iter())
            .map(|(v, c)| v * *c as f64)
            .sum::<f64>()
            / n;
        let var: f64 = vals
            .iter()
            .zip(self.counts.iter())
            .map(|(v, c)| (v - mean).powi(2) * *c as f64)
            .sum::<f64>()
            / n;
        (mean, var)
    }

    pub fn elo(&self) -> (f64, f64) {
        let (mean, var) = self.mean_var();
        let n = self.pairs() as f64;
        if n == 0.0 {
            return (0.0, f64::INFINITY);
        }
        if var <= 0.0 {
            // Every pair scored the same. The point estimate is exact; there
            // is simply no spread to build an interval from.
            let e = if mean <= 0.0 { f64::NEG_INFINITY }
                    else if mean >= 1.0 { f64::INFINITY }
                    else { -400.0 * (1.0 / mean - 1.0).log10() };
            return (e, f64::INFINITY);
        }
        let se = (var / n).sqrt();
        let e = |s: f64| {
            let s = s.clamp(1e-9, 1.0 - 1e-9);
            -400.0 * (1.0 / s - 1.0).log10()
        };
        let lo = e((mean - 1.96 * se).clamp(1e-9, 1.0 - 1e-9));
        let hi = e((mean + 1.96 * se).clamp(1e-9, 1.0 - 1e-9));
        (e(mean), (hi - lo) / 2.0)
    }

    /// Generalised SPRT log-likelihood ratio for H0: elo0 vs H1: elo1.
    pub fn llr(&self, elo0: f64, elo1: f64) -> f64 {
        let (mean, var) = self.mean_var();
        let n = self.pairs() as f64;
        if n == 0.0 || var <= 0.0 {
            return 0.0;
        }
        let s = |elo: f64| 1.0 / (1.0 + 10f64.powf(-elo / 400.0));
        let (s0, s1) = (s(elo0), s(elo1));
        n * (s1 - s0) * (mean - (s0 + s1) / 2.0) / var
    }
}

pub fn sprt_bounds(alpha: f64, beta: f64) -> (f64, f64) {
    ((beta / (1.0 - alpha)).ln(), ((1.0 - beta) / alpha).ln())
}

// ------------------------------------------------------- machine-readable results

/// Everything a match produces, in a form that survives the process dying and
/// can be summed across parallel runs.
#[derive(Default, Clone)]
pub struct Results {
    pub a: String,
    pub b: String,
    pub cycles_a: u64,
    pub cycles_b: u64,
    pub book: u64,
    pub penta: Penta,
    pub wins_a: u64,
    pub draws: u64,
    pub wins_b: u64,
    pub plies: u64,
    pub games: u64,
    pub cycles_sum: u64,
    pub cycles_n: u64,
    pub failures: [u64; 6],
}

impl Results {
    pub fn to_text(&self) -> String {
        let j = |v: &[u64]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(",");
        format!(
            "version 1\na {}\nb {}\ncycles_a {}\ncycles_b {}\nbook {:016x}\n\
             penta {}\nwdl {},{},{}\nplies {}\ngames {}\ncycles {} {}\nfailures {}\n",
            self.a, self.b, self.cycles_a, self.cycles_b, self.book,
            j(&self.penta.counts),
            self.wins_a, self.draws, self.wins_b,
            self.plies, self.games, self.cycles_sum, self.cycles_n,
            j(&self.failures)
        )
    }

    /// Parse a result file, refusing one that does not describe a whole match.
    ///
    /// The file is rewritten every 25 pairs while a match runs, so a worker
    /// that dies leaves a partial one on disk -- and `harness merge` reads
    /// whatever it is pointed at.
    ///
    /// Two identities make the file self-checking, and both hold by
    /// construction on every write: the win/draw/loss counts add up to the
    /// games played, and each pentanomial entry is one pair, so twice their
    /// sum is also the games played.
    pub fn from_text(t: &str) -> Result<Results, String> {
        let mut r = Results::default();
        let mut seen: Vec<&str> = Vec::new();
        let num = |k: &str, v: &str| -> Result<u64, String> {
            v.trim().parse::<u64>()
                .map_err(|_| format!("{k}: {:?} is not a number", v.trim()))
        };
        let list = |k: &str, v: &str, n: usize| -> Result<Vec<u64>, String> {
            let parts: Vec<&str> = v.split(',').map(|x| x.trim()).collect();
            if parts.len() != n {
                return Err(format!("{k}: {} values, expected {n}", parts.len()));
            }
            parts.iter()
                .map(|x| x.parse::<u64>().map_err(|_| format!("{k}: {x:?} is not a number")))
                .collect()
        };
        for line in t.lines() {
            let (k, v) = match line.split_once(' ') {
                Some(kv) => kv,
                None => continue,
            };
            seen.push(k);
            match k {
                "a" => r.a = v.trim().to_string(),
                "b" => r.b = v.trim().to_string(),
                "cycles_a" => r.cycles_a = num(k, v)?,
                "cycles_b" => r.cycles_b = num(k, v)?,
                "book" => {
                    r.book = u64::from_str_radix(v.trim(), 16)
                        .map_err(|_| format!("book: {:?} is not hex", v.trim()))?
                }
                "penta" => r.penta.counts.copy_from_slice(&list(k, v, 5)?),
                "wdl" => {
                    let n = list(k, v, 3)?;
                    r.wins_a = n[0];
                    r.draws = n[1];
                    r.wins_b = n[2];
                }
                "plies" => r.plies = num(k, v)?,
                "games" => r.games = num(k, v)?,
                "cycles" => {
                    let p: Vec<&str> = v.split_whitespace().collect();
                    if p.len() != 2 {
                        return Err(format!("cycles: {} values, expected 2", p.len()));
                    }
                    r.cycles_sum = num(k, p[0])?;
                    r.cycles_n = num(k, p[1])?;
                }
                "failures" => r.failures.copy_from_slice(&list(k, v, 6)?),
                "version" => {
                    if v.trim() != "1" {
                        return Err(format!("results version {v} is not understood"));
                    }
                }
                _ => {}
            }
        }

        for need in ["a", "b", "cycles_a", "cycles_b", "book", "games", "wdl", "penta"] {
            if !seen.contains(&need) {
                return Err(format!("result file is missing {need}; it is probably truncated"));
            }
        }
        if r.a.is_empty() || r.b.is_empty() {
            return Err("result file names no engines".into());
        }
        if r.games == 0 {
            return Err("result file has no games".into());
        }
        let wdl = r.wins_a + r.draws + r.wins_b;
        if wdl != r.games {
            return Err(format!(
                "result file is inconsistent: {wdl} wins/draws/losses over {} games", r.games));
        }
        let pairs: u64 = r.penta.counts.iter().sum();
        if pairs * 2 != r.games {
            return Err(format!(
                "result file is inconsistent: pentanomial covers {} games, not {}",
                pairs * 2, r.games));
        }
        Ok(r)
    }

    /// Sum two runs. Refuses to merge runs that were not measuring the same
    /// thing -- silently combining different engines, budgets or books would
    /// produce a confident and meaningless number.
    pub fn merge(&mut self, o: &Results) -> Result<(), String> {
        if self.games > 0 {
            if (self.a.as_str(), self.b.as_str()) != (o.a.as_str(), o.b.as_str()) {
                return Err(format!(
                    "refusing to merge {} vs {} with {} vs {}", self.a, self.b, o.a, o.b));
            }
            if (self.cycles_a, self.cycles_b) != (o.cycles_a, o.cycles_b) {
                return Err("refusing to merge runs with different budgets".into());
            }
            if self.book != o.book {
                return Err(format!(
                    "refusing to merge runs with different books ({:016x} vs {:016x})",
                    self.book, o.book));
            }
        } else {
            self.a = o.a.clone();
            self.b = o.b.clone();
            self.cycles_a = o.cycles_a;
            self.cycles_b = o.cycles_b;
            self.book = o.book;
        }
        for i in 0..5 {
            self.penta.counts[i] += o.penta.counts[i];
        }
        for i in 0..6 {
            self.failures[i] += o.failures[i];
        }
        self.wins_a += o.wins_a;
        self.draws += o.draws;
        self.wins_b += o.wins_b;
        self.plies += o.plies;
        self.games += o.games;
        self.cycles_sum += o.cycles_sum;
        self.cycles_n += o.cycles_n;
        Ok(())
    }
}
