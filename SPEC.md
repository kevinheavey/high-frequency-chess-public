# High-Frequency Chess

Standard chess, but instead of a clock each engine gets a fixed budget of
**150,000 CPU core cycles per move**.

## Rules

| Rule | Value |
|---|---|
| Budget per move | **150,000 core cycles** (~30µs at 5GHz) |
| Carryover | None |
| Overrun grace | 2,000 cycles |
| `hfc_init` budget | 1,500,000 reference ticks |
| Max game length | 400 plies, then drawn |
| Toolchain | Pinned in `rust-toolchain.toml` |
| Dependencies | **None** beyond `hfc-rules` and `hfc-abi` from crates.io |
| Build scripts, proc macros | **Forbidden** |
| Lockfile | `Cargo.lock` must be committed |
| Source size | 10 MB, 2,000 files, measured after decompression |
| Max binary size | 16 MB |
| Entries per account | 4 |
| Submissions | one per account per 120 seconds |

## Your engine

Rewrite `entries/example`. Each engine runs in its own single-threaded
process with no I/O. Memory is capped at 256 MB.

## Forfeits

| Condition | Result |
|---|---|
| Spent more than budget + grace | Forfeit the game |
| Illegal move | Forfeit the game |
| Returned 0 with legal moves available | Forfeit the game |
| Segfault, panic, hang | Forfeit the game |
| Missing or mismatched ABI symbols | Rejected at load |
| A quarter of the match's games failing, 200 minimum | Forfeit the match |

## Testing locally

Clone [the repository](https://github.com/kevinheavey/high-frequency-chess-public),
`./build.sh`, then:

```sh
./target/release/harness verify entries/example/target/release/libexample_engine.so
./target/release/harness match entries/example/target/release/libexample_engine.so \
    build/reference.so --games 2000
```

The harness names any perf permission it is missing. Cycle counts only
compare within one microarchitecture.

## Submitting

Sign in with GitHub. Commit, then:

```sh
git archive HEAD:entries/example --format=tar.gz | curl -f -X POST \
  -H "Authorization: Bearer <your-api-token>" \
  "https://hfchess.com/api/submit?kind=tarball&name=my-entry" --data-binary @-
```

or upload the same tarball through the dashboard, which shows your API
token while you are signed in. Resubmitting a name rerates it from scratch.

## The ladder

The scheduler picks all matches. Each opening from `book.txt` is played
twice with colours reversed. Ratings are relative to **reference** at 0.

API reference: [/docs/api](/docs/api).
