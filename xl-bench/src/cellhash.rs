//! `recalc cell-hash` — the **serial-vs-parallel corpus sweep** instrument
//! (Lane D; the deferred half of the `rayon` dependency policy, condition 4, documented in
//! `docs/parallel-sweep.md`).
//!
//! # Why this exists
//! The `parallel` recalc feature (RFC-0014, M2 lane 9) may only ever ship once
//! there is corpus-scale evidence that the parallel executor is **bit-identical**
//! to the serial one — not merely "same summary counts", but the *same exact
//! value in every cell*. This module produces that evidence two ways:
//!
//! 1. **Cross-binary** (the primary, platform-portable check the task specifies):
//!    [`dump_workbook`] recalculates a workbook with whatever executor the
//!    running binary was built with — serial in a default build, parallel in a
//!    `--features parallel` build — and emits a **canonical, bit-exact**
//!    per-cell fingerprint. Run the sweep with each binary and diff the two
//!    dumps: byte-identical dumps ⇒ per-cell bit-identity across the two builds.
//!    The 128-bit [`CellDump::hash`] is a compact proxy (one changed cell flips
//!    exactly one workbook's hash); the full `--dump` file is the authoritative
//!    per-cell comparison.
//!
//! 2. **In-process self-check** (stronger, and the non-vacuity guard —
//!    only compiled under `--features parallel`): [`self_check_workbook`] loads
//!    the *same* workbook into two engines, runs one through the parallel
//!    [`recalc`](xl_engine::Engine::recalc) and one through the forced-serial
//!    [`recalc_serial`](xl_engine::Engine::recalc_serial), and compares every
//!    cell. It also records whether the parallel **gate was open**
//!    ([`Engine::parallel_gate_open`](xl_engine::Engine::parallel_gate_open)),
//!    so the sweep can report how many workbooks actually engaged the concurrent
//!    executor rather than silently falling back to serial — otherwise a "sweep"
//!    could be comparing serial to serial and prove nothing.
//!
//! # Value encoding (bit-exact, delimiter-safe)
//! Every cell value maps to an ASCII token with no tab/newline, so a dump is a
//! clean TSV that `cmp`/`diff` compares exactly:
//! - `N<16 hex>` — a [`Value::Number`] as the raw big-endian bits of its `f64`
//!   ([`f64::to_bits`]); captures `-0.0` vs `0.0` and every ULP exactly.
//! - `S<hex>` — a [`Value::Text`], the UTF-8 bytes hex-encoded (so embedded
//!   tabs/newlines/quotes can never corrupt the line).
//! - `B0` / `B1` — a [`Value::Bool`].
//! - `E<code>` — a [`Value::Error`], its Excel code (`#DIV/0!`, …); no code
//!   contains a token/array delimiter.
//! - `_` — [`Value::Blank`]; `X` — a formula cell the engine left with no
//!   stored value at all (distinct from `Blank`).
//! - `A<r>x<c>[t,t,…]` — a [`Value::Array`], row-major, elements recursively
//!   encoded (a spill anchor may hold one).
//! - `R<hex>` / `L?` — a [`Value::Ref`] / [`Value::Lambda`]; neither is a
//!   normal stored cell value (lambdas are born-refusing), encoded defensively.

use std::path::Path;

use xl_value::Value;

use crate::report::RunError;

// ---- 128-bit FNV-1a -------------------------------------------------------
// Standard FNV-1a-128 constants (dep-free; the workspace forbids external
// crates). A per-cell change flips the digest with probability 1 − 2⁻¹²⁸, and
// across the whole 3,640-workbook corpus the collision probability is
// astronomically small — but the full `--dump` file, not the digest, is the
// authoritative per-cell comparison; the digest is the compact summary.
const FNV128_OFFSET: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
const FNV128_PRIME: u128 = 0x0000_0000_0100_0000_0000_0000_0000_013b;

/// A streaming FNV-1a-128 hasher.
#[derive(Clone, Copy)]
pub struct Fnv128(u128);

impl Default for Fnv128 {
    fn default() -> Self {
        Fnv128(FNV128_OFFSET)
    }
}

impl Fnv128 {
    /// A fresh hasher seeded at the FNV-1a-128 offset basis.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Folds `bytes` into the running digest.
    pub fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 ^= u128::from(b);
            self.0 = self.0.wrapping_mul(FNV128_PRIME);
        }
    }

    /// The current 128-bit digest.
    #[must_use]
    pub fn finish(&self) -> u128 {
        self.0
    }
}

/// One workbook's canonical, bit-exact recalc fingerprint.
pub struct CellDump {
    /// `(sheet, row, col, encoded token)` for every formula cell, in canonical
    /// order: workbook sheet order, then ascending `(row, col)` (the sheet's
    /// `BTreeMap` key order). Identical inputs ⇒ identical order on every
    /// platform and both build configs, so two dumps diff line-for-line.
    pub cells: Vec<(String, u32, u32, String)>,
    /// FNV-1a-128 over the canonical `(sheet, row, col, token)` stream.
    pub hash: u128,
}

impl CellDump {
    /// Number of formula cells fingerprinted.
    #[must_use]
    pub fn n_cells(&self) -> usize {
        self.cells.len()
    }
}

/// Hex-encodes `bytes` (lower-case, two chars per byte) into `out`.
fn push_hex(out: &mut String, bytes: &[u8]) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
}

/// Bit-exact, delimiter-safe encoding of one value (see the module docs).
#[must_use]
pub fn encode_value(v: &Value) -> String {
    let mut s = String::new();
    encode_into(v, &mut s);
    s
}

fn encode_into(v: &Value, out: &mut String) {
    match v {
        Value::Number(n) => {
            out.push('N');
            push_hex(out, &n.to_bits().to_be_bytes());
        }
        Value::Text(t) => {
            out.push('S');
            push_hex(out, t.as_str().as_bytes());
        }
        Value::Bool(b) => out.push_str(if *b { "B1" } else { "B0" }),
        Value::Error(k) => {
            out.push('E');
            out.push_str(k.as_str());
        }
        Value::Blank => out.push('_'),
        Value::Array(a) => {
            out.push('A');
            out.push_str(&a.rows().to_string());
            out.push('x');
            out.push_str(&a.cols().to_string());
            out.push('[');
            for (i, e) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                encode_into(e, out);
            }
            out.push(']');
        }
        // Neither is a normal stored cell value; encode defensively so the sweep
        // can never mistake one for another (lambdas are born-refusing — BC-6).
        Value::Ref(r) => {
            out.push('R');
            push_hex(out, format!("{r:?}").as_bytes());
        }
        Value::Lambda(_) => out.push_str("L?"),
    }
}

/// The `(sheet, row, col)` of every formula cell, in canonical order.
fn formula_targets(workbook: &xl_io::Workbook) -> Vec<(String, u32, u32)> {
    let mut targets = Vec::new();
    for sheet in &workbook.sheets {
        for (&(row, col), cell) in &sheet.cells {
            if cell.formula.is_some() {
                targets.push((sheet.name.clone(), row, col));
            }
        }
    }
    targets
}

/// Encodes the engine's computed value for one cell, or `"X"` if the engine left
/// the formula cell with no stored value at all.
fn cell_token(engine: &xl_engine::Engine, sheet: &str, row: u32, col: u32) -> String {
    engine
        .sheet_id(sheet)
        .and_then(|sid| engine.value(sid, row, col))
        .map(encode_value)
        .unwrap_or_else(|| "X".to_string())
}

/// Loads `path`, recalculates it with the running binary's executor (serial in a
/// default build; parallel-when-safe in a `--features parallel` build), and
/// returns the canonical per-cell fingerprint of every formula cell.
pub fn dump_workbook(path: &Path) -> Result<CellDump, RunError> {
    let workbook = xl_io::open(path).map_err(RunError::Load)?;
    let targets = formula_targets(&workbook);

    let mut engine = xl_engine::Engine::load(workbook);
    engine.recalc();

    let mut cells = Vec::with_capacity(targets.len());
    let mut h = Fnv128::new();
    for (sheet, row, col) in targets {
        let token = cell_token(&engine, &sheet, row, col);
        h.write(sheet.as_bytes());
        h.write(&[0]);
        h.write(&row.to_le_bytes());
        h.write(&col.to_le_bytes());
        h.write(token.as_bytes());
        h.write(&[0x1e]); // ASCII record separator between cells
        cells.push((sheet, row, col, token));
    }
    Ok(CellDump {
        cells,
        hash: h.finish(),
    })
}

/// Outcome of an in-process parallel-vs-serial comparison for one workbook.
#[cfg(feature = "parallel")]
pub struct SelfCheck {
    /// Whether `recalc()` engaged the parallel executor for this workbook (the
    /// whole-workbook gate was open). Non-vacuity signal for the sweep.
    pub gate_open: bool,
    /// Formula cells compared.
    pub n_cells: usize,
    /// Number of cells whose parallel value differed from the serial value.
    /// **Any non-zero value is a determinism bug** and must STOP the sweep.
    pub divergent: usize,
    /// The first divergence in canonical order, as
    /// `(sheet, A1, serial_token, parallel_token)`, for the bug report.
    pub first_divergence: Option<(String, String, String, String)>,
}

/// Loads `path` into two independent engines, recalculates one through the
/// parallel executor and one forced-serial, and compares every formula cell
/// bit-exactly. This is the corpus-scale analogue of the in-crate
/// `parallel_determinism::assert_identical` test (RFC-0014 R8), and — unlike the
/// cross-binary diff — isolates *path* (parallel vs serial) rather than *build*.
#[cfg(feature = "parallel")]
pub fn self_check_workbook(path: &Path) -> Result<SelfCheck, RunError> {
    // Two independent opens ⇒ two byte-identical inputs (Engine::load consumes
    // the workbook).
    let wb_par = xl_io::open(path).map_err(RunError::Load)?;
    let wb_ser = xl_io::open(path).map_err(RunError::Load)?;
    let targets = formula_targets(&wb_par);

    let mut par = xl_engine::Engine::load(wb_par);
    let mut ser = xl_engine::Engine::load(wb_ser);

    let gate_open = par.parallel_gate_open();
    par.recalc(); // parallel iff gate_open (else serial fallback)
    ser.recalc_serial(); // always serial — the authoritative reference

    let mut divergent = 0usize;
    let mut first_divergence = None;
    for (sheet, row, col) in &targets {
        let par_tok = cell_token(&par, sheet, *row, *col);
        let ser_tok = cell_token(&ser, sheet, *row, *col);
        if par_tok != ser_tok {
            divergent += 1;
            if first_divergence.is_none() {
                first_divergence = Some((
                    sheet.clone(),
                    crate::addr::a1_ref(*row, *col),
                    ser_tok,
                    par_tok,
                ));
            }
        }
    }
    Ok(SelfCheck {
        gate_open,
        n_cells: targets.len(),
        divergent,
        first_divergence,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use xl_value::{Array, ErrorKind};

    #[test]
    fn number_encoding_is_bit_exact_and_distinguishes_signed_zero() {
        assert_ne!(
            encode_value(&Value::Number(0.0)),
            encode_value(&Value::Number(-0.0)),
            "+0.0 and -0.0 must encode distinctly (raw f64 bits)"
        );
        // Two values one ULP apart must differ.
        let a = 1.0_f64;
        let b = f64::from_bits(a.to_bits() + 1);
        assert_ne!(
            encode_value(&Value::Number(a)),
            encode_value(&Value::Number(b))
        );
    }

    #[test]
    fn text_encoding_survives_delimiters() {
        // Tabs/newlines/commas/brackets in text must not corrupt the token.
        let t1 = encode_value(&Value::Text("a\tb\nc,]".into()));
        assert!(t1.starts_with('S'));
        assert!(!t1.contains('\t') && !t1.contains('\n'));
        assert_ne!(t1, encode_value(&Value::Text("abc".into())));
        // Blank vs empty text are distinct.
        assert_ne!(
            encode_value(&Value::Blank),
            encode_value(&Value::Text("".into()))
        );
    }

    #[test]
    fn distinct_types_encode_distinctly() {
        let vals = [
            Value::Number(1.0),
            Value::Text("1".into()),
            Value::Bool(true),
            Value::Bool(false),
            Value::Error(ErrorKind::Div0),
            Value::Error(ErrorKind::Na),
            Value::Blank,
        ];
        let mut seen = std::collections::HashSet::new();
        for v in &vals {
            assert!(seen.insert(encode_value(v)), "collision on {v:?}");
        }
    }

    #[test]
    fn array_encoding_is_recursive_and_shape_sensitive() {
        let a =
            Value::Array(Array::new(1, 2, vec![Value::Number(1.0), Value::Number(2.0)]).unwrap());
        let b =
            Value::Array(Array::new(2, 1, vec![Value::Number(1.0), Value::Number(2.0)]).unwrap());
        assert_ne!(encode_value(&a), encode_value(&b), "1x2 vs 2x1 must differ");
    }

    #[test]
    fn fnv128_detects_a_single_byte_change() {
        let mut h1 = Fnv128::new();
        h1.write(b"hello world");
        let mut h2 = Fnv128::new();
        h2.write(b"hello worle");
        assert_ne!(h1.finish(), h2.finish());
    }
}
