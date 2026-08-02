# Remo — Project Specification

> Version: 0.5.0-dev
> Last updated: 2026-03-27

## 1. Vision

Remo is infrastructure for **agentic iOS development**. It gives AI coding agents the eyes and hands they need to autonomously build, test, and debug iOS apps — closing the write → build → test → fix loop that has historically required human interaction on the iOS platform.

The core idea: iOS developers register **capabilities** (named Swift handlers) via the Remo SDK. AI agents discover real devices (USB) and simulators, invoke those capabilities to manipulate app state or trigger actions, then **verify the result** via screenshot or captured video. The agent doesn't guess whether its code change worked — it looks at the screen, just like a human developer would.

The SDK starts a TCP server inside the app. Real devices are discovered via USB (usbmuxd), simulators via Bonjour/mDNS. Any client — an AI agent, a CLI tool, or a web dashboard — can discover the device, call capabilities, inspect the view tree, capture screenshots, or record video (useful for reviewing animations). This enables a fully autonomous loop: agent writes code → triggers build → calls Remo to verify → iterates.

### 1.1 Core principles

- **Agent-first**: Every design decision prioritizes programmatic access. CLI commands, JSON-RPC protocol, auto-discovery, and zero-config introspection are all built so that AI agents can operate iOS apps without human assistance.
- **Rust-heavy**: All protocol, transport, server, registry, and ObjC bridge logic is written in Rust. Swift is a thin shell for UI and FFI callbacks.
- **Capability-oriented**: iOS apps don't expose a fixed API. Instead, developers register named handlers at runtime. Agents (or humans) discover and call them.
- **Multi-device**: A single macOS process can manage multiple iOS devices (real or simulated) simultaneously through independent connections.
- **Transport-agnostic**: USB (via usbmuxd) for real devices and localhost TCP for simulators converge on the same framed TCP protocol. Wi-Fi support is planned but not primary.

### 1.2 Key use cases

1. **Agentic iOS development loop**: The primary use case. An AI agent writes Swift code, triggers a build, then uses Remo to verify the result: invoke capabilities → screenshot or captured video → decide if the UI is correct → iterate. No human in the loop.
2. **Capability registration**: iOS developers register named Swift handlers (capabilities). Agents discover and invoke them at runtime — read CoreData, toggle feature flags, navigate routes, inject test data. The capability system is the bridge between agent intent and app behavior.
3. **Visual verification**: Screenshots and captured video let agents **see what the user sees** after every action. Screenshots for static UI checks; video recording for reviewing animations and transitions.
4. **Remote state manipulation**: Read/write values in an app's `@Observable` store from the Mac. Change a counter, swap a username, inject test data — the iOS UI reacts immediately.
5. **Remote navigation**: Push a route, pop a stack, switch tabs — all from a CLI command or agent call.
6. **Event streaming**: iOS pushes events (state changes, navigation events, logs) to macOS in real time.

---

## 2. Architecture

### 2.1 System layers

```
┌───────────────────────────────────────────┐
│  macOS — Rust binary                      │
│  ┌──────────┐ ┌───────────┐ ┌──────────┐ │
│  │ remo CLI │ │ Device    │ │ RPC      │ │
│  │ (clap)   │ │ Manager   │ │ Client   │ │
│  └────┬─────┘ └─────┬─────┘ └────┬─────┘ │
│       │      ┌───────┼───────┐    │       │
│       │      │ Bonjour│usbmuxd│    │       │
│       │      │ browse │tunnel │    │       │
│       └──────┴───────┴───┬───┴────┘       │
│              ┌───────────┴──┐              │
│              │  transport   │              │
│              └───────┬──────┘              │
└──────────────────────┼────────────────────┘
                       │ USB tunnel / Wi-Fi / localhost
┌──────────────────────┼────────────────────┐
│  iOS — Rust static lib + Swift shell      │
│              ┌───────┴──────┐              │
│              │  transport   │              │
│              └───────┬──────┘              │
│       ┌──────────────┼──────────────┐      │
│  ┌────┴─────┐ ┌──────┴──────┐ ┌────┴───┐  │
│  │ RPC      │ │ Capability  │ │ ObjC   │  │
│  │ Server   │ │ Registry    │ │ Bridge │  │
│  └──────────┘ └──────┬──────┘ └────────┘  │
│       ┌──────────────┼──────────────┐      │
│  ┌────┴───┐ ┌────────┴────┐ ┌──────┴───┐  │
│  │Bonjour │ │ Built-in    │ │ Custom   │  │
│  │advertise│ │ view_tree   │ │ handlers │  │
│  │        │ │ screenshot  │ │ (Swift)  │  │
│  │        │ │ device_info │ │          │  │
│  └────────┘ └─────────────┘ └──────────┘  │
│                                            │
│  ─ ─ ─ ─ ─ ─ FFI boundary (C ABI) ─ ─ ─  │
│                                            │
│  ┌────────────────────────────────────┐    │
│  │ Swift host app (SwiftUI/UIKit)     │    │
│  │ RemoSwift wrapper + App stores     │    │
│  └────────────────────────────────────┘    │
└────────────────────────────────────────────┘
```

### 2.2 Crate topology

| Crate | Platform | Description | Key dependencies |
|---|---|---|---|
| `remo-protocol` | Cross | Message types (`Request`, `Response`, `Event`) + length-prefixed framing codec with JSON (0x00) and binary (0x01) frame types | serde, tokio-util, uuid |
| `remo-transport` | Cross | `Connection` (framed bidirectional TCP pipe) + `Listener` (async accept) | remo-protocol, tokio |
| `remo-usbmuxd` | macOS | usbmuxd Unix socket client: device discovery, TCP tunnel creation | plist, tokio |
| `remo-bonjour` | Cross* | Bonjour/mDNS service registration (iOS) and discovery (macOS) | dns-sd C API |
| `remo-sdk` | iOS | Embedded TCP server + capability registry + built-in capabilities + C ABI FFI layer | remo-protocol, remo-transport, remo-objc, remo-bonjour, base64, dashmap |
| `remo-objc` | iOS* | ObjC runtime bridge: view tree, screenshot, device/app info, GCD main-thread dispatch | objc2, objc2-foundation, objc2-ui-kit |
| `remo-cdp` | Cross | Real Chrome DevTools Protocol: HTTP discovery, WS transport/dispatcher, `Page`/`DOM`/`CSS`/`Overlay` domains, the custom `Remo.*` domain, dual-stack accept helper | axum, hyper, chromiumoxide_cdp, remo-objc |
| `remo-cli` | macOS | Thin CDP client: `devices`, `capabilities`, `call`, `screenshot`, `info` | remo-usbmuxd, remo-bonjour, tokio-tungstenite, clap |
| `remo-mcp` | macOS | Agent-facing MCP companion server: `list_capabilities`/`invoke_capability`, proxying to the `Remo.*` CDP domain | rmcp, tokio-tungstenite |

*`remo-objc` compiles on all platforms with stubs; real UIKit access requires the `uikit` feature and an Apple target.

> **This document predates the CDP rewrite and is only partially updated.** `remo-desktop`
> (device manager, RPC client, web dashboard, fMP4 muxer) and `remo-daemon` (connection pool,
> HTTP/WebSocket API, event bus) described elsewhere in this file (§3 Wire protocol, §6 macOS
> desktop, the milestone/roadmap tables) **have been deleted**. `remo-sdk` now speaks real CDP
> through `remo-cdp` instead of the length-prefixed protocol §3 describes — see
> `crates/remo-cdp`'s module docs and `crates/remo-sdk/src/server.rs` for the current wire
> behavior. Sections below that still describe the deleted subsystems are kept for historical
> context, not as current documentation; treat any conflict between this file and the code as
> the code being right. See "13. CDP rewrite (current architecture)" at the end of this file for
> what actually replaced them.

### 2.3 Swift layer

| Component | Role |
|---|---|
| `remo.h` | Manually maintained C header for the FFI boundary |
| `CRemo` module | SPM binary target wrapping `RemoSDK.xcframework` |
| `RemoSwift` package | Thin Swift wrapper with zero-config auto-start: `Remo.register("name") { ... }`, `Remo.unregister("name")`. Debug-only (`#if DEBUG`). Server starts automatically on first API access (random port on simulator, 9930 on device). |
| `RemoExample` app | Demo app: 4 tabs (Home, Items, Activity Log, Settings), 10+ capabilities. |

---

## 3. Wire protocol

### 3.1 Framing

Every message on the wire is:

```
┌──────────┬──────────┬─────────────────────────────────────┐
│ len (4B) │ type(1B) │           payload                   │
│ u32 BE   │          │           (len - 1 bytes)           │
└──────────┴──────────┴─────────────────────────────────────┘
```

The `len` field gives the number of bytes that follow (type byte + payload). The type byte selects the frame kind:

**Type 0x00 — JSON frame.** The payload is a UTF-8 JSON object — a `Request`, `Response`, or `Event` (see section 3.2).

**Type 0x01 — Binary frame.** The payload is structured as:

```
┌────────────────┬─────────────────────────┬──────────────────────┐
│ meta_len (4B)  │ JSON metadata           │ raw binary data      │
│ u32 BE         │ (meta_len bytes, UTF-8) │ (remaining bytes)    │
└────────────────┴─────────────────────────┴──────────────────────┘
```

The metadata JSON contains:
- `id` — UUID matching the originating request.
- `metadata` — Capability-specific fields (e.g., image format, dimensions).

Binary frames are used for large payloads (screenshots, file transfers) where base64 encoding would be wasteful.

**Type 0x02 — Stream frame.** The payload is a video/audio stream frame:

```
┌───────────┬─────────────────┬──────────────────────┐
│ flags(1B) │ timestamp_us(8B)│ NAL data             │
│           │ u64 BE          │ (remaining bytes)    │
└───────────┴─────────────────┴──────────────────────┘
```

Flags: `0x01` = keyframe, `0x02` = codec config (SPS/PPS), `0x80` = stream end.

Maximum frame size: 64 MiB. Frames exceeding this are rejected at the codec level.

### 3.2 Message types

All messages are JSON with a `"type"` discriminator:

**Request** (macOS → iOS):
```json
{
  "type": "request",
  "id": "550e8400-e29b-41d4-a716-446655440000",
  "capability": "navigate",
  "params": { "route": "/detail/42" }
}
```

**Response** (iOS → macOS):
```json
{
  "type": "response",
  "id": "550e8400-e29b-41d4-a716-446655440000",
  "status": "ok",
  "data": { "route": "/detail/42" }
}
```

```json
{
  "type": "response",
  "id": "550e8400-e29b-41d4-a716-446655440000",
  "status": "error",
  "code": "not_found",
  "message": "capability 'foo' not found"
}
```

**Event** (iOS → macOS, unsolicited push):
```json
{
  "type": "event",
  "kind": "state_changed",
  "payload": { "key": "counter", "value": 42 }
}
```

### 3.3 Error codes

| Code | Meaning |
|---|---|
| `not_found` | Capability not registered |
| `invalid_params` | Parameters failed validation |
| `internal` | Handler threw an error |
| `timeout` | Request timed out (client-side) |

### 3.4 Built-in capabilities

These are registered automatically by `RemoServer::new()` — no Swift code required:

| Name | Params | Returns | Description |
|---|---|---|---|
| `__ping` | none | `{"pong": true}` | Connectivity check |
| `__list_capabilities` | none | `["navigate", "state.get", ...]` | Discovery |
| `__view_tree` | `{"max_depth": N}` (optional) | `ViewNode` tree (JSON) | Snapshot the UIView hierarchy |
| `__screenshot` | `{"format": "jpeg"\|"png", "quality": 0.8}` | **BinaryResponse** (type 0x01 frame): raw image bytes. Metadata: `{"format", "width", "height", "scale", "size"}` | Capture the screen |
| `__device_info` | none | `{"name", "model", "system_name", "system_version", "screen_width", "screen_height", "screen_scale"}` | Device model and screen |
| `__app_info` | none | `{"bundle_id", "version", "build", "display_name"}` | App bundle metadata |
| `__start_mirror` | `{"fps": 30, "codec": "h264"}` | `{"stream_id": 1}` | Start screen mirror; frames arrive as Type 0x02 stream frames |
| `__stop_mirror` | `{"stream_id": 1}` | `{}` | Stop mirror session |

---

## 4. Transport layer

### 4.1 Simulator / Wi-Fi

Direct TCP to `<host>:<port>`. Port 0 (auto-assign) is recommended; each app instance gets a unique port advertised via Bonjour. For simulator, `host` is `127.0.0.1`.

### 4.2 USB via usbmuxd

The macOS usbmuxd daemon (`/var/run/usbmuxd`) multiplexes TCP connections over USB.

**Protocol flow:**

1. Connect to Unix domain socket `/var/run/usbmuxd`
2. Send `Listen` plist command → receive device attach/detach events
3. For each device, open a new Unix socket connection
4. Send `Connect` plist command with `DeviceID` + `PortNumber` (big-endian)
5. On success (result code 0), the socket becomes a raw TCP tunnel to the device's port
6. Run the Remo framing protocol over this tunnel

**Message format**: 16-byte header (all fields little-endian u32):

| Offset | Field | Value |
|---|---|---|
| 0 | length | Total message length including header |
| 4 | version | 1 (plist protocol) |
| 8 | msg_type | 8 (plist) |
| 12 | tag | Incrementing request tag |

Payload is XML plist.

### 4.3 Bonjour / mDNS

The `remo-bonjour` crate wraps the Apple dns_sd C API for zero-config networking:

- **iOS (server side)**: The server auto-starts on first API access and registers a Bonjour service of type `_remo._tcp` with the actual bound port. The service is de-advertised on `remo_stop()`.
- **macOS (client side)**: `remo devices` browses for `_remo._tcp` services, resolves hostnames to addresses, and lists discovered simulators/devices alongside USB devices.

Port 0 (auto-assign) is recommended for multi-simulator setups — each simulator gets a unique port, and Bonjour advertises the correct one.

---

## 5. iOS SDK (`remo-sdk`)

### 5.1 Capability registry

A concurrent `DashMap<String, BoxedHandler>` mapping capability names to async or sync handler functions.

```rust
type BoxedHandler = Arc<dyn Fn(Value) -> Pin<Box<dyn Future<Output = HandlerResult> + Send>> + Send + Sync>;
```

Handlers return `Result<serde_json::Value, HandlerError>`.

**Capability change events**: The registry holds an optional `tokio::sync::broadcast::Sender`. When a capability is registered or unregistered, it emits a `capabilities_changed` event containing the action (`"registered"` or `"unregistered"`), the capability name, and the full updated capability list. The server injects the broadcast sender on startup so events are forwarded to connected clients as push events.

### 5.2 FFI boundary

The Rust static library exposes 7 C functions:

| Function | Signature | Description |
|---|---|---|
| `remo_start` | `(port: u16)` | Start tokio runtime + TCP server. Idempotent — auto-called by Swift wrapper on first API access. |
| `remo_stop` | `()` | Graceful shutdown |
| `remo_register_capability` | `(name: *const c_char, context: *mut c_void, callback: fn)` | Register a Swift handler |
| `remo_unregister_capability` | `(name: *const c_char) -> bool` | Unregister a capability by name. Returns true if found and removed. |
| `remo_free_string` | `(ptr: *mut c_char)` | Free a Rust-allocated string |
| `remo_list_capabilities` | `() -> *mut c_char` | Return JSON array of names |
| `remo_get_port` | `() -> u16` | Return the actual port the server is listening on (0 if not started) |

**Callback convention**: Swift allocates the return string with `strdup()`. Rust reads it and calls `free()`. The `context` pointer is an `Unmanaged<HandlerBox>.toOpaque()` that Swift retains.

### 5.3 Threading model

- `remo_start()` is idempotent (guarded by `AtomicBool`). The Swift wrapper auto-calls it on first API access via a lazy `static let`. It initializes a global `tokio::runtime::Runtime` (multi-thread) stored in a `OnceLock<Mutex<RemoGlobal>>`.
- The TCP server and all connection handlers run on tokio's thread pool.
- Built-in capabilities that require UIKit access (view tree, screenshot, device info) use `remo_objc::run_on_main_sync()` to dispatch work to the main thread via GCD `dispatch_sync_f`.
- FFI capability callbacks (user-registered) are invoked on a tokio worker thread. For UI mutations, the Swift handler must dispatch to `DispatchQueue.main`.

### 5.4 ObjC bridge (`remo-objc`)

Uses `objc2` + `objc2-ui-kit` to inspect the ObjC runtime directly from Rust.

**Modules**:

| Module | Description |
|---|---|
| `main_thread` | GCD dispatch utility: `run_on_main_sync()` dispatches closures to the main queue via `dispatch_sync_f`. Detects if already on main thread to avoid deadlock. |
| `view_tree` | `snapshot_view_tree()`: Walk the key window's UIView hierarchy and serialize to `ViewNode` JSON (class name, frame, alpha, hidden, tag, accessibility identifier, children). |
| `screenshot` | `capture_screenshot(format, quality)`: Capture the key window using `UIGraphicsBeginImageContextWithOptions` + `drawViewHierarchyInRect:afterScreenUpdates:`. Returns raw JPEG/PNG bytes. |
| `device_info` | `get_device_info()`: Read `UIDevice.currentDevice` properties (name, model, systemVersion) and `UIScreen.mainScreen` properties (bounds, scale). `get_app_info()`: Read `NSBundle.mainBundle` info dictionary (bundleIdentifier, version, build, displayName). |

**Reachable via ObjC runtime** (not yet implemented):
- `performSelector:` on arbitrary ObjC objects
- Read/write UIView layer properties (cornerRadius, borderWidth, backgroundColor)
- Accessibility tree traversal

**Not reachable from ObjC runtime** (must go through FFI callbacks):
- Swift structs, enums, generics
- `@Observable` / `@Published` properties
- SwiftUI `NavigationPath` manipulation
- Any Swift-only type that doesn't bridge to ObjC

---

## 6. macOS desktop (`remo-desktop` + `remo-cli`)

### 6.1 Device manager

Maintains a `DashMap<u32, DeviceHandle>` of discovered devices. Supports three discovery modes:

- **USB**: Connects to usbmuxd, sends `Listen`, processes `Attached`/`Detached` events.
- **Bonjour**: Browses for `_remo._tcp` services, resolves hostnames, and adds discovered simulators/Wi-Fi devices.
- **Direct**: Connect to a known `host:port` (for simulators or Wi-Fi with known IP).

### 6.2 RPC client

Each device connection gets an `RpcClient` instance:

- Sends `Request` messages and matches `Response` by UUID using a `HashMap<MessageId, oneshot::Sender>`.
- Events are forwarded to a `mpsc::Sender<Event>` channel.
- Supports configurable timeout per call (default: 10s).

### 6.3 CLI commands

| Command | Description | Example |
|---|---|---|
| `remo devices` | Auto-discover devices (USB + Bonjour) | `remo devices` |
| `remo dashboard` | Web dashboard with multi-device UI | `remo dashboard --port 8080` |
| `remo call -a <addr> <cap> [params]` | Invoke a capability | `remo call -a 127.0.0.1:51363 navigate '{"route":"/home"}'` |
| `remo list -a <addr>` | List registered capabilities | `remo list -a 127.0.0.1:51363` |
| `remo watch -a <addr>` | Stream events from device | `remo watch -a 127.0.0.1:51363` |
| `remo tree -a <addr>` | Dump view hierarchy | `remo tree -a 127.0.0.1:51363 -m 3` |
| `remo screenshot -a <addr>` | Capture screenshot to file | `remo screenshot -a 127.0.0.1:51363 -o shot.jpg` |
| `remo info -a <addr>` | Show device and app info | `remo info -a 127.0.0.1:51363` |
| `remo mirror -a <addr>` | Live screen mirror | `remo mirror -a 127.0.0.1:51363 --web --port 8090` |
| `remo start` | Start the daemon (foreground or background) | `remo start --port 19630 -d` |
| `remo stop` | Stop the daemon | `remo stop` |
| `remo status` | Check daemon health and device count | `remo status` |

**Daemon fallback**: All existing commands (`call`, `list`, `screenshot`, etc.) first check for a running daemon via `~/.remo/daemon.json`. If the daemon is alive, requests are routed through its HTTP API; if not, the CLI falls back to a direct TCP connection. `remo dashboard` auto-starts the daemon.

### 6.4 Web dashboard

The `remo dashboard` command starts an axum HTTP/WebSocket server with an embedded single-page web UI.

**Architecture**: The dashboard holds a `DashboardState` with a `DeviceManager` for discovery, an optional `ActiveConnection` for the currently connected device, and broadcast channels for streaming video frames and push events.

**REST endpoints**:

| Endpoint | Method | Description |
|---|---|---|
| `/api/devices` | GET | List all discovered devices (USB + Bonjour) |
| `/api/connect` | POST | Connect to device (`{"id": "usb:15"}` or `{"id": "bonjour:Name"}`) |
| `/api/disconnect` | POST | Disconnect from current device |
| `/api/connection` | GET | Check connection status |
| `/api/info` | GET | Device + app info |
| `/api/capabilities` | GET | List registered capabilities |
| `/api/call` | POST | Invoke a capability (`{"name": "...", "params": {...}}`) |
| `/api/screenshot` | POST | Capture screenshot (returns JPEG bytes) |
| `/api/mirror/start` | POST | Start screen mirror (`{"fps": 30}`) |
| `/api/mirror/stop` | POST | Stop screen mirror |

**WebSocket endpoints**:

| Endpoint | Description |
|---|---|
| `/ws/stream` | Binary stream of video frames (`[flags(1B)][timestamp_us(8B)][nal_data]`) |
| `/ws/events` | JSON push events (device added/removed, connection established/lost, RPC events) |

**Device ID format**: `usb:<numeric_id>` for USB devices, `bonjour:<service_name>` for Bonjour devices.

### 6.5 Video streaming pipeline

Screen mirroring flows through:

1. **iOS**: `RPScreenRecorder` captures `CMSampleBuffer` → VideoToolbox H.264 encoder → Annex-B NAL units
2. **Wire**: NAL units sent as Type 0x02 stream frames (flags + timestamp + data)
3. **Desktop**: `StreamReceiver` collects frames → `Mp4Muxer` wraps in fMP4 (init segment + moof/mdat) → WebSocket binary to browser
4. **Browser**: MSE `MediaSource` API appends fMP4 segments to `<video>` element

### 6.6 Daemon (`remo-daemon`)

A long-running background process that acts as the central hub for all device connections and client interactions.

**Lifecycle**: Started via `remo start` (foreground or `-d` for background). Stores PID and port in `~/.remo/daemon.json`. Graceful shutdown via `remo stop` or SIGTERM.

**ConnectionPool**: Manages device connections with automatic lifecycle:
- Auto-connects on device discovery via Bonjour/USB
- Sends `__ping` keepalive every 5 seconds
- Detects disconnection after 3 consecutive 2-second timeouts
- Auto-reconnects with exponential backoff
- Device state machine: `discovered → connecting → connected → disconnected`

**HTTP API**:

| Endpoint | Method | Description |
|---|---|---|
| `/devices` | GET | List discovered devices + connection state |
| `/devices/{id}/connect` | POST | Manual connection |
| `/devices/{id}/disconnect` | POST | Disconnect device |
| `/call` | POST | Invoke capability (`await` or `fire` mode) |
| `/events?since=<cursor>` | GET | Poll event buffer |
| `/webhooks/{id}` | DELETE | Webhook management |
| `/ws/events` | GET | WebSocket event subscription |
| `/status` | GET | Daemon health |
| `/capabilities` | GET | List capabilities |
| `/screenshot` | POST | Capture screenshot |
| `/ws/stream` | GET | Video stream WebSocket |

**EventBus**: Ring buffer (default 1000 events) with atomic sequence numbering and broadcast channel for WebSocket subscribers. Event types: `device_discovered`, `device_lost`, `connection_established`, `connection_lost`, `connection_restored`, `call_completed`, `call_failed`, `capabilities_changed`, `app_event`.

**Call modes**:
- **await** (default): Synchronous — blocks until iOS responds (default 30s timeout).
- **fire**: Returns immediately with `call_id`; completion delivered as an event.

---

## 7. Example app (`RemoExample`)

A SwiftUI app demonstrating full SDK integration:

**Pages**: Home (counter + toast + confetti), Items (list with add/delete), Activity Log (real-time log), Settings (accent color picker + debug info).

The example app demonstrates both built-in and custom capabilities. Built-in capabilities (`__view_tree`, `__screenshot`, `__device_info`, `__app_info`) are available automatically.

**Global capabilities** (registered via `#remoCap` in root `ContentView.task {}`):

| Capability | Params | Effect |
|---|---|---|
| `navigate` | `{"route": "home"\|"items"\|"log"\|"settings"}` | Switch to a tab |
| `state.get` | `{"key": "counter"\|"items"\|"route"\|"accentColor"}` | Returns the current value |
| `state.set` | `{"key": "counter", "value": 42}` | Mutates the store |
| `ui.toast` | `{"message": "..."}` | Show a toast overlay |
| `ui.confetti` | none | Trigger confetti animation |
| `ui.setAccentColor` | `{"color": "red"}` | Change app accent color |

**Page-scoped capabilities** (registered via `#remoCap` in each view's `.task {}`, auto-unregistered when the task is cancelled):

| Capability | Available on | Params | Effect |
|---|---|---|---|
| `counter.increment` | Home tab | `{"amount": 5}` | Increments counter by amount |
| `items.add` | Items tab | `{"name": "..."}` | Add item to list |
| `items.remove` | Items tab | `{"index": 0}` | Remove item by index (or last) |
| `items.clear` | Items tab | none | Remove all items |

This per-view registration pattern demonstrates dynamic capability lifecycle — the dashboard's capability panel updates in real time as the user navigates between tabs, driven by `capabilities_changed` events.

---

## 8. Build & integration

### 8.1 macOS CLI

```bash
cargo build -p remo-cli --release
# Binary: target/release/remo
```

### 8.2 iOS static library

```bash
# Fast local development:
./build-ios.sh sim        # arm64 simulator only (debug)
./build-ios.sh device     # arm64 device only (debug)

# Full build:
./build-ios.sh release    # all targets (arm64 device, arm64 sim, x86_64 sim) + universal sim binary
```

Outputs an XCFramework at `swift/RemoSDK.xcframework/`.

### 8.3 SPM distribution

The release CI pipeline (`release.yml`) builds the XCFramework, zips it, and pushes to the [`remo-spm`](https://github.com/yjmeqt/remo-spm) repo as a binary SPM package. Consumer apps add the dependency:

```swift
.package(url: "https://github.com/yjmeqt/remo-spm.git", from: "0.2.0")
```

### 8.4 Xcode integration

1. Consumer apps add the `remo-spm` SPM package dependency. Inside this monorepo, the example app defaults to the local `RemoSwift` package and uses `REMO_USE_REMOTE=1` when explicitly validating the published package.
2. Register custom capabilities via `Remo.register(...)`. The server auto-starts on first API access — no explicit `Remo.start()` needed.
3. Built-in capabilities (view tree, screenshot, device/app info) are available immediately — no registration needed.

### 8.5 Tests

```bash
cargo test --workspace    # Unit tests (codec, registry) + integration test
```

The integration test spins up a `RemoServer` on localhost, connects an `RpcClient`, and verifies:
- `echo` capability round-trip
- `add` capability with params
- `__ping` built-in
- `__list_capabilities` built-in
- Unknown capability returns `not_found`

CI also runs `scripts/e2e-test.sh` which builds the SDK, CLI, and example app, launches on a simulator, and exercises every capability end-to-end via the Remo wire protocol.

---

## 9. Current status & known gaps

### 9.1 What is implemented (v0.4.0)

| Component | Status |
|---|---|
| `remo-protocol` (messages + codec + tests) | Complete |
| `remo-transport` (Connection + Listener) | Complete |
| `remo-usbmuxd` (client, types, Listen, Connect) | Complete |
| `remo-bonjour` (service registration + discovery) | Complete |
| `remo-sdk` (server, registry, built-in capabilities, FFI, capability change events) | Complete |
| `remo-objc` (view tree, screenshot, device/app info, main-thread dispatch) | Complete |
| `remo-desktop` (device manager w/ USB + Bonjour, RPC client, dashboard) | Complete |
| `remo-daemon` (connection pool, HTTP/WS API, event bus, keepalive) | Complete |
| `remo-cli` (devices, call, list, watch, tree, screenshot, info, mirror, start, stop, status) | Complete |
| `RemoSwift` (FFI wrapper incl. register/unregister, debug-only) | Complete |
| `RemoExample` (demo app w/ 4 tabs, per-view capability lifecycle) | Complete |
| Integration test + daemon API tests | Complete |
| CI/CD pipeline (check, lint, test, build, release) | Complete |
| SPM distribution (`remo-spm` binary package) | Complete |

### 9.2 Resolved TODOs

| ID | Issue | Resolution |
|---|---|---|
| T-001 | `remo_stop()` is a no-op | **Fixed.** Shutdown via broadcast channel. |
| T-002 | USB tunnel not wired to RPC | **Fixed.** `remo-transport` supports `UnixStream` via `IoStream` enum. |
| T-005 | Only view tree implemented in `remo-objc` | **Fixed.** Added screenshot, device info, app info, main-thread dispatch. |
| T-006 | No Bonjour/mDNS discovery | **Fixed.** `remo-bonjour` crate: iOS advertises, macOS browses. |
| T-003 | No reconnection logic | **Fixed.** `remo-daemon` ConnectionPool auto-reconnects with exponential backoff. |
| T-008 | `staticlib` not in Cargo.toml | **Fixed.** Feature-gated `crate-type = ["staticlib"]` in `remo-sdk`. |
| T-012 | No CI pipeline | **Fixed.** GitHub Actions: check (fmt, clippy, test), E2E (simulator). |

### 9.3 Open TODOs

#### P1 — Needed for full product

| ID | Component | Issue | Detail |
|---|---|---|---|
| T-004 | `remo-sdk` | Limited event push from iOS | `capabilities_changed` events are now emitted. General-purpose `remo_emit_event()` FFI for arbitrary app events is still needed. |
| T-007 | `remo-cli` | `devices` command requires usbmuxd | Fails on machines without usbmuxd (Linux CI). Should gracefully handle missing socket. |

#### P2 — Nice to have

| ID | Component | Issue | Detail |
|---|---|---|---|
| T-009 | `remo-desktop` | No macOS GUI | A SwiftUI macOS app with device list, view tree visualizer, and state inspector. |
| T-010 | `remo-sdk` | Capability middleware / hooks | Pre/post hooks on capability invocation (logging, auth, rate limiting). |
| ~~T-011~~ | `remo-protocol` | ~~No binary protocol option~~ | **Fixed.** Binary frame type (0x01) added to wire protocol for raw payloads (screenshots). |
| T-013 | `remo-objc` | No SwiftUI view identity mapping | UIView tree doesn't map well to SwiftUI's declarative hierarchy. |
| T-014 | `remo-sdk` | Thread safety audit for FFI callbacks | `SendPtr` wrapper needs documentation or replacement. |
| T-015 | `remo-protocol` | No versioning / handshake | Client and server don't negotiate protocol version. |
| T-016 | `remo-objc` | View property modification | `__view_set` to modify UIView properties remotely (frame, hidden, alpha, etc.). |

---

## 10. Dependency inventory

### 10.1 Rust (workspace)

| Crate | Version | Used for |
|---|---|---|
| `serde` + `serde_json` | 1.x | Message serialization |
| `tokio` | 1.x (full) | Async runtime, TCP, timers |
| `tokio-util` | 0.7 | `Framed` codec adapter |
| `bytes` | 1.x | Zero-copy buffer management |
| `uuid` | 1.x (v4, serde) | Request ID generation |
| `thiserror` | 2.x | Error derive macros |
| `anyhow` | 1.x | CLI error handling |
| `clap` | 4.x (derive) | CLI argument parsing |
| `plist` | 1.x | usbmuxd plist encoding |
| `tracing` + `tracing-subscriber` | 0.1 / 0.3 | Structured logging |
| `futures` | 0.3 | Stream/Sink utilities |
| `dashmap` | 6.x | Concurrent capability registry |
| `base64` | 0.22 | Base64 encoding for screenshot data |
| `objc2` | 0.6 | ObjC FFI (iOS only) |
| `objc2-foundation` | 0.3 | NSString, NSArray, NSRect, etc. |
| `objc2-ui-kit` | 0.3 | UIView, UIWindow, UIDevice, etc. (optional) |

### 10.2 Swift

| Dependency | Source | Used for |
|---|---|---|
| `CRemo` | Binary target in `RemoSDK.xcframework` | Rust FFI binding |
| `RemoSwift` | Local SPM package in this monorepo example, or via `remo-spm` for published distribution | Swift wrapper API |

### 10.3 Build tools

| Tool | Required | Used for |
|---|---|---|
| Rust stable toolchain | Yes | Compilation |
| Xcode + iOS SDK | Yes (for iOS) | iOS target, Swift compilation |
| `rustup target add aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios` | Yes (for iOS) | Cross-compilation targets |

---

## 11. File manifest

```
remo/
├── Cargo.toml                              # Workspace root
├── Makefile                                # setup, build, check, test, cli, ios, ios-sim, ios-device, fmt, lint
├── README.md                               # Overview + quick start
├── SPEC.md                                 # This document
├── build-ios.sh                            # Build iOS XCFramework (sim/device/debug/release modes)
├── rust-toolchain.toml                     # Pin stable + iOS targets
├── .githooks/
│   └── pre-commit                          # cargo fmt + clippy pre-commit hook
├── .github/
│   └── workflows/
│       ├── ci.yml                          # PR checks: fmt, clippy, test, E2E on simulator
│       └── release.yml                     # Tag-triggered: build XCFramework, push to remo-spm
├── scripts/
│   └── e2e-test.sh                         # E2E test: build, deploy, exercise capabilities on simulator
├── tests/
│   ├── integration.rs                      # Full round-trip server + client test
│   └── daemon_integration.rs               # Daemon HTTP API integration tests
├── crates/
│   ├── remo-protocol/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs                      # Re-exports + DEFAULT_PORT
│   │       ├── message.rs                  # Request, Response, Event, ErrorCode
│   │       └── codec.rs                    # RemoCodec (length-prefix + JSON) + tests
│   ├── remo-transport/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── connection.rs               # Connection: send/recv over framed TCP/Unix
│   │       └── listener.rs                 # Listener: async TCP accept loop
│   ├── remo-usbmuxd/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── types.rs                    # UsbmuxHeader, Device, DeviceAttached, etc.
│   │       └── client.rs                   # UsbmuxClient (Listen, Connect), list_devices()
│   ├── remo-bonjour/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs                      # ServiceRegistration, ServiceBrowser, TxtRecord
│   │       └── ...
│   ├── remo-sdk/
│   │   ├── Cargo.toml                      # features: ios (enables remo-objc/uikit + bonjour)
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── registry.rs                 # CapabilityRegistry (DashMap) + tests
│   │       ├── server.rs                   # RemoServer + register_builtins() + built-in capabilities
│   │       └── ffi.rs                      # C ABI: remo_start, remo_register_capability, etc.
│   ├── remo-objc/
│   │   ├── Cargo.toml                      # features: uikit (optional objc2-ui-kit)
│   │   └── src/
│   │       ├── lib.rs                      # Re-exports all modules
│   │       ├── main_thread.rs              # GCD dispatch: run_on_main_sync()
│   │       ├── view_tree.rs                # ViewNode, snapshot_view_tree()
│   │       ├── screenshot.rs               # capture_screenshot() → JPEG/PNG bytes
│   │       ├── device_info.rs              # get_device_info(), get_app_info()
│   │       ├── screen_capture.rs           # RPScreenRecorder → CMSampleBuffer capture
│   │       └── video_encoder.rs            # VideoToolbox H.264 encoder + AVCC→Annex-B
│   ├── remo-desktop/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── rpc_client.rs               # RpcClient (async call w/ timeout + event stream)
│   │       ├── device_manager.rs           # DeviceManager (USB + Bonjour discovery)
│   │       ├── stream_receiver.rs          # StreamReceiver (broadcast → ordered frames)
│   │       ├── mp4_muxer.rs               # fMP4 muxer (init segment + moof/mdat)
│   │       ├── dashboard/
│   │       │   ├── mod.rs                  # axum HTTP/WS server + REST API
│   │       │   └── dashboard.html          # Single-page web UI (embedded)
│   │       └── web_player/
│   │           ├── mod.rs                  # Standalone web player server
│   │           └── player.html             # MSE video player page
│   ├── remo-daemon/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── daemon.rs                   # Daemon lifecycle, auto-connect, webhook dispatch
│   │       ├── connection_pool.rs          # Device connection management + keepalive
│   │       ├── event_bus.rs                # Ring buffer + broadcast event system
│   │       ├── api.rs                      # HTTP/WebSocket API endpoints (axum)
│   │       └── types.rs                    # Shared daemon types
│   └── remo-cli/
│       ├── Cargo.toml
│       └── src/
│           └── main.rs                     # CLI: devices, call, list, watch, tree, screenshot, info, start, stop, status
├── swift/
│   ├── RemoSwift/
│   │   ├── Package.swift                   # SPM package
│   │   └── Sources/
│   │       └── RemoSwift/
│   │           ├── include/
│   │           │   └── remo.h              # C header for FFI (manually maintained)
│   │           └── Remo.swift              # Swift wrapper: auto-start + Remo.register()
│   └── RemoSDK.xcframework/               # Built by build-ios.sh (gitignored)
└── examples/ios/
    ├── RemoExample.xcworkspace/
    ├── RemoExample.xcodeproj/
    ├── RemoExample/
    │   └── RemoExampleApp.swift            # App entry point
    └── RemoExamplePackage/
        ├── Package.swift                   # SPM: local path (dev) or remote binary (release)
        ├── Sources/RemoExampleFeature/
        │   └── ContentView.swift           # 4 tabs, 10+ custom capabilities
        └── Tests/
            └── RemoExampleFeatureTests/    # Swift integration tests
```

---

## 12. Milestones

### ~~M1: Simulator loopback~~ Done (v0.1.0)
- Full RPC round-trip on simulator
- CLI: devices, call, list, watch

### ~~M2: Real device over USB~~ Done (v0.1.0)
- usbmuxd tunnel end-to-end
- Generic transport over UnixStream

### ~~M3: Auto-discovery + Multi-simulator~~ Done (v0.2.0)
- Bonjour service registration and browsing
- Auto-assigned ports, multiple simultaneous simulators
- Debug-only SDK (`#if DEBUG`)

### ~~M4: Built-in introspection~~ Done (v0.2.0)
- View tree, screenshot, device info, app info — all auto-registered
- GCD main-thread dispatch for safe UIKit access
- CLI: tree, screenshot, info commands

### ~~M5: CI/CD + SPM distribution~~ Done (v0.2.0)
- GitHub Actions CI (check, fmt, clippy, test, E2E on simulator)
- Automated release pipeline → `remo-spm` binary SPM package

### ~~M6: Video streaming + Web dashboard~~ Done (v0.3.0-dev)
- H.264 screen capture via RPScreenRecorder + VideoToolbox
- StreamFrame wire protocol (Type 0x02)
- fMP4 muxer + MSE web player
- Web dashboard with multi-device discovery, device selector, video streaming, screenshots, capabilities panel
- `remo dashboard` and `remo mirror` CLI commands

### ~~M7: Daemon + capability events~~ Done (v0.5.0-dev)
- `remo-daemon` crate: ConnectionPool, EventBus, HTTP/WebSocket API
- CLI `start`/`stop`/`status` commands with daemon fallback for all existing commands
- `capabilities_changed` events emitted on register/unregister
- `Remo.unregister()` API (Swift + ObjC + FFI)
- Per-view capability lifecycle in example app (`.onAppear`/`.onDisappear`)
- Dashboard handles `capabilities_changed` events for real-time updates
- Auto-reconnection with exponential backoff (T-003)

### M8: Event streaming
- Extend T-004: general-purpose `remo_emit_event()` FFI for arbitrary app events
- `remo watch` receives live state changes

### M9: macOS GUI
- Implement T-009 (desktop app)
- Device list, live view tree, state editor, event log

### M10: Production readiness
- Implement T-015 (handshake/protocol versioning)
- View property modification (T-016)
- Audit T-014 (thread safety)

---

## 13. CDP rewrite (current architecture)

Everything above §12 describes Remo before its rewrite onto real Chrome DevTools Protocol. This
section is the accurate, current picture; treat it as authoritative over any conflicting text
earlier in this file. The full rationale and phasing lived in a planning doc during the rewrite
(not checked in as a permanent artifact) — this section is what actually shipped.

### 13.1 Why

Remo's custom length-prefixed RPC protocol required a bespoke client for every use: a CLI, a
daemon, a dashboard. Real CDP is a protocol every browser devtools frontend, `chromiumoxide`,
Playwright, and countless agent scripts already speak. Rewriting onto it means any CDP-speaking
tool can drive/inspect the app with zero custom client software — and it deletes an entire
category of client-side code (`remo-desktop`'s dashboard/web player, `remo-daemon`'s connection
pooling) that a fixed, already-adopted protocol makes redundant.

### 13.2 Two tracks — do not conflate

- **Track A — raw wire compatibility.** A custom `Remo.*` CDP domain (`crates/remo-cdp/src/domain_remo.rs`)
  exposes the actual product: `Remo.invoke` (call a named, developer-registered capability) and
  `Remo.listCapabilities`. Legal CDP — the wire format is JSON-RPC-shaped method dispatch, and
  nothing in the protocol whitelists domain names — but Chrome's own frontend won't render a
  panel for it, which doesn't matter to a client (the CLI, `remo-mcp`, an agent script) that was
  never going to use Chrome's UI for this surface.
- **Track B — real Chrome DevTools frontend compatibility.** Standard domains (`Page`, `DOM`,
  `CSS`, `Overlay` in `crates/remo-cdp/src/domain_page.rs`/`domain_dom.rs`) answer real Chrome
  DevTools' bootstrap sequence with correctly-shaped replies, so `chrome://inspect` renders
  Elements/Console/screencast against the app with zero console errors. This is what let
  `remo-desktop`'s dashboard be deleted rather than ported — DevTools' own UI covers the same
  ground.

The two tracks meet in one deliberate place: `domain_remo.rs` also claims `Runtime.evaluate`/
`Runtime.getProperties` — the methods behind whatever you type into DevTools' real Console panel
— and recognizes a small grammar (`remo`, `remo.<dotted.name>`, `remo.<dotted.name>({...})`).
A capability's own dots (already Remo's naming convention — `grid.tab.select`) become real
object nesting: bare `remo` is a self-describing, `objectId`-bearing `RemoteObject` whose
children are computed fresh from the live capability list on every call, so DevTools' own
expand-arrow and Tab-completion work against it with no separate `listCapabilities()`-style
indirection. This closes Track A's own zero-install gap without a bespoke second UI — considered
and rejected in favor of this, since it needs nothing beyond Chrome itself, exactly like Track B.

### 13.3 Non-goals (v1)

- **DOM tree is a snapshot, not live.** `DOM.getDocument`/`requestChildNodes` walk the UIView
  hierarchy on demand; there are no `DOM.childNodeInserted`-style mutation events. Refresh to see
  updates.
- **CSS is read-only, geometry-derived.** Frame → `left`/`top`/`width`/`height`, alpha →
  `opacity`, etc. `CSS.setStyleTexts` and friends have nothing honest to back them — UIKit has no
  cascade, stylesheets, or `display`/`position` model.
- **`Remo.capabilitiesChanged` isn't wired up.** `RemoDomain::respond` doesn't forward the
  registry's event bus through its `EventSink` yet — no CDP client can currently observe a live
  capability registration. (`remo watch`, which relied on this over the old wire, was removed
  with it.)
- **The high-fidelity H.264 mirror has no CDP path yet.** `Page.startScreencast` (baseline JPEG,
  works with any generic CDP client and `chrome://inspect`'s own screencast panel) is implemented.
  The old `__start_mirror`/`__stop_mirror` H.264 pipeline (`crates/remo-sdk/src/streaming.rs`) is
  kept, unwired, as the basis for a planned `Remo.startMirror`/`Remo.stopMirror` extension —
  dropping it outright would be a real fidelity regression versus what Remo did before the
  rewrite, so it's parked, not deleted.

### 13.4 What deleted, what replaced it

| Deleted | Replaced by |
|---|---|
| `remo-desktop` (device manager, RPC client, web dashboard, fMP4 muxer) | `chrome://inspect`'s own screencast/screenshot panels; `crates/remo-cli/src/cdp_client.rs` for programmatic device resolution |
| `remo-daemon` (connection pool, HTTP/WS API, event bus) | Direct-dial CDP from the CLI/MCP server per call — no daemon in front of it |
| Length-prefixed `remo-protocol` wire (as the *only* protocol) | `crates/remo-cdp`'s CDP-over-WebSocket, described in §3 for historical reference only |
| `remo list`, `remo dashboard`, `remo mirror`, `remo start`/`stop`/`status`, `remo watch` | `remo capabilities`; deleted (see 13.3); deleted, pending `Remo.startMirror`; deleted (no daemon); deleted (no event wiring yet) |
| Agent access via a repurposed browser-automation MCP server | `remo-mcp`, a purpose-built companion exposing `list_capabilities`/`invoke_capability` directly over the `Remo.*` domain |
| `remo tree` / the `__view_tree` built-in capability | Real CDP `DOM.getDocument` — the actual Elements panel, live and inspectable, not a JSON dump of the same `remo_objc::snapshot_view_tree()` data under a different name |
| The `__screenshot` built-in capability | `Page.captureScreenshot` — what `remo screenshot` already called directly since Phase 2; the capability was a redundant second path to the same `remo_objc::capture_screenshot()` |
| `Remo.invoke` reachable only via `remo-cli`/`remo-mcp`/a hand-rolled WebSocket client | `crates/remo-cdp/src/domain_remo.rs` also claims `Runtime.evaluate`/`Runtime.getProperties`, so real Chrome DevTools' own Console panel can call `remo.<dotted.name>({...})` directly — a capability's own dots become real object nesting, with live, schema-driven Tab-completion (see the module's doc comment for the full design) |

### 13.5 Generic storage-debugging built-ins

Beyond `__ping`/`__device_info`/`__app_info`, `RemoServer::new` also registers built-ins that are
genuinely universal to any iOS app rather than something a project registers for itself — the
same category of capability a similar CDP Storage-panel bridge already proved out for one
specific app (the plan this rewrite is built on), generalized here to any Remo target:

- `userDefaults.list`/`.get`/`.set`/`.delete` — real `NSUserDefaults` access
  (`crates/remo-objc/src/user_defaults.rs`). Foundation-only, gated on `target_vendor = "apple"`
  alone (not the `uikit` feature), which is what let its round-trip tests run for real against
  this dev machine's own `NSUserDefaults` rather than only ever exercising a stub — and is
  exactly how a real ObjC ABI bug (`-[NSData bytes]`'s return type encodes as `^v`, not `*const
  u8`) got caught immediately instead of shipping unverified.
- `filesystem.list`/`.read`/`.delete` — sandbox file browsing
  (`crates/remo-objc/src/filesystem.rs`). Pure `std::fs`; the OS sandbox is already the real
  security boundary (same reasoning `__view_tree`/`__screenshot` never needed extra scoping
  either), so this layer adds none on top of it. The one Apple-specific piece is resolving the
  sandbox home directory (`NSHomeDirectory()`) so a relative path means something sensible.
- `sqlite.query` — arbitrary SQL against any `.sqlite`/`.db` file in the sandbox
  (`crates/remo-sdk/src/sqlite_query.rs`, via `rusqlite`, `bundled` feature for reliable iOS
  cross-compilation). Not tied to any specific ORM — SQLite is the thing most persistence
  stacks bottom out on eventually, which is what makes this generic in a way an app-specific
  "list my Widgets table" capability couldn't be.

All three are reachable through every path this rewrite built: `Remo.invoke` (CLI/MCP/hand-rolled
client), and the Console panel's `remo.userDefaults.set({...})`-style direct calls.
