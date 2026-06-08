//! Tests for the HLS / fMP4 segmenter.

use std::io::Cursor;
use std::time::Duration;

use bytes::BytesMut;
use mp4_atom::DecodeMaybe;

use crate::container::Timestamp;
use crate::container::hls::{Export, Segment};

/// One Annex-B access unit carrying SPS + PPS + IDR, so every keyframe can
/// stand on its own (and the first lets the exporter build the avcC init).
fn keyframe_payload() -> bytes::Bytes {
	const SC: &[u8] = &[0, 0, 0, 1];
	let sps = &[0x67u8, 0x42, 0xc0, 0x1f, 0xde, 0xad, 0xbe, 0xef][..];
	let pps = &[0x68u8, 0xce, 0x3c, 0x80][..];
	let idr = &[0x65u8, 0x88, 0x84, 0x21, 0x00, 0x11, 0x22, 0x33][..];

	let mut payload = BytesMut::new();
	for nal in [sps, pps, idr] {
		payload.extend_from_slice(SC);
		payload.extend_from_slice(nal);
	}
	payload.freeze()
}

/// Drain the exporter, returning the init segment and every media segment.
async fn drain(mut export: Export) -> (bytes::Bytes, Vec<(Duration, bytes::Bytes)>) {
	let mut init = None;
	let mut media = Vec::new();
	while let Some(segment) = tokio::time::timeout(Duration::from_secs(1), export.next())
		.await
		.expect("exporter timed out")
		.expect("exporter result")
	{
		match segment {
			Segment::Init(data) => init = Some(data),
			Segment::Media {
				data,
				duration,
				sequence,
			} => {
				assert_eq!(
					sequence as usize,
					media.len(),
					"segment sequence must be dense and ordered"
				);
				media.push((duration, data));
			}
		}
	}
	(init.expect("expected an init segment"), media)
}

/// The first 8 bytes of a media segment must be a box header whose type is
/// `moof` (the fragment's `moof`+`mdat` start).
fn assert_starts_with_moof(data: &[u8]) {
	assert!(data.len() >= 8, "segment too short to hold a box header");
	assert_eq!(&data[4..8], b"moof", "media segment must start with a moof box");
}

/// An Avc3 (Legacy) video broadcast with keyframes at 0s/1s/2s/3s, recorded at
/// a 2s target, must yield one init segment and two media segments cut on
/// keyframes: [0s,2s) then [2s,3s). Each is independently-playable fMP4.
#[tokio::test(start_paused = true)]
async fn segments_cut_on_keyframes_at_target_duration() {
	use hang::catalog::{Container, H264, VideoConfig};

	let broadcast = moq_net::Broadcast::new();
	let mut producer = broadcast.produce();
	let consumer = producer.consume();

	let mut catalog = crate::catalog::hang::Producer::new(&mut producer).unwrap();
	let track = producer.unique_track(".avc3").unwrap();
	let mut config = VideoConfig::new(H264 {
		profile: 0x42,
		constraints: 0xc0,
		level: 0x1f,
		inline: true,
	});
	config.coded_width = Some(320);
	config.coded_height = Some(240);
	config.container = Container::Legacy;
	catalog.lock().video.renditions.insert(track.name.clone(), config);

	let mut track_producer = crate::container::Producer::new(track, crate::catalog::hang::Container::Legacy);
	for secs in 0..4 {
		track_producer
			.write(crate::container::Frame {
				timestamp: Timestamp::from_micros(secs * 1_000_000).unwrap(),
				payload: keyframe_payload(),
				keyframe: true,
			})
			.unwrap();
	}
	track_producer.finish().unwrap();
	// Finish the catalog track so the exporter sees a clean end-of-broadcast
	// rather than a `Dropped` error; keep `producer` alive through the drain.
	catalog.finish().unwrap();

	let export = Export::new(consumer)
		.expect("new hls export")
		.with_segment_duration(Duration::from_secs(2));

	let (init, media) = drain(export).await;
	drop(producer);

	// Init segment parses to a single-track moov.
	let mut cursor = Cursor::new(init.as_ref());
	let mut moov = None;
	let mut saw_ftyp = false;
	while let Some(atom) = mp4_atom::Any::decode_maybe(&mut cursor).expect("decode init") {
		match atom {
			mp4_atom::Any::Ftyp(_) => saw_ftyp = true,
			mp4_atom::Any::Moov(m) => moov = Some(m),
			_ => {}
		}
	}
	assert!(saw_ftyp, "init segment missing ftyp");
	assert_eq!(moov.expect("init segment missing moov").trak.len(), 1);

	// Two segments, cut on keyframes: [0,2s) and [2s,3s).
	assert_eq!(media.len(), 2, "expected two media segments");
	assert_eq!(media[0].0, Duration::from_secs(2), "first segment spans 0s..2s");
	assert_eq!(media[1].0, Duration::from_secs(1), "final segment spans 2s..3s");
	for (_, data) in &media {
		assert_starts_with_moof(data);
	}
}

/// With a target longer than the whole broadcast, everything lands in a single
/// media segment (still one init first).
#[tokio::test(start_paused = true)]
async fn single_segment_when_target_exceeds_duration() {
	use hang::catalog::{Container, H264, VideoConfig};

	let broadcast = moq_net::Broadcast::new();
	let mut producer = broadcast.produce();
	let consumer = producer.consume();

	let mut catalog = crate::catalog::hang::Producer::new(&mut producer).unwrap();
	let track = producer.unique_track(".avc3").unwrap();
	let mut config = VideoConfig::new(H264 {
		profile: 0x42,
		constraints: 0xc0,
		level: 0x1f,
		inline: true,
	});
	config.coded_width = Some(320);
	config.coded_height = Some(240);
	config.container = Container::Legacy;
	catalog.lock().video.renditions.insert(track.name.clone(), config);

	let mut track_producer = crate::container::Producer::new(track, crate::catalog::hang::Container::Legacy);
	for secs in 0..3 {
		track_producer
			.write(crate::container::Frame {
				timestamp: Timestamp::from_micros(secs * 1_000_000).unwrap(),
				payload: keyframe_payload(),
				keyframe: true,
			})
			.unwrap();
	}
	track_producer.finish().unwrap();
	catalog.finish().unwrap();

	let export = Export::new(consumer)
		.expect("new hls export")
		.with_segment_duration(Duration::from_secs(60));

	let (_init, media) = drain(export).await;
	drop(producer);
	assert_eq!(media.len(), 1, "everything should fit in one segment");
	assert_starts_with_moof(&media[0].1);
}
