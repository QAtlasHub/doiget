# doiget-core fuzz harness

`cargo-fuzz` (libFuzzer) coverage of the input-validation surface that
guards every untrusted-input entry point in `doiget`:

- `doiget_core::Doi::parse`
- `doiget_core::ArxivId::parse`
- `doiget_core::Ref::parse`

These three functions are the trust boundary documented in
[`docs/SECURITY.md`](../../../docs/SECURITY.md) §1.1. Anything that
panics or unbounded-allocates here is a defect in defense-in-depth, so
this harness exists to catch regressions early.

## Why a separate workspace

`libfuzzer-sys` requires nightly Rust. The main `doiget` workspace pins
stable (see [`rust-toolchain.toml`](../../../rust-toolchain.toml) /
`Cargo.toml` `rust-version = "1.86"`), so this fuzz crate is
**deliberately excluded** from the main workspace via the root
`[workspace] exclude = [...]` entry. It is its own one-crate workspace
that you only ever invoke through `cargo +nightly fuzz`.

## Running locally

One-time install:

```sh
cargo +nightly install cargo-fuzz
```

Run a single target (default 60 s smoke; pass `-max_total_time=N` to
extend, or omit `--` to run open-ended until you Ctrl-C):

```sh
# from the repo root
cargo +nightly fuzz run doi_parse   --fuzz-dir crates/doiget-core/fuzz -- -max_total_time=60
cargo +nightly fuzz run arxiv_parse --fuzz-dir crates/doiget-core/fuzz -- -max_total_time=60
cargo +nightly fuzz run ref_parse   --fuzz-dir crates/doiget-core/fuzz -- -max_total_time=60
```

Each target accepts an arbitrary byte slice, decodes it as UTF-8 (the
parsers take `&str`), and calls the corresponding `parse` function. A
panic, abort, or libFuzzer-detected memory fault is a fuzzing failure;
returning either `Ok(_)` or `Err(RefParseError)` is success.

## CI

The [`fuzz-smoke` workflow](../../../.github/workflows/fuzz-smoke.yml)
runs each target for 60 seconds on every PR that touches
`crates/doiget-core/src/**` or `crates/doiget-core/fuzz/**`. This is a
*compile + smoke* check, not an exhaustive fuzz session — long fuzz runs
happen out-of-band.

## Corpus / artifacts

`corpus/` (seed inputs) and `artifacts/` (crash reproducers) are
git-ignored. Add representative inputs there if you want to seed a
campaign; otherwise libFuzzer starts cold and discovers structure.
