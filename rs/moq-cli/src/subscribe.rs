use std::path::PathBuf;
use std::time::Duration;

use clap::ValueEnum;
use hang::moq_net;
use moq_mux::catalog::CatalogFormat;
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

	/// Directory to save recorded chunks to disk.
	/// Example: --record ./recordings/my-stream
	#[arg(long)]
	pub record: Option<PathBuf>,

	/// Duration of each recorded chunk. Defaults to 30s.
	/// Only used when --record is set.
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
			return match self.args.format {
				SubscribeFormat::Fmp4 => self.run_fmp4_record(record_dir).await,
				SubscribeFormat::Ts => self.run_ts_record(record_dir).await,
				_ => anyhow::bail!("--record only supports --format fmp4 or ts"),
			};
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

	async fn run_fmp4_record(self, record_dir: PathBuf) -> anyhow::Result<()> {
		fs::create_dir_all(&record_dir).await?;

		let chunk_duration_secs = self.args.chunk_duration.as_secs_f64();
		let fragment_secs = self
			.args
			.fragment_duration
			.unwrap_or(Duration::from_millis(500))
			.as_secs_f64();

		let mut chunk_index: u32 = 0;
		let mut elapsed_secs: f64 = 0.0;

		let chunk_path = record_dir.join(format!("chunk_{:03}.mp4", chunk_index));
		eprintln!("[record] Starting chunk {} -> {:?}", chunk_index, chunk_path);
		let mut current_file = fs::File::create(&chunk_path).await?;

		let mut fmp4 = moq_mux::container::fmp4::Export::with_catalog_format(self.broadcast, self.catalog)?
			.with_latency(self.args.max_latency)
			.with_fragment_duration(self.args.fragment_duration);

		while let Some(chunk) = fmp4.next().await? {
			current_file.write_all(&chunk).await?;
			elapsed_secs += fragment_secs;

			if elapsed_secs >= chunk_duration_secs {
				current_file.flush().await?;
				elapsed_secs = 0.0;
				chunk_index += 1;
				let new_path = record_dir.join(format!("chunk_{:03}.mp4", chunk_index));
				eprintln!("[record] Rotating to chunk {} -> {:?}", chunk_index, new_path);
				current_file = fs::File::create(&new_path).await?;
			}
		}

		current_file.flush().await?;
		eprintln!("[record] Done. {} chunks saved in {:?}", chunk_index + 1, record_dir);
		Ok(())
	}

	async fn run_ts_record(self, record_dir: PathBuf) -> anyhow::Result<()> {
		fs::create_dir_all(&record_dir).await?;

		let chunk_duration_secs = self.args.chunk_duration.as_secs_f64();
		let fragment_secs = 0.5_f64;

		let mut chunk_index: u32 = 0;
		let mut elapsed_secs: f64 = 0.0;

		let chunk_path = record_dir.join(format!("chunk_{:03}.ts", chunk_index));
		eprintln!("[record] Starting chunk {} -> {:?}", chunk_index, chunk_path);
		let mut current_file = fs::File::create(&chunk_path).await?;

		let mut ts = moq_mux::container::ts::Export::with_catalog_format(self.broadcast, self.catalog)?
			.with_latency(self.args.max_latency);

		while let Some(chunk) = ts.next().await? {
			current_file.write_all(&chunk).await?;
			elapsed_secs += fragment_secs;

			if elapsed_secs >= chunk_duration_secs {
				current_file.flush().await?;
				elapsed_secs = 0.0;
				chunk_index += 1;
				let new_path = record_dir.join(format!("chunk_{:03}.ts", chunk_index));
				eprintln!("[record] Rotating to chunk {} -> {:?}", chunk_index, new_path);
				current_file = fs::File::create(&new_path).await?;
			}
		}

		current_file.flush().await?;
		eprintln!("[record] Done. {} chunks saved in {:?}", chunk_index + 1, record_dir);
		Ok(())
	}
}
