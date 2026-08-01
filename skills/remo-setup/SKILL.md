---
name: remo-setup
description: Use when adding Remo to an iOS project for the first time — integrating RemoSDK, wiring Remo.start(), choosing between installing the Remo CLI or using its zero-install web console, and verifying the running app is reachable.
---

# Remo Setup

Use this skill once per project to get Remo wired up correctly and confirm it's reachable
(either via the CLI or its zero-install web console).

Read `references/cli.md` before running install or verification commands, or when you need exact flag syntax, binary resolution, or current CLI caveats.

## Workflow

1. Confirm the app already builds and launches on a simulator.
2. Ask whether to install the Remo CLI, or skip it and drive the zero-install web console instead — then execute whichever path is chosen yourself.
3. Add the Remo SDK dependency with SPM or CocoaPods.
4. Call `Remo.start()` from the app lifecycle in `#if DEBUG`.
5. Verify the app is discoverable and reachable — via the CLI or the console, matching step 2.
6. Hand off to `remo` (or the console) for day-to-day verification and `remo-capabilities` for app-specific automation.

## Step 1: CLI or console?

Installing `remo-cli` is a real, if small, cost to the user (another binary on their machine) —
and it's optional: every Remo target also serves a zero-install `http://<addr>/console` page
covering the same core capability-invocation workflow (see `references/cli.md`'s "Zero-install
alternative" section). This is a genuine user preference, not something to decide unilaterally —
ask once, up front, then execute the chosen path yourself without asking again:

- **Install the CLI** — better for a project that will use `remo` repeatedly from scripts/CI, or
  where a human will run commands directly.
- **Skip it, use the web console** — better for a one-off session, or when you (the agent) already
  have a browser-automation tool (Playwright MCP, `chrome-devtools-mcp`, etc.) and would rather
  drive `window.remo.invoke(...)` directly than add a dependency the user didn't ask for.

If the CLI is chosen: prefer a project-local install so the version stays pinned to the project.
Use `REMO_INSTALL_PREFIX="$PWD/.remo"` with the release install script when you want the binary to
land at `.remo/bin/remo`. Use the install and verification commands from `references/cli.md`, then
resolve the binary in this order: `.remo/bin/remo`, then `remo`. If neither exists, stop and
complete the install first.

If the console is chosen: skip the install entirely. Confirm your browser-automation tool can
reach `http://<addr>/console` once the app is running (Step 4 covers getting `<addr>`), and use
`window.remo.invoke(...)`/`listCapabilities()`/`call(...)` for every verification step below that
would otherwise use a `remo` command — the result shapes match exactly (see `references/cli.md`).

## Step 2: Add the SDK

### Swift Package Manager

If the project has a `Package.swift`, add:

```swift
.package(url: "https://github.com/yjmeqt/remo-spm.git", from: "0.4.0"),
```

Then add:

```swift
.product(name: "RemoSwift", package: "remo-spm"),
```

If the app is managed directly in Xcode without a package manifest, instruct the user to add the package in Xcode:

`https://github.com/yjmeqt/remo-spm.git`

### CocoaPods

If the project uses CocoaPods, add:

```ruby
pod 'Remo', :podspec => 'https://raw.githubusercontent.com/yjmeqt/remo-spm/main/Remo.podspec'
```

For Objective-C support:

```ruby
pod 'Remo/ObjC', :podspec => 'https://raw.githubusercontent.com/yjmeqt/remo-spm/main/Remo.podspec'
```

## Step 3: Start Remo in Debug Builds

Wire Remo into the app lifecycle and keep it behind `#if DEBUG`. The same rule applies to all app-side Remo code: imports, `Remo.start()`, and capability registration should all stay in debug-only code paths.

UIKit example:

```swift
#if DEBUG
import RemoSwift

func application(
    _ application: UIApplication,
    didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]? = nil
) -> Bool {
    Remo.start()
    return true
}
#endif
```

SwiftUI example:

```swift
#if DEBUG
import RemoSwift
#endif

@main
struct MyApp: App {
    init() {
        #if DEBUG
        Remo.start()
        #endif
    }

    var body: some Scene {
        WindowGroup {
            ContentView()
        }
    }
}
```

Important notes:

- `Remo.start()` is idempotent.
- Release builds compile the SDK to no-ops, but keep the `#if DEBUG` wrapper for clarity.
- Simulators are typically discovered over Bonjour and may use a different port on each launch.

## Step 4: Verify the Integration

Run the minimal verification sequence, in either form depending on Step 1's choice:

1. discover the running app's address (`remo devices`, or `chrome://inspect`/Bonjour/USB output if
   going CLI-free) — this also gives you `http://<addr>/console` for the console path
2. call `__ping` — `remo call -a <addr> "__ping" '{}'`, or
   `await window.remo.invoke("__ping", {})` in the console
3. save a screenshot — `remo screenshot`, or
   `await window.remo.call("Page.captureScreenshot", {format: "jpeg", quality: 80})` (decode
   `.data` from base64)
4. inspect the view tree — `remo tree`, or `await window.remo.invoke("__view_tree", {})`

If any step fails, fix setup before moving on.

## Completion Criteria

The setup is complete when all of the following are true:

- `__ping` succeeds (via `remo call` or `window.remo.invoke`)
- screenshot capture works
- view-tree capture works
- if the CLI was chosen: the binary resolves correctly and the app appears in `remo devices`
- if the console was chosen: `http://<addr>/console` is reachable and `window.remo` is usable

After that, switch to `remo` (or the console) for verification work and `remo-capabilities` for
project-specific capabilities.
