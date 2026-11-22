use super::{process_table, trend_path};
use crate::collector::Process;

#[test]
fn process_names_cannot_supply_markup() {
    let process = Process {
        pid: 42,
        name: "<svg onload=\"x\"> & ' ".to_owned(),
        cpu: Some(5),
        rss: None,
        io_available: false,
        read: None,
        write: None,
    };
    let table = process_table("Test", std::iter::once(&process)).into_string();
    assert!(table.contains("<bdi>&lt;svg onload=&quot;x&quot;&gt; &amp; ' </bdi>"));
}

#[test]
fn a_missing_point_breaks_the_chart_path() {
    assert_eq!(
        trend_path([Some(0), None, Some(25), Some(100)].into_iter(), 4, 100),
        "M116 82h0.01 M118 62h0.01 L119 2 "
    );
}

#[test]
fn ssid_is_escaped_in_the_network_panel() {
    let wifi = crate::wireless::WirelessInfo {
        ssid: Some(b"<svg> &".to_vec()),
        peer: None,
        signal: None,
        rx_phy: None,
        tx_phy: None,
    };
    assert!(
        super::wireless_ssid(&wifi)
            .into_string()
            .contains("<bdi>&lt;svg&gt; &amp;</bdi>")
    );
}

#[test]
fn non_utf8_ssid_has_a_visible_fallback() {
    let wifi = crate::wireless::WirelessInfo {
        ssid: Some(vec![255]),
        peer: None,
        signal: None,
        rx_phy: None,
        tx_phy: None,
    };
    assert!(
        super::wireless_ssid(&wifi)
            .into_string()
            .contains("Binary SSID")
    );
}

#[test]
fn signal_scale_places_stronger_signal_above_weaker_signal() {
    assert_eq!(super::rssi_coordinate(-36), Some(94));
}
