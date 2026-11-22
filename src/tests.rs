use std::ffi::OsStr;
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use axum::http::{Request, StatusCode, header};
use tower::ServiceExt as _;

use super::{AllowedHosts, LocalDestination, MemoryLock, Published, router};

#[test]
fn unspecified_memory_lock_is_disabled() -> anyhow::Result<()> {
    assert_eq!(MemoryLock::parse(None)?, MemoryLock::Disabled);
    Ok(())
}

#[test]
fn zero_disables_memory_lock() -> anyhow::Result<()> {
    assert_eq!(
        MemoryLock::parse(Some(OsStr::new("0")))?,
        MemoryLock::Disabled
    );
    Ok(())
}

#[test]
fn one_enables_memory_lock() -> anyhow::Result<()> {
    assert_eq!(
        MemoryLock::parse(Some(OsStr::new("1")))?,
        MemoryLock::Enabled
    );
    Ok(())
}

#[test]
fn invalid_memory_lock_value_fails_closed() {
    assert!(MemoryLock::parse(Some(OsStr::new("true"))).is_err());
}

#[cfg(not(target_os = "linux"))]
#[test]
fn enabled_memory_lock_fails_on_non_linux() {
    assert!(MemoryLock::Enabled.apply().is_err());
}

async fn request(path: &str, host: &str) -> anyhow::Result<axum::response::Response> {
    request_with_sample(path, host, None).await
}

async fn request_with_sample(
    path: &str,
    host: &str,
    sample: Option<Published>,
) -> anyhow::Result<axum::response::Response> {
    let (_, state) = tokio::sync::watch::channel(sample);
    let app = router(state, AllowedHosts::default());
    Ok(app
        .oneshot(
            Request::get(path)
                .header(header::HOST, host)
                .extension(LocalDestination("127.0.0.1:8080".parse()?))
                .body(Body::empty())?,
        )
        .await?)
}

#[tokio::test]
async fn no_snapshot_returns_service_unavailable() -> anyhow::Result<()> {
    assert_eq!(
        request("/snapshot", "127.0.0.1:8080").await?.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    Ok(())
}

#[tokio::test]
async fn no_core_sample_is_not_healthy() -> anyhow::Result<()> {
    assert_eq!(
        request("/healthz", "127.0.0.1:8080").await?.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    Ok(())
}

#[tokio::test]
async fn unexpected_host_is_rejected() -> anyhow::Result<()> {
    assert_eq!(
        request("/", "other.invalid:8080").await?.status(),
        StatusCode::MISDIRECTED_REQUEST
    );
    Ok(())
}

#[tokio::test]
async fn requests_without_a_socket_destination_are_rejected() -> anyhow::Result<()> {
    let (_, state) = tokio::sync::watch::channel(None::<Published>);
    let response = router(state, AllowedHosts::default())
        .oneshot(
            Request::get("/")
                .header(header::HOST, "dashboard.example.test:8080")
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::MISDIRECTED_REQUEST);
    Ok(())
}

#[tokio::test]
async fn css_is_served_with_its_mime_type() -> anyhow::Result<()> {
    assert_eq!(
        request("/style.css", "localhost:8080")
            .await?
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|header| header.to_str().ok()),
        Some("text/css; charset=utf-8")
    );
    Ok(())
}

#[tokio::test]
async fn root_disables_unsafe_markup_injection() -> anyhow::Result<()> {
    let response = request("/", "127.0.0.1:8080").await?;
    assert!(
        response
            .headers()
            .get(header::CONTENT_SECURITY_POLICY)
            .and_then(|header| header.to_str().ok())
            .is_some_and(|policy| policy.contains("default-src 'none'"))
    );
    Ok(())
}

#[tokio::test]
async fn only_dashboard_routes_are_served() -> anyhow::Result<()> {
    assert_eq!(
        request("/unknown", "127.0.0.1:8080").await?.status(),
        StatusCode::NOT_FOUND
    );
    Ok(())
}

#[tokio::test]
async fn health_is_ready_after_a_fresh_core_sample() -> anyhow::Result<()> {
    let sample = Published {
        sampled_at: Instant::now(),
        healthy: true,
        html: Bytes::new(),
    };
    assert_eq!(
        request_with_sample("/healthz", "127.0.0.1:8080", Some(sample))
            .await?
            .status(),
        StatusCode::OK
    );
    Ok(())
}

#[tokio::test]
async fn a_stale_snapshot_returns_service_unavailable() -> anyhow::Result<()> {
    let sample = Published {
        sampled_at: Instant::now()
            .checked_sub(Duration::from_secs(13))
            .ok_or_else(|| anyhow::anyhow!("clock cannot represent the stale instant"))?,
        healthy: true,
        html: Bytes::new(),
    };
    assert_eq!(
        request_with_sample("/snapshot", "127.0.0.1:8080", Some(sample))
            .await?
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    Ok(())
}

#[tokio::test]
async fn htmx_is_served_as_an_embedded_script() -> anyhow::Result<()> {
    let response = request("/htmx.min.js", "127.0.0.1:8080").await?;
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|header| header.to_str().ok()),
        Some("text/javascript; charset=utf-8")
    );
    Ok(())
}

#[tokio::test]
async fn recent_incomplete_snapshot_is_displayed_but_marked() -> anyhow::Result<()> {
    let sample = Published {
        sampled_at: Instant::now(),
        healthy: false,
        html: Bytes::from_static(b"<p>synthetic</p>"),
    };
    let response = request_with_sample("/snapshot", "127.0.0.1:8080", Some(sample)).await?;
    assert_eq!(
        response
            .headers()
            .get(super::CORE_HEADER)
            .and_then(|header| header.to_str().ok()),
        Some("unavailable")
    );
    Ok(())
}

#[test]
fn host_matching_accepts_canonical_ipv6_address() -> anyhow::Result<()> {
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(header::HOST, "[0:0:0:0:0:0:0:1]:8080".parse()?);
    assert!(super::valid_host(
        &headers,
        "[::1]:8080".parse()?,
        &AllowedHosts::default()
    ));
    Ok(())
}

fn host_allowed(host: &str, destination: &str) -> anyhow::Result<bool> {
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(header::HOST, host.parse()?);
    let hosts = AllowedHosts::parse("dashboard.example.test, dashboard")?;
    Ok(super::valid_host(&headers, destination.parse()?, &hosts))
}

#[test]
fn host_matches_ipv4_connection_destination_on_each_network() -> anyhow::Result<()> {
    for host in ["192.0.2.24:8080", "198.51.100.13:8080", "203.0.113.37:8080"] {
        assert!(host_allowed(host, host)?);
    }
    Ok(())
}

#[test]
fn mapped_ipv6_connection_accepts_ipv4_host() -> anyhow::Result<()> {
    assert!(host_allowed("192.0.2.24:8080", "[::ffff:192.0.2.24]:8080")?);
    Ok(())
}

#[test]
fn destination_accepts_configured_dns_name() -> anyhow::Result<()> {
    assert!(host_allowed(
        "DASHBOARD.EXAMPLE.TEST:8080",
        "[::ffff:192.0.2.24]:8080"
    )?);
    Ok(())
}

#[test]
fn destination_accepts_configured_short_name() -> anyhow::Result<()> {
    assert!(host_allowed("dashboard:8080", "198.51.100.13:8080")?);
    Ok(())
}

#[test]
fn host_must_match_this_connection_destination() -> anyhow::Result<()> {
    assert!(!host_allowed("192.0.2.24:8080", "198.51.100.13:8080")?);
    Ok(())
}

#[test]
fn localhost_is_restricted_to_loopback_destinations() -> anyhow::Result<()> {
    assert!(!host_allowed("localhost:8080", "192.0.2.24:8080")?);
    Ok(())
}

#[test]
fn host_must_match_connection_port() -> anyhow::Result<()> {
    assert!(!host_allowed(
        "dashboard.example.test:80",
        "192.0.2.24:8080"
    )?);
    Ok(())
}

#[test]
fn duplicate_host_is_rejected() -> anyhow::Result<()> {
    let mut headers = axum::http::HeaderMap::new();
    headers.append(header::HOST, "192.0.2.24:8080".parse()?);
    headers.append(header::HOST, "192.0.2.24:8080".parse()?);
    assert!(!super::valid_host(
        &headers,
        "192.0.2.24:8080".parse()?,
        &AllowedHosts::default()
    ));
    Ok(())
}

const SAMPLE_HTML: &[u8] = b"<p>Zest snapshot</p>";

async fn snapshot_request(
    method: axum::http::Method,
    encodings: &[&str],
) -> anyhow::Result<axum::response::Response> {
    let (_, state) = tokio::sync::watch::channel(Some(Published {
        sampled_at: Instant::now(),
        healthy: true,
        html: Bytes::from_static(SAMPLE_HTML),
    }));
    let mut request = Request::builder()
        .method(method)
        .uri("/snapshot")
        .header(header::HOST, "127.0.0.1:8080")
        .extension(LocalDestination("127.0.0.1:8080".parse()?));
    for encoding in encodings {
        request = request.header(header::ACCEPT_ENCODING, *encoding);
    }
    Ok(router(state, AllowedHosts::default())
        .oneshot(request.body(Body::empty())?)
        .await?)
}

#[tokio::test]
async fn gzip_snapshot_has_matching_length_and_round_trips() -> anyhow::Result<()> {
    use std::io::Read as _;

    let response = snapshot_request(axum::http::Method::GET, &["br", "gzip"]).await?;
    let (parts, body) = response.into_parts();
    let encoded = axum::body::to_bytes(body, 1024).await?;
    let mut decoded = Vec::new();
    flate2::read::GzDecoder::new(encoded.as_ref()).read_to_end(&mut decoded)?;
    let length = parts
        .headers
        .get(header::CONTENT_LENGTH)
        .and_then(|length| length.to_str().ok())
        .and_then(|length| length.parse::<usize>().ok());
    assert_eq!(
        (
            parts.status,
            parts
                .headers
                .get(header::CONTENT_ENCODING)
                .and_then(|value| value.to_str().ok()),
            parts
                .headers
                .get(header::VARY)
                .and_then(|value| value.to_str().ok()),
            length,
            decoded.as_slice()
        ),
        (
            StatusCode::OK,
            Some("gzip"),
            Some("Accept-Encoding"),
            Some(encoded.len()),
            SAMPLE_HTML
        ),
    );
    Ok(())
}

#[tokio::test]
async fn no_encoding_header_keeps_identity() -> anyhow::Result<()> {
    let response = snapshot_request(axum::http::Method::GET, &[]).await?;
    let (parts, body) = response.into_parts();
    assert_eq!(
        (
            parts.status,
            parts.headers.contains_key(header::CONTENT_ENCODING),
            parts
                .headers
                .get(header::VARY)
                .and_then(|value| value.to_str().ok()),
            axum::body::to_bytes(body, 1024).await?
        ),
        (
            StatusCode::OK,
            false,
            Some("Accept-Encoding"),
            Bytes::from_static(SAMPLE_HTML)
        ),
    );
    Ok(())
}

#[tokio::test]
async fn head_advertises_gzip_without_a_computed_length() -> anyhow::Result<()> {
    let response = snapshot_request(axum::http::Method::HEAD, &["gzip, identity;q=0"]).await?;
    let (parts, body) = response.into_parts();
    assert_eq!(
        (
            parts.status,
            parts
                .headers
                .get(header::CONTENT_ENCODING)
                .and_then(|value| value.to_str().ok()),
            parts.headers.contains_key(header::CONTENT_LENGTH),
            axum::body::to_bytes(body, 1024).await?
        ),
        (StatusCode::OK, Some("gzip"), false, Bytes::new()),
    );
    Ok(())
}

#[tokio::test]
async fn when_neither_encoding_is_accepted_the_route_returns_406() -> anyhow::Result<()> {
    let response = snapshot_request(axum::http::Method::GET, &["br, *;q=0"]).await?;
    assert_eq!(
        (
            response.status(),
            response
                .headers()
                .get(header::VARY)
                .and_then(|value| value.to_str().ok())
        ),
        (StatusCode::NOT_ACCEPTABLE, Some("Accept-Encoding")),
    );
    Ok(())
}

#[tokio::test]
async fn configured_alias_works_in_router_without_allowing_arbitrary_dns() -> anyhow::Result<()> {
    let (_, state) = tokio::sync::watch::channel(None::<Published>);
    let host_from_env = AllowedHosts::parse("dashboard.example.test")?;
    let app = router(state, host_from_env);
    let mut responses = Vec::new();
    for host in ["dashboard.example.test:8080", "foreign.example.test:8080"] {
        let request = Request::get("/")
            .header(header::HOST, host)
            .extension(LocalDestination("192.0.2.24:8080".parse()?))
            .body(Body::empty())?;
        responses.push(app.clone().oneshot(request).await?.status());
    }
    assert_eq!(responses, [StatusCode::OK, StatusCode::MISDIRECTED_REQUEST]);
    Ok(())
}
