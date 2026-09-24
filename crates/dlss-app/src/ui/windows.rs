use dlss_core::{ReleaseState, SystemToolState};

pub(crate) fn format_timestamp(timestamp: i64) -> String {
    jiff::Timestamp::from_second(timestamp).map_or_else(
        |_| timestamp.to_string(),
        |timestamp| {
            timestamp
                .to_zoned(jiff::tz::TimeZone::system())
                .strftime("%Y-%m-%d %H:%M:%S")
                .to_string()
        },
    )
}

/// The local calendar date, for release and import dates.
pub(crate) fn format_date(timestamp: i64) -> String {
    jiff::Timestamp::from_second(timestamp).map_or_else(
        |_| timestamp.to_string(),
        |timestamp| {
            timestamp
                .to_zoned(jiff::tz::TimeZone::system())
                .strftime("%Y-%m-%d")
                .to_string()
        },
    )
}

/// Hours and minutes in local time, for "today" moments like the last update.
pub(crate) fn format_time(timestamp: i64) -> String {
    jiff::Timestamp::from_second(timestamp).map_or_else(
        |_| timestamp.to_string(),
        |timestamp| {
            timestamp
                .to_zoned(jiff::tz::TimeZone::system())
                .strftime("%H:%M")
                .to_string()
        },
    )
}

pub(crate) fn progress_label(state: ReleaseState, received: u64, total: Option<u64>) -> String {
    let amount = total
        .filter(|total| *total > 0)
        .map(|total| format!(" {}%", received.saturating_mul(100) / total))
        .unwrap_or_default();
    match state {
        ReleaseState::Downloading => format!("Downloading the official release…{amount}"),
        ReleaseState::Downloaded => "Download complete".into(),
        ReleaseState::Validating => "Checking NVIDIA signatures…".into(),
        ReleaseState::Invalid => "The release failed verification".into(),
        ReleaseState::Ready => "Release downloaded and verified".into(),
        ReleaseState::MetadataOnly => "Release metadata loaded".into(),
    }
}

pub(crate) fn state_label(state: &SystemToolState) -> String {
    match state {
        SystemToolState::NotConfigured => "Not configured".into(),
        SystemToolState::Off => "Off".into(),
        SystemToolState::DlssIndicatorDebug => "Debug only".into(),
        SystemToolState::DlssIndicatorProduction => "Production + debug".into(),
        SystemToolState::CustomDword(value) => format!("Custom value ({value})"),
        SystemToolState::UnexpectedType { .. } => "Unexpected registry type".into(),
        SystemToolState::Unavailable(reason) => format!("Unavailable ({reason})"),
    }
}
