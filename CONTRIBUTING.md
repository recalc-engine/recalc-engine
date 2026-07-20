# Contributing to Recalc

Thanks for your interest in Recalc. This is a bug-for-bug Excel-compatible
calculation engine, so contributions are held to an unusual standard: the goal
is not "reasonable spreadsheet behavior" but *exactly what Microsoft Excel
does*, measured, with anything we can't reproduce faithfully flagged rather
than guessed.

## Ground rules

These are the non-negotiables that keep the engine trustworthy. A change that
breaks one of them will not be merged, however useful it otherwise is.

1. **Never guess Excel semantics.** When Excel's behavior for some input is
   unclear, it must be *observed* on a real, pinned Excel build and cited — not
   inferred from memory or from "what a spreadsheet probably does." Plausible
   behavior is not a source. A verifier that fails loudly and covers less beats
   one that guesses and claims more.

2. **Never silently wrong.** Any function or construct the engine cannot
   reproduce faithfully must return a *distinguishable* error value
   (`#UNSUPPORTED!`, or `#BLOCKED!` for sandbox-blocked I/O) plus a
   workbook-level diagnostic. A cell is either exactly Excel's result to the
   documented tolerance, or it is explicitly flagged. It is never quietly
   approximated.

3. **Clean-room only.** Behavior is observed by running Excel as an oracle and
   matching its outputs. Do **not** read, copy, or paraphrase source from any
   copyleft (GPL) spreadsheet implementation — LibreOffice, Gnumeric, pycel, or
   any other. Observing a competing engine's *public* behavior is fine; reading
   its internals is not.

4. **No new dependencies without discussion.** The core crates run on a
   deliberately minimal, individually-reviewed, exactly-pinned dependency set;
   most carry zero external dependencies. If a change seems to need a new crate,
   open an issue first — the answer is often "implement it in-crate."

5. **No `unsafe` without sign-off**, **no network or filesystem access from a
   formula**, and **no fast-math / FMA-variant floating-point paths.**
   Cross-platform, reproducible float results are a feature.

## Building and testing

A Rust toolchain matching the workspace `rust-version` is all you need. A plain
build links zero binding dependencies (the Python/Node/WASM bindings live behind
off-by-default features).

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

CI runs all four, plus a compile check of each binding feature
(`python` / `node` / `wasm`), a check that the WASM build never pulls in the
parallel/threaded path, and two dependency-isolation guards. Please run `fmt`
and `clippy` locally before opening a pull request.

## Pull requests

- Keep changes focused; one behavioral change per PR is easiest to review.
- New or changed function behavior needs a test that pins the expected result,
  with a note in the module header on how that expectation was established.
- Public API changes and any tolerance change are reviewed with extra care —
  call them out explicitly in the PR description.

## License

Recalc is licensed under the Apache License, Version 2.0. By contributing, you
agree that your contributions are licensed under the same terms. See
[`LICENSE`](LICENSE).

Microsoft and Excel are trademarks of the Microsoft group of companies. Recalc
is an independent project, not affiliated with, sponsored by, or endorsed by
Microsoft; references to Excel are nominative — they identify the product whose
behavior Recalc reproduces.
