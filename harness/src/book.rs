pub const BOOK: &str = "book.txt";

/// A 64-bit checksum over the opening lines, so both players can confirm they
/// are testing against the same book. Not cryptographic; it only needs to
/// catch accidental divergence.
pub fn checksum(openings: &[String]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for line in openings {
        for b in line.as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(0x1000_0000_01b3);
        }
        h ^= 0xff;
        h = h.wrapping_mul(0x1000_0000_01b3);
    }
    h
}

/// Read the frozen book. Never regenerates: a book that silently changed would
/// make results from different days, or from the two players' machines,
/// quietly incomparable.
pub fn load() -> Result<Vec<String>, String> {
    let text = std::fs::read_to_string(BOOK).map_err(|_| {
        format!(
            "no {BOOK} found. The opening book ships with the repository and \
             is shared by both players; restore it from git."
        )
    })?;

    let mut declared: Option<u64> = None;
    let mut openings = Vec::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("# checksum=") {
            declared = u64::from_str_radix(rest.trim(), 16).ok();
        } else if !line.starts_with('#') && !line.trim().is_empty() {
            openings.push(line.to_string());
        }
    }
    if openings.is_empty() {
        return Err(format!("{BOOK} contains no openings"));
    }
    let actual = checksum(&openings);
    // No readable header is refused like a mismatch: a deleted or mangled
    // checksum line must not disable the check it exists for.
    let Some(declared) = declared else {
        return Err(format!(
            "{BOOK} has no readable '# checksum=' header. The book is part of the              rules; restore it from git."
        ));
    };
    match Some(declared) {
        Some(d) if d != actual => {
            return Err(format!(
                "{BOOK} checksum mismatch: header says {d:016x}, contents hash to {actual:016x}.\n  \
                 The book has been edited. Both players must use an identical book."
            ))
        }
        _ => {}
    }
    Ok(openings)
}
