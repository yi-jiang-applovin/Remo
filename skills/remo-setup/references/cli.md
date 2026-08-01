# Remo CLI Reference

This reference travels with the skill so the setup workflow does not depend on repository-only docs.

Remo speaks real Chrome DevTools Protocol now — `remo` is a thin CDP client, not a
protocol/daemon/dashboard bundle. `chrome://inspect` (or any CDP client) can drive the same
target directly; this CLI exists for the one thing a fixed protocol can't provide on its own:
invoking arbitrary, developer-named app capabilities (`Remo.invoke`).

## Install the Binary

Choose one install path:

```bash
# Project-local install (recommended for pinned repo installs)
env REMO_INSTALL_PREFIX="$PWD/.remo" \
  bash -c "$(curl -fsSL https://github.com/yjmeqt/Remo/releases/latest/download/install-remo.sh)"

# Homebrew (recommended for global installs)
brew install yjmeqt/tap/remo

# Build from source
cargo install --git https://github.com/yjmeqt/Remo.git remo-cli
```

Project-local installs place the binary at `.remo/bin/remo`. Global installs place `remo` on `PATH`.

Verify the install before continuing:

```bash
test -x .remo/bin/remo && .remo/bin/remo --help
command -v remo >/dev/null && remo --help
```

## Resolve the Binary

Use this order:

1. `.remo/bin/remo`
2. `remo`

Prefer the project-local binary for pinned installs.

## Global Options

```bash
remo --help
remo -v <command>
remo -vv <command>
remo -vvv <command>
```

## Command Summary

| Command | Purpose | Example |
|---------|---------|---------|
| `remo devices` | Discover simulators (Bonjour) and devices (USB), resolved to dialable `ws://` CDP URLs | `remo devices` |
| `remo call` | Invoke a capability (`Remo.invoke`) | `remo call -a $ADDR "__ping" '{}'` |
| `remo capabilities` | List registered capabilities (`Remo.listCapabilities`) | `remo capabilities -a $ADDR` |
| `remo tree` | Dump the view hierarchy | `remo tree -a $ADDR -m 4` |
| `remo screenshot` | Save a screenshot (`Page.captureScreenshot`) | `remo screenshot -a $ADDR -o shot.jpg` |
| `remo info` | Print device and app metadata | `remo info -a $ADDR` |

There is no `dashboard`/`start`/`stop`/`status`/`mirror` command anymore — see "What moved" below.

## Connection Model

Every command takes one of these:

- `-a, --addr <host:port>` for direct TCP (simulator; over Bonjour discovery for wired setups this resolves to `127.0.0.1:<port>`)
- `-d, --device <usb-device-id>` for a real device over usbmuxd, which overrides `--addr`

Simulator addresses can change on each launch. Re-run `remo devices` if a saved address stops working.

## Setup Verification Sequence

Use these commands in order:

```bash
remo devices
remo call -a <ADDRESS> "__ping" '{}'
remo screenshot -a <ADDRESS> -o /tmp/remo-verify.jpg
remo tree -a <ADDRESS>
```

## Command Notes

### `remo screenshot`

```bash
remo screenshot -a $ADDR -o shot.jpg
remo screenshot -a $ADDR -o shot.png --format png
remo screenshot -a $ADDR -o shot.jpg --format jpeg --quality 0.9
```

- screenshot output is written directly to the requested path
- this calls the standard CDP `Page.captureScreenshot` method directly, not a custom capability

### `remo call` result shape

`remo call`'s printed JSON is the capability's own result directly — there is no `.data` (or
`.result`) wrapper around it anymore. A capability that used to answer `{"data": {"status": "ok"}}`
now answers `{"status": "ok"}`.

## What moved

- **Dashboard / web mirror player / `remo mirror --web`**: gone. `chrome://inspect`'s own
  remote-device view already renders a live screencast for any CDP target, and DevTools' own
  Command Menu has "Capture screenshot" — there's no remaining reason for a bespoke dashboard.
- **`remo mirror --save` (H.264 recording)**: not yet ported. The high-fidelity mirror is planned
  to come back as a `Remo.startMirror`/`Remo.stopMirror` CDP extension (tracked separately, not
  silently dropped) — until it lands, use `xcrun simctl io ... recordVideo` for simulator
  recordings.
- **`remo start`/`remo stop`/`remo status` (local daemon)**: gone. There's no connection-pooling
  daemon anymore; `remo` dials the target directly for each command.
- **`remo watch` (event stream)**: gone for now. `Remo.capabilitiesChanged` isn't wired up on the
  server side yet, so there's currently no live event to watch.
- **`remo list` renamed to `remo capabilities`.**

## Built-ins

These built-in capabilities are always available (invoke with `remo call`, or the dedicated
`remo tree`/`remo info` commands, which are just `remo call` against these under the hood):

- `__ping`
- `__list_capabilities`
- `__view_tree`
- `__screenshot` (prefer `remo screenshot` / `Page.captureScreenshot` directly instead)
- `__device_info`
- `__app_info`

## Troubleshooting

| Symptom | What to do |
|---------|------------|
| `remo devices` shows nothing | Ensure the app is running and the CDP server is enabled on the debug path |
| Connection refused | Re-run `remo devices` and use the fresh address |
| Capability not found | Run `remo capabilities -a $ADDR` |
| Screenshot is black | Bring the simulator to the foreground |

## Zero-install alternative: the web console

Installing `remo` is optional, not required. Every Remo target also serves a self-contained
`http://<addr>/console` page — no download, just a browser (or a browser-automation tool an
agent already has). It covers the same core capability workflow as the CLI:

- A human-usable form: enter a capability name + JSON args, click Invoke; "List capabilities"
  populates a clickable list.
- A `window.remo` JS object for programmatic use: `await remo.listCapabilities()`,
  `await remo.invoke(name, args)`, and `await remo.call(method, params)` (the raw CDP escape
  hatch — any domain method, e.g. `remo.call("Page.captureScreenshot", {format: "jpeg"})`, not
  just `Remo.*`). Opening the page logs a one-line usage hint to the browser console, so it's
  self-discoverable without reading this file.

**If you (the agent) have a browser-automation tool available** (Playwright MCP,
`chrome-devtools-mcp`, or similar) and remo-cli isn't installed and the user didn't ask for it,
drive the console directly instead of asking the user to install anything:

1. Navigate the tool to `http://<addr>/console` (get `<addr>` from `chrome://inspect`'s device
   list, a known port, or Bonjour/USB discovery output if you have another way to get it without
   the CLI).
2. Evaluate JS in that page's context: `await window.remo.invoke("__ping", {})`,
   `await window.remo.listCapabilities()`, etc. — same result shapes as `remo call`/
   `remo capabilities` (`invoke`'s return value is already unwrapped, matching `remo call`'s
   `.data`-free output).
3. For a screenshot, `await window.remo.call("Page.captureScreenshot", {format: "jpeg", quality: 80})`
   returns `{data: <base64>}` — decode `data` to bytes the same way `remo screenshot` does.

This is not a replacement for `remo tree`/`remo info`/`remo screenshot`'s convenience (those still
need the CLI or hand-rolled equivalents against `__view_tree`/`__device_info`/`__app_info`), but
it means "no CLI installed yet" is never a hard blocker for basic capability invocation and
verification.
