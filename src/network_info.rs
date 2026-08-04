//! Local adapter information shown on the dashboard.

use std::os::windows::process::CommandExt;
use std::process::Command;

use serde::Deserialize;

const NO_WINDOW: u32 = 0x0800_0000;

#[derive(Clone, Default, PartialEq)]
pub struct DeviceNetworkInfo {
    pub adapters: Vec<AdapterInfo>,
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
}

impl DeviceNetworkInfo {
    pub fn summary(&self) -> String {
        if self.adapters.is_empty() {
            return "No active IPv4 network adapters detected".to_string();
        }

        self.adapters
            .iter()
            .map(AdapterInfo::summary)
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl AdapterInfo {
    fn summary(&self) -> String {
        let mut details = vec![
            format!("{}  {}/{}", self.name, self.ip_address, self.subnet_prefix),
            format!(
                "Gateway: {}",
                self.gateway.as_deref().unwrap_or("not reported")
            ),
        ];

        if self.dhcp_server.is_some()
            || self.lease_obtained.is_some()
            || self.lease_expires.is_some()
        {
            details.push(format!(
                "DHCP: {}",
                self.dhcp_server.as_deref().unwrap_or("enabled")
            ));
            details.push(format!(
                "Lease: {} to {}",
                self.lease_obtained.as_deref().unwrap_or("unknown"),
                self.lease_expires.as_deref().unwrap_or("unknown")
            ));
        } else {
            details.push("DHCP: not enabled".to_string());
        }

        details.join("\n")
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct PowerShellAdapter {
    interface_alias: String,
    ip_address: String,
    prefix_length: u32,
    #[serde(default, deserialize_with = "string_or_first")]
    default_gateway: Option<String>,
    #[serde(default, deserialize_with = "string_or_first")]
    dhcp_server: Option<String>,
    #[serde(default)]
    lease_obtained: Option<String>,
    #[serde(default)]
    lease_expires: Option<String>,
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
      }
    }
) | ConvertTo-Json -Compress
"#;

    let output = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .creation_flags(NO_WINDOW)
        .output();
    let Ok(output) = output else {
        return DeviceNetworkInfo::default();
    };
    if !output.status.success() {
        eprintln!(
            "failed to query local network information: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        return DeviceNetworkInfo::default();
    }

    let text = String::from_utf8_lossy(&output.stdout);
    let parsed = serde_json::from_str::<Vec<PowerShellAdapter>>(text.trim());
    match parsed {
        Ok(adapters) => DeviceNetworkInfo {
            adapters: adapters
                .into_iter()
                .map(|adapter| AdapterInfo {
                    name: adapter.interface_alias,
                    ip_address: adapter.ip_address,
                    subnet_prefix: adapter.prefix_length,
                    gateway: adapter.default_gateway,
                    dhcp_server: adapter.dhcp_server,
                    lease_obtained: adapter.lease_obtained,
                    lease_expires: adapter.lease_expires,
                })
                .collect(),
        },
        Err(error) => {
            eprintln!("failed to parse local network information: {error}");
            DeviceNetworkInfo::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::PowerShellAdapter;

    #[test]
    fn parses_adapter_information() {
        let adapters: Vec<PowerShellAdapter> = serde_json::from_str(
            r#"[{"InterfaceAlias":"Ethernet","IPAddress":"192.168.1.20","PrefixLength":24,"DefaultGateway":["192.168.1.1"],"DHCPServer":"192.168.1.1","LeaseObtained":"8/4/2026 1:00 PM","LeaseExpires":"8/5/2026 1:00 PM"}]"#,
        )
        .unwrap();

        assert_eq!(adapters[0].interface_alias, "Ethernet");
        assert_eq!(adapters[0].default_gateway.as_deref(), Some("192.168.1.1"));
        assert_eq!(adapters[0].dhcp_server.as_deref(), Some("192.168.1.1"));
    }
}
