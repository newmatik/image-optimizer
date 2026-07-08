//! Image format detection by content (magic bytes), independent of file
//! extension.

/// A recognized image format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageFormat {
    Jpeg,
    Png,
    Gif,
    WebP,
    Avif,
    Svg,
    /// Unrecognized / unsupported.
    Unknown,
}

/// Static per-format metadata. Keeping the canonical name, MIME type, and
/// extensions for every known format in one table means adding a format (e.g.
/// AVIF, JXL) is a single row edit instead of touching several parallel `match`
/// arms.
struct FormatSpec {
    format: ImageFormat,
    name: &'static str,
    mime: &'static str,
    extensions: &'static [&'static str],
}

const SPECS: &[FormatSpec] = &[
    FormatSpec {
        format: ImageFormat::Jpeg,
        name: "jpeg",
        mime: "image/jpeg",
        extensions: &["jpg", "jpeg", "jpe", "jfif"],
    },
    FormatSpec {
        format: ImageFormat::Png,
        name: "png",
        mime: "image/png",
        extensions: &["png"],
    },
    FormatSpec {
        format: ImageFormat::Gif,
        name: "gif",
        mime: "image/gif",
        extensions: &["gif"],
    },
    FormatSpec {
        format: ImageFormat::WebP,
        name: "webp",
        mime: "image/webp",
        extensions: &["webp"],
    },
    FormatSpec {
        format: ImageFormat::Avif,
        name: "avif",
        mime: "image/avif",
        extensions: &["avif"],
    },
    FormatSpec {
        format: ImageFormat::Svg,
        name: "svg",
        mime: "image/svg+xml",
        extensions: &["svg"],
    },
];

impl ImageFormat {
    /// Every known image format (excludes [`ImageFormat::Unknown`]).
    pub const ALL: &'static [ImageFormat] = &[
        ImageFormat::Jpeg,
        ImageFormat::Png,
        ImageFormat::Gif,
        ImageFormat::WebP,
        ImageFormat::Avif,
        ImageFormat::Svg,
    ];

    fn spec(self) -> Option<&'static FormatSpec> {
        SPECS.iter().find(|s| s.format == self)
    }

    /// Lowercase short name, e.g. `"jpeg"`.
    pub fn as_str(self) -> &'static str {
        self.spec().map(|s| s.name).unwrap_or("unknown")
    }

    /// MIME type for the format.
    pub fn mime(self) -> &'static str {
        self.spec()
            .map(|s| s.mime)
            .unwrap_or("application/octet-stream")
    }

    /// File extensions (lowercase, no dot) commonly used for the format.
    pub fn extensions(self) -> &'static [&'static str] {
        self.spec().map(|s| s.extensions).unwrap_or(&[])
    }

    /// Best-guess format from a file extension (case-insensitive). Used only to
    /// pre-filter directory walks; real detection is content-based.
    pub fn from_extension(ext: &str) -> ImageFormat {
        let ext = ext.to_ascii_lowercase();
        SPECS
            .iter()
            .find(|s| s.extensions.contains(&ext.as_str()))
            .map(|s| s.format)
            .unwrap_or(ImageFormat::Unknown)
    }
}

/// Detect an image format from its leading bytes.
pub fn detect_format(bytes: &[u8]) -> ImageFormat {
    let b = bytes;
    if b.len() >= 3 && b[0] == 0xFF && b[1] == 0xD8 && b[2] == 0xFF {
        return ImageFormat::Jpeg;
    }
    if b.len() >= 8 && b[..8] == [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1A, b'\n'] {
        return ImageFormat::Png;
    }
    if b.len() >= 6 && (&b[..6] == b"GIF87a" || &b[..6] == b"GIF89a") {
        return ImageFormat::Gif;
    }
    if b.len() >= 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        return ImageFormat::WebP;
    }
    // ISO-BMFF `ftyp` box. AVIF advertises `avif`/`avis` either as the major
    // brand (bytes 8..12) or among the compatible brands (from byte 16 on, in
    // 4-byte entries) — e.g. `ftypmif1` with `avif` in the compatible list.
    if b.len() >= 12 && &b[4..8] == b"ftyp" {
        let is_avif = |x: &[u8]| x == b"avif" || x == b"avis";
        let box_size = u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize;
        let end = box_size.clamp(8, b.len());
        if is_avif(&b[8..12]) {
            return ImageFormat::Avif;
        }
        let mut i = 16; // skip major_brand (8..12) and minor_version (12..16)
        while i + 4 <= end {
            if is_avif(&b[i..i + 4]) {
                return ImageFormat::Avif;
            }
            i += 4;
        }
    }
    if looks_like_svg(b) {
        return ImageFormat::Svg;
    }
    ImageFormat::Unknown
}

/// Heuristic SVG sniff: look for an `<svg` tag near the start, tolerating a
/// UTF-8/UTF-16 BOM, leading whitespace, an XML prolog, comments, and a
/// DOCTYPE.
fn looks_like_svg(bytes: &[u8]) -> bool {
    // Strip a UTF-8 BOM if present.
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    // Only inspect a reasonable prefix.
    let window = &bytes[..bytes.len().min(1024)];
    let text = match std::str::from_utf8(window) {
        Ok(t) => t,
        Err(e) => std::str::from_utf8(&window[..e.valid_up_to()]).unwrap_or(""),
    };
    let trimmed = text.trim_start();
    // Must look like XML/markup and contain an <svg element.
    let starts_markup = trimmed.starts_with('<');
    starts_markup && (text.contains("<svg") || text.contains("<SVG"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_magic_bytes() {
        assert_eq!(detect_format(&[0xFF, 0xD8, 0xFF, 0xE0]), ImageFormat::Jpeg);
        assert_eq!(
            detect_format(&[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1A, b'\n', 0]),
            ImageFormat::Png
        );
        assert_eq!(detect_format(b"GIF89a....."), ImageFormat::Gif);
        assert_eq!(detect_format(b"RIFF\0\0\0\0WEBPVP8 "), ImageFormat::WebP);
        assert_eq!(detect_format(b"\0\0\0\x20ftypavif"), ImageFormat::Avif);
        // AVIF advertised only via a compatible brand (major brand mif1).
        assert_eq!(
            detect_format(b"\x00\x00\x00\x18ftypmif1\x00\x00\x00\x00mif1avif"),
            ImageFormat::Avif
        );
        assert_eq!(detect_format(b""), ImageFormat::Unknown);
        assert_eq!(detect_format(b"not an image"), ImageFormat::Unknown);
    }

    #[test]
    fn detects_svg_variants() {
        assert_eq!(
            detect_format(b"<svg xmlns=\"...\"></svg>"),
            ImageFormat::Svg
        );
        assert_eq!(
            detect_format(b"<?xml version=\"1.0\"?>\n<svg></svg>"),
            ImageFormat::Svg
        );
        assert_eq!(
            detect_format("\u{feff}<svg></svg>".as_bytes()),
            ImageFormat::Svg
        );
        assert_eq!(
            detect_format(b"<html><body></body></html>"),
            ImageFormat::Unknown
        );
    }
}
