use std::os::fd::{AsRawFd as _, OwnedFd};
use std::time::{Duration, Instant};

use nix::sys::socket::{
    AddressFamily, NetlinkAddr, SockFlag, SockProtocol, SockType, bind, recvfrom, sendto,
    setsockopt, sockopt,
};
use nix::sys::time::TimeVal;

use super::{WirelessInfo, packet};

const GENL_ID_CTRL: u16 = 16;
const CTRL_CMD_GETFAMILY: u8 = 3;
const CTRL_ATTR_FAMILY_NAME: u16 = 2;
const NL80211_ATTR_IFINDEX: u16 = 3;
const NL80211_CMD_GET_INTERFACE: u8 = 5;
const NL80211_CMD_GET_STATION: u8 = 17;
const NL80211_IFTYPE_STATION: u32 = 2;
const NL80211_IFTYPE_P2P_CLIENT: u32 = 8;
const NLMSG_ERROR: u16 = 2;
const NLMSG_DONE: u16 = 3;
const NLM_F_REQUEST: u16 = 1;
const NLM_F_DUMP: u16 = 0x300;
const NLM_F_DUMP_INTR: u16 = 0x10;
const SOCKET_TIMEOUT: TimeVal = TimeVal::new(0, 25_000);
const QUERY_BUDGET: Duration = Duration::from_millis(60);

pub(super) fn sample(index: u32) -> WirelessInfo {
    query_radio(index).unwrap_or_default()
}

fn query_radio(index: u32) -> Option<WirelessInfo> {
    let mut socket = QuerySocket::open()?;
    let family = socket.query(
        GENL_ID_CTRL,
        CTRL_CMD_GETFAMILY,
        (CTRL_ATTR_FAMILY_NAME, b"nl80211\0"),
        ReplyMode::Single,
        packet::family,
    )?;
    let attributes = (NL80211_ATTR_IFINDEX, index.to_ne_bytes());
    let attr = (attributes.0, attributes.1.as_slice());
    let radio = socket.query(
        family,
        NL80211_CMD_GET_INTERFACE,
        attr,
        ReplyMode::Single,
        packet::interface,
    )?;
    let mut info = if matches!(
        radio.kind,
        NL80211_IFTYPE_STATION | NL80211_IFTYPE_P2P_CLIENT
    ) {
        socket
            .query(
                family,
                NL80211_CMD_GET_STATION,
                attr,
                ReplyMode::Dump,
                packet::station,
            )
            .unwrap_or_default()
    } else {
        WirelessInfo::default()
    };
    info.ssid = radio.ssid;
    Some(info)
}

#[derive(Clone, Copy)]
enum ReplyMode {
    Single,
    Dump,
}

struct QuerySocket {
    fd: OwnedFd,
    sequence: u32,
    started: Instant,
}

impl QuerySocket {
    fn open() -> Option<Self> {
        let fd = nix::sys::socket::socket(
            AddressFamily::Netlink,
            SockType::Raw,
            SockFlag::SOCK_CLOEXEC,
            SockProtocol::NetlinkGeneric,
        )
        .ok()?;
        let started = Instant::now();
        bind(fd.as_raw_fd(), &NetlinkAddr::new(0, 0)).ok()?;
        setsockopt(&fd, sockopt::ReceiveTimeout, &SOCKET_TIMEOUT).ok()?;
        setsockopt(&fd, sockopt::SendTimeout, &SOCKET_TIMEOUT).ok()?;
        Some(Self {
            fd,
            sequence: 0,
            started,
        })
    }

    fn query<T>(
        &mut self,
        family: u16,
        command: u8,
        attr: (u16, &[u8]),
        mode: ReplyMode,
        parse: fn(&[u8]) -> Option<T>,
    ) -> Option<T> {
        self.sequence = self.sequence.checked_add(1)?;
        let flags = match mode {
            ReplyMode::Single => NLM_F_REQUEST,
            ReplyMode::Dump => NLM_F_REQUEST | NLM_F_DUMP,
        };
        let frame = request(family, command, self.sequence, flags, attr)?;
        let raw = self.fd.as_raw_fd();
        if self.started.elapsed() >= QUERY_BUDGET
            || sendto(
                raw,
                &frame,
                &NetlinkAddr::new(0, 0),
                nix::sys::socket::MsgFlags::empty(),
            )
            .ok()?
                != frame.len()
        {
            return None;
        }

        let mut result = None;
        let mut buffer = [0; 8192];
        for _ in 0..8 {
            if self.started.elapsed() >= QUERY_BUDGET {
                return None;
            }
            let (length, source) = recvfrom::<NetlinkAddr>(raw, &mut buffer).ok()?;
            if source?.pid() != 0 || length == buffer.len() {
                return None;
            }
            let mut datagram = buffer.get(..length)?;
            while !datagram.is_empty() {
                let message = packet::message(&mut datagram)?;
                if message.sequence != self.sequence {
                    continue;
                }
                if message.flags & NLM_F_DUMP_INTR != 0 || message.kind == NLMSG_ERROR {
                    return None;
                }
                if message.kind == NLMSG_DONE {
                    return (message.body.get(..4)? == [0; 4]).then_some(result)?;
                }
                if message.kind != family || result.is_some() {
                    return None;
                }
                result = Some(parse(message.body.get(4..)?)?);
                if matches!(mode, ReplyMode::Single) {
                    return result;
                }
            }
        }
        None
    }
}

fn request(
    family: u16,
    command: u8,
    sequence: u32,
    flags: u16,
    attr: (u16, &[u8]),
) -> Option<Vec<u8>> {
    let (kind, value) = attr;
    let size = 4_usize.checked_add(value.len())?;
    let padded = size.checked_add(3)? & !3;
    let length = 20_usize.checked_add(padded)?;
    let mut header = Vec::with_capacity(length);
    header.extend(u32::try_from(length).ok()?.to_ne_bytes());
    header.extend(family.to_ne_bytes());
    header.extend(flags.to_ne_bytes());
    header.extend(sequence.to_ne_bytes());
    header.extend(0_u32.to_ne_bytes());
    header.extend([command, 1, 0, 0]);
    header.extend(u16::try_from(size).ok()?.to_ne_bytes());
    header.extend(kind.to_ne_bytes());
    header.extend(value);
    header.resize(length, 0);
    Some(header)
}
