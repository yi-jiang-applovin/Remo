# Remo CLI Docs

This file is the repository-maintainer guide for Remo CLI documentation.

## Source of Truth

The distributed, user-facing CLI guidance lives in the per-skill references:

- [`../skills/remo-setup/references/cli.md`](../skills/remo-setup/references/cli.md)
- [`../skills/remo/references/cli.md`](../skills/remo/references/cli.md)
- [`../skills/remo-capabilities/references/cli.md`](../skills/remo-capabilities/references/cli.md)
- [`../skills/remo-design-review/references/cli.md`](../skills/remo-design-review/references/cli.md)

There is no longer a single shared `skills/cli-reference.md`. This is intentional so every distributed skill folder stays self-contained.

## What This File Covers

Use this file when maintaining the repository to keep the CLI docs surface aligned across:

- [`../skills/remo-setup/references/cli.md`](../skills/remo-setup/references/cli.md)
- [`../skills/remo/references/cli.md`](../skills/remo/references/cli.md)
- [`../skills/remo-capabilities/references/cli.md`](../skills/remo-capabilities/references/cli.md)
- [`../skills/remo-design-review/references/cli.md`](../skills/remo-design-review/references/cli.md)
- [`../skills/README.md`](../skills/README.md)
- [`../skills/remo-setup/SKILL.md`](../skills/remo-setup/SKILL.md)
- [`../skills/remo/SKILL.md`](../skills/remo/SKILL.md)
- [`../skills/remo-capabilities/SKILL.md`](../skills/remo-capabilities/SKILL.md)
- [`../skills/remo-design-review/SKILL.md`](../skills/remo-design-review/SKILL.md)
- [`../README.md`](../README.md)
- [`../AGENTS.md`](../AGENTS.md)

## Update Checklist

Whenever the CLI changes, update all of the following as needed:

1. Command list and examples for any newly added, removed, or renamed command
2. Option names, defaults, and flag spellings
3. Connection semantics for `--addr` and `--device`
4. Output behavior for `screenshot`, `info`, and `call` (note whether the result is wrapped)
5. The "What moved" section listing anything dropped in the CDP rewrite (dashboard, mirror,
   daemon, watch) and where its replacement lives or is tracked
6. Known caveats and tracked gaps — e.g. `Remo.capabilitiesChanged` not being wired up yet, or
   the H.264 mirror not having landed as `Remo.startMirror`

## Current Known Gaps Worth Preserving

`remo` was rewritten (see the repo's CDP rewrite plan) to speak real Chrome DevTools Protocol
instead of a custom length-prefixed RPC — `remo-desktop` (dashboard, web mirror player) and
`remo-daemon` (connection pooling) were deleted as part of that, since `chrome://inspect`'s own
screencast/screenshot panels made them unnecessary. Two things from the old CLI have no CDP
equivalent yet and are tracked as follow-up work, not silently dropped:

- **`remo watch`** (capability-change event stream) — `Remo.capabilitiesChanged` isn't wired up
  server-side yet.
- **`remo mirror --save`** (H.264 recording) — planned to return as a `Remo.startMirror`/
  `Remo.stopMirror` CDP extension; until then there's no recording command at all. When it does
  land, keep documenting its old caveat: fixed per-frame sample durations in the MP4 muxer
  compress idle periods, so saved videos can be shorter than wall-clock time.
