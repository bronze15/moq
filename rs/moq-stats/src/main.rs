//! `moq-stats`: a live stats dashboard for `moq-relay`.
//!
//! Subscribes to the relay's `.stats` broadcast over MoQ, aggregates the latest
//! snapshots, and serves an HTML dashboard plus a JSON API over HTTP. Run it next
//! to a relay that has `stats.enabled = true`.

mod aggregator;
mod web;

use anyhow::Context;
use clap::Parser;
use tokio::task::JoinHandle;

use aggregator::{Aggregator, Track};

#[derive(Parser, Clone)]
struct Config {
	/// The relay URL to connect to (https://...). May carry `?jwt=<token>` for auth.
	#[arg(long)]
	url: url::Url,

	/// Address for the dashboard HTTP server.
	#[arg(long, default_value = "0.0.0.0:8090")]
	listen: String,

	/// The relay's stats broadcast path (matches the relay's `stats.prefix` + node).
	#[arg(long, default_value = ".stats/node")]
	stats_broadcast: String,

	/// The MoQ client configuration.
	#[command(flatten)]
	client: moq_native::ClientConfig,

	/// The log configuration.
	#[command(flatten)]
	log: moq_native::Log,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
	let config = Config::parse();
	config.log.init()?;

	let aggregator = Aggregator::default();

	// Serve the dashboard in the background; it outlives reconnects.
	tokio::spawn({
		let aggregator = aggregator.clone();
		let listen = config.listen.clone();
		async move {
			if let Err(err) = web::serve(listen, aggregator).await {
				tracing::error!(%err, "dashboard server failed");
			}
		}
	});

	// Connect to the relay and consume the stats broadcast, reconnecting on drop.
	let client = config.client.init()?;
	let origin = moq_net::Origin::random().produce();
	let reconnect = client.with_consume(origin.clone()).reconnect(config.url.clone());

	let stats_path: moq_net::Path = config.stats_broadcast.clone().into();
	let mut consumer = origin
		.scope(&[stats_path])
		.context("not allowed to consume the stats broadcast")?
		.consume();

	tracing::info!(broadcast = %config.stats_broadcast, listen = %config.listen, "moq-stats started");

	let mut readers: Vec<JoinHandle<()>> = Vec::new();
	loop {
		tokio::select! {
			Some(announce) = consumer.announced() => match announce {
				(path, Some(broadcast)) => {
					tracing::info!(broadcast = %path, "stats broadcast online, subscribing");
					abort_all(&mut readers);
					readers.push(spawn_reader(&broadcast, "publisher.json", Track::Publisher, aggregator.clone())?);
					readers.push(spawn_reader(&broadcast, "subscriber.json", Track::Subscriber, aggregator.clone())?);
					readers.push(spawn_reader(&broadcast, "sessions.json", Track::Sessions, aggregator.clone())?);
				}
				(path, None) => {
					tracing::warn!(broadcast = %path, "stats broadcast offline");
					abort_all(&mut readers);
				}
			},
			res = reconnect.closed() => return res,
		}
	}
}

/// Subscribe to one stats track and spawn a task that pumps its frames into the
/// aggregator.
fn spawn_reader(
	broadcast: &moq_net::BroadcastConsumer,
	track: &str,
	kind: Track,
	aggregator: Aggregator,
) -> anyhow::Result<JoinHandle<()>> {
	let consumer = broadcast.subscribe_track(&moq_net::Track {
		name: track.to_string(),
		priority: 0,
	})?;
	Ok(tokio::spawn(read_track(consumer, kind, aggregator)))
}

/// Read frames from a stats track, applying the latest frame of each group to
/// the aggregator. Each frame is a full JSON snapshot, so only the newest one
/// per group matters.
async fn read_track(mut track: moq_net::TrackConsumer, kind: Track, aggregator: Aggregator) {
	loop {
		match track.recv_group().await {
			Ok(Some(mut group)) => {
				let mut latest = None;
				loop {
					match group.read_frame().await {
						Ok(Some(frame)) => latest = Some(frame),
						Ok(None) => break,
						Err(err) => {
							tracing::warn!(%err, "stats frame read error");
							return;
						}
					}
				}
				if let Some(frame) = latest {
					aggregator.update(kind, &frame);
				}
			}
			Ok(None) => return,
			Err(err) => {
				tracing::warn!(%err, "stats group read error");
				return;
			}
		}
	}
}

fn abort_all(readers: &mut Vec<JoinHandle<()>>) {
	for handle in readers.drain(..) {
		handle.abort();
	}
}
