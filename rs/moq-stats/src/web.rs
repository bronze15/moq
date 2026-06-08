//! The dashboard HTTP server: an HTML page plus a JSON stats endpoint.

use anyhow::Context;
use axum::http::Method;
use axum::response::Html;
use axum::routing::get;
use axum::{Json, Router, extract::State};
use tower_http::cors::{Any, CorsLayer};

use crate::aggregator::{Aggregator, StatsView};

/// The dashboard page, bundled into the binary (no build step, no assets).
const INDEX_HTML: &str = include_str!("index.html");

/// Serve the dashboard on `bind` (e.g. `0.0.0.0:8090`) until the process exits.
pub async fn serve(bind: String, aggregator: Aggregator) -> anyhow::Result<()> {
	let listen = tokio::net::lookup_host(&bind)
		.await
		.context("invalid listen address")?
		.next()
		.context("invalid listen address")?;

	let app = Router::new()
		.route("/", get(|| async { Html(INDEX_HTML) }))
		.route("/api/stats", get(api_stats))
		.layer(CorsLayer::new().allow_origin(Any).allow_methods([Method::GET]))
		.with_state(aggregator);

	tracing::info!(%bind, "dashboard listening");
	axum_server::bind(listen).serve(app.into_make_service()).await?;
	Ok(())
}

async fn api_stats(State(aggregator): State<Aggregator>) -> Json<StatsView> {
	Json(aggregator.view())
}
