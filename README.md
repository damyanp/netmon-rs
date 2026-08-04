# netmon-rs

A native Windows network monitor. It continuously pings a set of targets and
shows live latency, packet loss, and history in a WinUI 3 desktop window.

Built in Rust with [windows-rs](https://github.com/microsoft/windows-rs), using
`windows-reactor` and `windows-canvas`. The dashboard renders on demand with
`CanvasImageSource` + Direct2D (no animated swapchain).

## Features

- **Per-target cards** — name, IP, current latency, packet-loss %, and a
  sparkline of recent samples.
- **Device network details** — active IPv4 addresses, subnet prefixes, gateways,
  DHCP servers, and current lease times.
- **Latency chart** — all targets over a configurable time window, with red
  markers where pings were dropped. Samples from before a target existed show as
  gaps, not loss.
- **Settings pane** — adjust the ping interval and display window, clear history,
  configure packet-loss notifications, test Windows notification delivery, and
  add/edit/reorder/remove targets.
- **Packet-loss alerts** — after 10 samples, a native Windows notification fires
  once when any configured target rises above the threshold (15% by default).
  The alert uses the selected display window and re-arms after recovery.
- **Target editor** — full-detail form for each target. Optionally pin a target
  to a MAC address so it self-heals to the right IP via the ARP table, with a
  **Resolve** button that looks up the MAC for the entered host.
- **Clean startup** — history is cleared on launch, so you never see stale
  pre-run loss.

Targets, ping interval, window size, and the alert threshold persist to
`settings.json` next to the executable. Notification presentation is subject to
Windows notification permissions and Focus Assist. A fresh install starts with
your detected gateway plus two well-known internet endpoints.

## Requirements

- Windows 10/11
- Rust (edition 2024, see `Cargo.toml` for the pinned toolchain)
- The [Windows App Runtime](https://learn.microsoft.com/windows/apps/windows-app-sdk/downloads).
  The app is framework-dependent; if the runtime is missing it offers to install
  it on first launch.

## Build & run

```powershell
cargo run --release
```

The debug build works too (`cargo run`); the release profile builds with
`panic = "abort"`.

## Installer

Build a per-user Windows installer (no admin required) with
[Inno Setup](https://jrsoftware.org/isinfo.php):

```powershell
powershell -ExecutionPolicy Bypass -File installer\build-installer.ps1
```

The script reads the version from `Cargo.toml`, builds the release binary,
installs the Inno Setup compiler via `winget` if it's missing, and writes
`installer\Output\NetworkMonitor-Setup-<version>.exe`. Pass `-SkipBuild` to
reuse an existing `--release` build.

The installer deploys to `%LOCALAPPDATA%\Programs\Network Monitor`, adds Start
Menu (and optional desktop) shortcuts, and registers an uninstaller. Because the
app is framework-dependent, first launch offers to install the Windows App
Runtime if it isn't already present.

## License

See [LICENSE](LICENSE).
