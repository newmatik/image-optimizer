//! Helpers for translating the high-level [`MetadataPolicy`] into the per-codec
//! notions of "what to strip".

use crate::options::MetadataPolicy;

/// Whether the ICC color profile should be preserved under a given policy.
pub fn keep_color_profile(policy: MetadataPolicy) -> bool {
    matches!(
        policy,
        MetadataPolicy::KeepColorProfile | MetadataPolicy::KeepAll
    )
}

/// Whether *all* metadata (EXIF, XMP, comments, …) should be preserved.
pub fn keep_all(policy: MetadataPolicy) -> bool {
    matches!(policy, MetadataPolicy::KeepAll)
}

/// What the EXIF data of an image says about how to show it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExifOrientation {
    /// No orientation tag, or one that leaves the pixels as stored.
    Upright,
    /// Viewers rotate or mirror the pixels (values 2 to 8). Browsers apply
    /// this to JPEG and PNG, so dropping it with the rest of the EXIF data
    /// would change the picture on screen.
    Turned(u8),
    /// The data could not be read far enough to tell.
    Unreadable,
}

/// Read the orientation from EXIF data (a TIFF structure).
pub fn exif_orientation(tiff: &[u8]) -> ExifOrientation {
    read_orientation(tiff).unwrap_or(ExifOrientation::Unreadable)
}

/// The orientation value if it turns the picture.
pub fn display_orientation(tiff: &[u8]) -> Option<u8> {
    match exif_orientation(tiff) {
        ExifOrientation::Turned(value) => Some(value),
        _ => None,
    }
}

fn read_orientation(tiff: &[u8]) -> Option<ExifOrientation> {
    const ORIENTATION: u16 = 0x0112;
    const SHORT: u16 = 3;

    let big_endian = match tiff.get(..2)? {
        b"MM" => true,
        b"II" => false,
        _ => return None,
    };
    let u16_at = |at: usize| -> Option<u16> {
        let b: [u8; 2] = tiff.get(at..at.checked_add(2)?)?.try_into().ok()?;
        Some(if big_endian {
            u16::from_be_bytes(b)
        } else {
            u16::from_le_bytes(b)
        })
    };
    let u32_at = |at: usize| -> Option<u32> {
        let b: [u8; 4] = tiff.get(at..at.checked_add(4)?)?.try_into().ok()?;
        Some(if big_endian {
            u32::from_be_bytes(b)
        } else {
            u32::from_le_bytes(b)
        })
    };
    if u16_at(2)? != 42 {
        return None;
    }
    let ifd = u32_at(4)? as usize;
    let entries = u16_at(ifd)? as usize;
    for index in 0..entries {
        let entry = ifd.checked_add(2 + index * 12)?;
        if u16_at(entry)? == ORIENTATION {
            // The whole 12-byte entry must be there; readers reject a cut one.
            tiff.get(entry..entry.checked_add(12)?)?;
            if u16_at(entry + 2)? != SHORT || u32_at(entry + 4)? != 1 {
                return None;
            }
            return Some(match u16_at(entry + 8)? {
                value @ 2..=8 => ExifOrientation::Turned(value as u8),
                _ => ExifOrientation::Upright,
            });
        }
    }
    Some(ExifOrientation::Upright)
}

/// A TIFF structure that holds nothing but the orientation tag: what is kept
/// of the EXIF data when the rest is stripped.
pub fn orientation_only_tiff(orientation: u8) -> Vec<u8> {
    let mut tiff = Vec::with_capacity(26);
    tiff.extend_from_slice(b"II*\0"); // little endian, magic 42
    tiff.extend_from_slice(&8u32.to_le_bytes()); // first IFD follows
    tiff.extend_from_slice(&1u16.to_le_bytes()); // one entry
    tiff.extend_from_slice(&0x0112u16.to_le_bytes());
    tiff.extend_from_slice(&3u16.to_le_bytes()); // SHORT
    tiff.extend_from_slice(&1u32.to_le_bytes());
    tiff.extend_from_slice(&[orientation, 0, 0, 0]);
    tiff.extend_from_slice(&0u32.to_le_bytes()); // no further IFD
    tiff
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_profile_is_kept_unless_stripping_all() {
        assert!(!keep_color_profile(MetadataPolicy::StripAll));
        assert!(keep_color_profile(MetadataPolicy::KeepColorProfile));
        assert!(keep_color_profile(MetadataPolicy::KeepAll));
    }

    #[test]
    fn only_keep_all_preserves_everything() {
        assert!(!keep_all(MetadataPolicy::StripAll));
        assert!(!keep_all(MetadataPolicy::KeepColorProfile));
        assert!(keep_all(MetadataPolicy::KeepAll));
    }

    #[test]
    fn orientation_is_read_from_both_byte_orders_and_written_back() {
        for value in 2..=8u8 {
            let tiff = orientation_only_tiff(value);
            assert_eq!(display_orientation(&tiff), Some(value));
        }
        // Upright, or not an orientation at all: nothing to keep.
        assert_eq!(display_orientation(&orientation_only_tiff(1)), None);
        assert_eq!(display_orientation(&orientation_only_tiff(9)), None);

        // Big endian, orientation as the second of two entries.
        let mut tiff = b"MM\0*\0\0\0\x08\0\x02".to_vec();
        tiff.extend_from_slice(&[0x01, 0x0F, 0, 2, 0, 0, 0, 1, 0, 0, 0, 0]); // Make
        tiff.extend_from_slice(&[0x01, 0x12, 0, 3, 0, 0, 0, 1, 0, 6, 0, 0]);
        tiff.extend_from_slice(&[0, 0, 0, 0]);
        assert_eq!(display_orientation(&tiff), Some(6));

        // Data that ends before the tag is unreadable, never "upright".
        for cut in 0..34 {
            assert_eq!(
                exif_orientation(&tiff[..cut]),
                ExifOrientation::Unreadable,
                "{cut}"
            );
        }
        assert_eq!(
            exif_orientation(&orientation_only_tiff(1)),
            ExifOrientation::Upright
        );
        // An entry cut after its value is not read either.
        assert_eq!(
            exif_orientation(&orientation_only_tiff(6)[..20]),
            ExifOrientation::Unreadable
        );
        assert_eq!(display_orientation(b"II*\0\xff\xff\xff\xff"), None);
        assert_eq!(display_orientation(b"XX*\0\x08\0\0\0"), None);
    }
}
