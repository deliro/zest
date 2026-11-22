use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::{Bytes, to_bytes};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};

use super::{CompressionJob, Encoding, Gzip, negotiate};
use crate::{AppState, Published, latest_snapshot};

#[test]
fn negotiation_honors_quality_wildcards_and_exclusions() -> anyhow::Result<()> {
    let cases = [
        ("", Encoding::Identity),
        ("br, deflate", Encoding::Identity),
        (
            "GZip; Q=0.5",
            Encoding::Gzip {
                identity_allowed: true,
            },
        ),
        ("gzip;q=0, *;q=1", Encoding::Identity),
        ("*;q=0, identity", Encoding::Identity),
        (
            "*;q=0.2",
            Encoding::Gzip {
                identity_allowed: true,
            },
        ),
        ("gzip;q=0.5, identity;q=0.8", Encoding::Identity),
        (
            "gzip;q=0.8, identity;q=0",
            Encoding::Gzip {
                identity_allowed: false,
            },
        ),
        (
            "gzip, *;q=0",
            Encoding::Gzip {
                identity_allowed: false,
            },
        ),
        ("*;q=0", Encoding::Unacceptable),
        ("gzip;q=1.001, identity;q=0", Encoding::Unacceptable),
        ("gzip;q=bad, *;q=1", Encoding::Identity),
        ("gzip;q=0.0001", Encoding::Identity),
    ];
    let mut headers = HeaderMap::new();
    for (field, expected) in cases {
        headers.insert(header::ACCEPT_ENCODING, field.parse()?);
        assert_eq!(negotiate(&headers), expected, "{field}");
    }
    Ok(())
}

#[test]
fn separate_field_lines_cannot_override_an_explicit_exclusion() {
    let mut headers = HeaderMap::new();
    headers.append(
        header::ACCEPT_ENCODING,
        HeaderValue::from_static("gzip;q=0"),
    );
    headers.append(
        header::ACCEPT_ENCODING,
        HeaderValue::from_static("GZIP, *;q=1"),
    );
    assert_eq!(negotiate(&headers), Encoding::Identity);
}

#[tokio::test]
async fn busy_compressor_returns_identity_when_allowed() -> anyhow::Result<()> {
    let (state, mut headers) = busy_fixture();
    let _permit = Arc::clone(&state.gzip.slot).try_acquire_owned()?;
    headers.insert(header::ACCEPT_ENCODING, "gzip".parse()?);
    let response = latest_snapshot(&state, &Method::GET, &headers).await;
    let (parts, body) = response.into_parts();
    assert_eq!(
        (
            parts.status,
            parts.headers.contains_key(header::CONTENT_ENCODING),
            to_bytes(body, 1024).await?
        ),
        (StatusCode::OK, false, Bytes::from_static(b"snapshot")),
    );
    Ok(())
}

#[tokio::test]
async fn busy_compressor_refuses_gzip_only_get_but_allows_head() -> anyhow::Result<()> {
    let (state, mut headers) = busy_fixture();
    let _permit = Arc::clone(&state.gzip.slot).try_acquire_owned()?;
    headers.insert(header::ACCEPT_ENCODING, "gzip, identity;q=0".parse()?);
    let get = latest_snapshot(&state, &Method::GET, &headers).await;
    let head = latest_snapshot(&state, &Method::HEAD, &headers).await;
    assert_eq!(
        (
            get.status(),
            head.status(),
            head.headers().contains_key(header::CONTENT_ENCODING)
        ),
        (StatusCode::SERVICE_UNAVAILABLE, StatusCode::OK, true),
    );
    Ok(())
}

fn busy_fixture() -> (AppState, HeaderMap) {
    let (_, snapshot) = tokio::sync::watch::channel(Some(Published {
        sampled_at: Instant::now(),
        healthy: true,
        html: Bytes::from_static(b"snapshot"),
    }));
    (
        AppState {
            snapshot,
            gzip: Gzip::new(),
        },
        HeaderMap::new(),
    )
}

#[test]
fn cancellation_aborts_queued_work_and_preserves_running_permit() -> anyhow::Result<()> {
    use std::sync::mpsc;

    use tokio::{sync::oneshot, task::spawn_blocking, time::timeout};

    const LIMIT: Duration = Duration::from_secs(2);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .max_blocking_threads(1)
        .enable_time()
        .build()?;
    runtime.block_on(async {
        let running = Gzip::new();
        let running_permit = Arc::clone(&running.slot).try_acquire_owned()?;
        let (ready, started) = oneshot::channel();
        let (release, wait) = mpsc::channel();
        let running_job = CompressionJob(spawn_blocking(move || {
            let _permit = running_permit;
            let _ = ready.send(());
            wait.recv_timeout(LIMIT).map_err(std::io::Error::other)?;
            Ok(Bytes::new())
        }));
        timeout(LIMIT, started).await??;

        let queued = Gzip::new();
        let queued_permit = Arc::clone(&queued.slot).try_acquire_owned()?;
        let (ran, mut observation) = oneshot::channel();
        let queued_job = CompressionJob(spawn_blocking(move || {
            let _permit = queued_permit;
            let _ = ran.send(());
            Ok(Bytes::new())
        }));

        drop(queued_job);
        drop(running_job);
        let held_after_cancel = running.slot.available_permits();
        release.send(())?;
        timeout(LIMIT, spawn_blocking(|| ())).await??;
        let skipped = matches!(
            observation.try_recv(),
            Err(oneshot::error::TryRecvError::Closed)
        );
        assert_eq!(
            (
                held_after_cancel,
                running.slot.available_permits(),
                queued.slot.available_permits(),
                skipped
            ),
            (0, 1, 1, true),
        );
        Ok(())
    })
}
