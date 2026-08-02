---
name: example-ios-feature-dev
description: Use when building or changing a feature inside examples/ios/RemoExamplePackage/ and you need the agentic dev loop — register Remo capabilities for the new surface, then drive and validate it with the remo CLI plus XcodeBuildMCP.
---

# Example iOS Feature Dev

Use this skill when working on the bundled example app at `examples/ios/` as a
Remo repo contributor. It assumes the SDK itself already builds — its job is 
the UI feature loop on top.

This skill is **repo-internal**. Downstream projects consuming the Remo SDK
should use `remo-setup` / `remo-capabilities` / `remo` instead.

## When to use

- Adding or changing a screen, store, or controller under
  `examples/ios/RemoExamplePackage/Sources/RemoExampleFeature/`
- Redesigning an existing demo (e.g. the `UIKitDemo*` grid) to match a Figma
  mock
- Any example-app task where you need an agent to both drive state and
  visually confirm the result

## Prerequisites

- XcodeBuildMCP available — **always load the `xcodebuildmcp-cli` skill before
  calling any XcodeBuildMCP tool** (required by `AGENTS.md`).
- Remo CLI reachable at `.remo/bin/remo` or on `$PATH`.
- SDK XCFramework built for the simulator (see step 3 caveat).
- For Figma-driven work, load `figma:figma-implement-design` first to pull
  design context.

## The loop

```
scope → register → build → connect → drive/capture → iterate → report
```

### 1. Scope the surface

- Read the current implementation of the feature you are changing. For
  UIKit demos, start from `UIKitDemoViewController.swift` and the matching
  `*+Remo.swift` file — that pair is the template for everything below.
- Identify what the agent needs to be able to do to verify the change:
  navigation (pick tab, scroll), reads (what is visible, how many items),
  writes (append, reset, seed).
- Sketch the capability list before touching code. Prefer extending the
  existing `grid.*` / `<feature>.*` namespace over inventing a new one.

### 2. Register capabilities first

Mirror the pattern in
`examples/ios/RemoExamplePackage/Sources/RemoExampleFeature/UIKitDemo/UIKitDemoViewController+Remo.swift`:

- Put everything behind `#if DEBUG`. Use the `#Remo { … }` island — do **not**
  wrap nested code in manual `#if DEBUG` blocks.
- Group names in a `<Feature>CapabilityNames` enum using
  `<feature>.<group>.<action>` (e.g. `grid.tab.select`).
- Declare `RemoCapability` types with `Request` / `Response` structs that are
  `Encodable` / `Decodable`, `Equatable`, `Sendable`.
- Register handlers inside `#remoScope(scopedTo: self) { … }` so they tear
  down with the owning object.
- Funnel every handler through a `handle<Feature>Capability { … }` helper
  that maps thrown `…CapabilityError` cases to a structured error response
  with a `status` or `error` field.
- UIKit access must run on the main actor. Use a `*CapabilityBridge`
  wrapper (see `UIKitDemoCapabilityBridge`) so background `#remoCap`
  callbacks hop to `MainActor` cleanly.

Good capabilities to register for a new screen:

- **navigation** — select tab, push/pop, scroll to position
- **state reads** — which items are visible, current tab, totals
- **state writes / seeding** — append, reset, force empty/error
- **UI actions** — tap specific controls that are hard to reach visually

If a capability would be valuable but is not yet safe to implement, leave a
`// TODO(remo):` comment rather than a half-wired handler.

### 3. Build for the simulator

Build the SDK XCFramework first, then the example app via XcodeBuildMCP.

Use XcodeBuildMCP (not raw `xcodebuild`) to build and launch the example
app. The xcworkspace is `examples/ios/RemoExample.xcworkspace`, scheme
`RemoExample`. Boot a simulator, install, and launch before moving on.

### 4. Connect and verify registration

Read `skills/remo/references/cli.md` for exact flag syntax. At minimum:

```bash
.remo/bin/remo devices
.remo/bin/remo call -a "$ADDR" __ping '{}'
.remo/bin/remo list -a "$ADDR"
```

`remo list` must show every new capability you registered. If one is
missing, the app did not rebuild or the `#remoCap` is outside `#remoScope` —
fix before continuing.

### 5. Drive and capture

Run a checkpoint loop. For each state you want to verify:

1. **Drive** state with `remo call -a "$ADDR" <capability> '<json>'`.
2. **Wait** for animations to settle before capturing. The pager and scroll
   animations in the demo take ~300 ms; after a tab switch or scroll, allow
   roughly 3 s before a screenshot so the capture matches the settled state.
3. **Capture** via XcodeBuildMCP's simulator screenshot tool. Prefer it over
   `remo screenshot` when you want the lossless device-native PNG for a
   design-review diff. For structural checks, open the running app's
   `devtools://devtools/bundled/inspector.html?ws=<addr>/devtools/page/1` with
   `open -a "Google Chrome" "<that URL>"` (skips `chrome://inspect`'s manual
   "Configure..." step) and use the real Elements panel — there's no
   `remo tree` command anymore.
4. **Read** hidden state with the read capabilities you registered
   (`<feature>.visible`, counts, selected tab) to assert non-visual
   behaviour.
5. **Compare** against the expectation. Record pass/fail.

Save artifacts under `.remo/verifications/<task-id>/` (or, for a design
review, `review/<task-id>/` at repo root — the `review/` layout seen in the
current branch is the pattern).

### 6. Iterate

On failure, edit → re-run step 3's build → re-checkpoint. Do **not** overwrite
a failed screenshot or report entry; append a new checkpoint so the retry log
is reconstructable.

### 7. Write the report

Close with a short markdown summary containing:

- target app + scheme + simulator UDID + OS
- Figma node IDs (if design-driven)
- per-file change summary
- per-checkpoint table with pass/fail and the CLI call used to drive it
- known deviations from the design
- retry log (what failed, what you changed)

The `review/new-collection-design.md` on the current branch is a good
reference shape.

## Rules

- Capability code lives with the feature it describes, under `#if DEBUG`,
  inside `#Remo { … }`. No `#if DEBUG` inside the island.
- Capability names use `<feature>.<group>.<action>` and are centralized in a
  `<Feature>CapabilityNames` enum.
- Every handler returns a typed response with `status` on success and `error`
  on failure. Funnel through a single `handle…Capability` helper.
- UIKit work runs on `MainActor` — always through a bridge, never directly in
  the `#remoCap` callback.
- Load `xcodebuildmcp-cli` before using XcodeBuildMCP tools, and
  `skills/remo/references/cli.md` before using `remo` commands.
- On Tart / shared-folder worktrees, always set
  `CARGO_TARGET_DIR=/tmp/remo-cargo-target` for SDK builds.
- Allow ~3 s settle after a navigation/scroll capability before capturing.
- Don't overwrite failed checkpoint evidence — append.
