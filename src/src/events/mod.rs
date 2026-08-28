//! Device input, the window's own calls into the store, and what drives the hardware.
//!
//! The three names below are what is left of a plugin protocol: `inbound` was a plugin's socket,
//! `outbound` was what the app pushed to it, and `frontend` was the Tauri window's command surface.
//! Everything now runs in-process, so these are ordinary modules.

pub mod frontend;
pub mod inbound;
pub mod outbound;
