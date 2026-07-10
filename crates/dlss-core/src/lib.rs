//! Platform-neutral domain model and provider contracts.

mod model;
mod tools;
mod traits;
mod workflow;

pub use model::*;
pub use tools::*;
pub use traits::*;
pub use workflow::*;

pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default()
}
