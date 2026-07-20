//! Build script for `xl-ffi`.
//!
//! Its only job is per-binding linker setup for the loadable `cdylib`s. With
//! **no** binding feature enabled it does nothing, so the default
//! `cargo build --workspace` is unaffected.
//!
//! # Python (`python` feature)
//! pyo3's `extension-module` feature deliberately does **not** link libpython,
//! so a direct `cargo build`/`cargo test` of the `cdylib` would fail to resolve
//! the `Py_*` symbols at link time. On macOS the fix is to let those symbols
//! resolve dynamically at load time (`-undefined dynamic_lookup`); on
//! Linux/Windows undefined symbols in a shared object are permitted by default,
//! so nothing is needed there. This is emitted by a plain, dependency-free
//! branch (it is **not** maturin — pyo3 approval condition 5).
//!
//! # Node (`node` feature)
//! The `.node` addon resolves the N-API (`napi_*`) symbols from the hosting
//! Node process at load time, so — exactly as with pyo3 above — the `cdylib`
//! link must tolerate undefined symbols. [`napi_build::setup`] emits the right
//! per-platform link args (on macOS the same `-undefined dynamic_lookup`).
//! `napi-build` is the approved, build-only helper (napi-rs ratification,
//! 2026-07-14); it is an OPTIONAL build-dependency enabled only by the `node`
//! feature, so it is compiled only for a Node build and never linked into any
//! artifact.
//!
//! # WASM (`wasm` feature)
//! Nothing is needed here: `wasm-bindgen` targets `wasm32-*`, where the module
//! is produced by the `wasm-bindgen` post-processor and there is no native
//! link step for this script to influence.
//!
//! Emitting all of this from *xl-ffi's own* build script is the correct
//! mechanism: xl-ffi is the crate that produces the `cdylib`, and
//! `rustc-cdylib-link-arg` only applies to the emitting package's own cdylib.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    // Node: delegate the addon's per-platform link args to napi-build. Gated on
    // the `node` feature so `napi_build` is only referenced (and only compiled)
    // when that feature — and hence the optional build-dependency — is active.
    #[cfg(feature = "node")]
    napi_build::setup();

    // Python: dependency-free macOS dynamic-lookup for the abi3 extension.
    // `CARGO_FEATURE_<NAME>` is present only when that feature is enabled.
    let python_feature = std::env::var_os("CARGO_FEATURE_PYTHON").is_some();
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if python_feature && target_os == "macos" {
        println!("cargo:rustc-cdylib-link-arg=-undefined");
        println!("cargo:rustc-cdylib-link-arg=dynamic_lookup");
    }
}
