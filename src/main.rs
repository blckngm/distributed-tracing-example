use std::{net::Ipv4Addr, time::Duration};

use anyhow::{Context as _, Result};
use opentelemetry::{
    sdk::{trace, Resource},
    trace::{SpanContext, SpanId, TraceContextExt, TraceFlags, TraceId, TraceState},
    Context, KeyValue,
};
use opentelemetry_otlp::WithExportConfig;
use tokio::net::UdpSocket;
use tonic::metadata::{AsciiMetadataKey, MetadataMap};
use tracing::{instrument, Span};
use tracing_opentelemetry::OpenTelemetrySpanExt;
use tracing_subscriber::{prelude::*, util::SubscriberInitExt};

fn serialize_context(cx: &Context) -> [u8; 24] {
    let span = cx.span();
    let span_context = span.span_context();
    let mut result = [0u8; 24];
    result[..16].copy_from_slice(&span_context.trace_id().to_bytes());
    result[16..].copy_from_slice(&span_context.span_id().to_bytes());
    result
}

fn deserialize_context(bytes: &[u8]) -> Option<SpanContext> {
    if bytes.len() != 24 {
        return None;
    }
    let trace_id = bytes[..16].try_into().unwrap();
    let span_id = bytes[16..].try_into().unwrap();
    let span_cx = SpanContext::new(
        TraceId::from_bytes(trace_id),
        SpanId::from_bytes(span_id),
        TraceFlags::SAMPLED,
        true,
        TraceState::default(),
    );
    Some(span_cx)
}

#[instrument]
async fn consume(msg: &[u8]) {
    let cx = if let Some(span_cx) = deserialize_context(msg) {
        Context::current().with_remote_span_context(span_cx)
    } else {
        Context::current()
    };

    Span::current().set_parent(cx);

    tokio::time::sleep(Duration::from_secs(1)).await;
}

async fn run_consumer() -> Result<()> {
    let sock = UdpSocket::bind("127.0.0.1:3000").await?;
    let mut buf = [0u8; 24];
    loop {
        let (len, _) = sock.recv_from(&mut buf).await?;
        consume(&buf[..len]).await;
    }
}

#[instrument]
async fn produce(sock: &UdpSocket) -> Result<()> {
    let cx = Span::current().context();
    let msg = serialize_context(&cx);
    sock.send_to(&msg, (Ipv4Addr::LOCALHOST, 3000)).await?;
    Ok(())
}

#[instrument]
async fn produce_many() -> Result<()> {
    let sock = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    for _ in 0..10 {
        produce(&sock).await?;
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let is_consumer = std::env::args().nth(1).context("command")? == "consumer";
    let _guard = init_tracing()?;

    if is_consumer {
        run_consumer().await?;
    } else {
        produce_many().await?;
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
