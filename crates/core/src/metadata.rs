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
}
