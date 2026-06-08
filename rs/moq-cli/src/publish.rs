use clap::Subcommand;
use hang::moq_net;
use moq_mux::container::{fmp4, hls, ts};

#[derive(Subcommand, Clone)]
pub enum PublishFormat {
	Avc3,
	Fmp4,
	/// MPEG-TS (transport stream) read from stdin.
	Ts,
	// NOTE: No aac support because it needs framing.
	Hls {
		/// URL or file path of an HLS playlist to ingest.
		#[arg(long)]
		playlist: String,
	},
}

enum PublishDecoder {
	Avc3(Box<moq_mux::codec::h264::Import>),
	Fmp4(Box<fmp4::Import>),
	Ts(Box<ts::Import>),
	Hls(Box<hls::Import>),
}

impl PublishDecoder {
	/// Decode a chunk of bytes from stdin (Avc3, Fmp4, or Ts).
	fn decode_buf(&mut self, buffer: &mut bytes::BytesMut) -> anyhow::Result<()> {
		match self {
			Self::Avc3(d) => d.decode_stream(buffer, None),
			Self::Fmp4(d) => d.decode(buffer),
			Self::Ts(d) => d.decode(buffer),
			Self::Hls(_) => unreachable!(),
		}
	}

	/// Finish all media tracks when the input ends, so subscribers see a clean
	/// end-of-track instead of a dropped connection.
	fn finish(&mut self) -> anyhow::Result<()> {
		match self {
			Self::Avc3(d) => d.finish(),
			Self::Fmp4(d) => d.finish(),
			Self::Ts(d) => d.finish(),
			Self::Hls(_) => Ok(()), // the HLS importer finishes its own tracks when it ends
		}
	}
}

pub struct Publish {
	decoder: PublishDecoder,
	broadcast: moq_net::BroadcastProducer,
	catalog: moq_mux::catalog::hang::Producer,
}

impl Publish {
	pub fn new(format: &PublishFormat) -> anyhow::Result<Self> {
		let mut broadcast = moq_net::Broadcast::new().produce();
		let catalog = moq_mux::catalog::hang::Producer::new(&mut broadcast)?;

		let decoder = match format {
			PublishFormat::Avc3 => {
				let avc3 = moq_mux::codec::h264::Import::new(broadcast.clone(), catalog.clone())
					.with_mode(moq_mux::codec::h264::Mode::Avc3)?;
				PublishDecoder::Avc3(Box::new(avc3))
			}
			PublishFormat::Fmp4 => {
				let fmp4 = fmp4::Import::new(broadcast.clone(), catalog.clone());
				PublishDecoder::Fmp4(Box::new(fmp4))
			}
			PublishFormat::Ts => {
				let ts = ts::Import::new(broadcast.clone(), catalog.clone());
				PublishDecoder::Ts(Box::new(ts))
			}
			PublishFormat::Hls { playlist } => {
				let hls = hls::Import::new(broadcast.clone(), catalog.clone(), hls::Config::new(playlist.clone()))?;
				PublishDecoder::Hls(Box::new(hls))
			}
		};

		Ok(Self {
			decoder,
			broadcast,
			catalog,
		})
	}

	pub fn consume(&self) -> moq_net::BroadcastConsumer {
		self.broadcast.consume()
	}

	pub async fn run(mut self) -> anyhow::Result<()> {
		if let PublishDecoder::Hls(decoder) = &mut self.decoder {
			decoder.init().await?;
			decoder.run().await?;
		} else {
			let mut stdin = tokio::io::stdin();
			let mut buffer = bytes::BytesMut::new();

			loop {
				let n = tokio::io::AsyncReadExt::read_buf(&mut stdin, &mut buffer).await?;
				if n == 0 {
					break;
				}
				self.decoder.decode_buf(&mut buffer)?;
			}
		}

		// Input ended: finish the tracks and the catalog cleanly so subscribers
		// (e.g. recorders) observe an end-of-broadcast and finalize, instead of
		// seeing the connection drop or hanging.
		if let Err(err) = self.decoder.finish() {
			tracing::warn!(%err, "error finishing tracks at end of input");
		}
		if let Err(err) = self.catalog.finish() {
			tracing::warn!(%err, "error finishing catalog at end of input");
		}

		// Hold the session open briefly so the finish actually flushes to the
		// relay over QUIC before we drop the connection; otherwise subscribers
		// see a reset instead of a clean end-of-broadcast.
		tokio::time::sleep(std::time::Duration::from_secs(3)).await;

		Ok(())
	}
}
