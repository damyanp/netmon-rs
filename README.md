# netmon-rs

A native Windows network monitor. It continuously pings a set of targets and
shows live latency, packet loss, and history in a WinUI 3 desktop window.

Built in Rust with [windows-rs](https://github.com/microsoft/windows-rs) and its
`windows-reactor` declarative UI library. The dashboard renders on demand with
`SurfaceImageSource` + Direct2D (no animated swapchain).

## Features

- **Per-target cards** — name, IP, current latency, packet-loss %, and a
  sparkline of recent samples.
- **Latency chart** — all targets over a configurable time window, with red
  markers where pings were dropped. Samples from before a target existed show as
  gaps, not loss.
- **Settings pane** — adjust the ping interval and display window, clear history,
  and add/edit/reorder/remove targets.
- **Target editor** — full-detail form for each target. Optionally pin a target
  to a MAC address so it self-heals to the right IP via the ARP table, with a
  **Resolve** button that looks up the MAC for the entered host.
- **Clean startup** — history is cleared on launch, so you never see stale
  pre-run loss.

Targets, ping interval, and window size persist to `settings.json` next to the
executable. A fresh install starts with your detected gateway plus two
well-known internet endpoints.

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

## License

See [LICENSE](LICENSE).
