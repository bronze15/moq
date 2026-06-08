//! Aggregates the relay's `.stats` broadcast into a dashboard-friendly view.
//!
//! The relay publishes one JSON snapshot per stats track each tick (see
//! `rs/moq-net/src/stats.rs`). We keep the latest snapshot per track and derive
//! the numbers the dashboard shows: connected sessions, live streams, and
//! viewers per stream.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};

/// Per-broadcast counter snapshot. A subset of the JSON object published by
/// moq-net's stats aggregator (`Snapshot` in `rs/moq-net/src/stats.rs`); the
/// extra counters (subscriptions/bytes/frames/groups) are ignored. Field names
/// must match the wire contract, which the unit tests guard.
#[derive(Debug, Default, Clone, Copy, Deserialize)]
struct Snapshot {
	#[serde(default)]
	announced: u64,
	#[serde(default)]
	announced_closed: u64,
	#[serde(default)]
	broadcasts: u64,
	#[serde(default)]
	broadcasts_closed: u64,
}

/// Per-auth-root session snapshot (the `sessions.json` track).
#[derive(Debug, Default, Clone, Copy, Deserialize)]
struct SessionSnapshot {
	#[serde(default)]
	sessions: u64,
	#[serde(default)]
	sessions_closed: u64,
}

/// Which stats track a frame belongs to.
#[derive(Debug, Clone, Copy)]
pub enum Track {
	/// `publisher.json` (egress): `broadcasts - broadcasts_closed` per path is
	/// the live viewer count for that broadcast.
	Publisher,
	/// `subscriber.json` (ingress): `announced - announced_closed > 0` marks a
	/// broadcast as currently live (someone is transmitting it).
	Subscriber,
	/// `sessions.json`: `sessions - sessions_closed` per root is the live
	/// connected-session count.
	Sessions,
}

#[derive(Default)]
struct Latest {
	publisher: BTreeMap<String, Snapshot>,
	subscriber: BTreeMap<String, Snapshot>,
	sessions: BTreeMap<String, SessionSnapshot>,
}

/// Shared, cheaply-cloneable handle to the latest aggregated stats.
#[derive(Clone, Default)]
pub struct Aggregator {
	latest: Arc<RwLock<Latest>>,
}

impl Aggregator {
	/// Replace the stored snapshot for `track` from a raw JSON frame. A frame
	/// that fails to parse is logged and dropped, leaving the previous snapshot.
	pub fn update(&self, track: Track, json: &[u8]) {
		let mut latest = self.latest.write().expect("stats lock poisoned");
		match track {
			Track::Publisher => match serde_json::from_slice(json) {
				Ok(map) => latest.publisher = map,
				Err(err) => tracing::warn!(%err, "dropping malformed publisher stats frame"),
			},
			Track::Subscriber => match serde_json::from_slice(json) {
				Ok(map) => latest.subscriber = map,
				Err(err) => tracing::warn!(%err, "dropping malformed subscriber stats frame"),
			},
			Track::Sessions => match serde_json::from_slice(json) {
				Ok(map) => latest.sessions = map,
				Err(err) => tracing::warn!(%err, "dropping malformed sessions stats frame"),
			},
		}
	}

	/// Compute the dashboard view from the latest snapshots.
	pub fn view(&self) -> StatsView {
		let latest = self.latest.read().expect("stats lock poisoned");

		let connected = latest
			.sessions
			.values()
			.map(|s| s.sessions.saturating_sub(s.sessions_closed))
			.sum();

		// A stream is worth listing if it's live (being transmitted) or has any
		// viewers. Union the keys from both tracks so zero-viewer live streams
		// and (briefly) viewer-without-announce races both show up.
		let mut ids: BTreeSet<&str> = BTreeSet::new();
		for (id, s) in &latest.subscriber {
			if s.announced > s.announced_closed {
				ids.insert(id);
			}
		}
		for (id, s) in &latest.publisher {
			if s.broadcasts > s.broadcasts_closed {
				ids.insert(id);
			}
		}

		let streams: Vec<StreamView> = ids
			.into_iter()
			.map(|id| {
				let live = latest
					.subscriber
					.get(id)
					.is_some_and(|s| s.announced > s.announced_closed);
				let viewers = latest
					.publisher
					.get(id)
					.map(|s| s.broadcasts.saturating_sub(s.broadcasts_closed))
					.unwrap_or(0);
				StreamView {
					id: id.to_string(),
					viewers,
					live,
				}
			})
			.collect();

		let live_streams = streams.iter().filter(|s| s.live).count() as u64;

		StatsView {
			connected,
			live_streams,
			streams,
		}
	}
}

/// The JSON payload served at `/api/stats` and rendered by the dashboard.
#[derive(Debug, Clone, Serialize)]
pub struct StatsView {
	/// Total connected sessions across all auth roots.
	pub connected: u64,
	/// Number of broadcasts currently being transmitted.
	pub live_streams: u64,
	/// Per-stream breakdown, sorted by id.
	pub streams: Vec<StreamView>,
}

/// One stream's row in the dashboard.
#[derive(Debug, Clone, Serialize)]
pub struct StreamView {
	pub id: String,
	pub viewers: u64,
	pub live: bool,
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn connected_sums_live_sessions_across_roots() {
		let agg = Aggregator::default();
		agg.update(
			Track::Sessions,
			br#"{"anon":{"sessions":10,"sessions_closed":3},"room":{"sessions":5,"sessions_closed":5}}"#,
		);
		// anon: 10-3 = 7 live, room: 5-5 = 0 live.
		assert_eq!(agg.view().connected, 7);
	}

	#[test]
	fn streams_report_viewers_and_live_flag() {
		let agg = Aggregator::default();
		// Two broadcasts announced (being transmitted); one fully unannounced.
		agg.update(
			Track::Subscriber,
			br#"{"demo/ana":{"announced":1,"announced_closed":0},
			     "demo/leo":{"announced":1,"announced_closed":0},
			     "demo/old":{"announced":2,"announced_closed":2}}"#,
		);
		// Viewers per broadcast on the egress side.
		agg.update(
			Track::Publisher,
			br#"{"demo/ana":{"broadcasts":230,"broadcasts_closed":0},
			     "demo/leo":{"broadcasts":90,"broadcasts_closed":2}}"#,
		);

		let view = agg.view();
		assert_eq!(view.live_streams, 2, "two broadcasts are live");

		let ana = view.streams.iter().find(|s| s.id == "demo/ana").unwrap();
		assert_eq!(ana.viewers, 230);
		assert!(ana.live);

		let leo = view.streams.iter().find(|s| s.id == "demo/leo").unwrap();
		assert_eq!(leo.viewers, 88, "90 open - 2 closed");
		assert!(leo.live);

		// Fully-unannounced broadcast is excluded.
		assert!(view.streams.iter().all(|s| s.id != "demo/old"));
	}

	#[test]
	fn live_stream_with_zero_viewers_still_listed() {
		let agg = Aggregator::default();
		agg.update(
			Track::Subscriber,
			br#"{"demo/quiet":{"announced":1,"announced_closed":0}}"#,
		);
		// No publisher entry yet (nobody watching).
		let view = agg.view();
		assert_eq!(view.live_streams, 1);
		let s = &view.streams[0];
		assert_eq!(s.id, "demo/quiet");
		assert_eq!(s.viewers, 0);
		assert!(s.live);
	}
}
