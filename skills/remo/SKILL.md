---
name: remo
description: Use when Remo is already integrated and you need day-to-day iOS verification with screenshots, capability calls, checkpoints, or a reviewable report of what changed.
---

# Remo

Use this skill for the normal development loop after setup is complete.

Read `references/cli.md` before running commands, when you need exact CLI syntax, or when you need to know what moved in the CDP rewrite (no more `mirror`/`dashboard`/`start`/`stop`/`status`/`watch`).

## Core Loop

Follow this loop for each task:

1. Connect to the running app.
2. Capture a baseline.
3. Make a code change.
4. Rebuild and checkpoint with screenshots, the Elements panel's view hierarchy, or capability calls.
5. Record pass or fail evidence in a verification report.
6. Repeat until the task is verified.

## Step 1: Connect

Discover the app, store the current address, and verify connectivity before writing the report.

Use `references/cli.md` for the exact commands. At minimum:

- discover devices
- ping the selected target
- capture device and app metadata

## Step 2: Start a Verification Session

Create a task-specific directory:

```text
.remo/verifications/<task-id>/
  report.md
  assets/
```

Use a short task identifier such as `fix-avatar-radius` or `add-settings-screen`.

Record these fields in the report header:

- date and time
- device name and OS
- app bundle ID and version
- branch name

## Step 3: Capture the Baseline

Before making changes, capture the current screen and any supporting state that will matter later.

Typical baseline evidence:

- screenshot
- the Elements panel's view hierarchy (`chrome://inspect`/a `devtools://` URL — there's no
  `remo tree` command; this is real CDP `DOM.getDocument`)
- capability output for relevant internal state

Add a short observation describing what is wrong or what you expect to change.

## Step 4: Checkpoint After Each Change

After each meaningful build:

1. capture a screenshot
2. check the Elements panel's view hierarchy if layout or navigation changed
3. call relevant verification capabilities if internal state matters
4. compare the result against your expectation
5. mark the step as pass or fail

Every failed checkpoint gets its own report entry. Do not overwrite failed evidence.

## Step 5: Add Verification Nodes When Needed

If the UI does not expose enough information, register Remo-only verification capabilities.

Good candidates:

- read hidden internal state
- force a specific state such as empty, error, or logged out
- navigate directly to a screen
- seed test data

Pattern:

```swift
import RemoSwift

// The #Remo debug island strips all nested Remo code from release builds automatically.
.task {
    await #Remo {
        struct VerifyFeedCountResponse: Encodable {
            let count: Int
        }

        enum VerifyFeedCount: RemoCapability {
            static let name = "verify.feed_count"
            typealias Response = VerifyFeedCountResponse
        }

        await #remoScope {
            #remoCap(VerifyFeedCount.self) { _ in
                return VerifyFeedCountResponse(count: FeedRepository.shared.items.count)
            }
        }
    }
}
```

`#remoCap` handlers execute on a background callback path, so any UI mutation or actor-isolated work must be explicitly handed off instead of performed directly in the callback.

The `#Remo` debug island ensures all nested Remo code is fully stripped from release builds — no manual `#if DEBUG` wrappers needed.

## Step 6: Write the Summary

End each verification with a summary table covering:

- baseline captured
- each checkpoint
- pass or fail status
- total steps

The report should let another reviewer understand exactly what changed and what was verified without rerunning the task.

## Verification Modes

Choose the lightest mode that proves the behavior:

- screenshot for visual checks
- the Elements panel for structure and hierarchy
- capability call for hidden state
- multi-screen navigation checks for regression coverage

There is no `remo mirror` anymore — the high-fidelity recording path hasn't been ported to CDP
yet (see `references/cli.md`'s "What moved"). For a moving interaction, fall back to
`xcrun simctl io ... recordVideo`, or Chrome's own `Page.startScreencast`-backed screencast panel
in `chrome://inspect`.
