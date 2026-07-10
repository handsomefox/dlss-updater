//! OS adapters and portable discovery helpers.

mod discovery;
mod portable;
pub use discovery::*;
pub use portable::*;

#[cfg(windows)]
pub mod windows;
