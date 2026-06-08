use std::time::Duration;

use bytes::{Bytes, BytesMut};

use crate::catalog::CatalogFormat;
use crate::container::Timestamp;
use crate::container::fmp4::{Emit, Export as Fmp4Export};

/// Default target segment duration when [`Export::with_segment_duration`] isn't set.
const DEFAULT_SEGMENT_DURATION: Duration = Duration::from_secs(30);

/// One output unit from [`Export`]: the leading init segment, or a complete
/// media segment ready to write to disk and reference from the playlist.
pub enum Segment {
	/// The init segment (ftyp + multi-track moov). Emitted once, before any
	/// media segment. Write it once (e.g. `init.mp4`) and reference it from the
	/// playlist via `EXT-X-MAP`.
	Init(Bytes),

	/// A media segment: a run of moof+mdat fragments starting on a keyframe.
	/// Valid fragmented fMP4, independently decodable given the init segment.
	Media {
		/// Concatenated fragment bytes.
		data: Bytes,
		/// Wall-clock span of the segment, for the playlist's `#EXTINF`.
		duration: Duration,
		/// Zero-based segment index (use it to name `seg_NNNNN.m4s`).
		sequence: u64,
	},
}

/// Subscribe to a moq broadcast and produce HLS / fMP4 (CMAF) segments.
///
/// Wraps an [`fmp4::Export`](crate::container::fmp4::Export) and groups its
/// per-GOP fragments into media segments. A new segment is cut at the first
/// video keyframe once the open segment has reached the target duration, so
/// every segment starts on a keyframe and plays on its own. Segment durations
/// come from real frame timestamps, not a fragment count, so they track media
/// time exactly (the final segment's duration is approximate).
///
/// Audio+video and video-only broadcasts cut on keyframes. Audio-only
/// broadcasts have no keyframes, so they fall back to cutting on time alone.
pub struct Export {
	inner: Fmp4Export,
	target: Duration,

	/// Bytes of the media segment currently being accumulated.
	buffer: BytesMut,
	/// Timestamp of the open segment's first fragment.
	seg_start: Option<Timestamp>,
	/// Timestamp of the most recent fragment, for the final segment's duration.
	last_ts: Option<Timestamp>,
	/// Index of the next media segment to emit.
	sequence: u64,
	/// True once any keyframe fragment has been seen (i.e. the stream has video).
	saw_keyframe: bool,
	/// Set once the inner export has ended; flush the open buffer, then stop.
	finished: bool,
}

impl Export {
	/// Subscribe to `broadcast` using the default catalog format.
	pub fn new(broadcast: moq_net::BroadcastConsumer) -> Result<Self, crate::Error> {
		Self::with_catalog_format(broadcast, CatalogFormat::default())
	}

	/// Subscribe to `broadcast`, selecting an explicit catalog format for track discovery.
	pub fn with_catalog_format(
		broadcast: moq_net::BroadcastConsumer,
		catalog_format: CatalogFormat,
	) -> Result<Self, crate::Error> {
		Ok(Self {
			inner: Fmp4Export::with_catalog_format(broadcast, catalog_format)?,
			target: DEFAULT_SEGMENT_DURATION,
			buffer: BytesMut::new(),
			seg_start: None,
			last_ts: None,
			sequence: 0,
			saw_keyframe: false,
			finished: false,
		})
	}

	/// Set the maximum buffering latency for each per-track source.
	pub fn with_latency(mut self, latency: Duration) -> Self {
		self.inner = self.inner.with_latency(latency);
		self
	}

	/// Set the target segment duration. Segments are cut at the first keyframe
	/// at or after this much media time has elapsed. Defaults to 30s.
	pub fn with_segment_duration(mut self, duration: Duration) -> Self {
		self.target = duration;
		self
	}

	/// Get the next segment.
	///
	/// The first call returns [`Segment::Init`]; subsequent calls return
	/// [`Segment::Media`]. Returns `None` once the broadcast ends and the final
	/// segment has been flushed.
	pub async fn next(&mut self) -> anyhow::Result<Option<Segment>> {
		if self.finished {
			return Ok(None);
		}

		loop {
			match self.inner.emit().await? {
				Some(Emit::Init(data)) => return Ok(Some(Segment::Init(data))),
				Some(Emit::Fragment {
					data,
					timestamp,
					keyframe,
				}) => {
					self.last_ts = Some(timestamp);
					if keyframe {
						self.saw_keyframe = true;
					}

					// A rotation point is a keyframe (or any fragment for an
					// audio-only stream, which never has one).
					let rotate_point = keyframe || !self.saw_keyframe;
					if let Some(start) = self.seg_start {
						let elapsed = duration_between(start, timestamp);
						if rotate_point && elapsed >= self.target {
							let segment = self.close_segment(elapsed);
							// This fragment opens the next segment.
							self.buffer.extend_from_slice(&data);
							self.seg_start = Some(timestamp);
							return Ok(Some(segment));
						}
					} else {
						self.seg_start = Some(timestamp);
					}

					self.buffer.extend_from_slice(&data);
				}
				None => {
					self.finished = true;
					if self.buffer.is_empty() {
						return Ok(None);
					}
					let start = self.seg_start.unwrap_or_default();
					let duration = duration_between(start, self.last_ts.unwrap_or(start));
					return Ok(Some(self.close_segment(duration)));
				}
			}
		}
	}

	/// Take the accumulated buffer as a finished media segment with `duration`.
	fn close_segment(&mut self, duration: Duration) -> Segment {
		let data = std::mem::take(&mut self.buffer).freeze();
		let sequence = self.sequence;
		self.sequence += 1;
		Segment::Media {
			data,
			duration,
			sequence,
		}
	}
}

/// Media time elapsed from `start` to `end`, saturating at zero.
fn duration_between(start: Timestamp, end: Timestamp) -> Duration {
	end.checked_sub(start).map(Into::into).unwrap_or_default()
}
