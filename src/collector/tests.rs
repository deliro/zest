use super::{
    Address, Collector, Interface, Link, LinkCounters, LinkIdentity, LinkSpeeds, ScanState, Scope,
    Traffic, WifiPass, WirelessInfo, link_rates,
};
use std::collections::HashMap;
use std::time::{Duration, Instant};

fn counters(time: Instant, rx: u64, tx: u64) -> LinkCounters {
    LinkCounters { time, rx, tx }
}

#[test]
fn rx_reset_does_not_hide_valid_tx_rate() {
    let old = Instant::now();
    let identity = LinkIdentity { index: 3, node: 44 };
    let previous = HashMap::from([(identity, counters(old, 10_000, 700))]);
    let now = counters(old + Duration::from_secs(2), 12, 1100);
    assert_eq!(
        link_rates(&now, identity, &previous),
        Traffic {
            rx: None,
            tx: Some(200)
        }
    );
}

#[test]
fn interface_reuse_cannot_inherit_the_old_rate() {
    let old = Instant::now();
    let identity = LinkIdentity { index: 3, node: 44 };
    let previous = HashMap::from([(identity, counters(old, 900, 700))]);
    let now = counters(old + Duration::from_secs(2), 1200, 1100);
    assert_eq!(
        link_rates(&now, LinkIdentity { index: 3, node: 45 }, &previous),
        Traffic::default(),
    );
}

#[test]
fn disconnected_wifi_never_inherits_a_stale_proc_signal() {
    let mut pass = WifiPass {
        fallback: Some("wlan0: 0000 70. -36. -256"),
        remaining: 0,
    };
    assert_eq!(pass.inspect("wlan0", None, Some(false)).signal, None);
}

fn radios(identity: LinkIdentity, ssid: &[u8], peer: Option<[u8; 6]>) -> Scope<Interface> {
    Scope {
        state: ScanState::Complete,
        rows: vec![Interface {
            name: "wlan0".to_owned(),
            link: Link::Wireless(WirelessInfo {
                ssid: Some(ssid.to_vec()),
                peer,
                signal: Some(-61),
                rx_phy: Some(722),
                tx_phy: Some(540),
            }),
            carrier: Some(true),
            address: Address::Unassigned,
            traffic: Traffic {
                rx: Some(700),
                tx: Some(500),
            },
            identity: Some(identity),
        }],
    }
}

#[test]
fn unavailable_peer_leaves_a_gap_without_attributing_signal_or_traffic() {
    let identity = LinkIdentity { index: 3, node: 44 };
    let mut collector = Collector::default();
    collector.record_link_samples(&radios(identity, b"first", Some([1; 6])));
    collector.record_link_samples(&radios(identity, b"first", Some([1; 6])));
    collector.record_link_samples(&radios(identity, b"first", None));
    assert!(
        collector
            .link_history
            .get(&identity)
            .is_some_and(|history| {
                history.points.len() == 3
                    && history
                        .points
                        .front()
                        .is_some_and(|point| point.signal == Some(-61))
                    && history.points.back().is_some_and(|point| {
                        point.signal.is_none() && point.traffic == Traffic::default()
                    })
            })
    );
}

#[test]
fn link_history_keeps_only_recent_samples() {
    let identity = LinkIdentity { index: 3, node: 44 };
    let mut collector = Collector::default();
    let interface = radios(identity, b"first", Some([1; 6]));
    for _ in 0..=super::HISTORY {
        collector.record_link_samples(&interface);
    }
    let length = collector
        .link_history
        .get(&identity)
        .map(|history| history.points.len());
    assert_eq!(length, Some(super::HISTORY));
}

#[test]
fn disconnect_discards_link_history_even_if_interface_identity_is_reused() {
    let identity = LinkIdentity { index: 3, node: 44 };
    let mut collector = Collector::default();
    let mut interface = radios(identity, b"first", Some([1; 6]));
    collector.record_link_samples(&interface);
    collector.record_link_samples(&interface);
    if let Some(row) = interface.rows.first_mut() {
        row.carrier = Some(false);
    }
    collector.record_link_samples(&interface);
    collector.record_link_samples(&radios(identity, b"first", Some([1; 6])));
    let length = collector
        .link_history
        .get(&identity)
        .map(|history| history.points.len());
    assert_eq!(length, Some(1));
}

#[test]
fn changed_ssid_starts_new_history_and_drops_cross_association_traffic() {
    let identity = LinkIdentity { index: 3, node: 44 };
    let mut collector = Collector::default();
    let old = radios(identity, b"first", Some([1; 6]));
    collector.record_link_samples(&old);
    collector.record_link_samples(&old);
    collector.record_link_samples(&radios(identity, b"second", None));
    let series = collector.link_history.get(&identity);
    assert!(series.is_some_and(|history| history.points.len() == 1
        && history.points.back().is_some_and(|point| {
            point.speed
                == LinkSpeeds::Wireless {
                    rx: Some(722),
                    tx: Some(540),
                }
                && point.signal == Some(-61)
                && point.traffic == Traffic::default()
        })));
}

#[test]
fn each_wifi_interface_retains_its_own_signal_and_traffic() {
    let first = LinkIdentity { index: 3, node: 44 };
    let second = LinkIdentity { index: 4, node: 45 };
    let mut collector = Collector::default();
    let mut interfaces = radios(first, b"first", Some([1; 6]));
    let mut other = radios(second, b"second", Some([2; 6]));
    if let Some(row) = other.rows.first_mut() {
        row.name = "wlan1".to_owned();
        row.traffic = Traffic {
            rx: Some(90),
            tx: None,
        };
        if let Link::Wireless(wifi) = &mut row.link {
            wifi.signal = Some(-82);
        }
    }
    interfaces.rows.append(&mut other.rows);
    collector.record_link_samples(&interfaces);
    collector.record_link_samples(&interfaces);
    let latest = |identity| {
        collector
            .link_history
            .get(&identity)
            .and_then(|history| history.points.back())
            .map(|point| (point.signal, point.traffic.rx))
    };
    assert_eq!(
        (latest(first), latest(second)),
        (Some((Some(-61), Some(700))), Some((Some(-82), Some(90)))),
    );
}

#[test]
fn sysfs_device_filter_includes_disk_and_passthrough_or_virtual_pci_devices() -> anyhow::Result<()>
{
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::path::PathBuf;

    struct Fixture(PathBuf);

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    let root = std::env::temp_dir().join(format!("zest-test-{}-devices", std::process::id()));
    fs::create_dir(&root)?;
    let fixture = Fixture(root);
    let pci = fixture.0.join("devices/pci");
    fs::create_dir_all(&pci)?;
    for (class, names, virtual_names) in [
        (
            "block",
            ["nvme0n1", "sda", "vda", "xvda"].as_slice(),
            ["dm-0", "loop0", "zram0"].as_slice(),
        ),
        (
            "net",
            ["ens3", "eth0"].as_slice(),
            ["lo", "veth0", "tun0"].as_slice(),
        ),
    ] {
        let class = fixture.0.join(class);
        for name in names {
            let path = class.join(name);
            fs::create_dir_all(&path)?;
            symlink(&pci, path.join("device"))?;
        }
        for name in virtual_names {
            fs::create_dir_all(class.join(name))?;
        }
    }
    let scan = |class| {
        super::devices(&fixture.0.join(class).to_string_lossy(), Instant::now())
            .0
            .into_iter()
            .map(|(name, _)| name)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        (scan("block"), scan("net")),
        (
            ["nvme0n1", "sda", "vda", "xvda"]
                .map(str::to_owned)
                .to_vec(),
            ["ens3", "eth0"].map(str::to_owned).to_vec()
        ),
    );
    Ok(())
}
