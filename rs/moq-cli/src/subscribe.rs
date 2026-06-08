use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use clap::ValueEnum;
use hang::moq_net;
use moq_mux::catalog::CatalogFormat;
use moq_mux::container::hls;
use tokio::fs;
use tokio::io::AsyncWriteExt;

#[derive(ValueEnum, Clone, Copy)]
pub enum SubscribeFormat {
	Fmp4,
	Mkv,
	Ts,
}

#[derive(ValueEnum, Clone, Copy)]
pub enum CatalogFormatArg {
	Hang,
	Msf,
}

impl From<CatalogFormatArg> for CatalogFormat {
	fn from(format: CatalogFormatArg) -> Self {
		match format {
			CatalogFormatArg::Hang => Self::Hang,
			CatalogFormatArg::Msf => Self::Msf,
		}
	}
}

#[derive(clap::Args, Clone)]
pub struct SubscribeArgs {
	#[arg(long)]
	pub format: SubscribeFormat,

	#[arg(long, default_value = "500ms", value_parser = humantime::parse_duration)]
	pub max_latency: Duration,

	#[arg(long, value_parser = humantime::parse_duration)]
	pub fragment_duration: Option<Duration>,

	#[arg(long)]
	pub catalog: Option<CatalogFormatArg>,

	/// Record to an HLS / fMP4 directory instead of writing to stdout.
	///
	/// Writes `init.mp4`, `seg_NNNNN.m4s` segments, and an `index.m3u8` VOD
	/// playlist that plays directly in ffmpeg, VLC, Safari and hls.js. Video
	/// and audio are muxed together. Implies fMP4 output, so `--format` and
	/// `--fragment-duration` are ignored.
	/// Example: --record ./recordings/my-stream
	#[arg(long)]
	pub record: Option<PathBuf>,

	/// Target duration of each recorded segment (e.g. `6s`, `30s`).
	///
	/// Segments are cut at the first keyframe at or after this much media time,
	/// so each one starts on a keyframe and plays on its own. Only used with
	/// `--record`. Defaults to 30s.
	#[arg(long, default_value = "30s", value_parser = humantime::parse_duration)]
	pub chunk_duration: Duration,
}

impl SubscribeArgs {
	pub fn catalog_format(&self, broadcast: &str) -> CatalogFormat {
		self.catalog
			.map(Into::into)
			.or_else(|| CatalogFormat::detect(broadcast))
			.unwrap_or_default()
	}
}

pub struct Subscribe {
	broadcast: moq_net::BroadcastConsumer,
	catalog: CatalogFormat,
	args: SubscribeArgs,
}

impl Subscribe {
	pub fn new(broadcast: moq_net::BroadcastConsumer, catalog: CatalogFormat, args: SubscribeArgs) -> Self {
		Self {
			broadcast,
			catalog,
			args,
		}
	}

	pub async fn run(self) -> anyhow::Result<()> {
		if let Some(record_dir) = self.args.record.clone() {
			return self.run_hls_record(record_dir).await;
		}

		match self.args.format {
			SubscribeFormat::Fmp4 => self.run_fmp4().await,
			SubscribeFormat::Mkv => self.run_mkv().await,
			SubscribeFormat::Ts => self.run_ts().await,
		}
	}

	async fn run_fmp4(self) -> anyhow::Result<()> {
		let mut stdout = tokio::io::stdout();
		let mut fmp4 = moq_mux::container::fmp4::Export::with_catalog_format(self.broadcast, self.catalog)?
			.with_latency(self.args.max_latency)
			.with_fragment_duration(self.args.fragment_duration);
		while let Some(chunk) = fmp4.next().await? {
			stdout.write_all(&chunk).await?;
			stdout.flush().await?;
		}
		Ok(())
	}

	async fn run_mkv(self) -> anyhow::Result<()> {
		let mut stdout = tokio::io::stdout();
		let mut mkv = moq_mux::container::mkv::Export::with_catalog_format(self.broadcast, self.catalog)?
			.with_latency(self.args.max_latency)
			.with_fragment_duration(self.args.fragment_duration);
		while let Some(chunk) = mkv.next().await? {
			stdout.write_all(&chunk).await?;
			stdout.flush().await?;
		}
		Ok(())
	}

	async fn run_ts(self) -> anyhow::Result<()> {
		let mut stdout = tokio::io::stdout();
		let mut ts = moq_mux::container::ts::Export::with_catalog_format(self.broadcast, self.catalog)?
			.with_latency(self.args.max_latency);
		while let Some(chunk) = ts.next().await? {
			stdout.write_all(&chunk).await?;
			stdout.flush().await?;
		}
		Ok(())
	}

	async fn run_hls_record(self, record_dir: PathBuf) -> anyhow::Result<()> {
		let export = hls::Export::with_catalog_format(self.broadcast, self.catalog)?
			.with_latency(self.args.max_latency)
			.with_segment_duration(self.args.chunk_duration);
		// Idle backstop, well above a normal segment interval.
		record_hls(export, record_dir, Duration::from_secs(30)).await
	}
}

/// Drive an HLS export to disk: an `init.mp4`, numbered `seg_NNNNN.m4s`
/// segments, and an `index.m3u8` rewritten after each segment and finalized to a
/// VOD playlist (`#EXT-X-ENDLIST`) once recording stops. Shared by both
/// `subscribe --record` and the `record` auto-recorder.
///
/// Recording stops and finalizes when the broadcast ends cleanly (`None`), when
/// the publisher disconnects (error), or when no media arrives for
/// `idle_timeout` (a backstop for a relay that never signals the end). So the
/// playlist always gets its `#EXT-X-ENDLIST` rather than hanging open.
pub async fn record_hls(mut export: hls::Export, record_dir: PathBuf, idle_timeout: Duration) -> anyhow::Result<()> {
	fs::create_dir_all(&record_dir).await?;

	const INIT_NAME: &str = "init.mp4";
	let playlist_path = record_dir.join("index.m3u8");
	let mut playlist: Option<hls::Playlist> = None;

	loop {
		let segment = match tokio::time::timeout(idle_timeout, export.next()).await {
			Ok(Ok(Some(segment))) => segment,
			Ok(Ok(None)) => break, // clean end of broadcast
			Ok(Err(err)) => {
				// Publisher disconnected (or a stream error): finalize what we have.
				tracing::warn!(%err, dir = %record_dir.display(), "recording stopped (broadcast ended)");
				break;
			}
			Err(_) => {
				tracing::warn!(dir = %record_dir.display(), "no media for {idle_timeout:?}, finalizing recording");
				break;
			}
		};

		match segment {
			hls::Segment::Init(data) => {
				fs::write(record_dir.join(INIT_NAME), &data).await?;
				playlist = Some(hls::Playlist::new(INIT_NAME));
				eprintln!("[record] {} init ({} bytes)", record_dir.display(), data.len());
			}
			hls::Segment::Media {
				data,
				duration,
				sequence,
			} => {
				let name = format!("seg_{:05}.m4s", sequence);
				fs::write(record_dir.join(&name), &data).await?;

				let playlist = playlist.as_mut().context("media segment arrived before init segment")?;
				playlist.push(name.clone(), duration);
				fs::write(&playlist_path, playlist.render(false)).await?;

				eprintln!(
					"[record] {} {} ({:.2}s)",
					record_dir.display(),
					name,
					duration.as_secs_f64()
				);
			}
		}
	}

	if let Some(playlist) = playlist.as_ref() {
		fs::write(&playlist_path, playlist.render(true)).await?;
	}
	eprintln!("[record] done -> {}", record_dir.display());
	Ok(())
}
