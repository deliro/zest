use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::time::Instant;

use super::Temperature;

struct Fixture(PathBuf);

impl Fixture {
    fn new(label: &str) -> anyhow::Result<Self> {
        let root = std::env::temp_dir().join(format!("zest-test-{}-{label}", std::process::id()));
        fs::create_dir(&root)?;
        Ok(Self(root.canonicalize()?))
    }

    fn root(&self) -> &Path {
        &self.0
    }

    fn device(&self, kind: &str, name: &str, files: &[(&str, &str)]) -> anyhow::Result<()> {
        let class = self.0.join("class").join(kind);
        let device = self.0.join("devices").join(kind).join(name);
        fs::create_dir_all(&class)?;
        fs::create_dir_all(&device)?;
        for (file, value) in files {
            fs::write(device.join(file), value)?;
        }
        symlink(device, class.join(name))?;
        Ok(())
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn x86_uses_hottest_intel_package_instead_of_core_or_drive() -> anyhow::Result<()> {
    let fixture = Fixture::new("intel")?;
    fixture.device(
        "hwmon",
        "hwmon0",
        &[
            ("name", "coretemp\n"),
            ("temp1_label", "Package id 0\n"),
            ("temp1_input", "53000\n"),
            ("temp2_label", "Core 0\n"),
            ("temp2_input", "59000\n"),
        ],
    )?;
    fixture.device(
        "hwmon",
        "hwmon1",
        &[
            ("name", "coretemp\n"),
            ("temp1_label", "Package id 1\n"),
            ("temp1_input", "55000\n"),
        ],
    )?;
    fixture.device(
        "hwmon",
        "hwmon2",
        &[
            ("name", "nvme\n"),
            ("temp1_label", "Composite\n"),
            ("temp1_input", "78000\n"),
        ],
    )?;
    let mut meter = Temperature::default();
    assert_eq!(
        meter.sample_at(fixture.root(), Instant::now()),
        Some(55_000)
    );
    Ok(())
}

#[test]
fn x86_amd_prefers_die_sensor_over_control_offset() -> anyhow::Result<()> {
    let fixture = Fixture::new("amd")?;
    fixture.device(
        "hwmon",
        "hwmon0",
        &[
            ("name", "k10temp\n"),
            ("temp1_label", "Tctl\n"),
            ("temp1_input", "64000\n"),
            ("temp2_label", "Tdie\n"),
            ("temp2_input", "54000\n"),
        ],
    )?;
    assert_eq!(
        Temperature::default().sample_at(fixture.root(), Instant::now()),
        Some(54_000)
    );
    Ok(())
}

#[test]
fn arm_uses_cpu_and_soc_zones_instead_of_first_gpu_zone() -> anyhow::Result<()> {
    let fixture = Fixture::new("arm")?;
    fixture.device(
        "thermal",
        "thermal_zone0",
        &[("type", "gpu-thermal\n"), ("temp", "71000\n")],
    )?;
    fixture.device(
        "thermal",
        "thermal_zone1",
        &[("type", "soc-thermal\n"), ("temp", "48000\n")],
    )?;
    fixture.device(
        "thermal",
        "thermal_zone2",
        &[("type", "bigcore0-thermal\n"), ("temp", "53000\n")],
    )?;
    assert_eq!(
        Temperature::default().sample_at(fixture.root(), Instant::now()),
        Some(53_000)
    );
    Ok(())
}

#[test]
fn gpu_and_nvme_only_leave_temperature_unknown() -> anyhow::Result<()> {
    let fixture = Fixture::new("unrelated")?;
    fixture.device(
        "thermal",
        "thermal_zone0",
        &[("type", "gpu-thermal\n"), ("temp", "71000\n")],
    )?;
    fixture.device(
        "hwmon",
        "hwmon0",
        &[("name", "nvme\n"), ("temp1_input", "68000\n")],
    )?;
    assert_eq!(
        Temperature::default().sample_at(fixture.root(), Instant::now()),
        None
    );
    Ok(())
}

#[test]
fn explicit_sensor_overrides_automatic_selection() -> anyhow::Result<()> {
    let fixture = Fixture::new("override")?;
    fixture.device(
        "thermal",
        "thermal_zone0",
        &[("type", "cpu_thermal\n"), ("temp", "55000\n")],
    )?;
    fixture.device(
        "hwmon",
        "hwmon7",
        &[("name", "nvme\n"), ("temp1_input", "64000\n")],
    )?;
    let mut meter = Temperature::configure_at(
        fixture.root(),
        Some(OsStr::new("/sys/class/hwmon/hwmon7/temp1_input")),
    )?;
    assert_eq!(
        meter.sample_at(fixture.root(), Instant::now()),
        Some(64_000)
    );
    Ok(())
}

#[test]
fn explicit_sensor_rejects_bad_path_or_symbolic_escape() -> anyhow::Result<()> {
    let fixture = Fixture::new("escape")?;
    let directory = fixture.root().join("class/thermal/thermal_zone0");
    fs::create_dir_all(&directory)?;
    let outside = fixture.root().join("temp");
    fs::write(&outside, "42000")?;
    symlink(outside, directory.join("temp"))?;
    let invalid = [
        "/tmp/temp",
        "/sys/class/thermal/thermal_zone0/type",
        "/sys/class/hwmon/hwmon0/temp1_label",
        "/sys/class/hwmon/../hwmon0/temp1_input",
        "/sys/class/thermal/thermal_zone0/temp",
    ];
    for value in invalid {
        assert!(
            Temperature::configure_at(fixture.root(), Some(OsStr::new(value))).is_err(),
            "{value}"
        );
    }
    Ok(())
}
