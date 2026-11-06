//! Window-focus and browser-history activity (opt-in), ported from the Mnemo prototype.
pub mod filters;
#[cfg(windows)]
pub mod focus;
pub mod history;
pub mod sessions;
pub mod store;
pub use sessions::{Session, summarize, totals_by_app};
pub use store::{ActivityStore, Event};
pub mod service;
