//! Finds the phone, in order of preference:
//! - USB tethering: Windows gets a USB network adapter whose IPv4 gateway is the phone itself;
//! - Wi-Fi / LAN: a broadcast on each local network, answered by the phone app.

use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::time::{Duration, Instant};

use audiorast_core::{DISCOVERY_PORT, DISCOVERY_REPLY, DISCOVERY_REQUEST};
use windows::Win32::NetworkManagement::IpHelper::{
    GAA_FLAG_INCLUDE_GATEWAYS, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST,
    GetAdaptersAddresses, IP_ADAPTER_ADDRESSES_LH,
};
use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;
use windows::Win32::Networking::WinSock::{AF_INET, SOCKADDR, SOCKADDR_IN};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Link {
    UsbTethering,
    Network,
}

impl Link {
    pub fn label(self) -> &'static str {
        match self {
            Link::UsbTethering => "USB tethering",
            Link::Network => "Wi-Fi",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Candidate {
    pub ip: Ipv4Addr,
    pub link: Link,
    pub adapter: String,
}

/// Adapter descriptions used by Android USB tethering (RNDIS or NCM drivers).
fn is_usb_tether(description: &str) -> bool {
    let d = description.to_ascii_lowercase();
    ["ndis", "ncm", "android", "usb", "samsung"].iter().any(|k| d.contains(k))
}

/// Gateways of active USB-tethering adapters.
pub fn candidates() -> Vec<Candidate> {
    adapters()
        .into_iter()
        .filter(|a| is_usb_tether(&a.description))
        .flat_map(|a| {
            a.gateways.into_iter().map(move |ip| Candidate { ip, link: Link::UsbTethering, adapter: a.description.clone() })
        })
        .collect()
}

/// Phones that answer a discovery broadcast on any local network within `wait`.
pub fn lan_phones(wait: Duration) -> Vec<Candidate> {
    let Ok(socket) = UdpSocket::bind(("0.0.0.0", 0)) else { return Vec::new() };
    let _ = socket.set_broadcast(true);
    let mut sent = false;
    for a in adapters() {
        for (ip, prefix) in &a.unicast {
            // Directed broadcast per network: 255.255.255.255 leaves through one adapter only.
            let mask = u32::MAX.checked_shl(32 - u32::from(*prefix)).unwrap_or(0);
            let broadcast = Ipv4Addr::from(u32::from(*ip) | !mask);
            sent |= socket.send_to(DISCOVERY_REQUEST, SocketAddrV4::new(broadcast, DISCOVERY_PORT)).is_ok();
        }
    }
    if !sent {
        return Vec::new();
    }

    let mut found: Vec<Candidate> = Vec::new();
    let deadline = Instant::now() + wait;
    let mut buf = [0u8; 64];
    while let Some(left) = deadline.checked_duration_since(Instant::now()).filter(|d| !d.is_zero()) {
        let _ = socket.set_read_timeout(Some(left));
        let Ok((n, std::net::SocketAddr::V4(from))) = socket.recv_from(&mut buf) else { break };
        if &buf[..n] == DISCOVERY_REPLY && !found.iter().any(|c| c.ip == *from.ip()) {
            found.push(Candidate { ip: *from.ip(), link: Link::Network, adapter: "network".into() });
        }
    }
    found
}

struct Adapter {
    description: String,
    gateways: Vec<Ipv4Addr>,
    /// IPv4 addresses with their prefix length.
    unicast: Vec<(Ipv4Addr, u8)>,
}

/// Active (up) IPv4 adapters, excluding loopback.
fn adapters() -> Vec<Adapter> {
    let flags = GAA_FLAG_INCLUDE_GATEWAYS | GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
    let mut size = 16 * 1024u32;
    let mut buf: Vec<u64> = Vec::new();
    for _ in 0..3 {
        // u64 elements keep the buffer aligned for IP_ADAPTER_ADDRESSES_LH.
        buf.resize((size as usize).div_ceil(8), 0);
        let ret = unsafe {
            GetAdaptersAddresses(AF_INET.0 as u32, flags, None, Some(buf.as_mut_ptr().cast()), &mut size)
        };
        match ret {
            0 => break,
            111 => continue, // ERROR_BUFFER_OVERFLOW: `size` now holds the required size.
            e => {
                log::warn!("GetAdaptersAddresses failed: {e}");
                return Vec::new();
            }
        }
    }

    const IF_TYPE_SOFTWARE_LOOPBACK: u32 = 24;
    let mut out = Vec::new();
    let mut adapter = buf.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
    // SAFETY: walking the linked lists GetAdaptersAddresses wrote into `buf`, which outlives the loop.
    unsafe {
        while let Some(a) = adapter.as_ref() {
            adapter = a.Next;
            if a.OperStatus != IfOperStatusUp || a.IfType == IF_TYPE_SOFTWARE_LOOPBACK {
                continue;
            }
            let mut info = Adapter {
                description: a.Description.to_string().unwrap_or_default(),
                gateways: Vec::new(),
                unicast: Vec::new(),
            };
            let mut gw = a.FirstGatewayAddress;
            while let Some(g) = gw.as_ref() {
                gw = g.Next;
                info.gateways.extend(ipv4(g.Address.lpSockaddr));
            }
            let mut uni = a.FirstUnicastAddress;
            while let Some(u) = uni.as_ref() {
                uni = u.Next;
                if let Some(ip) = ipv4(u.Address.lpSockaddr) {
                    info.unicast.push((ip, u.OnLinkPrefixLength));
                }
            }
            out.push(info);
        }
    }
    out
}

/// # Safety
/// `sa` must be null or point to a valid socket address.
unsafe fn ipv4(sa: *const SOCKADDR) -> Option<Ipv4Addr> {
    let sa = unsafe { sa.as_ref() }?;
    if sa.sa_family != AF_INET {
        return None;
    }
    let sin = unsafe { &*(sa as *const SOCKADDR as *const SOCKADDR_IN) };
    Some(Ipv4Addr::from(unsafe { sin.sin_addr.S_un.S_addr }.to_ne_bytes()))
}
