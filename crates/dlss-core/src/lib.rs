//! Platform-neutral domain model and provider contracts.

mod imports;
mod model;
mod tools;
mod traits;
mod workflow;

pub use imports::*;
pub use model::*;
pub use tools::*;
pub use traits::*;
pub use workflow::*;

#[must_use]
pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or_default()
}
