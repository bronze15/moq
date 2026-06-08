use std::time::Duration;

use m3u8_rs::{Map, MediaPlaylist, MediaPlaylistType, MediaSegment};

/// Builds an HLS media playlist (`index.m3u8`) for an fMP4 recording.
///
/// Every segment shares one init segment via `EXT-X-MAP`. Push each segment as
/// it's written, then [`render`](Self::render) the playlist; rendering with
/// `finished = true` marks it VOD and appends `EXT-X-ENDLIST`. Rendering after
/// every segment keeps a partially-recorded playlist usable.
pub struct Playlist {
	init_uri: String,
	segments: Vec<(String, Duration)>,
}

impl Playlist {
	/// Start a playlist whose segments all map to `init_uri` (e.g. `init.mp4`).
	pub fn new(init_uri: impl Into<String>) -> Self {
		Self {
			init_uri: init_uri.into(),
			segments: Vec::new(),
		}
	}

	/// Append a segment with its `#EXTINF` duration.
	pub fn push(&mut self, uri: impl Into<String>, duration: Duration) {
		self.segments.push((uri.into(), duration));
	}

	/// Render the playlist as `index.m3u8` text. `finished` marks the recording
	/// complete: VOD playlist type plus `EXT-X-ENDLIST`.
	pub fn render(&self, finished: bool) -> String {
		let map = Map {
			uri: self.init_uri.clone(),
			..Default::default()
		};

		let segments = self
			.segments
			.iter()
			.enumerate()
			.map(|(i, (uri, duration))| MediaSegment {
				uri: uri.clone(),
				duration: duration.as_secs_f32(),
				// EXT-X-MAP carries to following segments, so emit it once.
				map: (i == 0).then(|| map.clone()),
				..Default::default()
			})
			.collect();

		// EXT-X-TARGETDURATION must be >= every segment's rounded duration.
		let target_duration = self
			.segments
			.iter()
			.map(|(_, d)| d.as_secs_f64().ceil() as u64)
			.max()
			.unwrap_or(0);

		let playlist = MediaPlaylist {
			version: Some(7),
			target_duration,
			segments,
			end_list: finished,
			playlist_type: finished.then_some(MediaPlaylistType::Vod),
			independent_segments: true,
			..Default::default()
		};

		let mut out = Vec::new();
		// write_to only fails if the writer fails; writing to a Vec can't.
		playlist.write_to(&mut out).expect("writing m3u8 to Vec is infallible");
		String::from_utf8(out).expect("m3u8 output is valid UTF-8")
	}
}
