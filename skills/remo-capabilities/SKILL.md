---
name: remo-capabilities
description: Use when mapping an iOS app's screens, routing, and state into Remo capabilities, registering new capabilities, or updating the capabilities reference after feature changes.
---

# Remo Capabilities

Use this skill to decide what the app should expose to Remo and to keep the capability inventory in sync with the code.

Read `references/cli.md` before using the CLI commands in this workflow or when you need exact flag syntax for discovery, screenshots, tree inspection, capability listing, or capability invocation.

## Workflow

1. Read the project's architecture and feature docs.
2. Identify feature domains, screen entry points, and routing patterns.
3. Observe the running app with screenshots, view trees, and current capability listings.
4. Design the capability map.
5. Implement the highest-value capabilities first.
6. Document registered capabilities and TODOs in `.remo/capabilities.md`.

## Step 1: Explore the App

Start from the project's own documentation:

- `AGENTS.md` or `CLAUDE.md`
- architecture docs
- feature or product README files

Then map:

- tabs and root screens
- pushed and modal flows
- auth states
- empty, loaded, and error states
- useful deep-link or router entry points

Use `references/cli.md` for the observation commands that capture the current screen, tree, and capability list.

## Step 2: Design the Capability Map

Organize capabilities into practical groups:

- navigation
- state reads
- state writes
- data seeding
- UI actions

Prioritize in this order:

1. navigation
2. state reads
3. state writes
4. data seeding
5. UI actions

The highest-value capability is usually the one that unlocks repeatable screen setup for verification.

## Step 3: Implement the Capabilities

Keep Remo capability code in a dedicated debug-only file, for example:

- `Debug/RemoCapabilities.swift`
- `Support/RemoCapabilities.swift`

Pattern:

```swift
#if DEBUG
import RemoSwift

enum RemoCapabilities {
    static func registerAll() {
        registerNavigation()
        registerState()
    }
}
#endif
```

Implementation rules:

- keep all app-side Remo code under `#if DEBUG`, including imports and helper types
- keep all capability registration under `#if DEBUG`
- use the app's real router and state types
- run UIKit work on the main actor
- return structured JSON with a success or error shape
- leave explicit TODOs for capabilities that are valuable but not yet safe to implement

## Step 4: Wire Registration

Call `RemoCapabilities.registerAll()` immediately after `Remo.start()` in the debug startup path.

Do not scatter registration across unrelated files unless the project already has a clear pattern for it.

## Step 5: Document the Inventory

Treat `.remo/capabilities.md` as the single source of truth for:

- registered capabilities
- capability parameters
- useful built-ins
- TODO capabilities and blockers

Update it whenever capability names, parameters, or availability change.

## Maintenance Rules

Re-run this skill after:

- a navigation refactor
- a major feature addition
- a state-management rewrite
- removing or renaming existing capabilities

When a later workflow reveals a missing capability, add it to the TODO section first, then ask whether to implement it now.
