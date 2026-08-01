# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **`Remo.invoke` reachable from real Chrome DevTools' Console panel.** No CLI or MCP server
  needed: type `remo` in the Console to see every registered capability (grouped by namespace,
  self-describing), and call one directly with `remo.<dotted.name>({...})` — a capability's own
  dots (`grid.tab.select`, already Remo's naming convention) become real object nesting, not a
  separate `invoke(name, args)` indirection. Real, schema-driven Tab-completion works too
  (`remo.` suggests actual registered names). A call attempted while DevTools is only rendering
  a live preview (before Enter is pressed) is refused, matching how a real JS engine refuses
  anything with a possible side effect in that situation — a capability like "delete this key"
  can't fire mid-keystroke.

### Removed

- **The `__view_tree`/`__screenshot` built-in capabilities, `remo tree`, and `remo-mcp`'s
  `get_view_tree` tool.** Both capabilities duplicated what real CDP already does better:
  `DOM.getDocument` backs the actual, live, inspectable Elements panel, and
  `Page.captureScreenshot` is what `remo screenshot`/`remo-mcp` already called directly. Use
  `chrome://inspect`/a `devtools://` URL for the view hierarchy instead of `remo tree`.

### Changed

- **Rewritten onto real Chrome DevTools Protocol.** `remo-sdk`'s embedded server now speaks CDP
  (HTTP discovery + WebSocket, `Page`/`DOM`/`CSS`/`Overlay` domains, plus a custom `Remo.*`
  domain for `Remo.invoke`/`Remo.listCapabilities`) instead of a bespoke length-prefixed RPC
  protocol. `chrome://inspect`, `remo` (rewritten as a thin CDP client), the new `remo-mcp`
  agent-facing companion server, or any other CDP client can all drive/inspect the app directly.
  See `SPEC.md` §13 for the full rationale, the Track A/B compatibility split, and documented
  non-goals.
- **`remo` CLI commands**: `list` renamed to `capabilities`; `screenshot` now calls the standard
  `Page.captureScreenshot` method directly. `remo call`'s printed result is no longer wrapped in
  a `.data` envelope.

### Removed

- **`remo-desktop`** (device manager, RPC client, web dashboard, fMP4 muxer) and **`remo-daemon`**
  (connection pooling, HTTP/WebSocket API, event bus) — `chrome://inspect`'s own
  screencast/screenshot panels and a daemon-free, direct-dial CLI made both unnecessary.
- **`remo mirror`/`dashboard`/`start`/`stop`/`status`/`watch`** CLI commands, along with the
  crates above. The high-fidelity H.264 mirror is planned to return as a `Remo.startMirror`
  CDP extension (tracked separately, not abandoned); `Remo.capabilitiesChanged` (which `watch`
  relied on) isn't wired up on the CDP path yet.

## [0.4.4] - 2026-04-19

### Added

- **Tart-first contributor workflow**: Reproducible Tart VM setup for contributors

### Changed

- **Example app**: Default to local SDK source for easier in-tree development
- **Docs**: Repositioned Remo around capabilities

## [0.4.3] - 2026-03-29

### Added

- **Claude Code skills**: Skills for Remo-powered iOS development workflows (`remo-setup`, `remo` daily skill)

### Fixed

- **Bonjour simulator discovery**: Improved reliability of mDNS-based simulator connections
- **CLI release distribution**: Addressed code review issues for packaging and install scripts

### Changed

- **Build artifacts**: Ignore local Xcode and SPM caches in version control

## [0.4.2] - 2026-03-29

### Added

- **`remo-daemon` crate**: Background daemon with ConnectionPool (auto-connect, keepalive, exponential backoff reconnection), EventBus (ring buffer + broadcast), and full HTTP/WebSocket API for device management, capability invocation, event streaming, and screenshots
- **Daemon CLI commands**: `remo start`, `remo stop`, `remo status` for daemon lifecycle management
- **Daemon fallback**: All existing CLI commands (`call`, `list`, `screenshot`, etc.) route through daemon HTTP API when available, falling back to direct TCP connections
- **Capability change events**: `capabilities_changed` events emitted by the SDK when capabilities are registered or unregistered, pushed to connected clients in real time
- **`Remo.unregister()` API**: Dynamic capability removal through the full stack — Rust FFI (`remo_unregister_capability`), Swift (`Remo.unregister(_:)`), and Objective-C (`[RMRemo unregisterCapability:]`)
- **`remo_get_port()` FFI**: Query the actual port the server is listening on
- **Per-view capability lifecycle**: Example app demonstrates registering capabilities in `.onAppear` and unregistering in `.onDisappear`, so capabilities reflect the active UI state
- **Dashboard `capabilities_changed` handling**: Dashboard refreshes its capability panel when it receives a `capabilities_changed` WebSocket event
- **Web dashboard** (`remo dashboard`): Browser-based UI with multi-device discovery, device selector, video streaming, screenshot capture, capabilities panel, and interactive terminal
- **Video streaming**: H.264 screen capture via RPScreenRecorder + VideoToolbox encoder on iOS, fMP4 muxer + MSE playback on desktop
- **StreamFrame wire protocol**: Type 0x02 binary frame for real-time video/audio data with flags and timestamps
- **Multi-device support in dashboard**: Auto-discovery via USB (usbmuxd) and Bonjour (mDNS), connect/disconnect via REST API
- **`remo mirror` CLI command**: Live screen mirror with `--web` option for browser playback and `--save` for MP4 recording
- **Bonjour multi-address fallback**: Tries all resolved addresses for reliable simulator connections
- **`remo-objc` screen capture**: `RPScreenRecorder` integration for capturing `CMSampleBuffer` frames
- **`remo-objc` video encoder**: VideoToolbox H.264 hardware encoder with AVCC-to-Annex-B NAL conversion
- **`remo-desktop` fMP4 muxer**: Generates ISO BMFF init segments and moof/mdat fragments for MSE playback
- **`remo-desktop` stream receiver**: Ordered frame collection from broadcast channel
- **`remo-desktop` web player**: Standalone MSE-based video player page
- **CLI release artifacts**: GitHub Release now includes macOS `arm64` and `x86_64` tarballs for `remo`
- **First-party Homebrew tap flow**: Release automation can update a dedicated tap repo from the same tagged release
- **CLI install scripts**: `install-remo.sh` and `uninstall-remo.sh` provide non-Homebrew install and removal flows

### Changed

- `remo dashboard` no longer requires `--addr`; discovers devices automatically
- `remo-protocol` max frame size increased from 16 MiB to 64 MiB for video frames
- `remo-protocol` codec handles JSON (0x00), Binary (0x01), and Stream (0x02) frame types
- `remo-desktop` device manager Bonjour connection tries all resolved addresses instead of only the first
- CLI installation docs now prioritize Homebrew and release artifacts before source installs

## [0.2.0] - 2026-03-21

### Added

- Bonjour/mDNS auto-discovery for simulators and Wi-Fi devices
- Multi-simulator support with auto-assigned ports
- Built-in introspection: view tree, screenshot, device info, app info
- Binary frame protocol (Type 0x01) for efficient screenshot transfer
- Debug-only SDK (`#if DEBUG` — compiles to no-ops in Release builds)
- GCD main-thread dispatch for safe UIKit access from Rust
- CI/CD pipeline (check, fmt, clippy, test, iOS build, Swift integration)
- Automated release pipeline (XCFramework → GitHub Release → SPM distribution)
- CLI commands: `tree`, `screenshot`, `info`

## [0.1.0] - 2026-03-20

### Added

- Full RPC round-trip: CLI → TCP → iOS SDK → capability handler → response
- `remo-protocol`: Message types + length-prefixed JSON framing codec
- `remo-transport`: Bidirectional TCP connection + async listener
- `remo-usbmuxd`: macOS usbmuxd client for USB device discovery + tunneling
- `remo-sdk`: iOS embedded TCP server + capability registry + C FFI
- `remo-objc`: ObjC runtime bridge via `objc2`
- `remo-desktop`: Device manager + RPC client
- `remo-cli`: `devices`, `call`, `list`, `watch` commands
- `RemoSwift`: Swift wrapper with zero-config auto-start
- `RemoExample`: Demo app with counter, items, activity log, settings
