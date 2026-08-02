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
```

For a structural (view hierarchy) check, there's no `remo tree` command — open `chrome://inspect`
or a `devtools://` URL against `<ADDRESS>` and use the real Elements panel instead (see "What
moved" below).

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
- **`remo tree` (view hierarchy dump) and the `__view_tree`/`__screenshot` built-in capabilities
  it (and the old `__screenshot`) called**: removed. Both duplicated what real CDP already does
  better — `DOM.getDocument` backs the actual, live, inspectable/highlightable Elements panel, and
  `Page.captureScreenshot` is what `remo screenshot` already calls directly. Open
  `chrome://inspect` or a `devtools://` URL against the target instead of `remo tree`.

## Built-ins

These built-in capabilities are always available (invoke with `remo call`, or the dedicated
`remo info` command, which is just `remo call` against two of them under the hood):

- `__ping`
- `__list_capabilities`
- `__device_info`
- `__app_info`
- `userDefaults.list` / `userDefaults.get {key}` / `userDefaults.set {key, value}` /
  `userDefaults.delete {key}` — generic `NSUserDefaults` access, any app
- `filesystem.list {path?}` / `filesystem.read {path}` / `filesystem.delete {path}` — sandbox
  file browsing (relative paths resolve against the sandbox home directory; `filesystem.read`
  returns `{"size", "data_base64"}`)
- `sqlite.query {path, sql}` — arbitrary SQL against any `.sqlite`/`.db` file in the sandbox;
  returns `{"columns", "rows"}` for a SELECT or `{"rows_affected"}` otherwise

## Calling capabilities from the real Console panel (no CLI needed)

Any Remo target's real Chrome DevTools Console (`chrome://inspect`/a `devtools://` URL) can call
`Remo.invoke` directly — no `remo call`, no CLI at all. Type `remo` alone to see a self-describing
preview of every registered capability (grouped by namespace, e.g. `{grid: {…}, ping: ƒ}`); a
capability's own dots become real object nesting, so `grid.tab.select` is called as
`remo.grid.tab.select({"id": "feed"})`. Tab-completion works — typing `remo.` suggests real,
currently-registered names, not guesses. This is the same underlying mechanism `remo call` uses
(`Remo.invoke`), just reachable without installing anything.

**Browsing vs. inspecting one capability**: expanding a parent object (e.g. clicking into `remo`
or `remo.grid`) shows each child truncated to just `name()` — enough to see what exists, not what
it does. To see a capability's full annotation (`function ping() { [remo capability: ping] }`,
its complete dotted name), evaluate it directly instead of browsing to it:

```js
remo.ping          // prints the full annotation inline, not just "ƒ ping()"
remo.grid.tab.select
```

Hovering a truncated `ƒ name()` in a parent listing also shows the same full text as a tooltip,
without needing to re-evaluate it.

## Troubleshooting

| Symptom | What to do |
|---------|------------|
| `remo devices` shows nothing | Ensure the app is running and the CDP server is enabled on the debug path |
| Connection refused | Re-run `remo devices` and use the fresh address |
| Capability not found | Run `remo capabilities -a $ADDR` |
| Screenshot is black | Bring the simulator to the foreground |
