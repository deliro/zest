use std::io::{self, Write as _};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::http::{HeaderMap, header};
use flate2::{Compression, write::GzEncoder};
use tokio::{sync::Semaphore, task::JoinHandle, time};

pub(super) const MAX_GZIP_BYTES: usize = 1024 * 1024;
const COMPRESSION_DEADLINE: Duration = Duration::from_millis(150);
const FULL_QUALITY: u16 = 1000;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Encoding {
    Gzip { identity_allowed: bool },
    Identity,
    Unacceptable,
}

#[derive(Default)]
struct Preferences {
    gzip: Option<u16>,
    identity: Option<u16>,
    wildcard: Option<u16>,
}

pub(super) fn negotiate(headers: &HeaderMap) -> Encoding {
    let mut values = Preferences::default();
    for item in headers
        .get_all(header::ACCEPT_ENCODING)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
    {
        let (coding, weight) = item
            .split_once(';')
            .map_or((item, None), |(coding, weight)| (coding, Some(weight)));
        let coding = coding.trim();
        let slot = if coding.eq_ignore_ascii_case("gzip") {
            &mut values.gzip
        } else if coding.eq_ignore_ascii_case("identity") {
            &mut values.identity
        } else if coding == "*" {
            &mut values.wildcard
        } else {
            continue;
        };
        let quality = weight.map_or(FULL_QUALITY, |weight| parse_weight(weight).unwrap_or(0));
        // Preserve explicit exclusions if a coding is listed repeatedly.
        *slot = Some(slot.map_or(quality, |old| old.min(quality)));
    }
    let gzip = values.gzip.or(values.wildcard).unwrap_or(0);
    let identity_allowed = values
        .identity
        .map_or_else(|| values.wildcard != Some(0), |value| value > 0);
    if gzip > 0 && values.identity.is_none_or(|identity| gzip >= identity) {
        Encoding::Gzip { identity_allowed }
    } else if identity_allowed {
        Encoding::Identity
    } else {
        Encoding::Unacceptable
    }
}

fn parse_weight(weight: &str) -> Option<u16> {
    let (name, value) = weight.split_once('=')?;
    if !name.trim().eq_ignore_ascii_case("q") {
        return None;
    }
    let value = value.trim();
    let (whole, digits) = value.split_once('.').unwrap_or((value, ""));
    if digits.len() > 3 || !digits.bytes().all(|digit| digit.is_ascii_digit()) {
        return None;
    }
    let mut decimal = 0_u16;
    for (digit, place) in digits.bytes().zip([100_u16, 10, 1]) {
        decimal = decimal.checked_add(u16::from(digit.checked_sub(b'0')?).checked_mul(place)?)?;
    }
    match whole {
        "0" => Some(decimal),
        "1" if decimal == 0 => Some(FULL_QUALITY),
        _ => None,
    }
}

#[derive(Clone)]
pub(super) struct Gzip {
    slot: Arc<Semaphore>,
}

impl Gzip {
    pub(super) fn new() -> Self {
        Self {
            slot: Arc::new(Semaphore::new(1)),
        }
    }

    pub(super) async fn encode(&self, input: &Bytes) -> Option<Bytes> {
        if input.len() > MAX_GZIP_BYTES {
            return None;
        }
        let permit = Arc::clone(&self.slot).try_acquire_owned().ok()?;
        let input = input.clone();
        let mut job = CompressionJob(tokio::task::spawn_blocking(move || {
            let _permit = permit;
            compress(&input)
        }));
        time::timeout(COMPRESSION_DEADLINE, &mut job.0)
            .await
            .ok()?
            .ok()?
            .ok()
    }
}

struct CompressionJob(JoinHandle<io::Result<Bytes>>);

impl Drop for CompressionJob {
    fn drop(&mut self) {
        // Queued work is cancellable. Running work keeps its permit until the encoder finishes.
        self.0.abort();
    }
}

fn compress(input: &[u8]) -> io::Result<Bytes> {
    let mut stream = GzEncoder::new(LimitedBuffer(Vec::new()), Compression::new(1));
    stream.write_all(input)?;
    Ok(Bytes::from(stream.finish()?.0))
}

struct LimitedBuffer(Vec<u8>);

impl io::Write for LimitedBuffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_GZIP_BYTES.saturating_sub(self.0.len()) {
            return Err(io::ErrorKind::InvalidData.into());
        }
        self.0.try_reserve(bytes.len()).map_err(io::Error::other)?;
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
