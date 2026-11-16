//! The apps. Each is built alone, by its feature, and the driver in `lib.rs` runs it as `current`.

#[cfg(feature = "availability")]
pub mod availability;
#[cfg(feature = "calendar")]
pub mod calendar;
#[cfg(feature = "checklist")]
pub mod checklist;
#[cfg(feature = "expenses")]
pub mod expenses;
#[cfg(feature = "poll")]
pub mod poll;

#[cfg(feature = "availability")]
pub use availability as current;
#[cfg(feature = "calendar")]
pub use calendar as current;
#[cfg(feature = "checklist")]
pub use checklist as current;
#[cfg(feature = "expenses")]
pub use expenses as current;
#[cfg(feature = "poll")]
pub use poll as current;
