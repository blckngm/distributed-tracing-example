use std::time::Duration;

use anyhow::{Context as _, Result};
use axum::{
    http::{HeaderMap, HeaderValue},
    routing::get,
    Router,
};
use opentelemetry::{
    trace::{SpanContext, SpanId, TraceContextExt, TraceFlags, TraceId, TraceState},
    Context,
};
use tracing::{instrument, Span};
use tracing_opentelemetry::OpenTelemetrySpanExt;
use tracing_subscriber::{prelude::*, util::SubscriberInitExt};

fn inject_context(cx: &Context, headers: &mut HeaderMap) {
    let span = cx.span();
    let span_context = span.span_context();
    if span_context.is_valid() {
        headers.insert(
            "my-trace-id",
            HeaderValue::from_str(&format!(
                "{}-{}",
                span_context.trace_id(),
                span_context.span_id(),
            ))
            .unwrap(),
        );
    };
}

fn extract_context(headers: &HeaderMap) -> Option<SpanContext> {
    let v = headers.get("my-trace-id")?.to_str().ok()?;
    let mut parts = v.split('-');
    let trace_id = parts.next()?;
    let span_id = parts.next()?;
    let span_cx = SpanContext::new(
        TraceId::from_hex(trace_id).ok()?,
        SpanId::from_hex(span_id).ok()?,
        TraceFlags::SAMPLED,
        true,
        TraceState::default(),
    );
    Some(span_cx)
}

#[instrument]
async fn handler(headers: HeaderMap) -> &'static str {
    let cx = if let Some(span_cx) = extract_context(&headers) {
        Context::current().with_remote_span_context(span_cx)
    } else {
        Context::current()
    };

    Span::current().set_parent(cx);

    tokio::time::sleep(Duration::from_secs(1)).await;

    "ok"
}

async fn run_server() -> Result<()> {
    let app = Router::new().route("/", get(handler));
    axum::Server::bind(&"0.0.0.0:3000".parse().unwrap())
        .serve(app.into_make_service())
        .await?;
    Ok(())
}

#[instrument]
async fn req() -> Result<()> {
    let mut headers = HeaderMap::new();

    let cx = Span::current().context();
    inject_context(&cx, &mut headers);

    let c = reqwest::Client::new();
    c.get("http://127.0.0.1:3000")
        .headers(headers)
        .send()
        .await?;
    Ok(())
}

#[instrument]
async fn run_client() -> Result<()> {
    for _ in 0..10 {
        req().await?;
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let is_server = std::env::args().nth(1).context("command")? == "server";
    let _guard = init_tracing()?;

    if is_server {
        run_server().await?;
    } else {
        run_client().await?;
    }
    Ok(())
}

pub struct ShutdownGuard;

impl Drop for ShutdownGuard {
    fn drop(&mut self) {
        opentelemetry::global::shutdown_tracer_provider(); // Sending remaining spans
    }
}

pub fn init_tracing() -> anyhow::Result<ShutdownGuard> {
    let env_filter_layer = tracing_subscriber::EnvFilter::try_from_default_env()
        .or_else(|_| tracing_subscriber::EnvFilter::try_new("info"))?;

    let registry = tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_ansi(false))
        .with(env_filter_layer);

    let jaeger_layer = {
        let service_name = std::env::var("OTEL_SERVICE_NAME").unwrap_or_else(|_| "godwoken".into());
        let tracer = opentelemetry_jaeger::new_pipeline()
            .with_service_name(service_name)
            .with_auto_split_batch(true)
            .install_batch(opentelemetry::runtime::Tokio)?;
        tracing_opentelemetry::layer().with_tracer(tracer)
    };

    registry.with(jaeger_layer).try_init()?;

    Ok(ShutdownGuard)
}
