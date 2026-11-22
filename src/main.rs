pub mod collector;
pub mod compression;
pub mod config;
pub mod metrics;
pub mod render;
pub mod temperature;
pub mod wireless;

use std::env;
use std::ffi::OsStr;
use std::io::{self, Write as _};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{Request, State};
use axum::http::uri::Authority;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse as _, Response};
use axum::routing::get;
use hyper::server::conn::http1;
use hyper::service::{Service as _, service_fn};
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::service::TowerToHyperService;
use socket2::SockRef;
use tokio::net::{TcpListener, TcpSocket};
use tokio::sync::{Semaphore, watch};
use tokio::time::{self, MissedTickBehavior};

use compression::{Encoding, Gzip, MAX_GZIP_BYTES};
use config::{AllowedHosts, without_final_dot};
use temperature::Temperature;

const BIND_ENV: &str = "ZEST_BIND";
const DEFAULT_BIND: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8080);
const PERIOD: Duration = Duration::from_secs(3);
const FRESHNESS: Duration = Duration::from_secs(12);
const RUNTIME_SHUTDOWN: Duration = Duration::from_secs(1);
const MAX_CONNECTIONS: usize = 24;
const MAX_PENDING_CONNECTIONS: u32 = 32;
const MAX_HEADER_BYTES: usize = 8192;
const HEADER_DEADLINE: Duration = Duration::from_secs(2);
const CONNECTION_DEADLINE: Duration = Duration::from_secs(5);

const CORE_HEADER: &str = "x-zest-core";
const CONTENT_SECURITY_POLICY: &str = "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";
const HTML: &str = "text/html; charset=utf-8";
const TEXT: &str = "text/plain; charset=utf-8";

type SharedSnapshot = watch::Receiver<Option<Published>>;

#[derive(Clone)]
struct AppState {
    snapshot: SharedSnapshot,
    gzip: Gzip,
}

#[derive(Clone, Copy)]
struct LocalDestination(SocketAddr);

struct Published {
    sampled_at: Instant,
    healthy: bool,
    html: Bytes,
}

#[derive(Debug, PartialEq, Eq)]
enum MemoryLock {
    Disabled,
    Enabled,
}

impl MemoryLock {
    fn parse(value: Option<&OsStr>) -> anyhow::Result<Self> {
        match value {
            None => Ok(Self::Disabled),
            Some(value) if value == "0" => Ok(Self::Disabled),
            Some(value) if value == "1" => Ok(Self::Enabled),
            Some(_) => anyhow::bail!("ZEST_LOCK_MEMORY must be 0 or 1"),
        }
    }

    fn apply(self) -> anyhow::Result<()> {
        match self {
            Self::Disabled => Ok(()),
            Self::Enabled => lock_process_memory(),
        }
    }
}

#[cfg(target_os = "linux")]
fn lock_process_memory() -> anyhow::Result<()> {
    use rustix::mm::{MlockAllFlags, mlockall};

    mlockall(MlockAllFlags::CURRENT | MlockAllFlags::FUTURE | MlockAllFlags::ONFAULT)
        .context("lock dashboard memory against swap (check RLIMIT_MEMLOCK)")
}

#[cfg(not(target_os = "linux"))]
fn lock_process_memory() -> anyhow::Result<()> {
    anyhow::bail!("ZEST_LOCK_MEMORY=1 requires Linux")
}

fn main() -> anyhow::Result<()> {
    let mut arguments = env::args_os().skip(1);
    if let Some(argument) = arguments.next() {
        anyhow::ensure!(arguments.next().is_none(), "Run zest --help for usage");
        let mut output = io::stdout().lock();
        match argument.to_str() {
            Some("--version" | "-V") => writeln!(output, "zest {}", env!("CARGO_PKG_VERSION"))?,
            Some("--help" | "-h") => writeln!(
                output,
                "Zest — Linux system dashboard\n\nUsage: zest [--help | --version]\n\nEnvironment:\n  ZEST_BIND              Numeric IP:port (default: 127.0.0.1:8080)\n  ZEST_ALLOWED_HOSTS     Comma-separated DNS aliases for the HTTP Host header\n  ZEST_LOCK_MEMORY       Set to 1 to lock process memory on Linux\n  ZEST_TEMPERATURE_PATH  Optional sysfs temperature sensor path"
            )?,
            _ => anyhow::bail!("Unrecognized option. Run zest --help for usage"),
        }
        return Ok(());
    }
    let address = match env::var(BIND_ENV) {
        Ok(value) => value
            .parse()
            .context("ZEST_BIND must be an IP address and port")?,
        Err(env::VarError::NotPresent) => DEFAULT_BIND,
        Err(error) => return Err(error).context("ZEST_BIND must be valid Unicode"),
    };
    anyhow::ensure!(
        !address.ip().to_canonical().is_multicast(),
        "ZEST_BIND cannot be a multicast address"
    );
    let hosts = match env::var("ZEST_ALLOWED_HOSTS") {
        Ok(value) => AllowedHosts::parse(&value)?,
        Err(env::VarError::NotPresent) => AllowedHosts::default(),
        Err(error) => return Err(error).context("ZEST_ALLOWED_HOSTS must be valid Unicode"),
    };
    let temperature = Temperature::configure(env::var_os("ZEST_TEMPERATURE_PATH").as_deref())?;
    MemoryLock::parse(env::var_os("ZEST_LOCK_MEMORY").as_deref())?.apply()?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .max_blocking_threads(1)
        .enable_all()
        .build()?;
    let result = runtime.block_on(run(address, hosts, temperature));
    runtime.shutdown_timeout(RUNTIME_SHUTDOWN);
    result
}

async fn run(
    address: SocketAddr,
    hosts: AllowedHosts,
    temperature: Temperature,
) -> anyhow::Result<()> {
    let socket = if address.is_ipv4() {
        TcpSocket::new_v4()?
    } else {
        let socket = TcpSocket::new_v6()?;
        SockRef::from(&socket)
            .set_only_v6(false)
            .context("enable IPv4 on the IPv6 dashboard listener")?;
        socket
    };
    socket.set_reuseaddr(true)?;
    socket.bind(address).context("bind dashboard listener")?;
    let listener = socket.listen(MAX_PENDING_CONNECTIONS)?;
    let (sender, state) = watch::channel(None);
    let app = router(state, hosts);
    tokio::select! {
        result = sample(sender, temperature) => result,
        result = serve(&listener, app) => result,
    }
}

async fn sample(
    sender: watch::Sender<Option<Published>>,
    temperature: Temperature,
) -> anyhow::Result<()> {
    let mut collector = collector::Collector::new(temperature);
    let mut timer = time::interval(PERIOD);
    timer.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        timer.tick().await;
        let worker = tokio::task::spawn_blocking(move || {
            let sampled_at = Instant::now();
            let snapshot = collector.sample();
            let healthy = snapshot.healthy;
            let html = Bytes::from(render::fragment(snapshot).into_string());
            (
                collector,
                Published {
                    sampled_at,
                    healthy,
                    html,
                },
            )
        });
        let (next, published) = time::timeout(FRESHNESS, worker)
            .await
            .context("dashboard sampler timed out")?
            .context("dashboard sampler failed")?;
        collector = next;
        sender.send_replace(Some(published));
    }
}

async fn serve(listener: &TcpListener, app: Router) -> anyhow::Result<()> {
    let available = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    loop {
        let permit = Arc::clone(&available).acquire_owned().await?;
        let stream = match listener.accept().await {
            Ok((stream, _)) => stream,
            Err(error) if error.kind() == io::ErrorKind::ConnectionAborted => continue,
            Err(error) => return Err(error).context("accept dashboard connection"),
        };
        let Ok(destination) = stream.local_addr() else {
            continue;
        };
        let app = TowerToHyperService::new(app.clone());
        let service = service_fn(move |mut request: hyper::Request<hyper::body::Incoming>| {
            request
                .extensions_mut()
                .insert(LocalDestination(destination));
            app.call(request)
        });
        tokio::spawn(async move {
            let _permit = permit;
            let mut http = http1::Builder::new();
            http.timer(TokioTimer::new())
                .keep_alive(false)
                .max_headers(32)
                .max_buf_size(MAX_HEADER_BYTES)
                .header_read_timeout(HEADER_DEADLINE);
            let connection = http.serve_connection(TokioIo::new(stream), service);
            // Client disconnects, malformed requests and deadlines end only this connection.
            let _ = time::timeout(CONNECTION_DEADLINE, connection).await;
        });
    }
}

fn router(state: SharedSnapshot, hosts: AllowedHosts) -> Router {
    let page = Bytes::from(render::page().into_string());
    Router::new()
        .route(
            "/",
            get(move || async move { ([(header::CONTENT_TYPE, HTML)], page).into_response() }),
        )
        .route(
            "/htmx.min.js",
            get(|| async {
                asset(
                    "text/javascript; charset=utf-8",
                    include_str!("../assets/htmx.min.js"),
                )
            }),
        )
        .route(
            "/style.css",
            get(|| async {
                asset(
                    "text/css; charset=utf-8",
                    include_str!("../assets/style.css"),
                )
            }),
        )
        .route(
            "/app.js",
            get(|| async {
                asset(
                    "text/javascript; charset=utf-8",
                    include_str!("../assets/app.js"),
                )
            }),
        )
        .route("/snapshot", get(latest))
        .route(
            "/healthz",
            get(|State(state): State<AppState>| async move { health(&state.snapshot) }),
        )
        .with_state(AppState {
            snapshot: state,
            gzip: Gzip::new(),
        })
        .layer(middleware::from_fn_with_state(hosts, check_request))
}

async fn check_request(
    State(hosts): State<AllowedHosts>,
    request: Request,
    next: Next,
) -> Response {
    let valid = request.extensions().get::<LocalDestination>().is_some_and(
        |LocalDestination(destination)| valid_host(request.headers(), *destination, &hosts),
    );
    let mut response = if valid {
        next.run(request).await
    } else {
        (
            StatusCode::MISDIRECTED_REQUEST,
            "Request host is not configured.",
        )
            .into_response()
    };
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CONTENT_SECURITY_POLICY),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    response
}

fn valid_host(headers: &HeaderMap, destination: SocketAddr, hosts: &AllowedHosts) -> bool {
    let mut values = headers.get_all(header::HOST).iter();
    let Some(authority) = values
        .next()
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.contains('@'))
        .and_then(|value| value.parse::<Authority>().ok())
    else {
        return false;
    };
    let port = if authority.as_str() == authority.host() {
        Some(80)
    } else {
        authority.port_u16()
    };
    if values.next().is_some() || port != Some(destination.port()) {
        return false;
    }
    let host = authority.host();
    let ip_text = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    let destination = destination.ip().to_canonical();
    if let Ok(address) = ip_text.parse::<IpAddr>() {
        return address.to_canonical() == destination;
    }
    hosts.contains(host)
        || (destination.is_loopback() && without_final_dot(host).eq_ignore_ascii_case("localhost"))
}

fn asset(content_type: &'static str, body: &'static str) -> Response {
    ([(header::CONTENT_TYPE, content_type)], body).into_response()
}

async fn latest(State(state): State<AppState>, method: Method, headers: HeaderMap) -> Response {
    let mut response = latest_snapshot(&state, &method, &headers).await;
    response
        .headers_mut()
        .insert(header::VARY, HeaderValue::from_static("Accept-Encoding"));
    response
}

async fn latest_snapshot(state: &AppState, method: &Method, headers: &HeaderMap) -> Response {
    let Some((healthy, html)) = state
        .snapshot
        .borrow()
        .as_ref()
        .filter(|sample| sample.sampled_at.elapsed() <= FRESHNESS)
        .map(|sample| (sample.healthy, sample.html.clone()))
    else {
        return unavailable("Snapshot unavailable.");
    };
    match compression::negotiate(headers) {
        Encoding::Identity => snapshot_body(healthy, Body::from(html)),
        Encoding::Unacceptable => StatusCode::NOT_ACCEPTABLE.into_response(),
        Encoding::Gzip { identity_allowed } => {
            let gzip = if html.len() > MAX_GZIP_BYTES {
                None
            } else if method == Method::HEAD {
                // No compressed length is available without running the encoder.
                Some(Body::from_stream(futures_util::stream::empty::<
                    io::Result<Bytes>,
                >()))
            } else {
                state.gzip.encode(&html).await.map(Body::from)
            };
            match gzip {
                Some(body) => (
                    [(header::CONTENT_ENCODING, "gzip")],
                    snapshot_body(healthy, body),
                )
                    .into_response(),
                None if identity_allowed => snapshot_body(healthy, Body::from(html)),
                None => StatusCode::SERVICE_UNAVAILABLE.into_response(),
            }
        }
    }
}

fn snapshot_body(healthy: bool, body: Body) -> Response {
    let status = if healthy { "available" } else { "unavailable" };
    (
        [(header::CONTENT_TYPE, HTML)],
        [(CORE_HEADER, status)],
        body,
    )
        .into_response()
}

fn health(state: &SharedSnapshot) -> Response {
    if state
        .borrow()
        .as_ref()
        .is_some_and(|sample| sample.healthy && sample.sampled_at.elapsed() <= FRESHNESS)
    {
        asset(TEXT, "ok\n")
    } else {
        unavailable("Core telemetry unavailable.")
    }
}

fn unavailable(body: &'static str) -> Response {
    (StatusCode::SERVICE_UNAVAILABLE, body).into_response()
}

#[cfg(test)]
mod tests;
