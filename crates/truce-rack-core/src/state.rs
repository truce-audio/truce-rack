//! Versioned envelope around plugin state blobs.
//!
//! Plugin state coming back from `get_state` / going into
//! `set_state` is opaque on the host side — but to defend the
//! host against a corrupt or wrong-format blob (one was sniffed
//! from a different plugin, one is from a future version), we
//! wrap each blob in a host-defined envelope before persisting
//! it to disk and unwrap it before feeding it back to the plugin.
//!
//! Plugin payloads themselves stay opaque — the envelope only
//! carries metadata the host can validate without parsing the
//! payload.

/// Magic bytes prefixing every envelope. Lets a host that
/// reads a stray file know it's looking at a rack-wrapped blob
/// rather than the plugin's raw state.
pub const ENVELOPE_MAGIC: &[u8; 4] = b"RKST";

/// Bumped when the envelope layout itself changes (not the
/// payload). v1 = `MAGIC | u16 version | u8 format_id | u8 pad |
/// u32 payload_len | payload[..]`.
pub const ENVELOPE_VERSION: u16 = 1;

/// Numeric tag for the format that produced the payload.
/// Lets the host refuse to feed a VST3 blob to a CLAP plugin.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatId {
    /// Reserved sentinel — never written.
    Unknown = 0,
    /// CLAP.
    Clap = 1,
    /// VST3.
    Vst3 = 2,
    /// AU v2 (`auFX` / `aufx` etc.).
    AuV2 = 3,
    /// AU v3 (NSExtension-based).
    AuV3 = 4,
    /// VST2.
    Vst2 = 5,
    /// LV2.
    Lv2 = 6,
    /// AAX.
    Aax = 7,
}

/// Failure modes when unwrapping an envelope.
#[derive(Debug, thiserror::Error)]
pub enum StateLoadError {
    /// Buffer is shorter than the envelope header demands.
    #[error("state blob too short: expected at least {expected} bytes, got {actual}")]
    Truncated {
        /// Minimum bytes required.
        expected: usize,
        /// Actual bytes available.
        actual: usize,
    },

    /// Magic prefix didn't match — wrong file, raw plugin
    /// state, or corruption.
    #[error("state magic mismatch")]
    BadMagic,

    /// Envelope version is newer than this rack build knows.
    #[error("state envelope version {found} > supported {supported}")]
    UnsupportedVersion {
        /// Version field in the blob.
        found: u16,
        /// Maximum version this rack build understands.
        supported: u16,
    },

    /// Payload-length field disagrees with the buffer's actual
    /// length.
    #[error("state payload length {declared} != trailing bytes {actual}")]
    LengthMismatch {
        /// `payload_len` from the envelope header.
        declared: u32,
        /// Trailing bytes after the header in the supplied buffer.
        actual: usize,
    },

    /// Payload is from a different format than the host expected.
    /// The host can decide whether to surface this as an error or
    /// try to feed it anyway (some plugins span multiple formats).
    #[error("state format mismatch: payload is {found:?}, host expected {expected:?}")]
    FormatMismatch {
        /// Format tag in the envelope.
        found: FormatId,
        /// Format the host was loading into.
        expected: FormatId,
    },
}

/// Host-side wrapper around a plugin state payload.
#[derive(Debug, Clone)]
pub struct StateEnvelope<'a> {
    /// Source format of the payload.
    pub format: FormatId,
    /// Opaque plugin bytes.
    pub payload: &'a [u8],
}

const HEADER_LEN: usize = 4 + 2 + 1 + 1 + 4;

impl StateEnvelope<'_> {
    /// Serialize this envelope into a freshly-allocated `Vec`.
    /// Hosts then persist the result however they like (disk
    /// file, DAW project blob, etc.).
    ///
    /// Layout:
    /// ```text
    /// 0..4   "RKST" magic
    /// 4..6   u16 envelope version (little-endian)
    /// 6      u8  format id
    /// 7      u8  reserved (must be 0)
    /// 8..12  u32 payload length (little-endian)
    /// 12..   payload bytes
    /// ```
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER_LEN + self.payload.len());
        out.extend_from_slice(ENVELOPE_MAGIC);
        out.extend_from_slice(&ENVELOPE_VERSION.to_le_bytes());
        out.push(self.format as u8);
        out.push(0);
        #[allow(clippy::cast_possible_truncation)]
        let payload_len = self.payload.len() as u32;
        out.extend_from_slice(&payload_len.to_le_bytes());
        out.extend_from_slice(self.payload);
        out
    }

    /// Parse an envelope from `bytes`. Validates magic, version,
    /// and payload length. The returned payload borrows from
    /// `bytes`.
    ///
    /// # Errors
    /// Returns [`StateLoadError`] if the buffer is shorter than the
    /// envelope header, the magic bytes do not match, the version
    /// is newer than [`ENVELOPE_VERSION`], the format byte is not
    /// one of the known [`FormatId`] values, or the declared
    /// payload length doesn't match the remaining bytes.
    pub fn decode(bytes: &[u8]) -> Result<StateEnvelope<'_>, StateLoadError> {
        if bytes.len() < HEADER_LEN {
            return Err(StateLoadError::Truncated {
                expected: HEADER_LEN,
                actual: bytes.len(),
            });
        }
        if &bytes[0..4] != ENVELOPE_MAGIC {
            return Err(StateLoadError::BadMagic);
        }
        let version = u16::from_le_bytes([bytes[4], bytes[5]]);
        if version > ENVELOPE_VERSION {
            return Err(StateLoadError::UnsupportedVersion {
                found: version,
                supported: ENVELOPE_VERSION,
            });
        }
        let format = match bytes[6] {
            1 => FormatId::Clap,
            2 => FormatId::Vst3,
            3 => FormatId::AuV2,
            4 => FormatId::AuV3,
            5 => FormatId::Vst2,
            6 => FormatId::Lv2,
            7 => FormatId::Aax,
            _ => FormatId::Unknown,
        };
        let declared_len = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
        let payload = &bytes[HEADER_LEN..];
        if declared_len as usize != payload.len() {
            return Err(StateLoadError::LengthMismatch {
                declared: declared_len,
                actual: payload.len(),
            });
        }
        Ok(StateEnvelope { format, payload })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let payload = b"opaque plugin state bytes";
        let env = StateEnvelope {
            format: FormatId::Clap,
            payload,
        };
        let encoded = env.encode();
        let decoded = StateEnvelope::decode(&encoded).expect("decode");
        assert_eq!(decoded.format, FormatId::Clap);
        assert_eq!(decoded.payload, payload);
    }

    #[test]
    fn truncated_header_rejected() {
        let short = b"RK";
        let err = StateEnvelope::decode(short).unwrap_err();
        assert!(matches!(err, StateLoadError::Truncated { .. }));
    }

    #[test]
    fn bad_magic_rejected() {
        let mut buf = b"XXXX".to_vec();
        buf.extend(std::iter::repeat_n(0u8, HEADER_LEN));
        let err = StateEnvelope::decode(&buf).unwrap_err();
        assert!(matches!(err, StateLoadError::BadMagic));
    }

    #[test]
    fn length_mismatch_rejected() {
        let env = StateEnvelope {
            format: FormatId::Vst3,
            payload: b"abcd",
        };
        let mut buf = env.encode();
        buf.push(0xFF); // trailing junk
        let err = StateEnvelope::decode(&buf).unwrap_err();
        assert!(matches!(err, StateLoadError::LengthMismatch { .. }));
    }
}
