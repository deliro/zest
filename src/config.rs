use std::net::IpAddr;
use std::sync::Arc;

const MAX_HOSTS: usize = 16;
const MAX_NAME_LENGTH: usize = 253;

#[derive(Clone, Default)]
pub(super) struct AllowedHosts(Arc<[String]>);

impl AllowedHosts {
    pub(super) fn parse(value: &str) -> anyhow::Result<Self> {
        anyhow::ensure!(
            value.len() <= MAX_HOSTS * (MAX_NAME_LENGTH + 1),
            "ZEST_ALLOWED_HOSTS exceeds the input limit"
        );
        if value.trim().is_empty() {
            return Ok(Self::default());
        }
        let mut names = Vec::new();
        for (index, entry) in value.split(',').enumerate() {
            anyhow::ensure!(
                index < MAX_HOSTS,
                "ZEST_ALLOWED_HOSTS accepts at most 16 names"
            );
            let name = without_final_dot(entry.trim());
            anyhow::ensure!(
                dns_name(name) && !name.eq_ignore_ascii_case("localhost"),
                "ZEST_ALLOWED_HOSTS requires DNS names and reserves localhost for loopback"
            );
            names.push(name.to_ascii_lowercase());
        }
        Ok(Self(names.into()))
    }

    pub(super) fn contains(&self, host: &str) -> bool {
        let host = without_final_dot(host);
        self.0.iter().any(|name| name.eq_ignore_ascii_case(host))
    }
}

fn dns_name(name: &str) -> bool {
    name.len() <= MAX_NAME_LENGTH
        && name.bytes().any(|byte| byte.is_ascii_alphabetic())
        && name.parse::<IpAddr>().is_err()
        && name.split('.').all(|label| {
            label.len() <= 63
                && label
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && label
                    .as_bytes()
                    .last()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
}

pub(super) fn without_final_dot(host: &str) -> &str {
    host.strip_suffix('.').unwrap_or(host)
}

#[cfg(test)]
mod tests;
