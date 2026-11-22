use std::ffi::OsStr;
use std::fs::{self, DirEntry, File};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rustix::fs::{Mode, OFlags, open};

const SYSFS: &str = "/sys";
const MAX_ENTRIES: usize = 96;
const MAX_SENSORS: usize = 16;
const MAX_READING_BYTES: u64 = 128;
const RETRY_INTERVAL: Duration = Duration::from_secs(30);

pub(super) struct Temperature {
    source: Source,
}

enum Source {
    Automatic(Automatic),
    Explicit(PathBuf),
}

#[derive(Default)]
struct Automatic {
    paths: Vec<PathBuf>,
    last_probe: Option<Instant>,
}

impl Default for Temperature {
    fn default() -> Self {
        Self {
            source: Source::Automatic(Automatic::default()),
        }
    }
}

impl Temperature {
    pub(super) fn configure(value: Option<&OsStr>) -> anyhow::Result<Self> {
        Self::configure_at(Path::new(SYSFS), value)
    }

    fn configure_at(root: &Path, value: Option<&OsStr>) -> anyhow::Result<Self> {
        let Some(value) = value else {
            return Ok(Self::default());
        };
        let relative = Path::new(value).strip_prefix(SYSFS).ok().filter(|path| approved_path(path))
            .ok_or_else(|| anyhow::anyhow!("ZEST_TEMPERATURE_PATH must be a temperature sensor under /sys/class/hwmon or /sys/class/thermal"))?;
        if let Ok(target) = root.join(relative).canonicalize() {
            anyhow::ensure!(
                sensor_target(root, relative, &target)
                    && target.metadata().is_ok_and(|metadata| metadata.is_file()),
                "ZEST_TEMPERATURE_PATH must resolve to a temperature sensor inside sysfs"
            );
        }
        Ok(Self {
            source: Source::Explicit(relative.to_path_buf()),
        })
    }

    pub(super) fn sample(&mut self) -> Option<i32> {
        self.sample_at(Path::new(SYSFS), Instant::now())
    }

    fn sample_at(&mut self, root: &Path, now: Instant) -> Option<i32> {
        match &mut self.source {
            Source::Automatic(auto) => auto.sample(root, now),
            Source::Explicit(relative) => {
                let path = root.join(&*relative).canonicalize().ok()?;
                sensor_target(root, relative, &path).then_some(())?;
                reading(&path)
            }
        }
    }
}

impl Automatic {
    fn sample(&mut self, root: &Path, now: Instant) -> Option<i32> {
        if self.paths.is_empty()
            && self
                .last_probe
                .is_none_or(|last| now.saturating_duration_since(last) >= RETRY_INTERVAL)
        {
            self.paths = discover(root);
            self.last_probe = Some(now);
        }
        let value = self.paths.iter().filter_map(|path| reading(path)).max();
        if value.is_none() {
            self.paths.clear();
        }
        value
    }
}

fn approved_path(path: &Path) -> bool {
    if path.as_os_str().len() > 160 {
        return false;
    }
    let parts: Vec<_> = path.iter().collect();
    let [class, group, device, file] = parts.as_slice() else {
        return false;
    };
    if *class != "class" {
        return false;
    }
    let Some(device) = device.to_str() else {
        return false;
    };
    let Some(file) = file.to_str() else {
        return false;
    };
    match group.to_str() {
        Some("hwmon") => numbered(device, "hwmon") && channel(file).is_some(),
        Some("thermal") => numbered(device, "thermal_zone") && file == "temp",
        _ => false,
    }
}

fn sensor_target(root: &Path, requested: &Path, resolved: &Path) -> bool {
    resolved.starts_with(root.join("devices")) && requested.file_name() == resolved.file_name()
}

fn discover(root: &Path) -> Vec<PathBuf> {
    let mut selected = Sensors::default();
    for entry in entries(&root.join("class/hwmon")) {
        if entry
            .file_name()
            .to_str()
            .is_some_and(|name| numbered(name, "hwmon"))
        {
            let Some(device) = device_directory(root, &entry.path()) else {
                continue;
            };
            if let Some(chip) = text(&device.join("name")) {
                scan_hwmon(&mut selected, &device, chip.trim());
            }
        }
    }
    for entry in entries(&root.join("class/thermal")) {
        if entry
            .file_name()
            .to_str()
            .is_some_and(|name| numbered(name, "thermal_zone"))
        {
            let Some(device) = device_directory(root, &entry.path()) else {
                continue;
            };
            if text(&device.join("type")).is_some_and(|kind| cpu_thermal(&kind)) {
                selected.add(3, device.join("temp"));
            }
        }
    }
    selected.paths
}

fn scan_hwmon(selected: &mut Sensors, device: &Path, chip: &str) {
    if !matches!(chip, "coretemp" | "k10temp" | "zenpower") && !cpu_thermal(chip) {
        return;
    }
    // Keep the conventional package channels visible even on large chips.
    for id in ["1", "2"] {
        consider_channel(selected, device, chip, id);
    }
    for entry in entries(device) {
        let name = entry.file_name();
        let Some(id) = name
            .to_str()
            .and_then(channel)
            .filter(|id| !matches!(*id, "1" | "2"))
        else {
            continue;
        };
        consider_channel(selected, device, chip, id);
    }
}

fn consider_channel(selected: &mut Sensors, device: &Path, chip: &str, id: &str) {
    let label = text(&device.join(format!("temp{id}_label")));
    if let Some(rank) = hwmon_rank(chip, label.as_deref().map(str::trim)) {
        selected.add(rank, device.join(format!("temp{id}_input")));
    }
}

fn hwmon_rank(chip: &str, label: Option<&str>) -> Option<u8> {
    match (chip, label) {
        ("coretemp", Some(label)) if label.starts_with("Package id ") => Some(4),
        ("coretemp", Some(label)) if label.starts_with("Core ") => Some(2),
        ("k10temp" | "zenpower", Some("Tdie")) => Some(4),
        ("k10temp" | "zenpower", Some("Tctl")) => Some(3),
        ("coretemp" | "k10temp" | "zenpower", None) => Some(2),
        (other, _) if cpu_thermal(other) => Some(2),
        _ => None,
    }
}

fn cpu_thermal(kind: &str) -> bool {
    let kind = kind.trim().to_ascii_lowercase().replace('_', "-");
    matches!(
        kind.as_str(),
        "cpu"
            | "cpu-thermal"
            | "cpu-thermal-zone"
            | "soc"
            | "soc-thermal"
            | "x86-pkg-temp"
            | "littlecore-thermal"
            | "cpu-little"
            | "cpu-big"
    ) || kind.strip_suffix("-thermal").is_some_and(|name| {
        numbered(name, "cpu") || numbered(name, "bigcore") || numbered(name, "littlecore")
    })
}

#[derive(Default)]
struct Sensors {
    rank: u8,
    paths: Vec<PathBuf>,
}

impl Sensors {
    fn add(&mut self, rank: u8, path: PathBuf) {
        if rank < self.rank
            || (rank == self.rank && self.paths.len() >= MAX_SENSORS)
            || self.paths.contains(&path)
            || reading(&path).is_none()
        {
            return;
        }
        if rank > self.rank {
            self.paths.clear();
            self.rank = rank;
        }
        self.paths.push(path);
    }
}

fn entries(path: &Path) -> Vec<DirEntry> {
    let mut entries: Vec<_> = fs::read_dir(path)
        .into_iter()
        .flatten()
        .take(MAX_ENTRIES)
        .filter_map(Result::ok)
        .collect();
    entries.sort_unstable_by_key(DirEntry::file_name);
    entries
}

fn device_directory(root: &Path, path: &Path) -> Option<PathBuf> {
    let device = path.canonicalize().ok()?;
    device.starts_with(root.join("devices")).then_some(device)
}

fn numbered(name: &str, prefix: &str) -> bool {
    name.strip_prefix(prefix).is_some_and(digits)
}

fn channel(name: &str) -> Option<&str> {
    let id = name.strip_prefix("temp")?.strip_suffix("_input")?;
    digits(id).then_some(id)
}

fn digits(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit())
}

fn reading(path: &Path) -> Option<i32> {
    text(path)?.trim().parse().ok()
}

fn text(path: &Path) -> Option<String> {
    let fd = open(
        path,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .ok()?;
    let file = File::from(fd);
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut text = String::new();
    let length = file
        .take(MAX_READING_BYTES + 1)
        .read_to_string(&mut text)
        .ok()?;
    (u64::try_from(length).ok()? <= MAX_READING_BYTES).then_some(text)
}

#[cfg(test)]
mod tests;
