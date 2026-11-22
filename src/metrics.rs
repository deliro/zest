use std::time::Duration;

#[derive(Debug, PartialEq, Eq)]
pub(super) struct ProcessStat<'a> {
    pub name: &'a str,
    pub ticks: u64,
    pub started: u64,
    pub rss_pages: Option<u64>,
}

pub(super) fn process_stat(text: &str) -> Option<ProcessStat<'_>> {
    let (_, tail) = text.split_once(" (")?;
    let (name, fields) = tail.rsplit_once(") ")?;
    let mut fields = fields.split_ascii_whitespace();
    let user: u64 = fields.nth(11)?.parse().ok()?;
    let system = fields.next()?.parse().ok()?;
    let started = fields.nth(6)?.parse().ok()?;
    let rss_pages = fields.nth(1).and_then(|pages| pages.parse().ok());
    Some(ProcessStat {
        name,
        ticks: user.checked_add(system)?,
        started,
        rss_pages,
    })
}

pub(super) fn memory(text: &str) -> Option<(u64, u64)> {
    let total = unsigned_field(text, "MemTotal")?;
    let available = unsigned_field(text, "MemAvailable")?;
    (total > 0).then_some((delta(total, available)?, total))
}

pub(super) fn storage_io(text: &str) -> Option<(u64, u64)> {
    Some((
        unsigned_field(text, "read_bytes")?,
        unsigned_field(text, "write_bytes")?,
    ))
}

fn unsigned_field(text: &str, key: &str) -> Option<u64> {
    text.lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| *name == key)?
        .1
        .split_ascii_whitespace()
        .next()?
        .parse()
        .ok()
}

pub(super) fn uptime(text: &str) -> Option<u64> {
    let (seconds, _) = text.split_ascii_whitespace().next()?.split_once('.')?;
    seconds.parse().ok()
}

pub(super) fn load(text: &str) -> Option<[f64; 3]> {
    let mut values = text.split_ascii_whitespace().take(3).map(|field| {
        let value = field.parse::<f64>().ok()?;
        (value.is_finite() && value >= 0.0).then_some(value)
    });
    Some([values.next()??, values.next()??, values.next()??])
}

pub(super) fn cpu_ticks(text: &str) -> Option<[u64; 8]> {
    let mut fields = text.lines().next()?.split_ascii_whitespace();
    (fields.next()? == "cpu").then_some(())?;
    let mut values = [0; 8];
    for value in &mut values {
        *value = fields.next()?.parse().ok()?;
    }
    Some(values)
}

pub(super) fn cpu_tenths(current: [u64; 8], previous: [u64; 8]) -> Option<u64> {
    let elapsed = current
        .into_iter()
        .zip(previous)
        .try_fold(0_u64, |total, (now, last)| {
            total.checked_add(delta(now, last)?)
        })?;
    let [_, _, _, idle, wait, _, _, _] = current;
    let [_, _, _, last_idle, last_wait, _, _, _] = previous;
    let inactive = delta(idle, last_idle)?.checked_add(delta(wait, last_wait)?)?;
    scaled(delta(elapsed, inactive)?, 1000, u128::from(elapsed))
}

pub(super) fn disk_sectors(text: &str) -> Option<(u64, u64)> {
    let mut fields = text.split_ascii_whitespace();
    Some((fields.nth(2)?.parse().ok()?, fields.nth(3)?.parse().ok()?))
}

pub(super) const fn delta(now: u64, last: u64) -> Option<u64> {
    now.checked_sub(last)
}

pub(super) const fn same_process_delta(
    started: u64,
    previous_start: u64,
    counter: u64,
    previous_counter: u64,
) -> Option<u64> {
    if started == previous_start {
        delta(counter, previous_counter)
    } else {
        None
    }
}

pub(super) fn per_second(change: u64, units: u64, window: Duration) -> Option<u64> {
    scaled(change, units.checked_mul(1_000_000_000)?, window.as_nanos())
}

pub(super) fn process_cpu_tenths(change: u64, hz: u64, window: Duration) -> Option<u64> {
    scaled(
        change,
        1_000_000_000_000,
        window.as_nanos().checked_mul(u128::from(hz))?,
    )
}

pub(super) fn scaled(amount: u64, factor: u64, divisor: u128) -> Option<u64> {
    let numerator = u128::from(amount) * u128::from(factor);
    u64::try_from(numerator.checked_div(divisor)?).ok()
}

pub(super) fn wireless_signal(text: &str, name: &str) -> Option<i32> {
    let (_, columns) = text.lines().find_map(|line| {
        let (interface, columns) = line.split_once(':')?;
        (interface.trim() == name).then_some((interface, columns))
    })?;
    let level: i32 = columns
        .split_ascii_whitespace()
        .nth(2)?
        .trim_end_matches('.')
        .parse()
        .ok()?;
    (-127..=-1).contains(&level).then_some(level)
}

#[cfg(test)]
mod tests;
