//! `xl-ffi` — foreign bindings for Recalc.
//!
//! Hosts thin, versioned bindings exposing `xl-engine` to other
//! languages/runtimes:
//!
//! * **Python** — a PyO3 module `recalc` (see [`python`], `python` feature).
//! * **Node** — a napi-rs addon (see [`node`], `node` feature).
//! * **WASM** — a wasm-bindgen module (see [`wasm`], `wasm` feature).
//! * **C ABI** — documentation-only in M2 (RFC-0013 BC-8); not built here.
//!
//! Bindings stay thin: all calculation logic lives upstream in `xl-engine` and
//! friends, and this crate only marshals values across the language boundary.
//!
//! # One value mapping, three bindings (RFC-0013 §2 / the unified surface)
//! Every binding marshals [`xl_value::Value`] through the *same* canonical
//! table, so a given workbook + cell yields the semantically identical value in
//! Python, Node, and WASM (same f64 bits, same text, same error code, same
//! array shape). Errors are a **distinct wrapper type** (`CellError`, carrying
//! the exact Excel string), never a bare string and never a thrown exception;
//! `Blank` is `None`/`null`, distinct from `Text("")`. Each binding's marshal
//! is an **exhaustive `match` with no `_` arm** (RFC-0013 BC-11), so a new
//! `Value` variant is a compile break forcing a conscious table row in every
//! binding. The Python binding ([`python`]) is the reference the Node and WASM
//! bindings mirror (the unified-binding-API-surface spec, 2026-07-15).
//!
//! # Feature gating (binding-approval condition 2)
//! `pyo3`, `napi`/`napi-derive`, and `wasm-bindgen` are each **optional**
//! dependencies behind a non-default feature (`python` / `node` / `wasm`).
//! `cargo build --workspace` (no `--features`) compiles **zero** binding deps;
//! the three features are mutually independent (the crate compiles under any
//! one alone and under `--all-features`), and every line of binding-touching
//! code lives behind its `#[cfg(feature = "…")]` module.
//!
//! # `unsafe` posture (binding-approval condition 6)
//! This crate contains **no hand-written `unsafe`**. The pyo3, napi-derive, and
//! wasm-bindgen procedural macros expand to `unsafe` code *inside their own
//! macro output*, which is why `#![forbid(unsafe_code)]` cannot be applied here
//! (the `unsafe_code` lint fires on macro-generated `unsafe` too). All such
//! `unsafe` is generated and maintained by those crates upstream; a reviewer
//! auditing this crate's own source will find no `unsafe` block, `unsafe fn`,
//! or `unsafe impl` authored by hand. In particular the Node marshal builds
//! every foreign value through **safe** napi APIs (`Env::create_array`,
//! `Array::set`/`get`, `JavaScriptClassExt::into_instance`,
//! `JsValue::into_unknown`) rather than the `unsafe` `ToNapiValue::to_napi_value`
//! extension point.

mod a1;

pub use a1::{A1Error, parse_a1};

#[cfg(feature = "python")]
mod python;

// `node` and `wasm` are `pub` (unlike the private `python` module) so their
// binding entry points — `openBytes`, the `Workbook`/`CellError`/`Diagnostic`
// types, `SURFACE_VERSION` — count as reachable crate API. napi-derive and
// wasm-bindgen strip their JS-module *registration* glue under `cfg(test)`, so
// without this those free items would trip `dead_code` in the `--all-targets`
// (lib-test) clippy build (pyo3 avoids it because its `#[pymodule]` fn keeps
// referencing them). The JS API surface remains a human release checkpoint.
#[cfg(feature = "node")]
pub mod node;

#[cfg(feature = "wasm")]
pub mod wasm;
