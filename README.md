# Remo

**Infrastructure for agentic iOS development.**

[![Remo demo preview](docs/assets/remo_preview.png)](https://github.com/yjmeqt/Remo/releases/download/v0.3.0-demo/remo_demo.mov)

AI agents can already write Swift and trigger builds, but they still need a clean way to drive app-specific behavior at runtime. Remo gives them a programmable interface inside the app: discover devices, list capabilities, invoke named handlers, and move the app into the exact state they need.

The result is a tighter loop: **write code → build → call capabilities → inspect the app with your preferred tooling → iterate.** Remo focuses on semantic app control, not rebuilding the entire simulator toolchain.

## Demo

**[Interactive showcase →](https://yjmeqt.github.io/Remo/)** Watch Claude Code register and invoke app-defined capabilities through Remo.

Or watch the raw demo video: [remo_demo.mov](https://github.com/yjmeqt/Remo/releases/download/v0.3.0-demo/remo_demo.mov)

```
# Agent writes code, triggers a build, then drives the app via Remo:

remo devices                                                          # discover real devices (USB) & simulators
remo list -a <addr>                                                   # inspect available capabilities
remo call -a <addr> grid.feed.append '{"title":"Ship It"}'            # invoke a capability
remo call -a <addr> grid.tab.select '{"id":"feed"}'                   # move the app into the next state
```

For simulator automation, screenshots, recording, and broader inspection, pair Remo with `xcodebuildmcp`. Remo focuses on the part that tooling outside the app cannot provide: app-defined capability registration and semantic runtime control.

## Why Remo?

- **Capability-first.** Developers register named handlers in Swift. Agents discover and call them at runtime — read CoreData, toggle feature flags, navigate routes, inject test data. If you can write it in Swift, an agent can call it.
- **Semantic control.** Remo operates in the language of your app, not generic taps and pixels. Capabilities take structured input and return structured output.
- **Runtime discovery.** Agents find real devices over USB and simulators over Bonjour, then connect without hand-written per-device setup.
- **Composes with `xcodebuildmcp`.** Use `xcodebuildmcp` for simulator automation, screenshots, recording, and broader inspection. Use Remo for in-app semantics and capability registration.
- **Debug-only by default.** The SDK compiles to no-ops in Release builds (`#if DEBUG`), so it never ships to production.

## Quick Start

All app-side Remo integration code should stay in debug-only paths. Wrap imports, startup, and capability registration in `#if DEBUG`.

### 1. Add the SDK to your iOS app

**Swift (SPM)**

Add the SPM dependency in Xcode:

```
https://github.com/yjmeqt/remo-spm.git
```

**Swift (CocoaPods)**

```ruby
pod 'Remo', :podspec => 'https://raw.githubusercontent.com/yjmeqt/remo-spm/main/Remo.podspec'
```

**Objective-C (CocoaPods)**

```ruby
pod 'Remo/ObjC', :podspec => 'https://raw.githubusercontent.com/yjmeqt/remo-spm/main/Remo.podspec'
```

### 2. Register capabilities

**Swift — typed `#Remo` + `#remoCap` + `#remoScope` macros (recommended)**

Remo macros strip all Remo code from release builds automatically. No `#if DEBUG` wrappers needed.

```swift
import RemoSwift

// SwiftUI — declare and register inside the same debug island
.task {
    await #Remo {
        struct ToggleResponse: Encodable {
            let toggled: Bool
        }

        enum MyFeatureToggle: RemoCapability {
            static let name = "myFeature.toggle"

            struct Request: Decodable {
                let enabled: Bool?
            }

            typealias Response = ToggleResponse
        }

        await #remoScope {
            #remoCap(MyFeatureToggle.self) { req in
                let enabled = req.enabled ?? false
                Task { @MainActor in
                    FeatureFlags.shared.myFeature = enabled
                }
                return ToggleResponse(toggled: enabled)
            }
        }
    }
}

// UIKit — local capability type plus view-controller scoped lifecycle
override func viewDidAppear(_ animated: Bool) {
    super.viewDidAppear(animated)
    #Remo {
        struct GridVisibleResponse: Encodable {
            let items: [String]
        }

        enum GridVisible: RemoCapability {
            static let name = "grid.visible"
            typealias Response = GridVisibleResponse
        }

        #remoScope(scopedTo: self) {
            #remoCap(GridVisible.self) { [weak self] _ in
                return GridVisibleResponse(items: self?.visibleItems() ?? [])
            }
        }
    }
}
```

**Objective-C**

```objc
#if DEBUG
#import <RemoObjC/RMRemo.h>

// The server starts automatically on first API access.
// Objective-C handlers run on Remo's background callback path.
[RMRemo registerCapability:@"myFeature.toggle"
                   handler:^NSDictionary *(NSDictionary *params) {
    BOOL enabled = [params[@"enabled"] boolValue];
    dispatch_async(dispatch_get_main_queue(), ^{
        [FeatureFlags shared].myFeature = enabled;
    });
    return @{@"toggled": @(enabled)};
}];

// Unregister when no longer needed:
[RMRemo unregisterCapability:@"myFeature.toggle"];
#endif
```

Remo handlers execute on a background callback path and must remain `@Sendable`. Do not assume main-thread or `MainActor` execution inside the callback — explicitly hand off UI mutations to the main thread.

The iOS example app includes a dedicated Grid tab that demonstrates UIKit integration with `grid.*` capabilities wired through `scopedTo:` lifecycle management.

### 3. Install the CLI

```bash
# Homebrew (recommended)
brew install yjmeqt/tap/remo

# One-command install
curl -fsSL https://github.com/yjmeqt/Remo/releases/latest/download/install-remo.sh | bash

# Or from source
cargo install --git https://github.com/yjmeqt/Remo.git remo-cli
```

To uninstall:

```bash
# Homebrew install
brew uninstall remo

# Script-managed install (download, inspect, then run)
curl -fsSL https://github.com/yjmeqt/Remo/releases/latest/download/uninstall-remo.sh -o uninstall-remo.sh
bash uninstall-remo.sh
```

Manual release downloads are also available on the GitHub Releases page if you prefer to place `remo` on your `PATH` yourself.

### 4. Discover and invoke

```bash
remo devices                                            # discover real devices & simulators
remo list -a <addr>                                     # inspect registered capabilities
remo call -a <addr> myFeature.toggle '{"enabled":true}' # invoke your capability
remo dashboard                                          # open the multi-device web dashboard
```

For simulator automation, screenshots, recording, and broader inspection, use `xcodebuildmcp` alongside Remo.

## How It Works

```
┌──────────────────────────────────────┐
│  macOS                               │
│  remo CLI / AI agent                 │
│  ├── USB discovery (usbmuxd)        │
│  ├── Simulator discovery (Bonjour)   │
│  └── RPC client                      │
└──────────┬───────────────────────────┘
           │ TCP (USB tunnel / localhost)
┌──────────▼───────────────────────────┐
│  iOS                                 │
│  remo-sdk (Rust static lib)          │
│  ├── TCP server (tokio)              │
│  ├── Capability registry             │
│  ├── Bonjour advertisement           │
│  ├── Built-in: view tree, app info   │
│  └── ObjC bridge (objc2)             │
│  ── FFI boundary ──                  │
│  RemoSwift (Swift wrapper)           │
│  Your app registers capabilities     │
└──────────────────────────────────────┘
```

The iOS SDK starts a TCP server inside your app. Real devices are discovered via USB (usbmuxd), simulators via Bonjour/mDNS. The macOS CLI (or any AI agent) sends JSON-RPC requests to discover and invoke capabilities. Pair it with `xcodebuildmcp` when you need simulator automation or inspection outside the app boundary.

## CLI Commands

```bash
remo devices                              # Auto-discover devices (USB + Bonjour)
remo call -a <addr> <capability> [params] # Invoke a capability
remo list -a <addr>                       # List registered capabilities
remo screenshot -a <addr> -o out.jpg      # Take a screenshot
remo tree -a <addr>                       # Dump view hierarchy
remo info -a <addr>                       # Show device & app info
remo mirror -a <addr> --web               # Live screen mirror (H.264 → fMP4)
remo mirror -a <addr> --save out.mp4      # Record screen to file
remo watch -a <addr>                      # Stream events from device
remo dashboard                            # Web demo page
remo start [-d]                           # Start the daemon (foreground or background)
remo stop                                 # Stop the daemon
remo status                               # Check daemon health and device count
```

For a full command guide, see:

- [`skills/remo-setup/references/cli.md`](skills/remo-setup/references/cli.md) for the distributed onboarding CLI reference
- [`docs/cli.md`](docs/cli.md) for the repository maintenance checklist that keeps CLI docs aligned

## Built-in Capabilities

These are registered automatically by the SDK — no setup required:

| Capability | Description |
|------------|-------------|
| `__ping` | Connectivity check |
| `__list_capabilities` | List all registered capabilities |
| `__view_tree` | Snapshot the UIView hierarchy as JSON |
| `__screenshot` | Capture the screen (JPEG/PNG, configurable quality) |
| `__device_info` | Device model, OS version, screen dimensions |
| `__app_info` | Bundle ID, version, build number, display name |
| `__start_mirror` | Start H.264 screen mirror stream |
| `__stop_mirror` | Stop mirror stream |

## Claude Code Skills

Remo ships a set of [Claude Code skills](https://docs.anthropic.com/en/docs/claude-code/skills) that give AI agents structured workflows for capability-driven iOS development. Install them into any iOS project to get a loop of setup → capabilities → runtime control → design review.

| Skill | Type | Purpose |
|-------|------|---------|
| [`remo-setup`](skills/remo-setup/SKILL.md) | One-time | Install CLI, integrate SDK, verify connection |
| [`remo-capabilities`](skills/remo-capabilities/SKILL.md) | Periodic | Map app features → register capabilities → document |
| [`remo`](skills/remo/SKILL.md) | Ongoing | Capability-driven development with checkpoints and timeline reports |
| [`remo-design-review`](skills/remo-design-review/SKILL.md) | Periodic | Compare running app against Figma designs |

### Install skills into your iOS project

```bash
mkdir -p .claude/skills
cp -R /path/to/Remo/skills/remo-setup .claude/skills/
cp -R /path/to/Remo/skills/remo-capabilities .claude/skills/
cp -R /path/to/Remo/skills/remo .claude/skills/
cp -R /path/to/Remo/skills/remo-design-review .claude/skills/
```

See [`skills/README.md`](skills/README.md) for the skill overview. Each distributed skill folder carries its own `references/cli.md`; start with [`skills/remo-setup/references/cli.md`](skills/remo-setup/references/cli.md) for the broadest CLI guide.

---

## Development

Everything below is for contributing to Remo itself.

### Prerequisites

| Tool | Version | Notes |
|------|---------|-------|
| Rust | 1.82+ | Auto-installed via `rust-toolchain.toml` |
| Xcode | 16+ | iOS SDK + Swift 6.1 |

### Build from source

To build locally:

```bash
cargo build -p remo-cli              # Build the CLI
./build-ios.sh sim                   # Build XCFramework (simulator)
./build-ios.sh device                # Build XCFramework (real device)
./build-ios.sh release               # Build all targets, optimized
```

### Crates

| Crate | Description |
|-------|-------------|
| `remo-protocol` | Message types + length-prefixed JSON framing codec |
| `remo-transport` | Bidirectional connection over TCP or Unix socket |
| `remo-usbmuxd` | macOS usbmuxd client — device discovery + USB tunneling |
| `remo-bonjour` | Bonjour/mDNS service registration and discovery |
| `remo-sdk` | iOS embedded server + capability registry + C FFI |
| `remo-objc` | ObjC runtime bridge via `objc2` (view tree, device/app info, media hooks) |
| `remo-desktop` | macOS library — device manager, RPC client, web dashboard, fMP4 muxer |
| `remo-daemon` | Background daemon — connection pool, HTTP/WebSocket API, event bus |
| `remo-cli` | CLI entry point |

### Project Status

**v0.3.0** — See [SPEC.md](SPEC.md) for the full architecture.

#### Roadmap
- [x] Auto-reconnection on disconnect (daemon ConnectionPool)
- [x] Capability change events + dynamic unregister API
- [ ] Skill installation and update (`remo init` / `remo skills update` to install/update `.claude/skills/` from release assets, with version pinning)
- [ ] macOS GUI (SwiftUI device inspector)
- [ ] View property modification (`__view_set`)
- [ ] Protocol versioning / handshake

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for development setup and guidelines.

## License

[MIT](LICENSE)
