pub(crate) fn desired_label(desired: &dlss_core::DesiredDll) -> String {
    match desired {
        dlss_core::DesiredDll::KeepInstalled => "Keep installed".into(),
        dlss_core::DesiredDll::LatestOfficial => "Latest official".into(),
        dlss_core::DesiredDll::Cached { release, .. } => format!("Cached {}", release.0),
        dlss_core::DesiredDll::Restore { .. } => "Restore point".into(),
    }
}
