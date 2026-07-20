# xl-bench

Conformance harness and corpus runner ("ExcelBench") for Recalc: it scores a
calculation engine on cell-level agreement with a pinned build of desktop
Excel, over a corpus of real workbooks.

**Licensing note (partly proprietary).** The harness code in this crate is
Apache-2.0, like the rest of the workspace. The oracle corpus, the sidecar
expected-value artifacts, and the certified fidelity reports produced from a
licensed Excel farm are **proprietary** and are never committed to this
repository — they are fetched read-only artifacts, not source. This crate
ships the harness only; point it at your own corpus and oracle.
