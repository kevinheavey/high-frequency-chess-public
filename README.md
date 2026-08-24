# High-Frequency Chess

A tournament for extreme low latency chess engines.
The ladder lives at [hfchess.com](https://hfchess.com); the rules are in
[SPEC.md](SPEC.md). `entries/example` is your entry; rewrite it.

```sh
./build.sh
cargo build --release --manifest-path entries/example/Cargo.toml
./target/release/harness verify entries/example/target/release/libexample_engine.so
./target/release/harness match entries/example/target/release/libexample_engine.so \
    build/reference.so --games 2000
```
