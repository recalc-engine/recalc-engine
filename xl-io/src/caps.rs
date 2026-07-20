//! Hardening caps for zip/XML ingestion.
//!
//! `xl-io` treats every input package as actively hostile (the Recalc design rules, "no
//! network calls", `implementation-plan.md` §8 hardening targets). [`Caps`]
//! bounds resource usage independent of what a package's own metadata
//! claims: a crafted zip central directory can under-report a part's
//! uncompressed size, so caps are enforced twice — once as a cheap
//! pre-filter against declared metadata (fails fast, before allocating
//! anything sized from that metadata), and again as a hard ceiling on actual
//! bytes produced while streaming the decompressor, which does not trust the
//! declared size. See [`crate::package::read_part`] for the enforcement
//! point.

/// Resource limits enforced while opening and reading an OOXML package.
///
/// All limits are pre-allocation caps: exceeding one returns
/// [`crate::IoError::Cap`] before the corresponding buffer is grown past the
/// limit, never after. Construct via [`Caps::default`] and override
/// individual fields, or use [`Caps::new`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Caps {
    /// Maximum sum of uncompressed bytes actually read across every part
    /// this crate decompresses. Default 512 MiB.
    pub max_total_uncompressed: u64,
    /// Maximum uncompressed size of any single part. Default 256 MiB.
    pub max_single_part: u64,
    /// Maximum number of entries the zip central directory may list, checked
    /// before any part is opened. Default 10,000.
    pub max_parts: usize,
    /// Maximum allowed ratio of declared uncompressed size to compressed
    /// size for any one part (a coarse zip-bomb pre-filter; the real
    /// backstop is [`Caps::max_single_part`] enforced against actual
    /// decompressed bytes). Default 200.
    pub max_compression_ratio: u64,
    /// Maximum XML element nesting depth. Default 256.
    pub max_xml_depth: u32,
}

impl Caps {
    /// Same as [`Caps::default`]; provided for call-site clarity.
    #[must_use]
    pub fn new() -> Caps {
        Caps::default()
    }
}

impl Default for Caps {
    fn default() -> Caps {
        Caps {
            max_total_uncompressed: 512 * 1024 * 1024,
            max_single_part: 256 * 1024 * 1024,
            max_parts: 10_000,
            max_compression_ratio: 200,
            max_xml_depth: 256,
        }
    }
}
