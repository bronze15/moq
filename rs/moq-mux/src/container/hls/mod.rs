//! HLS (HTTP Live Streaming).
//!
//! [`Import`] watches an HLS master or media playlist, downloads each fMP4
//! segment as it appears, and feeds it through the fMP4 importer.
//!
//! [`Export`] goes the other way: it wraps [`fmp4::Export`](crate::container::fmp4::Export)
//! and groups its fragments into independently-playable fMP4 media segments,
//! cut on video keyframe boundaries at a target duration. Pair the emitted
//! [`Segment`]s with [`Playlist`] to write a standard `index.m3u8` + `init.mp4`
//! + `seg_*.m4s` layout that ffmpeg, VLC, Safari and hls.js play directly.

mod export;
mod import;
mod playlist;

pub use export::*;
pub use import::*;
pub use playlist::*;

#[cfg(test)]
mod export_test;
