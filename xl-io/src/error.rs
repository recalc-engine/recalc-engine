//! Error types returned by `xl-io`.
//!
//! Every fallible entry point returns [`IoError`]; nothing in this crate
//! panics on untrusted input (`implementation-plan.md` §8, "zero known
//! parser crashes"). Each variant carries enough context (an OOXML part
//! name, where applicable) to locate the problem in the package.

use core::fmt;

/// Which hardening cap (see [`crate::Caps`]) was violated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapKind {
    /// The zip central directory lists more entries than [`crate::Caps::max_parts`].
    PartCount,
    /// A single part's uncompressed size exceeds [`crate::Caps::max_single_part`].
    SinglePart,
    /// The running total of uncompressed bytes actually read across all parts
    /// exceeds [`crate::Caps::max_total_uncompressed`].
    TotalUncompressed,
    /// `declared_uncompressed_size / compressed_size` exceeds
    /// [`crate::Caps::max_compression_ratio`].
    CompressionRatio,
    /// XML element nesting exceeds [`crate::Caps::max_xml_depth`].
    XmlDepth,
}

impl fmt::Display for CapKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            CapKind::PartCount => "part count",
            CapKind::SinglePart => "single part uncompressed size",
            CapKind::TotalUncompressed => "total uncompressed size",
            CapKind::CompressionRatio => "compression ratio",
            CapKind::XmlDepth => "XML nesting depth",
        })
    }
}

/// Everything that can go wrong opening or reading an OOXML package.
///
/// This is the single error type for the crate's public API. Variants keep
/// the OOXML part name (e.g. `"xl/worksheets/sheet1.xml"`) that was being
/// read wherever one is known, so a caller can locate the problem without a
/// debugger.
#[derive(Debug)]
pub enum IoError {
    /// The input is not a well-formed zip archive (bad signature, truncated
    /// file, corrupt central directory, ...), or a zip-level operation
    /// (opening a named part) failed.
    Zip {
        /// The part being read when the error occurred, if known.
        part: Option<String>,
        /// Human-readable detail from the underlying zip error.
        message: String,
    },
    /// A part's bytes are not well-formed XML, or use a construct this
    /// hardened parser refuses outright (an unresolvable entity reference —
    /// see [`IoError::Doctype`] for `<!DOCTYPE`, which has its own variant).
    Xml {
        /// The part being parsed.
        part: String,
        /// Byte offset into the part's XML text where the problem was
        /// detected: the start of the offending markup where the underlying
        /// parser can pin it (`quick_xml::Reader::error_position`),
        /// otherwise the reader's position when the error surfaced.
        offset: u64,
        /// Human-readable detail from the underlying XML error.
        message: String,
    },
    /// The XML was well-formed but violates an OOXML structural expectation
    /// this crate relies on: a required element/attribute is missing, a
    /// relationship id doesn't resolve, a cell reference doesn't parse, a
    /// numeric field isn't numeric, etc.
    Structure {
        /// The part in which the problem was found.
        part: String,
        /// Description of what was expected and what was found.
        message: String,
    },
    /// A part declared a `<!DOCTYPE ...>`. Rejected unconditionally: this
    /// crate performs no DTD processing and no external entity resolution
    /// (no XXE), by construction — see the crate-level docs for the
    /// enforcement mechanism.
    Doctype {
        /// The part containing the `<!DOCTYPE`.
        part: String,
        /// Byte offset into the part's XML text of the start of the
        /// `<!DOCTYPE` markup.
        offset: u64,
    },
    /// A hardening cap from [`crate::Caps`] was violated.
    Cap {
        /// The part being read when the cap was hit, if known (the
        /// part-count cap is checked before any part is named).
        part: Option<String>,
        /// Which cap.
        kind: CapKind,
        /// The configured limit.
        limit: u64,
        /// The value that would have exceeded it.
        actual: u64,
    },
    /// A plain I/O error reading the underlying file (not a zip-format
    /// error — e.g. permission denied, path not found).
    Io(String),
}

impl IoError {
    pub(crate) fn zip(part: Option<&str>, message: impl fmt::Display) -> Self {
        IoError::Zip {
            part: part.map(str::to_string),
            message: message.to_string(),
        }
    }

    pub(crate) fn xml(part: &str, offset: u64, message: impl fmt::Display) -> Self {
        IoError::Xml {
            part: part.to_string(),
            offset,
            message: message.to_string(),
        }
    }

    pub(crate) fn structure(part: &str, message: impl Into<String>) -> Self {
        IoError::Structure {
            part: part.to_string(),
            message: message.into(),
        }
    }

    pub(crate) fn doctype(part: &str, offset: u64) -> Self {
        IoError::Doctype {
            part: part.to_string(),
            offset,
        }
    }

    pub(crate) fn cap(part: Option<&str>, kind: CapKind, limit: u64, actual: u64) -> Self {
        IoError::Cap {
            part: part.map(str::to_string),
            kind,
            limit,
            actual,
        }
    }
}

impl fmt::Display for IoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IoError::Zip {
                part: Some(p),
                message,
            } => write!(f, "zip error in `{p}`: {message}"),
            IoError::Zip {
                part: None,
                message,
            } => write!(f, "zip error: {message}"),
            IoError::Xml {
                part,
                offset,
                message,
            } => write!(f, "malformed XML in `{part}` at byte {offset}: {message}"),
            IoError::Structure { part, message } => {
                write!(f, "invalid OOXML structure in `{part}`: {message}")
            }
            IoError::Doctype { part, offset } => write!(
                f,
                "`{part}` declares a <!DOCTYPE> at byte {offset}; rejected (no DTD/XXE processing)"
            ),
            IoError::Cap {
                part: Some(p),
                kind,
                limit,
                actual,
            } => write!(
                f,
                "cap exceeded in `{p}`: {kind} limit {limit}, got {actual}"
            ),
            IoError::Cap {
                part: None,
                kind,
                limit,
                actual,
            } => write!(f, "cap exceeded: {kind} limit {limit}, got {actual}"),
            IoError::Io(message) => write!(f, "I/O error: {message}"),
        }
    }
}

impl std::error::Error for IoError {}
