use std::time::Duration;

use anyhow::{Context as _, Result};
use axum::{
    http::{HeaderMap, StatusCode},
    routing::get,
    Router,
};
use opentelemetry::{
    global,
    sdk::{propagation::TraceContextPropagator, trace, Resource},
    KeyValue,
};
use opentelemetry_http::HeaderExtractor;
use opentelemetry_otlp::WithExportConfig;
use tonic::metadata::{AsciiMetadataKey, MetadataMap};
use tracing::instrument;
use tracing_opentelemetry::OpenTelemetrySpanExt;
use tracing_subscriber::{prelude::*, util::SubscriberInitExt};

#[instrument(skip(headers))]
async fn handle_get_index(headers: HeaderMap) -> StatusCode {
    let cx = global::get_text_map_propagator(|p| p.extract(&HeaderExtractor(&headers)));
    tracing::Span::current().set_parent(cx);

    tracing::info!("handling get request");
    tokio::time::sleep(Duration::from_millis(10)).await;
    tracing::error!("error message");

    StatusCode::NO_CONTENT
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenv::dotenv().context("dotenv")?;
    let _guard = init_tracing()?;

    let app = Router::new().route("/", get(handle_get_index));
    axum::Server::bind(&"127.0.0.1:3001".parse().unwrap())
        .serve(app.into_make_service())
        .await?;
    Ok(())
}

pub struct ShutdownGuard;

impl Drop for ShutdownGuard {
    fn drop(&mut self) {
        opentelemetry::global::shutdown_tracer_provider(); // Sending remaining spans
    }
}

pub fn init_tracing() -> anyhow::Result<ShutdownGuard> {
    global::set_text_map_propagator(TraceContextPropagator::new());

    let env_filter_layer = tracing_subscriber::EnvFilter::try_from_default_env()
        .or_else(|_| tracing_subscriber::EnvFilter::try_new("info"))?;

    let registry = tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_ansi(false))
        .with(env_filter_layer);

    let otlp_layer = {
        let service_name = std::env::var("OTEL_SERVICE_NAME").unwrap_or_else(|_| "godwoken".into());
        let mut meta = MetadataMap::with_capacity(1);
        let kvs = std::env::var("OTEL_EXPORTER_OTLP_HEADERS").unwrap_or_else(|_| "".into());
        for kv in kvs.split(',') {
            let mut kv = kv.splitn(2, '=');
            if let (Some(k), Some(v)) = (kv.next(), kv.next()) {
                meta.insert(
                    match k.parse::<AsciiMetadataKey>() {
                        Ok(k) => k,
                        _ => {
                            eprintln!("OTEL_EXPORTER_OTLP_HEADERS: invalid header key: {k}");
                            continue;
                        }
                    },
                    match v.parse() {
                        Ok(v) => v,
                        Err(_) => {
                            eprintln!("OTEL_EXPORTER_OTLP_HEADERS: invalid header value: {v}");
                            continue;
                        }
                    },
                );
            }
        }
        let tracer =
            opentelemetry_otlp::new_pipeline()
                .tracing()
                .with_exporter(
                    opentelemetry_otlp::new_exporter()
                        .tonic()
                        .with_env()
                        .with_metadata(meta),
                )
                .with_trace_config(trace::config().with_resource(Resource::new(vec![
                    KeyValue::new("service.name", service_name),
                ])))
                .install_batch(opentelemetry::runtime::Tokio)?;
        tracing_opentelemetry::layer().with_tracer(tracer)
    };

    registry.with(otlp_layer).try_init()?;

    Ok(ShutdownGuard)
}
