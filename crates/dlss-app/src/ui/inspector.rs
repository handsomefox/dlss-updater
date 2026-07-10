pub(crate) fn desired_label(desired: &dlss_core::DesiredDll) -> String {
    match desired {
        dlss_core::DesiredDll::KeepInstalled => "Keep installed".into(),
        dlss_core::DesiredDll::LatestOfficial => "Latest official".into(),
        dlss_core::DesiredDll::Cached { release, .. }
            if dlss_core::is_imported_release(release) =>
        {
            "Imported".into()
        }
        dlss_core::DesiredDll::Cached { release, .. } => format!("Cached {}", release.0),
        dlss_core::DesiredDll::Restore { .. } => "Restore point".into(),
    }
}

pub(crate) const fn signature_label(status: dlss_core::SignatureStatus) -> &'static str {
    match status {
        dlss_core::SignatureStatus::Trusted => "Signed (trusted)",
        dlss_core::SignatureStatus::Untrusted => "Signed (untrusted)",
        dlss_core::SignatureStatus::Unsigned => "Unsigned",
        dlss_core::SignatureStatus::Unavailable => "Signature unavailable",
    }
}

pub(crate) const fn comparison_label(comparison: dlss_core::Comparison) -> &'static str {
    match comparison {
        dlss_core::Comparison::Upgrade => "Update available",
        dlss_core::Comparison::Downgrade => "Newer than target",
        dlss_core::Comparison::Identical => "Up to date",
        dlss_core::Comparison::DifferentBuild => "Different build",
        dlss_core::Comparison::Unknown => "Version unknown",
        dlss_core::Comparison::Unavailable => "No target available",
    }
}
