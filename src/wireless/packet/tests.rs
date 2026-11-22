use super::{bitrate, family, interface, message, station};

fn attribute(kind: u16, value: &[u8]) -> Vec<u8> {
    let len = 4_usize.saturating_add(value.len());
    let mut result = Vec::new();
    result.extend(u16::try_from(len).unwrap_or(u16::MAX).to_ne_bytes());
    result.extend(kind.to_ne_bytes());
    result.extend(value);
    result.resize(len.saturating_add(3) & !3, 0);
    result
}

#[test]
fn two_byte_rate_is_in_tenths_of_a_megabit() {
    assert_eq!(bitrate(&attribute(1, &540_u16.to_ne_bytes())), Some(540));
}

#[test]
fn extended_bitrate_takes_precedence() {
    let mut attributes = attribute(1, &540_u16.to_ne_bytes());
    attributes.extend(attribute(5, &70_000_u32.to_ne_bytes()));
    assert_eq!(bitrate(&attributes), Some(70_000));
}

#[test]
fn truncated_attribute_leaves_rate_unknown() {
    assert_eq!(bitrate(&[6, 0, 1, 0, 0]), None);
}

#[test]
fn controller_response_resolves_dynamic_family() {
    assert_eq!(family(&attribute(1, &26_u16.to_ne_bytes())), Some(26));
}

#[test]
fn interface_retains_ssid_bytes_without_utf8_assumption() {
    let mut attributes = attribute(5, &2_u32.to_ne_bytes());
    attributes.extend(attribute(52, b"<WiFi\xff>"));
    assert_eq!(
        interface(&attributes).map(|radio| (radio.kind, radio.ssid)),
        Some((2, Some(b"<WiFi\xff>".to_vec())))
    );
}

#[test]
fn station_reports_signed_signal_and_optional_rates() {
    let mut attrs = attribute(7, &(-36_i8).to_ne_bytes());
    attrs.extend(attribute(0x8008, &attribute(1, &722_u16.to_ne_bytes())));
    let data = attribute(0x8015, &attrs);
    assert_eq!(
        station(&data).map(|info| (info.signal, info.tx_phy, info.rx_phy)),
        Some((Some(-36), Some(722), None))
    );
}

#[test]
fn message_bounds_and_extracts_metadata() {
    let mut frame = Vec::new();
    frame.extend(20_u32.to_ne_bytes());
    frame.extend(23_u16.to_ne_bytes());
    frame.extend(16_u16.to_ne_bytes());
    frame.extend(32_u32.to_ne_bytes());
    frame.extend(0_u32.to_ne_bytes());
    frame.extend([7, 1, 0, 0]);
    let mut input = frame.as_slice();
    assert_eq!(
        message(&mut input).map(|item| (item.kind, item.flags, item.sequence, item.body)),
        Some((23, 16, 32, [7, 1, 0, 0].as_slice()))
    );
}
