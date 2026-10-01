//! Library root — pure-logic modules that compile on both host (std, for
//! testing) and the embedded target (no_std).
//!
//! Hardware-specific modules (`display`, `battery`, `buttons`) live exclusively
//! in `src/main.rs` and are only compiled for the embedded target.
#![cfg_attr(not(test), no_std)]

pub mod config;
pub mod grocy;
pub mod mesh;
pub mod qr;
