use std::collections::VecDeque;
use std::fmt::{self, Write as _};

use maud::{DOCTYPE, Markup, html};

use crate::collector::{
    Address, Duplex, HISTORY, Interface, Link, LinkSample, LinkSpeeds, Process, ScanState, Snapshot,
};
use crate::metrics::scaled;
use crate::wireless::WirelessInfo;

const PROCESS_ROWS: usize = 6;
const HTMX_CONFIG: &str = r#"{"allowEval":false,"allowScriptTags":false,"includeIndicatorStyles":false,"selfRequestsOnly":true,"historyCacheSize":0,"timeout":5500}"#;

pub(super) fn page() -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width,initial-scale=1";
                meta name="color-scheme" content="dark";
                meta name="htmx-config" content=(HTMX_CONFIG);
                title { "Zest · System Monitor" }
                link rel="stylesheet" href="/style.css";
                script src="/htmx.min.js" defer {}
                script src="/app.js" defer {}
            }
            body {
                div.frame {
                    header.page-header {
                        div.identity {
                            div.mark aria-hidden="true" { i {} i {} i {} i {} }
                            h1 { "Zest" }
                        }
                        div #status.status.unknown role="status" { i {} span { "Waiting for data" } }
                    }
                    main #snapshot aria-label="System metrics" hx-get="/snapshot"
                        hx-trigger="load, every 3s" hx-swap="innerHTML" hx-sync="this:drop" hx-history="false" {
                        div.card.skeleton { p { "Loading system metrics…" } }
                    }
                    noscript { p { a href="/snapshot" { "Open the latest snapshot" } } }
                }
            }
        }
    }
}

pub(super) fn fragment(mut sample: Snapshot<'_>) -> Markup {
    html! {
        (overview(&sample))
        section.chart-grid aria-label="Disks and network" {
            (disk_chart(&sample))
            (interfaces(&sample))
        }
        (processes(&mut sample))
    }
}

fn overview(sample: &Snapshot<'_>) -> Markup {
    let (loads, short_load) = sample.load.map_or_else(
        || (None, "—".to_owned()),
        |[one, five, fifteen]| {
            (
                Some(format!("5 min {five:.2} · 15 min {fifteen:.2}")),
                format!("{one:.2}"),
            )
        },
    );
    let memory = sample
        .memory
        .map(|(used, total)| format!("{} / {}", kib(used), kib(total)));
    let ram = sample.trends.back().and_then(|trend| trend.ram);
    let temperature = sample.temperature.map_or_else(
        || "—".to_owned(),
        |value| format!("{:.1} °C", f64::from(value) / 1000.0),
    );
    let uptime = sample.uptime.map_or_else(|| "—".to_owned(), uptime);

    let len = sample.trends.len();
    let cpu_path = trend_path(sample.trends.iter().map(|trend| trend.cpu), len, 1000);
    let ram_path = trend_path(sample.trends.iter().map(|trend| trend.ram), len, 1000);
    let temp_path = trend_path(
        sample.trends.iter().map(|trend| {
            trend
                .temperature
                .map(|value| u64::from(value.clamp(0, 100_000).unsigned_abs()))
        }),
        len,
        100_000,
    );
    let (load_path, load_label) = load_spark(sample);

    html! {
        section.summary aria-label="System overview" {
            (metric("mint", "CPU", &percentage(sample.cpu), None,
                Some((&cpu_path, "CPU history, all cores, 0–100%, up to 120 samples."))))
            (metric("blue", "RAM", &percentage(ram), memory.as_deref(),
                Some((&ram_path, "RAM history, 0–100%, up to 120 samples."))))
            (metric("gold", "Temperature", &temperature, None,
                Some((&temp_path, "Temperature history, up to 120 samples. Scale 0–100 °C, with out-of-range values drawn at the nearest limit."))))
            (metric("muted", "Uptime", &uptime, None, None))
            (metric("mauve", "Load · 1 min", &short_load, loads.as_deref(),
                Some((&load_path, &load_label))))
        }
    }
}

fn metric(
    color: &str,
    label: &str,
    value: &str,
    description: Option<&str>,
    history: Option<(&str, &str)>,
) -> Markup {
    let scale = history.and_then(|(path, label)| (!path.is_empty()).then_some(label));
    html! {
        article.card.metric title=[scale] {
            @if let Some((path, axis)) = history { (sparkline(color, path, axis)) }
            div.metric-content {
                div.label { i class=(format!("dot {color}")) {} (label) }
                strong { (value) }
                @if let Some(description) = description {
                    span { (description) }
                }
            }
        }
    }
}

fn sparkline(color: &str, path: &str, label: &str) -> Markup {
    html! {
        @if !path.is_empty() {
            svg class=(format!("sparkline sparkline-{color}")) viewBox="0 0 119 84"
                preserveAspectRatio="none" role="img" aria-label=(label) focusable="false" {
                title { (label) }
                path vector-effect="non-scaling-stroke" d=(path) {}
            }
        }
    }
}

fn load_spark(sample: &Snapshot<'_>) -> (String, String) {
    let values = sample.trends.iter().map(|trend| trend.load_one);
    let ceiling = values.clone().flatten().fold(1.0_f64, f64::max).ceil();
    let path = plot_path(
        values.map(|value| value.map(|value| DecimalY((value / ceiling).mul_add(-80.0, 82.0)))),
        sample.trends.len(),
    );
    (
        path,
        format!("One-minute load history, up to 120 samples, scale 0–{ceiling:.0}."),
    )
}

struct DecimalY(f64);

impl fmt::Display for DecimalY {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:.1}", self.0)
    }
}

fn disk_chart(sample: &Snapshot<'_>) -> Markup {
    let max = sample
        .trends
        .iter()
        .flat_map(|trend| [trend.disk_read, trend.disk_write])
        .flatten()
        .max()
        .map_or(1024, |rate| rate.max(1024));
    let len = sample.trends.len();
    let read = trend_path(sample.trends.iter().map(|trend| trend.disk_read), len, max);
    let write = trend_path(sample.trends.iter().map(|trend| trend.disk_write), len, max);

    let current = sample.trends.back();
    let label = format!(
        "Disk I/O history in bytes per second. Current read rate {}, write rate {}.",
        rate(current.and_then(|item| item.disk_read)),
        rate(current.and_then(|item| item.disk_write)),
    );
    html! {
        article.card.panel.chart-panel {
            header {
                h2 { "Disks" }
                @if let [disk] = sample.disks.rows.as_slice() {
                    span.mono { (&disk.name) }
                }
            }
            div.legend { span { i.key.mint {} "Read" } span { i.key.dashed.gold {} "Write" } }
            (chart(&label, [&rate(Some(max)), "0"], &read, Some((&write, "gold-line"))))
            @if sample.disks.rows.is_empty() {
                p.note { (empty(&sample.disks.state, "No whole storage devices detected.")) }
            } @else {
                (notice(&sample.disks.state))
                @for disk in &sample.disks.rows {
                    @if sample.disks.rows.len() > 1 {
                        p.note.mono { (&disk.name) }
                    }
                    dl.traffic-values {
                        div { dt { "Read" } dd { (rate(disk.read)) } }
                        div { dt { "Write" } dd { (rate(disk.write)) } }
                    }
                }
            }
        }
    }
}

fn rssi_coordinate(signal: i32) -> Option<u64> {
    (-130..=0).contains(&signal).then_some(())?;
    u64::try_from(signal.checked_add(130)?).ok()
}

fn chart(label: &str, scale: [&str; 2], first: &str, second: Option<(&str, &str)>) -> Markup {
    let [maximum, minimum] = scale;
    html! {
        div.plot {
            div.scale aria-hidden="true" { span { (maximum) } span { (minimum) } }
            div.plot-body {
                @if first.is_empty() && second.is_none_or(|(path, _)| path.is_empty()) {
                    span.chart-message { "Unavailable" }
                }
                svg viewBox="0 0 119 84" preserveAspectRatio="none" role="img" aria-label=(label) focusable="false" {
                    path.grid-line vector-effect="non-scaling-stroke" d="M0 2H119 M0 42H119 M0 82H119" {}
                    path.mint-line vector-effect="non-scaling-stroke" d=(first) {}
                    @if let Some((path, style)) = second {
                        path class=(style) vector-effect="non-scaling-stroke" d=(path) {}
                    }
                }
            }
        }
    }
}

fn trend_path(values: impl Iterator<Item = Option<u64>>, len: usize, ceiling: u64) -> String {
    let coordinates = values.map(|point| {
        let rise = scaled(point?.min(ceiling), 80, u128::from(ceiling))?;
        Some(82_u64.saturating_sub(rise))
    });
    plot_path(coordinates, len)
}

fn plot_path<T: fmt::Display>(positions: impl Iterator<Item = Option<T>>, len: usize) -> String {
    let mut path = String::new();
    let offset = HISTORY.saturating_sub(len);
    let mut connected = false;
    for (x, position) in positions.enumerate() {
        let Some(y) = position else {
            connected = false;
            continue;
        };
        let x = x.saturating_add(offset);
        if connected {
            let _ = write!(path, "L{x} {y} ");
        } else {
            let _ = write!(path, "M{x} {y}h0.01 ");
        }
        connected = true;
    }
    path
}

fn interfaces(sample: &Snapshot<'_>) -> Markup {
    html! {
        @if sample.interfaces.rows.is_empty() {
            article.card.panel {
                header { h2 { "Network" } }
                p.note { (empty(&sample.interfaces.state, "No LAN or Wi-Fi interfaces detected.")) }
            }
        } @else {
            @for (index, interface) in sample.interfaces.rows.iter().enumerate() {
                (interface_card(interface, sample.link_samples(interface), (index == 0).then_some(&sample.interfaces.state)))
            }
        }
    }
}

fn interface_card(
    interface: &Interface,
    history: Option<&VecDeque<LinkSample>>,
    status: Option<&ScanState>,
) -> Markup {
    let (label, class) = match interface.link {
        Link::Ethernet { .. } => ("LAN", "card panel network lan"),
        Link::Wireless(_) => ("Wi‑Fi", "card panel network wlan"),
    };
    let (link, style) = match interface.carrier {
        Some(true) => ("Connected", "active"),
        Some(false) => ("Disconnected", "inactive"),
        None => ("Unavailable", "unknown"),
    };
    let address = match interface.address {
        Address::Known(value) => value.to_string(),
        Address::Unassigned => "Unassigned".to_owned(),
        Address::Unavailable => "Unavailable".to_owned(),
    };
    html! {
        article class=(class) {
            header {
                div { h2 { (label) } span.mono { (&interface.name) } }
                span class=(format!("status {style}")) { i {} (link) }
            }
            dl.interface-meta {
                div { dt { "IPv4" } dd { (address) } }
                @if let Link::Wireless(info) = &interface.link {
                    (wireless_ssid(info))
                }
            }
            dl.phy-metrics {
                @match &interface.link {
                    Link::Ethernet { speed, duplex } => {
                        (ethernet_details(*speed, duplex.as_ref(), history))
                    }
                    Link::Wireless(_) => {
                        (wireless_details(history))
                    }
                }
            }
            dl.net-traffic { (traffic_tiles(history)) }
            @if let Some(status) = status { (notice(status)) }
        }
    }
}

fn ethernet_details(
    speed: Option<u32>,
    duplex: Option<&Duplex>,
    history: Option<&VecDeque<LinkSample>>,
) -> Markup {
    let duplex = match duplex {
        Some(Duplex::Full) => Some("Full duplex"),
        Some(Duplex::Half) => Some("Half duplex"),
        None => None,
    };
    let value = speed.map_or_else(|| "—".to_owned(), |value| format!("{value} Mbit/s"));
    let speeds = history
        .into_iter()
        .flatten()
        .map(|point| match point.speed {
            LinkSpeeds::Ethernet(value) => value.map(u64::from),
            LinkSpeeds::Wireless { rx: _, tx: _ } => None,
        });
    let max = speeds
        .clone()
        .flatten()
        .max()
        .map_or(1, |value| value.max(1));
    let path = trend_path(speeds, history.map_or(0, VecDeque::len), max);
    let label = format!("Ethernet link rate history, up to 120 samples. Scale 0–{max} Mbit/s.");
    link_tile("blue", "Link rate", &value, duplex, &path, &label)
}

fn wireless_ssid(info: &WirelessInfo) -> Markup {
    let ssid = info.ssid.as_deref().map_or_else(|| "—".to_owned(), |ssid| {
        std::str::from_utf8(ssid).map_or_else(
            |_| "Binary SSID".to_owned(),
            |text| text.chars().map(|character| {
                if character.is_control() || matches!(character, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}') {
                    '�'
                } else {
                    character
                }
            }).collect(),
        )
    });
    html! { div { dt { "SSID" } dd { bdi { (ssid) } } } }
}

fn wireless_details(history: Option<&VecDeque<LinkSample>>) -> Markup {
    let rates = history.into_iter().flatten().map(wireless_rates);
    let max = rates
        .clone()
        .filter_map(|(rx, tx)| rx.max(tx))
        .max()
        .map_or(10, |value| value.max(10));
    let len = history.map_or(0, VecDeque::len);
    let rx = trend_path(
        rates.clone().map(|(rx, _)| rx.map(u64::from)),
        len,
        u64::from(max),
    );
    let tx = trend_path(rates.map(|(_, tx)| tx.map(u64::from)), len, u64::from(max));
    let current = history.and_then(VecDeque::back);
    let (rx_now, tx_now) = current.map_or((None, None), wireless_rates);
    let signal_now = current.and_then(|point| point.signal);
    let signal = trend_path(
        history
            .into_iter()
            .flatten()
            .map(|point| point.signal.and_then(rssi_coordinate)),
        len,
        130,
    );
    let signal_now = signal_now.map_or_else(|| "—".to_owned(), |value| format!("{value} dBm"));
    let axis = format!("Up to 120 samples, scale 0–{}.", phy_rate(Some(max)));
    html! {
        (link_tile("mint", "Signal", &signal_now, None, &signal, "Wi-Fi signal history, up to 120 samples. Scale −130–0 dBm."))
        (link_tile("blue", "PHY RX", &phy_rate(rx_now), None, &rx, &format!("Wi-Fi PHY RX history. {axis}")))
        (link_tile("gold", "PHY TX", &phy_rate(tx_now), None, &tx, &format!("Wi-Fi PHY TX history. {axis}")))
    }
}

const fn wireless_rates(point: &LinkSample) -> (Option<u32>, Option<u32>) {
    match point.speed {
        LinkSpeeds::Ethernet(_) => (None, None),
        LinkSpeeds::Wireless { rx, tx } => (rx, tx),
    }
}

fn traffic_tiles(history: Option<&VecDeque<LinkSample>>) -> Markup {
    let points = history.into_iter().flatten();
    let max = points
        .clone()
        .filter_map(|point| point.traffic.rx.max(point.traffic.tx))
        .max()
        .map_or(1024, |value| value.max(1024));
    let len = history.map_or(0, VecDeque::len);
    let rx = trend_path(points.clone().map(|point| point.traffic.rx), len, max);
    let tx = trend_path(points.map(|point| point.traffic.tx), len, max);
    let current = history.and_then(VecDeque::back);
    let axis = format!("Up to 120 samples, scale 0–{}.", rate(Some(max)));
    html! {
        (link_tile("mint", "Traffic RX", &rate(current.and_then(|point| point.traffic.rx)), None, &rx,
            &format!("Interface receive traffic history. {axis}")))
        (link_tile("gold", "Traffic TX", &rate(current.and_then(|point| point.traffic.tx)), None, &tx,
            &format!("Interface transmit traffic history. {axis}")))
    }
}

fn link_tile(
    color: &str,
    label: &str,
    value: &str,
    description: Option<&str>,
    path: &str,
    scale: &str,
) -> Markup {
    html! {
        div.link-stat title=[(!path.is_empty()).then_some(scale)] {
            dt { (label) }
            dd {
                (sparkline(color, path, scale))
                strong { (value) }
                @if let Some(description) = description {
                    small { (description) }
                }
            }
        }
    }
}

fn phy_rate(value: Option<u32>) -> String {
    value.map_or_else(
        || "—".to_owned(),
        |value| format!("{}.{} Mbit/s", value / 10, value % 10),
    )
}

fn processes(sample: &mut Snapshot<'_>) -> Markup {
    let rows = &mut sample.processes.rows;
    let count = rows.len();
    let readable = rows.iter().filter(|process| process.io_available).count();
    let confirmed_rates = rows
        .iter()
        .filter(|process| io_score(process).is_some())
        .count();

    rows.sort_unstable_by_key(|process| (std::cmp::Reverse(process.cpu), process.pid));
    let cpu = process_table(
        "By CPU",
        rows.iter().filter(|process| process.cpu.is_some()),
    );
    rows.sort_unstable_by_key(|process| (std::cmp::Reverse(io_score(process)), process.pid));
    let io = process_table(
        "By disk I/O",
        rows.iter().filter(|process| io_score(process).is_some()),
    );

    html! {
        section.card.panel {
            header { h2 { "Processes" } span.caption { "Up to " (PROCESS_ROWS) " rows per table" } }
            (notice(&sample.processes.state))
            @if count > 0 {
                div.process-grid { (cpu) (io) }
                p.note {
                    "I/O " (readable) "/" (count) " readable · " (confirmed_rates) " with rates · 100% CPU = one core"
                }
            } @else {
                p.note { "No process data available." }
            }
        }
    }
}

fn io_score(process: &Process) -> Option<u64> {
    process
        .read
        .zip(process.write)
        .map(|(read, write)| read.saturating_add(write))
}

fn process_table<'a>(title: &str, rows: impl Iterator<Item = &'a Process>) -> Markup {
    let mut rows = rows.take(PROCESS_ROWS).peekable();
    html! {
        div.process-list {
            h3 { (title) }
            @if rows.peek().is_none() {
                p.note { "No rate samples available." }
            } @else {
                div.table-wrap tabindex="0" {
                    table {
                        thead {
                            tr {
                                th scope="col" { "Name" } th scope="col" { "PID" } th scope="col" { "CPU" }
                                th scope="col" { "RSS" } th scope="col" { "Read" } th scope="col" { "Write" }
                            }
                        }
                        tbody {
                            @for row in rows {
                                tr {
                                    th scope="row" title=(&row.name) { bdi { (&row.name) } } td { (row.pid) } td { (percentage(row.cpu)) }
                                    td { (row.rss.map_or_else(|| "—".to_owned(), bytes)) }
                                    td { (rate(row.read)) } td { (rate(row.write)) }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

const fn empty<'a>(state: &ScanState, empty_label: &'a str) -> &'a str {
    match state {
        ScanState::Complete => empty_label,
        ScanState::Partial => "Partial sample.",
        ScanState::Unavailable => "Source unavailable.",
    }
}

fn notice(state: &ScanState) -> Markup {
    html! {
        @if matches!(state, ScanState::Partial | ScanState::Unavailable) {
            p.note { (empty(state, "")) }
        }
    }
}

fn percentage(value: Option<u64>) -> String {
    value.map_or_else(
        || "—".to_owned(),
        |value| format!("{}.{}%", value / 10, value % 10),
    )
}

fn uptime(seconds: u64) -> String {
    let days = seconds / 86_400;
    let hours = seconds / 3600 % 24;
    let minutes = seconds / 60 % 60;
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{minutes}m")
    }
}

fn rate(value: Option<u64>) -> String {
    value.map_or_else(|| "—".to_owned(), |value| format!("{}/s", bytes(value)))
}

fn kib(value: u64) -> String {
    bytes(value.saturating_mul(1024))
}

fn bytes(value: u64) -> String {
    let (unit, suffix) = if value >= 1_073_741_824 {
        (1_073_741_824, "GiB")
    } else if value >= 1_048_576 {
        (1_048_576, "MiB")
    } else if value >= 1024 {
        (1024, "KiB")
    } else {
        return format!("{value} B");
    };
    let fraction = value % unit * 10 / unit;
    format!("{}.{} {suffix}", value / unit, fraction)
}

#[cfg(test)]
mod tests;
