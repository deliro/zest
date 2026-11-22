#[derive(Default)]
pub(super) struct WirelessInfo {
    pub ssid: Option<Vec<u8>>,
    pub peer: Option<[u8; 6]>,
    pub signal: Option<i32>,
    pub rx_phy: Option<u32>,
    pub tx_phy: Option<u32>,
}

#[cfg(any(target_os = "linux", test))]
mod packet;

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "linux")]
pub(super) fn sample(index: u32) -> WirelessInfo {
    linux::sample(index)
}

#[cfg(not(target_os = "linux"))]
pub(super) fn sample(_index: u32) -> WirelessInfo {
    WirelessInfo::default()
}
