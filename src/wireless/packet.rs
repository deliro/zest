use super::WirelessInfo;

// Stable Linux UAPI identifiers from genetlink.h and nl80211.h.
const CTRL_ATTR_FAMILY_ID: u16 = 1;
const ATTR_MAC: u16 = 6;
const ATTR_IFTYPE: u16 = 5;
const ATTR_STA_INFO: u16 = 21;
const ATTR_SSID: u16 = 52;
const STA_SIGNAL: u16 = 7;
const STA_TX_BITRATE: u16 = 8;
const STA_RX_BITRATE: u16 = 14;
const RATE_BITRATE: u16 = 1;
const RATE_BITRATE32: u16 = 5;

const NLA_TYPE_MASK: u16 = 0x3fff;

pub(super) struct Message<'a> {
    pub kind: u16,
    pub flags: u16,
    pub sequence: u32,
    pub body: &'a [u8],
}

pub(super) fn message<'a>(input: &mut &'a [u8]) -> Option<Message<'a>> {
    let size = usize::try_from(u32::from_ne_bytes(bytes(input, 0)?)).ok()?;
    let kind = u16::from_ne_bytes(bytes(input, 4)?);
    let flags = u16::from_ne_bytes(bytes(input, 6)?);
    let sequence = u32::from_ne_bytes(bytes(input, 8)?);
    let body = take_aligned(input, size)?.get(16..)?;
    Some(Message {
        kind,
        flags,
        sequence,
        body,
    })
}

fn take_aligned<'a>(input: &mut &'a [u8], size: usize) -> Option<&'a [u8]> {
    let body = input.get(..size)?;
    let padded = size.checked_add(3)? & !3;
    *input = input
        .get(padded..)
        .or_else(|| (size == input.len()).then_some(&[][..]))?;
    Some(body)
}

fn bytes<const N: usize>(input: &[u8], offset: usize) -> Option<[u8; N]> {
    input.get(offset..offset.checked_add(N)?)?.try_into().ok()
}

fn attribute(mut input: &[u8], wanted: u16) -> Option<&[u8]> {
    while !input.is_empty() {
        let size = usize::from(u16::from_ne_bytes(bytes(input, 0)?));
        let kind = u16::from_ne_bytes(bytes(input, 2)?) & NLA_TYPE_MASK;
        let value = take_aligned(&mut input, size)?.get(4..)?;
        if kind == wanted {
            return Some(value);
        }
    }
    None
}

pub(super) fn family(input: &[u8]) -> Option<u16> {
    let id = attribute(input, CTRL_ATTR_FAMILY_ID)?;
    let id = u16::from_ne_bytes(id.try_into().ok()?);
    (id > 16).then_some(id)
}

pub(super) struct Radio {
    pub ssid: Option<Vec<u8>>,
    pub kind: u32,
}

pub(super) fn interface(input: &[u8]) -> Option<Radio> {
    let kind = u32::from_ne_bytes(attribute(input, ATTR_IFTYPE)?.try_into().ok()?);
    let ssid = attribute(input, ATTR_SSID)
        .filter(|ssid| (1..=32).contains(&ssid.len()))
        .map(<[u8]>::to_vec);
    Some(Radio { ssid, kind })
}

pub(super) fn station(input: &[u8]) -> Option<WirelessInfo> {
    let peer = attribute(input, ATTR_MAC).and_then(|data| data.try_into().ok());
    let info = attribute(input, ATTR_STA_INFO)?;
    let signal = attribute(info, STA_SIGNAL)
        .and_then(|data| data.try_into().ok())
        .map(|data| i32::from(i8::from_ne_bytes(data)))
        .filter(|value| (-127..=-1).contains(value));
    Some(WirelessInfo {
        ssid: None,
        peer,
        signal,
        rx_phy: attribute(info, STA_RX_BITRATE).and_then(bitrate),
        tx_phy: attribute(info, STA_TX_BITRATE).and_then(bitrate),
    })
}

fn bitrate(input: &[u8]) -> Option<u32> {
    attribute(input, RATE_BITRATE32)
        .and_then(|data| data.try_into().ok())
        .map(u32::from_ne_bytes)
        .or_else(|| {
            attribute(input, RATE_BITRATE)
                .and_then(|data| data.try_into().ok())
                .map(u16::from_ne_bytes)
                .map(u32::from)
        })
        .filter(|value| *value > 0)
}

#[cfg(test)]
mod tests;
