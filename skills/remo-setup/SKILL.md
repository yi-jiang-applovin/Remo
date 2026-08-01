---
name: remo-setup
description: Use when adding Remo to an iOS project for the first time, installing the Remo CLI, integrating RemoSDK, wiring Remo.start(), and verifying the running app is reachable from the CLI.
---

# Remo Setup

Use this skill once per project to get the CLI and SDK wired up correctly.

Read `references/cli.md` before running install or verification commands, or when you need exact flag syntax, binary resolution, or current CLI caveats.

## Workflow

1. Confirm the app already builds and launches on a simulator.
2. Install the Remo CLI, preferring a project-local binary at `.remo/bin/remo`.
3. Add the Remo SDK dependency with SPM or CocoaPods.
4. Call `Remo.start()` from the app lifecycle in `#if DEBUG`.
5. Verify the app is discoverable and reachable from the CLI.
6. Hand off to `remo` for day-to-day verification and `remo-capabilities` for app-specific automation.

## Step 1: Install the CLI

Prefer a project-local install so the CLI version stays pinned to the project. Use `REMO_INSTALL_PREFIX="$PWD/.remo"` with the release install script when you want the binary to land at `.remo/bin/remo`.

Use the install and verification commands from `references/cli.md`, then resolve the binary in this order:

1. `.remo/bin/remo`
2. `remo`

If neither exists, stop and complete the install first.

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

Run the minimal verification sequence from `references/cli.md`:

1. discover the running app
2. call `__ping`
3. save a screenshot
4. inspect the view tree

If any step fails, fix setup before moving on.

## Completion Criteria

The setup is complete when all of the following are true:

- the CLI binary resolves correctly
- the app appears in `remo devices`
- `remo call ... "__ping"` succeeds
- screenshot capture works
- view-tree capture works

After that, switch to `remo` for verification work and `remo-capabilities` for project-specific capabilities.
