//! Verify the `TraceLayer` is wired
//! and the request span carries `method`, `uri`, and `client_ip`. The
//! client-IP value is sourced from the `X-Forwarded-For` header when
//! present, falling back to `ConnectInfo<SocketAddr>` from the extensions,
//! and finally to `"-"` when neither is set.
//!
//! Verifying span fields directly requires a custom tracing subscriber
//! sink; this test installs one, drives the router, and inspects the
//! captured field set.

use axum::extract::ConnectInfo;
use axum::{body::Body, http::Request, Router};
use ecaa_workflow_server::build_trace_layer;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tower::ServiceExt;
use tracing::field::{Field, Visit};
use tracing::span::Attributes;
use tracing::Id;
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::Layer;

#[derive(Default, Debug, Clone)]
struct CapturedSpan {
    name: String,
    fields: std::collections::HashMap<String, String>,
}

#[derive(Default)]
struct CaptureLayer {
    spans: Arc<Mutex<Vec<CapturedSpan>>>,
}

struct FieldVisitor<'a> {
    fields: &'a mut std::collections::HashMap<String, String>,
}

impl<'a> Visit for FieldVisitor<'a> {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.fields
            .insert(field.name().to_string(), value.to_string());
    }
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.fields
            .insert(field.name().to_string(), format!("{:?}", value));
    }
}

impl<S: tracing::Subscriber> Layer<S> for CaptureLayer {
    fn on_new_span(&self, attrs: &Attributes<'_>, _id: &Id, _ctx: Context<'_, S>) {
        let mut captured = CapturedSpan {
            name: attrs.metadata().name().to_string(),
            ..Default::default()
        };
        let mut visitor = FieldVisitor {
            fields: &mut captured.fields,
        };
        attrs.record(&mut visitor);
        self.spans.lock().unwrap().push(captured);
    }
}

fn build_test_router() -> Router {
    Router::new()
        .route("/x", axum::routing::get(|| async { "ok" }))
        .layer(build_trace_layer())
}

#[tokio::test]
async fn trace_span_captures_method_uri_and_xff_ip() {
    let spans = Arc::new(Mutex::new(Vec::new()));
    let capture = CaptureLayer {
        spans: spans.clone(),
    };
    let subscriber = tracing_subscriber::registry().with(capture);
    let _guard = subscriber.set_default();

    let app = build_test_router();
    let req = Request::builder()
        .method("GET")
        .uri("/x")
        .header("x-forwarded-for", "203.0.113.42")
        .body(Body::empty())
        .unwrap();
    let _ = app.oneshot(req).await.unwrap();

    let captured = spans.lock().unwrap().clone();
    let http_span = captured
        .iter()
        .find(|s| s.name == "http")
        .expect("TraceLayer must produce a span named `http` for every request");
    assert_eq!(
        http_span.fields.get("method").map(String::as_str),
        Some("GET"),
        "span must capture HTTP method"
    );
    assert_eq!(
        http_span.fields.get("uri").map(String::as_str),
        Some("/x"),
        "span must capture request URI path"
    );
    assert_eq!(
        http_span.fields.get("client_ip").map(String::as_str),
        Some("203.0.113.42"),
        "span must surface client IP from X-Forwarded-For header"
    );
}

#[tokio::test]
async fn trace_span_falls_back_to_connect_info_when_no_xff() {
    let spans = Arc::new(Mutex::new(Vec::new()));
    let capture = CaptureLayer {
        spans: spans.clone(),
    };
    let subscriber = tracing_subscriber::registry().with(capture);
    let _guard = subscriber.set_default();

    let app = build_test_router();
    let peer: SocketAddr = "198.51.100.5:54321".parse().unwrap();
    let mut req = Request::builder()
        .method("POST")
        .uri("/x")
        .body(Body::empty())
        .unwrap();
    // The `into_make_service_with_connect_info::<SocketAddr>()` wiring in
    // `lib.rs::run` injects this extension on every request. Tower's
    // `oneshot` test path doesn't go through the make-service, so we
    // inject the extension directly to simulate the production setup.
    req.extensions_mut().insert(ConnectInfo(peer));
    let _ = app.oneshot(req).await.unwrap();

    let captured = spans.lock().unwrap().clone();
    let http_span = captured
        .iter()
        .find(|s| s.name == "http")
        .expect("TraceLayer must produce a span named `http` for every request");
    let ip = http_span
        .fields
        .get("client_ip")
        .cloned()
        .unwrap_or_default();
    assert!(
        ip.contains("198.51.100.5"),
        "span must surface client IP from ConnectInfo when XFF is absent; got {ip}"
    );
}

#[tokio::test]
async fn trace_span_emits_dash_when_no_ip_source() {
    let spans = Arc::new(Mutex::new(Vec::new()));
    let capture = CaptureLayer {
        spans: spans.clone(),
    };
    let subscriber = tracing_subscriber::registry().with(capture);
    let _guard = subscriber.set_default();

    let app = build_test_router();
    let req = Request::builder()
        .method("GET")
        .uri("/x")
        .body(Body::empty())
        .unwrap();
    let _ = app.oneshot(req).await.unwrap();

    let captured = spans.lock().unwrap().clone();
    let http_span = captured
        .iter()
        .find(|s| s.name == "http")
        .expect("TraceLayer must produce a span named `http` for every request");
    assert_eq!(
        http_span.fields.get("client_ip").map(String::as_str),
        Some("-"),
        "span must fall through to `-` when neither XFF nor ConnectInfo is set"
    );
}
