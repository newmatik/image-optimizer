//! Engine error type.

use thiserror::Error;

/// Errors produced while optimizing a single image in memory.
///
/// File-level problems (read/write failures) are reported through
/// [`crate::result::OptimizeStatus::Failed`] rather than this type.
#[derive(Debug, Error)]
pub enum Error {
    /// The input could not be decoded by the codec for its detected format.
    #[error("decode failed: {0}")]
    Decode(String),

    /// Re-encoding failed.
    #[error("encode failed: {0}")]
    Encode(String),

    /// The image exceeds the configured pixel limit (decompression-bomb guard).
    #[error("image too large: {pixels} pixels exceeds limit of {limit}")]
    TooLarge { pixels: u64, limit: u64 },

    /// A codec panicked (e.g. inside a C library) and the unwind was caught.
    #[error("codec aborted: {0}")]
    Panicked(String),

    /// I/O error.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// Anything else.
    #[error("{0}")]
    Other(String),
}

/// Extract a human-readable message from a caught panic payload (the value
/// returned by `std::panic::catch_unwind` on the `Err` path).
pub(crate) fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panic_message_reads_str_and_string_payloads() {
        assert_eq!(panic_message(Box::new("boom")), "boom");
        assert_eq!(panic_message(Box::new(String::from("bang"))), "bang");
    }

    #[test]
    fn panic_message_falls_back_for_other_payloads() {
        assert_eq!(panic_message(Box::new(42_u32)), "unknown panic");
    }

    #[test]
    fn display_messages_are_stable() {
        assert_eq!(Error::Decode("x".into()).to_string(), "decode failed: x");
        assert_eq!(Error::Encode("y".into()).to_string(), "encode failed: y");
        assert_eq!(
            Error::TooLarge {
                pixels: 10,
                limit: 5
            }
            .to_string(),
            "image too large: 10 pixels exceeds limit of 5"
        );
        assert_eq!(Error::Panicked("z".into()).to_string(), "codec aborted: z");
        assert_eq!(Error::Other("w".into()).to_string(), "w");
    }

    #[test]
    fn io_errors_convert_with_from() {
        let err: Error = std::io::Error::new(std::io::ErrorKind::NotFound, "gone").into();
        assert!(matches!(err, Error::Io(_)));
        assert_eq!(err.to_string(), "io error: gone");
    }
}
