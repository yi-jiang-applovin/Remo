# AGENTS.md

This file provides guidance to AI coding agents when working with code in this repository.

## What is Remo

Remo is infrastructure for **agentic iOS development** — it gives AI agents eyes and hands to autonomously build, test, and debug iOS apps. iOS apps embed the Remo SDK (a Rust static library), which starts a TCP server exposing registered **capabilities** (named handlers). macOS clients (CLI, web dashboard, AI agents) discover devices via USB (usbmuxd) or Bonjour/mDNS and invoke capabilities, inspect the view tree, capture screenshots, or record video.

## Build Commands

```bash
make setup          # Configure git hooks (run once after clone)
make build          # Build workspace (debug)
make check          # Type-check only (cargo check)
make test           # Run all tests (cargo test)
make lint           # Clippy with -D warnings
make fmt            # Auto-format (cargo fmt)
make cli            # Build macOS CLI binary (release) → target/release/remo
make ios-sim        # XCFramework for arm64 simulator only (~16s, local dev)
make ios-device     # XCFramework for arm64 device only (~16s)
make ios            # Full XCFramework, all targets, release (~50s, CI)
```

Device-dependent tests are `#[ignore]`d by default. Run with `cargo test -- --ignored` when a device is connected.

To run a single Rust test: `cargo test -p <crate-name> <test_name>`

E2E test (builds SDK + CLI + app, launches on simulator, exercises all capabilities):
```bash
./scripts/e2e-test.sh                     # full run
SKIP_BUILD=1 ./scripts/e2e-test.sh        # skip build phase
./scripts/e2e-test.sh --screenshots       # save screenshots to /tmp/remo-e2e/
```

## Architecture

### Crate topology (dependency order)

```
remo-protocol      — Message types (Request/Response/Event), framing codec
remo-transport     — Framed TCP/Unix socket bidirectional connection
remo-usbmuxd      — macOS USB device discovery via /var/run/usbmuxd
remo-bonjour       — DNS-SD wrapper for Bonjour discovery & registration
remo-objc          — ObjC runtime bridge (view tree, screenshot, device/app info)
remo-sdk           — iOS embedded TCP server + capability registry + FFI boundary
remo-desktop       — macOS device manager, RPC client, web dashboard, fMP4 muxer
remo-daemon        — Background daemon: connection pool, HTTP/WS API, event bus
remo-cli           — CLI entry point (clap), delegates to remo-desktop/remo-daemon
```

### Wire protocol

Length-prefixed framing: `[u32 BE length][u8 type][payload]`. Three frame types:
- `0x00` JSON — JSON-RPC messages (Request, Response, Event)
- `0x01` Binary — raw bytes (screenshots)
- `0x02` Stream — H.264 NALUs for video streaming

### FFI boundary (remo-sdk → Swift)

The `remo-sdk` crate compiles to a C static library for iOS. Seven exported C functions: `remo_start`, `remo_stop`, `remo_register_capability`, `remo_unregister_capability`, `remo_free_string`, `remo_list_capabilities`, `remo_get_port`. The C header is manually maintained at `swift/RemoSwift/Sources/RemoSwift/include/remo.h`.

The Rust side owns a global `OnceLock<Mutex<RemoGlobal>>` holding the tokio runtime, capability registry, and server state. `remo_start()` is idempotent. Main-thread work (UIKit access) is dispatched via GCD `dispatch_sync_f`.

### Swift layer

`swift/RemoSwift/` wraps the C FFI in a Swift-friendly API. The `RemoSDK.xcframework` is the binary artifact distributed via the `remo-spm` repo. The example app defaults to the local monorepo package and uses `REMO_USE_REMOTE=1` only when explicitly validating the published SPM package.

The SDK is `#if DEBUG` only — it does not ship in release builds.

### Example app (examples/ios/)

Workspace + SPM package architecture: the Xcode project (`RemoExample/`) is a thin shell; all feature code lives in `RemoExamplePackage/`. See `swift/CLAUDE.md` for Swift/SwiftUI conventions (MV pattern, no ViewModels, Swift Testing, @Observable).

## Website (Showcase)

The `website/` directory contains the interactive showcase site (React + Vite + Tailwind CSS), deployed to GitHub Pages at `yjmeqt.github.io/Remo/`.

```bash
cd website && npm install && npm run dev   # Dev server at localhost:5173
cd website && npm run build                # Production build → dist/
```

The demo hero is a React mock of the RemoExample UI (`website/src/components/DemoHero/PhoneScreen/`) driven by a single timeline (`timeline.ts`) — each `command` step carries a `phoneAction` that `useTimeline` replays into `PhoneState`, so terminal output and on-screen reactions stay locked in sync without a video.

## Claude Code Skills (`skills/`)

The `skills/` directory contains both distributed product skills for downstream
iOS projects and repo-internal contributor skills for working on Remo itself.

### Distributed product skills

| Skill | When to use |
|-------|-------------|
| `remo-setup` | First time adding Remo to a project — installs CLI to `.remo/bin/`, adds SDK dependency, wires `Remo.start()`, verifies connection |
| `remo-capabilities` | After setup or when features change — explores the app, maps features to capabilities, registers them, writes `.remo/capabilities.md` |
| `remo` | Every task — connect → baseline → [code → build → checkpoint with screenshots] → verification report |
| `remo-design-review` | Before release or after UI changes — fetches Figma designs, constructs app state, captures screenshots, produces side-by-side compliance report |

```
remo-setup → remo-capabilities → remo (daily) ↔ remo-design-review (periodic)
```

### Repo-internal contributor skills

| Skill | When to use |
|-------|-------------|
| `example-ios-feature-dev` | When building or changing a feature inside `examples/ios/RemoExamplePackage/` — register Remo capabilities, drive the app with `remo` CLI + XcodeBuildMCP, capture and report |

When editing skill files, verify that CLI commands, SDK API references, and example code match the current implementation. Keep each skill's `references/cli.md` aligned with the others, and keep `docs/cli.md`, `README.md`, and the skill folders aligned with those distributed references.

## Code Style

- **Rust**: `rustfmt.toml` enforces max_width=100. Workspace clippy lints are in `Cargo.toml` (e.g., `undocumented_unsafe_blocks`, `await_holding_lock`, `large_futures`).
- **Swift**: See `swift/CLAUDE.md` for full guidelines. Key points: MV pattern (no ViewModels), Swift Testing (`@Test`/`#expect`), Swift 6 strict concurrency.
- **Pre-commit hook** (`.githooks/pre-commit`): runs `cargo fmt --check` and `cargo clippy`. Set up with `make setup`.

## CI

GitHub Actions on `macos-26`. Two jobs: check (fmt + clippy + unit tests), e2e (build SDK/CLI/app, launch on simulator, exercise all capabilities via `scripts/e2e-test.sh`). Release workflow builds XCFramework, creates GitHub release, and auto-updates `remo-spm` repo.
- If using XcodeBuildMCP, use the installed XcodeBuildMCP skill before calling XcodeBuildMCP tools.
