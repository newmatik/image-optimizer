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
