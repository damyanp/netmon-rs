//! Local adapter information shown on the dashboard.

use std::collections::HashMap;
use std::ffi::CStr;
use std::net::Ipv4Addr;

use windows::ifdef::IfOperStatusUp;
use windows::iphlpapi::{GetAdaptersAddresses, GetAdaptersInfo};
use windows::iptypes::{
    GAA_FLAG_INCLUDE_GATEWAYS, IP_ADAPTER_ADDRESSES_LH, IP_ADAPTER_INFO, IP_ADDR_STRING,
};
use windows::minwinbase::SYSTEMTIME;
use windows::minwindef::FILETIME;
use windows::timezoneapi::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
use windows::winerror::{ERROR_BUFFER_OVERFLOW, NO_ERROR};
use windows::ws2::{AF_INET, SOCKADDR_IN, SOCKET_ADDRESS};

#[derive(Clone, Default, PartialEq)]
pub enum DeviceNetworkInfo {
    #[default]
    Loading,
    Loaded(Vec<AdapterInfo>),
    Error(String),
}

impl DeviceNetworkInfo {
    pub fn primary_address(&self) -> String {
        match self {
            Self::Loading => "Loading...".to_string(),
            Self::Loaded(adapters) => adapters
                .first()
                .map(|adapter| format!("{}/{}", adapter.ip_address, adapter.subnet_prefix))
                .unwrap_or_else(|| "No IPv4 address".to_string()),
            Self::Error(_) => "Unavailable".to_string(),
        }
    }

    pub fn adapter_name(&self) -> String {
        match self {
            Self::Loaded(adapters) => adapters
                .first()
                .map(|adapter| adapter.name.clone())
                .unwrap_or_default(),
            _ => String::new(),
        }
    }

    pub fn connection_details(&self, now_ms: i64) -> String {
        match self {
            Self::Loading => "Loading network information...".to_string(),
            Self::Loaded(adapters) => adapters
                .first()
                .map(|adapter| adapter.connection_details(now_ms))
                .unwrap_or_else(|| "No active adapter detected".to_string()),
            Self::Error(message) => format!("Network information unavailable: {message}"),
        }
    }
}

#[derive(Clone, PartialEq)]
pub struct AdapterInfo {
    pub name: String,
    pub ip_address: String,
    pub subnet_prefix: u32,
    pub gateway: Option<String>,
    pub dhcp_server: Option<String>,
    pub lease_obtained_ms: Option<i64>,
    pub lease_expires_ms: Option<i64>,
}

impl AdapterInfo {
    fn connection_details(&self, now_ms: i64) -> String {
        let gateway = self.gateway.as_deref().unwrap_or("not reported");
        match self.dhcp_server.as_deref() {
            Some(server) => {
                let obtained = self
                    .lease_obtained_ms
                    .map(format_timestamp)
                    .unwrap_or_else(|| "unknown".to_string());
                let expires = self
                    .lease_expires_ms
                    .map(format_timestamp)
                    .unwrap_or_else(|| "unknown".to_string());
                let remaining = self
                    .lease_expires_ms
                    .map(|expires_ms| format_remaining(expires_ms - now_ms))
                    .unwrap_or_else(|| "unknown".to_string());
                format!(
                    "Gateway: {gateway}\nDHCP: {server}\nLease: {obtained}\nto {expires}\nExpires in: {remaining}"
                )
            }
            None => format!("Gateway: {gateway}\nDHCP: not enabled"),
        }
    }
}

#[derive(Default)]
struct DhcpInfo {
    server: Option<String>,
    obtained_ms: Option<i64>,
    expires_ms: Option<i64>,
}

pub fn query() -> DeviceNetworkInfo {
    match query_native() {
        Ok(adapters) => DeviceNetworkInfo::Loaded(adapters),
        Err(error) => DeviceNetworkInfo::Error(error),
    }
}

fn query_native() -> Result<Vec<AdapterInfo>, String> {
    let dhcp = query_dhcp_info()?;
    let buffer = get_adapters_addresses()?;
    let mut adapters = Vec::new();

    // The buffer owns every node and nested pointer for the duration of this walk.
    unsafe {
        let mut adapter = buffer.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
        while let Some(current) = adapter.as_ref() {
            if current.OperStatus != IfOperStatusUp || current.FirstGatewayAddress.is_null() {
                adapter = current.Next;
                continue;
            }
            let mut unicast = current.FirstUnicastAddress;
            while let Some(address) = unicast.as_ref() {
                if let Some(ip_address) = socket_ipv4(&address.Address) {
                    let details = dhcp.get(&ip_address);
                    adapters.push(AdapterInfo {
                        name: wide_string(current.FriendlyName),
                        ip_address,
                        subnet_prefix: address.OnLinkPrefixLength as u32,
                        gateway: current
                            .FirstGatewayAddress
                            .as_ref()
                            .and_then(|gateway| socket_ipv4(&gateway.Address)),
                        dhcp_server: details.and_then(|info| info.server.clone()),
                        lease_obtained_ms: details.and_then(|info| info.obtained_ms),
                        lease_expires_ms: details.and_then(|info| info.expires_ms),
                    });
                    break;
                }
                unicast = address.Next;
            }
            adapter = current.Next;
        }
    }

    Ok(adapters)
}

fn get_adapters_addresses() -> Result<Vec<u8>, String> {
    let mut size = 15_000u32;
    loop {
        let mut buffer = vec![0u8; size as usize];
        let result = unsafe {
            GetAdaptersAddresses(
                AF_INET,
                GAA_FLAG_INCLUDE_GATEWAYS,
                None,
                Some(buffer.as_mut_ptr().cast()),
                &mut size,
            )
        };
        if result == NO_ERROR {
            return Ok(buffer);
        }
        if result != ERROR_BUFFER_OVERFLOW {
            return Err(format!("GetAdaptersAddresses failed with error {result}"));
        }
    }
}

fn query_dhcp_info() -> Result<HashMap<String, DhcpInfo>, String> {
    let mut size = 0u32;
    let first = unsafe { GetAdaptersInfo(None, &mut size) };
    if first != ERROR_BUFFER_OVERFLOW {
        return Err(format!("GetAdaptersInfo sizing failed with error {first}"));
    }

    let mut buffer = vec![0u8; size as usize];
    let result = unsafe {
        GetAdaptersInfo(
            Some(buffer.as_mut_ptr().cast::<IP_ADAPTER_INFO>()),
            &mut size,
        )
    };
    if result != NO_ERROR {
        return Err(format!("GetAdaptersInfo failed with error {result}"));
    }

    let mut result = HashMap::new();
    unsafe {
        let mut adapter = buffer.as_ptr() as *const IP_ADAPTER_INFO;
        while let Some(current) = adapter.as_ref() {
            let details = if current.DhcpEnabled != 0 {
                DhcpInfo {
                    server: ip_addr_string(&current.DhcpServer),
                    obtained_ms: unix_seconds_to_ms(current.LeaseObtained.0),
                    expires_ms: unix_seconds_to_ms(current.LeaseExpires.0),
                }
            } else {
                DhcpInfo::default()
            };
            if let Some(ip_address) = ip_addr_string(&current.IpAddressList) {
                result.insert(ip_address, details);
            }
            adapter = current.Next;
        }
    }
    Ok(result)
}

unsafe fn socket_ipv4(address: &SOCKET_ADDRESS) -> Option<String> {
    if address.lpSockaddr.is_null() || address.iSockaddrLength < size_of::<SOCKADDR_IN>() as i32 {
        return None;
    }
    let socket = unsafe { &*address.lpSockaddr.cast::<SOCKADDR_IN>() };
    if socket.sin_family.0 as u32 != AF_INET {
        return None;
    }
    let bytes = unsafe { socket.sin_addr.S_un.S_un_b };
    Some(Ipv4Addr::new(bytes.s_b1, bytes.s_b2, bytes.s_b3, bytes.s_b4).to_string())
}

unsafe fn wide_string(value: windows::winnt::PWCHAR) -> String {
    if value.is_null() {
        return String::new();
    }
    let mut len = 0;
    while unsafe { *value.add(len) } != 0 {
        len += 1;
    }
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(value, len) })
}

fn ip_addr_string(value: &IP_ADDR_STRING) -> Option<String> {
    let ptr = value.IpAddress.String.as_ptr().cast();
    let value = unsafe { CStr::from_ptr(ptr) }.to_string_lossy();
    (!value.is_empty() && value != "0.0.0.0").then(|| value.into_owned())
}

fn unix_seconds_to_ms(value: i64) -> Option<i64> {
    (value > 0).then(|| value.saturating_mul(1000))
}

fn format_remaining(remaining_ms: i64) -> String {
    if remaining_ms <= 0 {
        return "expired".to_string();
    }

    let total_seconds = (remaining_ms + 999) / 1000;
    let days = total_seconds / (24 * 60 * 60);
    let hours = total_seconds % (24 * 60 * 60) / (60 * 60);
    let minutes = total_seconds % (60 * 60) / 60;
    let seconds = total_seconds % 60;
    if days > 0 {
        format!("{days}d {hours}h {minutes}m {seconds}s")
    } else if hours > 0 {
        format!("{hours}h {minutes}m {seconds}s")
    } else if minutes > 0 {
        format!("{minutes}m {seconds}s")
    } else {
        format!("{seconds}s")
    }
}

fn format_timestamp(timestamp_ms: i64) -> String {
    const WINDOWS_EPOCH_OFFSET_SECS: i64 = 11_644_473_600;
    let ticks = (timestamp_ms / 1000 + WINDOWS_EPOCH_OFFSET_SECS) as u64 * 10_000_000;
    let file_time = FILETIME {
        dwLowDateTime: ticks as u32,
        dwHighDateTime: (ticks >> 32) as u32,
    };
    let mut utc = SYSTEMTIME::default();
    let mut local = SYSTEMTIME::default();
    if unsafe { FileTimeToSystemTime(&file_time, &mut utc) }.as_bool()
        && unsafe { SystemTimeToTzSpecificLocalTime(None, &utc, &mut local) }.as_bool()
    {
        let (hour, suffix) = match local.wHour {
            0 => (12, "AM"),
            1..=11 => (local.wHour, "AM"),
            12 => (12, "PM"),
            hour => (hour - 12, "PM"),
        };
        format!(
            "{}/{}/{} {}:{:02} {}",
            local.wMonth, local.wDay, local.wYear, hour, local.wMinute, suffix
        )
    } else {
        "unknown".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::format_remaining;

    #[test]
    fn formats_lease_countdown() {
        assert_eq!(format_remaining(0), "expired");
        assert_eq!(format_remaining(1_000), "1s");
        assert_eq!(format_remaining(61_000), "1m 1s");
        assert_eq!(format_remaining(3_661_000), "1h 1m 1s");
        assert_eq!(format_remaining(90_061_000), "1d 1h 1m 1s");
    }
}
