//! Local adapter information shown on the dashboard.

use std::os::windows::process::CommandExt;
use std::process::Command;

use serde::Deserialize;

const NO_WINDOW: u32 = 0x0800_0000;

#[derive(Clone, PartialEq)]
pub enum DeviceNetworkInfo {
    Loading,
    Loaded(Vec<AdapterInfo>),
    Error(String),
}

fn format_remaining(remaining_ms: i64) -> String {
    if remaining_ms <= 0 {
        return "expired".to_string();
    }

    let total_minutes = (remaining_ms + 59_999) / 60_000;
    let days = total_minutes / (24 * 60);
    let hours = total_minutes % (24 * 60) / 60;
    let minutes = total_minutes % 60;
    if days > 0 {
        format!("{days}d {hours}h {minutes}m")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{minutes}m")
    }
}

impl Default for DeviceNetworkInfo {
    fn default() -> Self {
        Self::Loading
    }
}

impl DeviceNetworkInfo {
    pub fn adapters(adapters: Vec<AdapterInfo>) -> Self {
        Self::Loaded(adapters)
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self::Error(message.into())
    }

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
    pub lease_obtained: Option<String>,
    pub lease_expires: Option<String>,
    pub lease_expires_ms: Option<i64>,
}

impl AdapterInfo {
    fn connection_details(&self, now_ms: i64) -> String {
        let gateway = self.gateway.as_deref().unwrap_or("not reported");
        match (
            self.dhcp_server.as_deref(),
            self.lease_obtained.as_deref(),
            self.lease_expires.as_deref(),
        ) {
            (Some(server), Some(obtained), Some(expires)) => {
                let remaining = self
                    .lease_expires_ms
                    .map(|expires_ms| format_remaining(expires_ms - now_ms))
                    .unwrap_or_else(|| "unknown".to_string());
                format!(
                    "Gateway: {gateway}\nDHCP: {server}\nLease: {obtained}\nto {expires}\nExpires in: {remaining}"
                )
            }
            (Some(server), _, _) => format!("Gateway: {gateway}\nDHCP: {server}"),
            _ => format!("Gateway: {gateway}\nDHCP: not enabled"),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct PowerShellAdapter {
    interface_alias: String,
    #[serde(rename = "IPAddress")]
    ip_address: String,
    prefix_length: u32,
    #[serde(
        rename = "DefaultGateway",
        default,
        deserialize_with = "string_or_first"
    )]
    default_gateway: Option<String>,
    #[serde(rename = "DHCPServer", default, deserialize_with = "string_or_first")]
    dhcp_server: Option<String>,
    #[serde(rename = "LeaseObtained", default)]
    lease_obtained: Option<String>,
    #[serde(rename = "LeaseExpires", default)]
    lease_expires: Option<String>,
    #[serde(rename = "LeaseExpiresMs", default)]
    lease_expires_ms: Option<i64>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum StringOrList {
    String(String),
    List(Vec<String>),
}

fn string_or_first<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<StringOrList>::deserialize(deserializer)?
        .and_then(|value| match value {
            StringOrList::String(value) => Some(value),
            StringOrList::List(values) => values.into_iter().next(),
        })
        .filter(|value| !value.is_empty()))
}

pub fn query() -> DeviceNetworkInfo {
    let script = r#"
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
@(
  Get-NetIPConfiguration |
    Where-Object { $_.NetAdapter.Status -eq 'Up' -and $_.IPv4Address } |
    ForEach-Object {
      $cfg = $_
      $ip = @($cfg.IPv4Address)[0]
      $dhcp = Get-CimInstance Win32_NetworkAdapterConfiguration -Filter "InterfaceIndex=$($cfg.InterfaceIndex)"
      [pscustomobject]@{
        InterfaceAlias = $cfg.InterfaceAlias
        IPAddress = $ip.IPAddress
        PrefixLength = $ip.PrefixLength
        DefaultGateway = @($cfg.IPv4DefaultGateway.NextHop)
        DHCPServer = $dhcp.DHCPServer
        LeaseObtained = if ($dhcp.DHCPLeaseObtained) { $dhcp.DHCPLeaseObtained.ToString('g') } else { $null }
        LeaseExpires = if ($dhcp.DHCPLeaseExpires) { $dhcp.DHCPLeaseExpires.ToString('g') } else { $null }
        LeaseExpiresMs = if ($dhcp.DHCPLeaseExpires) { [DateTimeOffset]$dhcp.DHCPLeaseExpires | ForEach-Object { $_.ToUnixTimeMilliseconds() } } else { $null }
      }
    }
) | ConvertTo-Json -Compress
"#;

    let output = Command::new("powershell.exe")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-OutputFormat",
            "Text",
            "-Command",
            script,
        ])
        .creation_flags(NO_WINDOW)
        .output();
    let Ok(output) = output else {
        return DeviceNetworkInfo::error("PowerShell could not be started");
    };
    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr);
        let error = error.trim();
        return DeviceNetworkInfo::error(if error.is_empty() {
            "the Windows network query failed"
        } else {
            error
        });
    }

    let text = String::from_utf8_lossy(&output.stdout);
    let text = text.trim();
    if text.is_empty() {
        return DeviceNetworkInfo::Loaded(Vec::new());
    }
    let parsed = parse_adapters(text);
    match parsed {
        Ok(adapters) => DeviceNetworkInfo::adapters(
            adapters
                .into_iter()
                .map(|adapter| AdapterInfo {
                    name: adapter.interface_alias,
                    ip_address: adapter.ip_address,
                    subnet_prefix: adapter.prefix_length,
                    gateway: adapter.default_gateway,
                    dhcp_server: adapter.dhcp_server,
                    lease_obtained: adapter.lease_obtained,
                    lease_expires: adapter.lease_expires,
                    lease_expires_ms: adapter.lease_expires_ms,
                })
                .collect(),
        ),
        Err(error) => DeviceNetworkInfo::error(format!("invalid query response: {error}")),
    }
}

fn parse_adapters(text: &str) -> serde_json::Result<Vec<PowerShellAdapter>> {
    if text.trim_start().starts_with('[') {
        serde_json::from_str(text)
    } else {
        serde_json::from_str(text).map(|adapter| vec![adapter])
    }
}

#[cfg(test)]
mod tests {
    use super::{format_remaining, parse_adapters};

    #[test]
    fn parses_adapter_information() {
        let adapters = parse_adapters(
            r#"[{"InterfaceAlias":"Ethernet","IPAddress":"192.168.1.20","PrefixLength":24,"DefaultGateway":["192.168.1.1"],"DHCPServer":"192.168.1.1","LeaseObtained":"8/4/2026 1:00 PM","LeaseExpires":"8/5/2026 1:00 PM"}]"#,
        )
        .unwrap();

        assert_eq!(adapters[0].interface_alias, "Ethernet");
        assert_eq!(adapters[0].default_gateway.as_deref(), Some("192.168.1.1"));
        assert_eq!(adapters[0].dhcp_server.as_deref(), Some("192.168.1.1"));
    }

    #[test]
    fn parses_single_adapter_object() {
        let adapters = parse_adapters(
            r#"{"InterfaceAlias":"Wi-Fi","IPAddress":"10.0.0.2","PrefixLength":24,"DefaultGateway":"10.0.0.1","DHCPServer":"10.0.0.1","LeaseObtained":null,"LeaseExpires":null}"#,
        )
        .unwrap();

        assert_eq!(adapters.len(), 1);
        assert_eq!(adapters[0].interface_alias, "Wi-Fi");
    }

    #[test]
    fn formats_lease_countdown() {
        assert_eq!(format_remaining(0), "expired");
        assert_eq!(format_remaining(60_000), "1m");
        assert_eq!(format_remaining(3_660_000), "1h 1m");
        assert_eq!(format_remaining(90_060_000), "1d 1h 1m");
    }
}
