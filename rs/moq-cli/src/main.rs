mod client;
mod publish;
mod server;
mod subscribe;
mod web;

use client::*;
use hang::moq_net;
use publish::*;
use server::*;
use subscribe::*;
use web::*;

use clap::{Parser, Subcommand};
use std::path::PathBuf;
use url::Url;

#[derive(Parser, Clone)]
#[command(version = env!("VERSION"))]
pub struct Cli {
	#[command(flatten)]
	log: moq_native::Log,

	/// Iroh configuration
	#[command(flatten)]
	#[cfg(feature = "iroh")]
	iroh: moq_native::IrohEndpointConfig,

	#[command(subcommand)]
	command: Command,
}

#[derive(Subcommand, Clone)]
pub enum Command {
	/// Run a relay and publish a single broadcast read from stdin into it.
	Serve {
		#[command(flatten)]
		config: moq_native::ServerConfig,

		/// The name of the broadcast to serve.
		#[arg(long, alias = "name")]
		broadcast: String,

		/// Optionally serve static files from the given directory.
		#[arg(long)]
		dir: Option<PathBuf>,

		/// The format of the input media.
		#[command(subcommand)]
		format: PublishFormat,
	},
	/// Run a relay and write the first incoming broadcast's media to stdout.
	Accept {
		#[command(flatten)]
		config: moq_native::ServerConfig,

		/// The name of the broadcast to accept.
		#[arg(long, alias = "name")]
		broadcast: String,

		/// Optionally serve static files from the given directory.
		#[arg(long)]
		dir: Option<PathBuf>,

		#[command(flatten)]
		args: SubscribeArgs,
	},
	/// Publish a broadcast read from stdin to a remote relay.
	Publish {
		/// The MoQ client configuration.
		#[command(flatten)]
		config: moq_native::ClientConfig,

		/// The URL of the MoQ server.
		///
		/// The URL must start with `https://` or `http://`.
		/// - If `http` is used, a HTTP fetch to "/certificate.sha256" is first made to get the TLS certificiate fingerprint (insecure).
		/// - If `https` is used, then A WebTransport connection is made via QUIC to the provided host/port.
		///
		/// The `?jwt=` query parameter is used to provide a JWT token from moq-token-cli.
		/// Otherwise, the public path (if any) is used instead.
		///
		/// The path currently must be `/` or you'll get an error on connect.
		#[arg(long)]
		url: Url,

		/// The name of the broadcast to publish.
		#[arg(long, alias = "name")]
		broadcast: String,

		/// The format of the input media.
		#[command(subcommand)]
		format: PublishFormat,
	},
	/// Subscribe to a broadcast on a remote relay and write the media to stdout.
	Subscribe {
		/// The MoQ client configuration.
		#[command(flatten)]
		config: moq_native::ClientConfig,

		/// The URL of the MoQ server.
		#[arg(long)]
		url: Url,

		/// The name of the broadcast to subscribe to.
		#[arg(long, alias = "name")]
		broadcast: String,

		#[command(flatten)]
		args: SubscribeArgs,
	},
	/// Auto-record every broadcast announced under a prefix to HLS on disk.
	///
	/// Watches the relay and, the moment a broadcast goes live, starts writing
	/// `<dir>/<broadcast>/{init.mp4,seg_*.m4s,index.m3u8}`. Each recording is
	/// finalized to a VOD playlist when its broadcast ends. Stays running.
	Record {
		/// The MoQ client configuration.
		#[command(flatten)]
		config: moq_native::ClientConfig,

		/// The URL of the MoQ server.
		#[arg(long)]
		url: Url,

		/// Only record broadcasts whose path starts with this prefix (e.g.
		/// `live/`). Empty records every announced broadcast.
		#[arg(long, default_value = "")]
		prefix: String,

		/// Directory to write recordings to; each broadcast lands in `<dir>/<broadcast>/`.
		#[arg(long)]
		dir: PathBuf,

		/// Catalog format. Auto-detected per broadcast from the name suffix when unset.
		#[arg(long)]
		catalog: Option<CatalogFormatArg>,

		/// Target duration of each recorded segment (e.g. `6s`, `30s`).
		#[arg(long, default_value = "30s", value_parser = humantime::parse_duration)]
		chunk_duration: std::time::Duration,

		/// Maximum latency before skipping groups (e.g. `500ms`, `1s`).
		#[arg(long, default_value = "500ms", value_parser = humantime::parse_duration)]
		max_latency: std::time::Duration,

		/// Finalize a recording if no media arrives for this long (backstop for a
		/// publisher that crashes without a clean disconnect).
		#[arg(long, default_value = "30s", value_parser = humantime::parse_duration)]
		idle_timeout: std::time::Duration,
	},
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
	// TODO: It would be nice to remove this and rely on feature flags only.
	// However, some dependency is pulling in `ring` and I don't know why, so meh for now.
	rustls::crypto::aws_lc_rs::default_provider()
		.install_default()
		.expect("failed to install default crypto provider");

	let cli = Cli::parse();
	cli.log.init()?;

	#[cfg(feature = "iroh")]
	let iroh = cli.iroh.bind().await?;

	match cli.command {
		Command::Serve {
			config,
			dir,
			broadcast,
			format,
		} => {
			warn_if_missing_format(&broadcast);
			let publish = Publish::new(&format)?;
			let web_bind = config.bind.clone().unwrap_or_else(|| "[::]:443".to_string());

			let server = config.init()?;
			#[cfg(feature = "iroh")]
			let server = server.with_iroh(iroh);

			let web_tls = server.tls_info();

			tokio::select! {
				res = run_server(server, broadcast, publish.consume()) => res,
				res = run_web(&web_bind, web_tls, dir) => res,
				res = publish.run() => res,
			}
		}
		Command::Accept {
			config,
			broadcast,
			dir,
			args,
		} => {
			let web_bind = config.bind.clone().unwrap_or_else(|| "[::]:443".to_string());

			let server = config.init()?;
			#[cfg(feature = "iroh")]
			let server = server.with_iroh(iroh);

			let web_tls = server.tls_info();

			let origin = moq_net::Origin::random().produce();
			let consumer = origin.consume();

			tokio::select! {
				res = run_accept(server, origin) => res,
				res = run_web(&web_bind, web_tls, dir) => res,
				res = run_announced_subscribe(consumer, broadcast, args) => res,
				_ = tokio::signal::ctrl_c() => Ok(()),
			}
		}
		Command::Publish {
			config,
			url,
			broadcast,
			format,
		} => {
			warn_if_missing_format(&broadcast);
			let publish = Publish::new(&format)?;
			let client = config.init()?;

			#[cfg(feature = "iroh")]
			let client = client.with_iroh(iroh);

			run_client(client, url, broadcast, publish).await
		}
		Command::Subscribe {
			config,
			url,
			broadcast,
			args,
		} => {
			let client = config.init()?;

			#[cfg(feature = "iroh")]
			let client = client.with_iroh(iroh);

			run_subscribe(client, url, broadcast, args).await
		}
		Command::Record {
			config,
			url,
			prefix,
			dir,
			catalog,
			chunk_duration,
			max_latency,
			idle_timeout,
		} => {
			let client = config.init()?;

			#[cfg(feature = "iroh")]
			let client = client.with_iroh(iroh);

			run_record(
				client,
				url,
				prefix,
				dir,
				catalog,
				chunk_duration,
				max_latency,
				idle_timeout,
			)
			.await
		}
	}
}

fn warn_if_missing_format(name: &str) {
	if moq_mux::catalog::CatalogFormat::detect(name).is_none() {
		tracing::warn!(
			name,
			"You should append .hang to your broadcast name to make the catalog format explicit."
		);
	}
}

async fn run_subscribe(
	client: moq_native::Client,
	url: Url,
	broadcast: String,
	args: SubscribeArgs,
) -> anyhow::Result<()> {
	let origin = moq_net::Origin::random().produce();
	let consumer = origin.consume();

	tracing::info!(%url, %broadcast, "connecting");

	let reconnect = client.with_consume(origin).reconnect(url);

	#[cfg(unix)]
	let _ = sd_notify::notify(&[sd_notify::NotifyState::Ready]);

	tokio::select! {
		res = run_announced_subscribe(consumer, broadcast, args) => res,
		res = reconnect.closed() => res,
		_ = tokio::signal::ctrl_c() => Ok(()),
	}
}

async fn run_announced_subscribe(
	consumer: moq_net::OriginConsumer,
	broadcast: String,
	args: SubscribeArgs,
) -> anyhow::Result<()> {
	let catalog = args.catalog_format(&broadcast);

	let consumer = consumer
		.announced_broadcast(&broadcast)
		.await
		.ok_or_else(|| anyhow::anyhow!("origin closed before broadcast was announced"))?;

	Subscribe::new(consumer, catalog, args).run().await
}

#[allow(clippy::too_many_arguments)]
async fn run_record(
	client: moq_native::Client,
	url: Url,
	prefix: String,
	dir: PathBuf,
	catalog: Option<CatalogFormatArg>,
	chunk_duration: std::time::Duration,
	max_latency: std::time::Duration,
	idle_timeout: std::time::Duration,
) -> anyhow::Result<()> {
	let origin = moq_net::Origin::random().produce();

	let prefix_path: moq_net::Path = prefix.clone().into();
	let consumer = origin
		.scope(&[prefix_path])
		.ok_or_else(|| anyhow::anyhow!("not allowed to consume broadcasts under {prefix:?}"))?
		.consume();

	tracing::info!(%url, prefix, dir = %dir.display(), "auto-recording broadcasts");

	let reconnect = client.with_consume(origin).reconnect(url);

	#[cfg(unix)]
	let _ = sd_notify::notify(&[sd_notify::NotifyState::Ready]);

	tokio::select! {
		res = record_loop(consumer, dir, catalog, chunk_duration, max_latency, idle_timeout) => res,
		res = reconnect.closed() => res,
		_ = tokio::signal::ctrl_c() => Ok(()),
	}
}

/// Spawn an HLS recording per announced broadcast and reap them as they finish.
/// Each recording self-finalizes when its broadcast ends (see `record_hls`).
async fn record_loop(
	mut consumer: moq_net::OriginConsumer,
	base_dir: PathBuf,
	catalog: Option<CatalogFormatArg>,
	chunk_duration: std::time::Duration,
	max_latency: std::time::Duration,
	idle_timeout: std::time::Duration,
) -> anyhow::Result<()> {
	// Broadcasts currently being recorded, so a re-announce doesn't double-record.
	let mut active: std::collections::HashSet<String> = std::collections::HashSet::new();
	let mut tasks: tokio::task::JoinSet<String> = tokio::task::JoinSet::new();

	loop {
		tokio::select! {
			announce = consumer.announced() => {
				let Some((path, broadcast)) = announce else {
					break; // origin closed
				};
				// Unannounce: the recording self-finalizes when its export ends.
				let Some(broadcast) = broadcast else {
					continue;
				};

				let name = path.to_string();
				if !active.insert(name.clone()) {
					continue; // already recording
				}

				let format = catalog
					.map(Into::into)
					.or_else(|| moq_mux::catalog::CatalogFormat::detect(&name))
					.unwrap_or_default();
				// Guard against path traversal in the on-disk layout.
				let dir = base_dir.join(name.replace("..", "_"));

				let export = match moq_mux::container::hls::Export::with_catalog_format(broadcast, format) {
					Ok(export) => export.with_latency(max_latency).with_segment_duration(chunk_duration),
					Err(err) => {
						tracing::warn!(broadcast = %name, %err, "failed to start recording");
						active.remove(&name);
						continue;
					}
				};

				tracing::info!(broadcast = %name, dir = %dir.display(), "recording started");
				tasks.spawn(async move {
					match record_hls(export, dir, idle_timeout).await {
						Ok(()) => tracing::info!(broadcast = %name, "recording finished"),
						Err(err) => tracing::warn!(broadcast = %name, %err, "recording failed"),
					}
					name
				});
			}
			Some(joined) = tasks.join_next(), if !tasks.is_empty() => match joined {
				Ok(name) => {
					active.remove(&name);
				}
				Err(err) => tracing::warn!(%err, "recording task panicked"),
			},
		}
	}

	Ok(())
}
