use std::collections::{HashMap, VecDeque};
use std::fs::{self, File, ReadDir};
use std::io::{self, Read};
use std::net::Ipv4Addr;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rustix::fs::{Mode, OFlags, open, openat};

use crate::metrics;
use crate::temperature::Temperature;
use crate::wireless::{self, WirelessInfo};

pub(super) const HISTORY: usize = 120;
const MAX_PROCESSES: usize = 2048;
const MAX_DEVICES: usize = 64;
const MAX_DIRECTORY_ENTRIES: usize = 4096;
const MAX_SAMPLE_TIME: Duration = Duration::from_millis(900);

#[derive(Default)]
pub(super) struct Collector {
    temperature: Temperature,
    last_cpu: Option<[u64; 8]>,
    processes: HashMap<u32, ProcessCounters>,
    disks: HashMap<String, DiskCounters>,
    links: HashMap<LinkIdentity, LinkCounters>,
    link_history: HashMap<LinkIdentity, LinkHistory>,
    trends: VecDeque<Trend>,
}

pub(super) struct Snapshot<'a> {
    pub cpu: Option<u64>,
    pub load: Option<[f64; 3]>,
    pub memory: Option<(u64, u64)>,
    pub temperature: Option<i32>,
    pub uptime: Option<u64>,
    pub trends: &'a VecDeque<Trend>,
    pub processes: Scope<Process>,
    pub disks: Scope<Disk>,
    pub interfaces: Scope<Interface>,
    pub healthy: bool,
    link_history: &'a HashMap<LinkIdentity, LinkHistory>,
}

impl Snapshot<'_> {
    pub(super) fn link_samples(&self, interface: &Interface) -> Option<&VecDeque<LinkSample>> {
        self.link_history
            .get(&interface.identity?)
            .map(|series| &series.points)
    }
}

pub(super) struct Scope<T> {
    pub rows: Vec<T>,
    pub state: ScanState,
}

pub(super) enum ScanState {
    Complete,
    Partial,
    Unavailable,
}

pub(super) struct Trend {
    pub cpu: Option<u64>,
    pub ram: Option<u64>,
    pub temperature: Option<i32>,
    pub load_one: Option<f64>,
    pub disk_read: Option<u64>,
    pub disk_write: Option<u64>,
}

pub(super) struct Process {
    pub pid: u32,
    pub name: String,
    pub cpu: Option<u64>,
    pub rss: Option<u64>,
    pub io_available: bool,
    pub read: Option<u64>,
    pub write: Option<u64>,
}

#[derive(Clone, Copy)]
struct Stamp {
    generation: u64,
    time: Instant,
}

struct ProcessCounters {
    stamp: Stamp,
    ticks: u64,
    io: Option<(u64, u64)>,
}

struct DiskCounters {
    stamp: Stamp,
    sectors: (u64, u64),
}

pub(super) struct Disk {
    pub name: String,
    pub read: Option<u64>,
    pub write: Option<u64>,
}

pub(super) struct Interface {
    pub name: String,
    pub link: Link,
    pub carrier: Option<bool>,
    pub address: Address,
    traffic: Traffic,
    identity: Option<LinkIdentity>,
}

pub(super) enum Address {
    Known(Ipv4Addr),
    Unassigned,
    Unavailable,
}

pub(super) enum Link {
    Ethernet {
        speed: Option<u32>,
        duplex: Option<Duplex>,
    },
    Wireless(WirelessInfo),
}

pub(super) enum Duplex {
    Full,
    Half,
}

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub(super) struct Traffic {
    pub rx: Option<u64>,
    pub tx: Option<u64>,
}

#[derive(Clone, Copy, Hash, PartialEq, Eq)]
struct LinkIdentity {
    index: u32,
    node: u64,
}

struct LinkCounters {
    time: Instant,
    rx: u64,
    tx: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum LinkSpeeds {
    Ethernet(Option<u32>),
    Wireless { rx: Option<u32>, tx: Option<u32> },
}

pub(super) struct LinkSample {
    pub speed: LinkSpeeds,
    pub signal: Option<i32>,
    pub traffic: Traffic,
}

#[derive(Default)]
struct LinkHistory {
    association: Option<Association>,
    points: VecDeque<LinkSample>,
}

impl LinkHistory {
    fn observe(&mut self, interface: &Interface) {
        let sample = match &interface.link {
            Link::Ethernet { speed, duplex: _ } => LinkSample {
                speed: LinkSpeeds::Ethernet(*speed),
                signal: None,
                traffic: interface.traffic,
            },
            Link::Wireless(wifi) => self.wireless_sample(wifi, interface.traffic),
        };
        if self.points.len() == HISTORY {
            self.points.pop_front();
        }
        self.points.push_back(sample);
    }

    fn wireless_sample(&mut self, wifi: &WirelessInfo, traffic: Traffic) -> LinkSample {
        let association = Association {
            ssid: wifi.ssid.clone(),
            peer: wifi.peer,
        };
        if self
            .association
            .as_ref()
            .is_some_and(|previous| previous.is_incomplete_update(&association))
        {
            return LinkSample {
                speed: LinkSpeeds::Wireless { rx: None, tx: None },
                signal: None,
                traffic: Traffic::default(),
            };
        }
        let traffic = if self.association.as_ref() == Some(&association) {
            traffic
        } else {
            self.points.clear();
            self.association = Some(association);
            Traffic::default()
        };
        LinkSample {
            speed: LinkSpeeds::Wireless {
                rx: wifi.rx_phy,
                tx: wifi.tx_phy,
            },
            signal: wifi.signal,
            traffic,
        }
    }
}

#[derive(PartialEq, Eq)]
struct Association {
    ssid: Option<Vec<u8>>,
    peer: Option<[u8; 6]>,
}

impl Association {
    fn is_incomplete_update(&self, next: &Self) -> bool {
        let ssid_changed = self
            .ssid
            .as_ref()
            .zip(next.ssid.as_ref())
            .is_some_and(|(old, new)| old != new);
        let peer_changed = self
            .peer
            .zip(next.peer)
            .is_some_and(|(old, new)| old != new);
        !ssid_changed
            && !peer_changed
            && ((self.ssid.is_some() && next.ssid.is_none())
                || (self.peer.is_some() && next.peer.is_none()))
    }
}

impl Collector {
    pub(super) fn new(temperature: Temperature) -> Self {
        Self {
            temperature,
            ..Self::default()
        }
    }

    pub(super) fn sample(&mut self) -> Snapshot<'_> {
        let started = Instant::now();
        let cpu_ticks = read_metric("/proc/stat", 131_072, metrics::cpu_ticks);
        let cpu = cpu_ticks
            .zip(self.last_cpu)
            .and_then(|(current, previous)| metrics::cpu_tenths(current, previous));
        self.last_cpu = cpu_ticks;
        let load = read_metric("/proc/loadavg", 512, metrics::load);
        let memory = read_metric("/proc/meminfo", 16_384, metrics::memory);
        let temperature = self.temperature.sample();
        let uptime = read_metric("/proc/uptime", 512, metrics::uptime);
        let disks = collect_disks(&mut self.disks, started);
        let interfaces = collect_interfaces(&mut self.links, started);
        let processes = collect_processes(&mut self.processes, started);
        let disk_read = disk_total(&disks, |disk| disk.read);
        let disk_write = disk_total(&disks, |disk| disk.write);
        self.record_link_samples(&interfaces);
        if self.trends.len() == HISTORY {
            self.trends.pop_front();
        }
        self.trends.push_back(Trend {
            cpu,
            ram: memory.and_then(|(used, total)| metrics::scaled(used, 1000, u128::from(total))),
            temperature,
            load_one: load.map(|[one, _, _]| one),
            disk_read,
            disk_write,
        });

        Snapshot {
            cpu,
            load,
            memory,
            temperature,
            uptime,
            trends: &self.trends,
            processes,
            disks,
            interfaces,
            healthy: cpu_ticks.is_some() && load.is_some() && memory.is_some() && uptime.is_some(),
            link_history: &self.link_history,
        }
    }

    fn record_link_samples(&mut self, interfaces: &Scope<Interface>) {
        self.link_history.retain(|identity, _| {
            interfaces
                .rows
                .iter()
                .any(|link| link.carrier == Some(true) && link.identity == Some(*identity))
        });
        for link in &interfaces.rows {
            if let Some(identity) = link.identity.filter(|_| link.carrier == Some(true)) {
                self.link_history.entry(identity).or_default().observe(link);
            }
        }
    }
}

fn directory(path: &str) -> (Option<ReadDir>, ScanState) {
    fs::read_dir(path).map_or_else(
        |_| (None, ScanState::Unavailable),
        |entries| (Some(entries), ScanState::Complete),
    )
}

fn collect_processes(
    previous: &mut HashMap<u32, ProcessCounters>,
    started: Instant,
) -> Scope<Process> {
    let (entries, mut state) = directory("/proc");
    let mut current = HashMap::with_capacity(previous.len());
    let mut rows = Vec::with_capacity(previous.len());
    let units = ProcessUnits {
        hz: rustix::param::clock_ticks_per_second(),
        bytes_per_page: u64::try_from(rustix::param::page_size()).ok(),
    };
    for (index, entry) in entries.into_iter().flatten().enumerate() {
        if scan_limit(index, rows.len(), MAX_PROCESSES, started) {
            state = ScanState::Partial;
            break;
        }
        let Ok(entry) = entry else {
            state = ScanState::Partial;
            continue;
        };
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        match read_process(&entry.path(), pid, previous.get(&pid), units) {
            Ok((row, counters)) => {
                rows.push(row);
                current.insert(pid, counters);
            }
            Err(error)
                if error.kind() == io::ErrorKind::NotFound || error.raw_os_error() == Some(3) => {}
            Err(_) => state = ScanState::Partial,
        }
    }
    *previous = current;
    Scope { rows, state }
}

#[derive(Clone, Copy)]
struct ProcessUnits {
    hz: u64,
    bytes_per_page: Option<u64>,
}

fn read_process(
    path: &Path,
    pid: u32,
    previous: Option<&ProcessCounters>,
    units: ProcessUnits,
) -> io::Result<(Process, ProcessCounters)> {
    // An open proc PID directory keeps subsequent reads from following a recycled PID.
    let directory = open(
        path,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::DIRECTORY,
        Mode::empty(),
    )?;
    let read = |name: &str| {
        let fd = openat(
            &directory,
            name,
            OFlags::RDONLY | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        bounded_file(File::from(fd), 1536)
    };
    let stat = read("stat")?;
    let stat = String::from_utf8_lossy(&stat);
    let stat = metrics::process_stat(&stat).ok_or(io::ErrorKind::InvalidData)?;
    let io = read("io")
        .ok()
        .and_then(|data| metrics::storage_io(&String::from_utf8_lossy(&data)));
    let stamp = Stamp {
        generation: stat.started,
        time: Instant::now(),
    };
    let cpu = previous.and_then(|last| {
        let change = metrics::same_process_delta(
            stat.started,
            last.stamp.generation,
            stat.ticks,
            last.ticks,
        )?;
        metrics::process_cpu_tenths(
            change,
            units.hz,
            stamp.time.checked_duration_since(last.stamp.time)?,
        )
    });
    let (read, write) = io_rates(stamp, io, previous.map(|last| (last.stamp, last.io)), 1);
    let name = stat
        .name
        .chars()
        .filter(|character| {
            !character.is_control()
                && !matches!(character, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
        .collect();
    Ok((
        Process {
            pid,
            name,
            cpu,
            rss: stat
                .rss_pages
                .zip(units.bytes_per_page)
                .and_then(|(pages, size)| pages.checked_mul(size)),
            io_available: io.is_some(),
            read,
            write,
        },
        ProcessCounters {
            stamp,
            ticks: stat.ticks,
            io,
        },
    ))
}

fn io_rates(
    stamp: Stamp,
    current: Option<(u64, u64)>,
    previous: Option<(Stamp, Option<(u64, u64)>)>,
    unit: u64,
) -> (Option<u64>, Option<u64>) {
    let Some(((read, write), (previous, Some((last_read, last_write))))) = current.zip(previous)
    else {
        return (None, None);
    };
    let Some(window) = stamp.time.checked_duration_since(previous.time) else {
        return (None, None);
    };
    let rate = |now, last| {
        let change = metrics::same_process_delta(stamp.generation, previous.generation, now, last)?;
        metrics::per_second(change, unit, window)
    };
    (rate(read, last_read), rate(write, last_write))
}

fn collect_disks(previous: &mut HashMap<String, DiskCounters>, started: Instant) -> Scope<Disk> {
    let (paths, mut state) = devices("/sys/block", started);
    let mut current = HashMap::with_capacity(previous.len());
    let rows = paths
        .into_iter()
        .map(|(name, path)| {
            let stamp = read_metric(path.join("diskseq"), 128, |text| text.trim().parse().ok())
                .map(|generation| Stamp {
                    generation,
                    time: Instant::now(),
                });
            let sectors = read_metric(path.join("stat"), 1024, metrics::disk_sectors);
            let (read, write) = if let Some((stamp, sectors)) = stamp.zip(sectors) {
                let last = previous
                    .get(&name)
                    .map(|disk| (disk.stamp, Some(disk.sectors)));
                let rates = io_rates(stamp, Some(sectors), last, 512);
                current.insert(name.clone(), DiskCounters { stamp, sectors });
                rates
            } else {
                state = ScanState::Partial;
                (None, None)
            };
            Disk { name, read, write }
        })
        .collect();
    *previous = current;
    Scope { rows, state }
}

fn disk_total(scope: &Scope<Disk>, value: impl Fn(&Disk) -> Option<u64>) -> Option<u64> {
    if scope.rows.is_empty() || !matches!(scope.state, ScanState::Complete) {
        return None;
    }
    scope
        .rows
        .iter()
        .try_fold(0_u64, |sum, disk| sum.checked_add(value(disk)?))
}

fn collect_interfaces(
    previous: &mut HashMap<LinkIdentity, LinkCounters>,
    started: Instant,
) -> Scope<Interface> {
    let (paths, state) = devices("/sys/class/net", started);
    let addresses = nix::ifaddrs::getifaddrs().ok().map(|mut addresses| {
        let known: HashMap<_, _> = addresses
            .by_ref()
            .take(256)
            .filter_map(|item| Some((item.interface_name, item.address?.as_sockaddr_in()?.ip())))
            .collect();
        (known, addresses.next().is_none())
    });
    let wireless = read_metric("/proc/net/wireless", 8192, |text| Some(text.to_owned()));

    let mut wifi = WifiPass {
        fallback: wireless.as_deref(),
        remaining: 4,
    };
    let mut current = HashMap::with_capacity(previous.len());
    let rows = paths
        .into_iter()
        .map(|(name, path)| {
            let carrier = read_metric(path.join("carrier"), 128, |text| match text.trim() {
                "1" => Some(true),
                "0" => Some(false),
                _ => None,
            })
            .or_else(|| {
                read_metric(path.join("operstate"), 128, |text| match text.trim() {
                    "up" => Some(true),
                    "down" | "lowerlayerdown" | "dormant" | "notpresent" => Some(false),
                    _ => None,
                })
            });
            let identity = (carrier == Some(true))
                .then(|| link_identity(&path))
                .flatten();
            let traffic = identity
                .and_then(|identity| {
                    let counters = link_counters(&path, identity)?;
                    let traffic = link_rates(&counters, identity, previous);
                    current.insert(identity, counters);
                    Some(traffic)
                })
                .unwrap_or_default();
            let link = if path.join("wireless").exists() || path.join("phy80211").exists() {
                Link::Wireless(wifi.inspect(&name, identity, carrier))
            } else {
                ethernet(&path, carrier)
            };
            Interface {
                carrier,
                address: addresses
                    .as_ref()
                    .map_or(Address::Unavailable, |(pool, complete)| {
                        pool.get(&name).map_or(
                            if *complete {
                                Address::Unassigned
                            } else {
                                Address::Unavailable
                            },
                            |ip| Address::Known(*ip),
                        )
                    }),
                name,
                link,
                traffic,
                identity,
            }
        })
        .collect();
    *previous = current;
    Scope { rows, state }
}

struct WifiPass<'a> {
    fallback: Option<&'a str>,
    remaining: usize,
}

impl WifiPass<'_> {
    fn inspect(
        &mut self,
        name: &str,
        identity: Option<LinkIdentity>,
        carrier: Option<bool>,
    ) -> WirelessInfo {
        if carrier != Some(true) {
            return WirelessInfo::default();
        }
        let mut wifi = identity.filter(|_| self.remaining > 0).map_or_else(
            WirelessInfo::default,
            |identity| {
                self.remaining = self.remaining.saturating_sub(1);
                wireless::sample(identity.index)
            },
        );
        if wifi.signal.is_none() {
            wifi.signal = self
                .fallback
                .and_then(|text| metrics::wireless_signal(text, name));
        }
        wifi
    }
}

fn ethernet(path: &Path, carrier: Option<bool>) -> Link {
    if carrier != Some(true) {
        return Link::Ethernet {
            speed: None,
            duplex: None,
        };
    }
    let speed = read_metric(path.join("speed"), 128, |text| {
        text.trim().parse::<u32>().ok()
    })
    .filter(|value| *value > 0);
    let duplex = read_metric(path.join("duplex"), 128, |text| match text.trim() {
        "full" => Some(Duplex::Full),
        "half" => Some(Duplex::Half),
        _ => None,
    });
    Link::Ethernet { speed, duplex }
}

fn link_identity(path: &Path) -> Option<LinkIdentity> {
    Some(LinkIdentity {
        node: fs::metadata(path).ok()?.ino(),
        index: read_metric(path.join("ifindex"), 128, |text| text.trim().parse().ok())?,
    })
}

fn link_counters(path: &Path, identity: LinkIdentity) -> Option<LinkCounters> {
    let counters = LinkCounters {
        time: Instant::now(),
        rx: read_metric(path.join("statistics/rx_bytes"), 128, |text| {
            text.trim().parse().ok()
        })?,
        tx: read_metric(path.join("statistics/tx_bytes"), 128, |text| {
            text.trim().parse().ok()
        })?,
    };
    (fs::metadata(path).ok()?.ino() == identity.node).then_some(counters)
}

fn link_rates(
    current: &LinkCounters,
    identity: LinkIdentity,
    previous: &HashMap<LinkIdentity, LinkCounters>,
) -> Traffic {
    let Some((previous, duration)) = previous
        .get(&identity)
        .and_then(|last| Some((last, current.time.checked_duration_since(last.time)?)))
    else {
        return Traffic::default();
    };
    let rate = |now, before| metrics::per_second(metrics::delta(now, before)?, 1, duration);
    Traffic {
        rx: rate(current.rx, previous.rx),
        tx: rate(current.tx, previous.tx),
    }
}

fn devices(root: &str, started: Instant) -> (Vec<(String, PathBuf)>, ScanState) {
    let (entries, mut state) = directory(root);
    let mut paths = Vec::new();
    for (index, entry) in entries.into_iter().flatten().enumerate() {
        if scan_limit(index, paths.len(), MAX_DEVICES, started) {
            state = ScanState::Partial;
            break;
        }
        let Ok(entry) = entry else {
            state = ScanState::Partial;
            continue;
        };
        let path = entry.path();
        match fs::metadata(path.join("device")) {
            Ok(_) => {
                if let Ok(name) = entry.file_name().into_string() {
                    paths.push((name, path));
                } else {
                    state = ScanState::Partial;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) => state = ScanState::Partial,
        }
    }
    paths.sort_unstable();
    (paths, state)
}

fn scan_limit(index: usize, count: usize, limit: usize, started: Instant) -> bool {
    index >= MAX_DIRECTORY_ENTRIES || count >= limit || started.elapsed() >= MAX_SAMPLE_TIME
}

fn read_metric<T>(
    path: impl AsRef<Path>,
    limit: u64,
    parse: impl FnOnce(&str) -> Option<T>,
) -> Option<T> {
    let data = bounded_file(File::open(path).ok()?, limit).ok()?;
    parse(std::str::from_utf8(&data).ok()?)
}

fn bounded_file(file: File, limit: u64) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let read = file.take(limit.saturating_add(1)).read_to_end(&mut bytes)?;
    if u64::try_from(read).map_err(io::Error::other)? > limit {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests;
