#!/usr/bin/env bash
set -euo pipefail

# =============================================================================
# Remo E2E Test
#
# Builds the SDK, CLI, and example app, then exercises every capability on a
# simulator and verifies the JSON responses.
#
# Usage:
#   ./scripts/e2e-test.sh                     # full run
#   SKIP_BUILD=1 ./scripts/e2e-test.sh        # skip build phase
#   ./scripts/e2e-test.sh --screenshots       # save screenshots
#
# Environment variables:
#   DEVICE_UUID    — simulator UDID (default: first booted device)
#   SKIP_BUILD     — set to 1 to skip build phase
#   ARTIFACTS_DIR  — where to save screenshots (default: /tmp/remo-e2e)
#   DERIVED_DATA_PATH — explicit Xcode DerivedData path for RemoExample builds
#   REMO_BIN       — path to remo binary (default: built from source)
#   REMO_USE_REMOTE — set to 1 to build the example app against published remo-spm
#
# Note: `remo` now speaks real CDP (see the CDP rewrite plan) and no longer
# has a `mirror`/`stop`/`status` command — the old --record (H.264 mirror
# recording) option is gone with it. The high-fidelity mirror is planned to
# come back as a `Remo.startMirror` CDP extension (see
# crates/remo-sdk/src/streaming.rs's module doc); until that lands, this
# script has no recording capability at all.
# =============================================================================

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# Options
OPT_SCREENSHOTS=false
for arg in "$@"; do
    case "$arg" in
        --screenshots) OPT_SCREENSHOTS=true ;;
        --help|-h)
            sed -n '3,/^# ====/p' "$0" | head -n -1 | sed 's/^# \?//'
            exit 0
            ;;
    esac
done

ARTIFACTS_DIR="${ARTIFACTS_DIR:-/tmp/remo-e2e}"
SKIP_BUILD="${SKIP_BUILD:-0}"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
CYAN='\033[0;36m'
BOLD='\033[1m'
RESET='\033[0m'

# Counters
PASS_COUNT=0
FAIL_COUNT=0

# Cleanup tracking
APP_LAUNCHED=false

cleanup() {
    echo ""
    echo -e "${CYAN}--- Cleanup ---${RESET}"
    if [ "$APP_LAUNCHED" = true ] && [ -n "${DEVICE_UUID:-}" ]; then
        echo "Terminating app..."
        xcrun simctl terminate "$DEVICE_UUID" com.remo.example 2>/dev/null || true
    fi
    # Kill any remaining remo processes to avoid orphan warnings in CI
    pkill -x remo 2>/dev/null || true
}
trap cleanup EXIT

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

log()  { echo -e "${CYAN}==>${RESET} ${BOLD}$*${RESET}"; }
pass() {
    PASS_COUNT=$((PASS_COUNT + 1))
    echo -e "  ${GREEN}PASS${RESET} $*"
}

fail() {
    FAIL_COUNT=$((FAIL_COUNT + 1))
    echo -e "  ${RED}FAIL${RESET} $*"
}

remo() {
    "$REMO_BIN" "$@" 2>/dev/null
}

# Call a capability and return the JSON response.
# Strips the "Calling '...' on ..." header line and any ANSI codes.
remo_call() {
    local capability="$1"
    local params="${2:-\{\}}"
    "$REMO_BIN" call -a "$ADDR" "$capability" "$params" 2>/dev/null \
        | sed 's/\x1b\[[0-9;]*m//g' \
        | sed -n '/^{/,/^}/p'
}

# Assert that a jq expression against the remo call result evaluates to true.
# Usage: assert_call "test name" "capability" '{"param":1}' '.status == "ok"'
#
# The jq path is against the capability's own JSON result directly — `remo
# call` (CDP's `Remo.invoke`) no longer wraps it in a `.data` envelope the
# way the old length-prefixed RPC's `Response` did.
assert_call() {
    local name="$1"
    local capability="$2"
    local params="$3"
    local jq_expr="$4"

    local result
    if ! result=$(remo_call "$capability" "$params" 2>&1); then
        fail "$name — call failed: $result"
        return
    fi

    if echo "$result" | jq -e "$jq_expr" >/dev/null 2>&1; then
        pass "$name"
    else
        fail "$name — expected $jq_expr, got: $(echo "$result" | jq -c '.' 2>/dev/null || echo "$result")"
    fi
}

# Take a screenshot if --screenshots is enabled.
maybe_screenshot() {
    local name="$1"
    if [ "$OPT_SCREENSHOTS" = true ]; then
        remo screenshot -a "$ADDR" -o "$ARTIFACTS_DIR/$name.jpg" --format jpeg --quality 0.8 2>/dev/null
        echo "  [screenshot: $name.jpg]"
    fi
}

resolve_device_uuid() {
    local booted_uuid available_uuid

    if [ -n "${DEVICE_UUID:-}" ]; then
        return 0
    fi

    booted_uuid="$(xcrun simctl list devices booted -j | jq -r '.devices[][] | select(.state == "Booted") | .udid' | head -1)"
    if [ -n "${booted_uuid}" ]; then
        DEVICE_UUID="${booted_uuid}"
        return 0
    fi

    available_uuid="$(xcrun simctl list devices available -j | jq -r '.devices[][] | select(.isAvailable == true and (.name | startswith("iPhone"))) | .udid' | tail -1)"
    if [ -z "${available_uuid}" ]; then
        echo -e "${RED}ERROR:${RESET} No available iPhone simulator found."
        exit 1
    fi

    DEVICE_UUID="${available_uuid}"
    xcrun simctl boot "$DEVICE_UUID" >/dev/null 2>&1 || true
    xcrun simctl bootstatus "$DEVICE_UUID" -b
}

build_example_app() {
    local -a xcodebuild_args
    xcodebuild_args=(
        clean
        build
        -workspace "$ROOT/examples/ios/RemoExample.xcworkspace"
        -scheme RemoExample
        -destination "platform=iOS Simulator,id=$DEVICE_UUID"
        -configuration Debug
        -quiet
    )

    if [ -n "${DERIVED_DATA_PATH:-}" ]; then
        xcodebuild_args+=(-derivedDataPath "$DERIVED_DATA_PATH")
    fi

    xcodebuild "${xcodebuild_args[@]}" 2>&1 | tail -5
}

find_app_path() {
    if [ -n "${DERIVED_DATA_PATH:-}" ]; then
        find "${DERIVED_DATA_PATH}" \
            -path "*/Debug-iphonesimulator/RemoExample.app" \
            -maxdepth 5 \
            -type d \
            -print0 2>/dev/null \
            | xargs -0 ls -td 2>/dev/null \
            | head -1
    else
        find ~/Library/Developer/Xcode/DerivedData \
            -path "*/Debug-iphonesimulator/RemoExample.app" \
            -maxdepth 5 \
            -type d \
            -print0 2>/dev/null \
            | xargs -0 ls -td 2>/dev/null \
            | head -1
    fi
}

# ---------------------------------------------------------------------------
# Phase 0: Build
# ---------------------------------------------------------------------------

log "Phase 0: Build"

if [ "$SKIP_BUILD" = "1" ]; then
    echo "  Skipping build (SKIP_BUILD=1)"
    REMO_BIN="${REMO_BIN:-$ROOT/target/debug/remo}"
    if [ ! -f "$REMO_BIN" ]; then
        echo -e "${RED}ERROR:${RESET} remo binary not found at $REMO_BIN"
        echo "Run without SKIP_BUILD or set REMO_BIN to an existing binary."
        exit 1
    fi
else
    log "Building iOS SDK (simulator)..."
    (cd "$ROOT" && make ios-sim)

    log "Building CLI..."
    (cd "$ROOT" && cargo build -p remo-cli)
    REMO_BIN="${REMO_BIN:-$ROOT/target/debug/remo}"

    log "Building example app..."
    resolve_device_uuid
    build_example_app

    # Find the built app bundle
    APP_PATH="$(find_app_path)"
    if [ -z "$APP_PATH" ]; then
        echo -e "${RED}ERROR:${RESET} Could not find built RemoExample.app in DerivedData"
        exit 1
    fi
fi

echo "  remo: $REMO_BIN"

# ---------------------------------------------------------------------------
# Phase 1: Install & Launch
# ---------------------------------------------------------------------------

log "Phase 1: Install & Launch"

resolve_device_uuid
DEVICE_NAME=$(xcrun simctl list devices -j | jq -r --arg uuid "$DEVICE_UUID" '.devices[][] | select(.udid == $uuid) | .name')
echo "  Device: $DEVICE_NAME ($DEVICE_UUID)"

# Find app path if not set during build phase
if [ -z "${APP_PATH:-}" ]; then
    APP_PATH="$(find_app_path)"
    if [ -z "$APP_PATH" ]; then
        echo -e "${RED}ERROR:${RESET} RemoExample.app not found. Run without SKIP_BUILD first."
        exit 1
    fi
fi

# Stop the app on ALL booted simulators so only our fresh instance advertises.
for uuid in $(xcrun simctl list devices booted -j | jq -r '.devices[][] | .udid'); do
    xcrun simctl terminate "$uuid" com.remo.example 2>/dev/null || true
done
# Uninstall on target device to wipe persisted state
xcrun simctl uninstall "$DEVICE_UUID" com.remo.example 2>/dev/null || true
sleep 1

echo "  Installing $APP_PATH..."
xcrun simctl install "$DEVICE_UUID" "$APP_PATH"

echo "  Launching..."
LAUNCH_OUTPUT=$(xcrun simctl launch "$DEVICE_UUID" com.remo.example)
APP_LAUNCHED=true
APP_PID=$(echo "$LAUNCH_OUTPUT" | grep -oE '[0-9]+$' || true)
echo "  PID: $APP_PID"

# ---------------------------------------------------------------------------
# Phase 2: Discover Port
# ---------------------------------------------------------------------------

log "Phase 2: Discover Remo Port"

# Strategy 1: Use lsof to find the TCP port the app is listening on.
# This is more reliable than Bonjour on CI runners where mDNS may not work.
ADDR=""
if [ -n "$APP_PID" ]; then
    for attempt in 1 2 3 4 5 6 7 8 9 10; do
        echo "  Attempt $attempt/10 (lsof)..."
        sleep 2
        PORT=$(lsof -nP -iTCP -sTCP:LISTEN -a -p "$APP_PID" 2>/dev/null \
            | awk '/LISTEN/{print $9}' \
            | grep -oE '[0-9]+$' \
            | head -1 || true)
        if [ -n "$PORT" ]; then
            ADDR="127.0.0.1:$PORT"
            # Verify with ping
            if remo_call __ping '{}' 2>/dev/null | jq -e '.pong == true' >/dev/null 2>&1; then
                echo "  Found device at $ADDR (via lsof)"
                break
            else
                ADDR=""
            fi
        fi
    done
fi

# Strategy 2: Fall back to Bonjour discovery if lsof didn't work.
# `remo devices`' Bonjour section prints one line per service like:
#   "  RemoExample._remo._tcp -> ws://127.0.0.1:9930/devtools/page/1 (dial with --addr 127.0.0.1:9930)"
if [ -z "$ADDR" ]; then
    echo "  Falling back to Bonjour discovery..."
    for attempt in 1 2 3 4 5; do
        echo "  Attempt $attempt/5 (Bonjour)..."
        sleep 3
        DEVICES_OUTPUT=$("$REMO_BIN" devices 2>/dev/null | sed 's/\x1b\[[0-9;]*m//g' || true)
        ADDR=$(echo "$DEVICES_OUTPUT" \
            | grep -oE -- '-> ws://[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+:[0-9]+/' \
            | sed -E 's#-> ws://([^/]+)/#\1#' \
            | head -1 || true)

        if [ -n "$ADDR" ]; then
            if remo_call __ping '{}' 2>/dev/null | jq -e '.pong == true' >/dev/null 2>&1; then
                echo "  Found device at $ADDR (via Bonjour)"
                break
            else
                ADDR=""
            fi
        fi
    done
fi

if [ -z "$ADDR" ]; then
    echo -e "${RED}ERROR:${RESET} Could not discover Remo device after all attempts"
    echo "  lsof output for PID $APP_PID:"
    lsof -nP -iTCP -a -p "$APP_PID" 2>&1 || echo "  (lsof failed)"
    echo "  remo devices output:"
    "$REMO_BIN" devices 2>&1 | head -20 || echo "  (remo devices failed)"
    exit 1
fi

# ---------------------------------------------------------------------------
# Phase 3: Setup Artifacts
# ---------------------------------------------------------------------------

rm -rf "$ARTIFACTS_DIR"
mkdir -p "$ARTIFACTS_DIR"

# ---------------------------------------------------------------------------
# Phase 4: Exercise & Assert
# ---------------------------------------------------------------------------

log "Phase 4: Exercise Capabilities"

# -- Connectivity --
echo ""
echo -e "${BOLD}[Connectivity]${RESET}"
assert_call "ping" "__ping" '{}' '.pong == true'
assert_call "device_info" "__device_info" '{}' '.system_name == "iOS"'
assert_call "app_info" "__app_info" '{}' '.bundle_id == "com.remo.example"'

maybe_screenshot "01-home-initial"

# -- UI Effects --
echo ""
echo -e "${BOLD}[UI Effects]${RESET}"
assert_call "ui.toast" "ui.toast" '{"message":"E2E test toast"}' '.status == "ok"'
sleep 1
maybe_screenshot "02-toast"
sleep 2  # wait for toast to dismiss (auto-hides after 3s)

assert_call "ui.confetti" "ui.confetti" '{}' '.status == "ok"'
sleep 0.5
maybe_screenshot "03-confetti"

assert_call "ui.setAccentColor" "ui.setAccentColor" '{"color":"purple"}' '.color == "purple"'
sleep 0.3
maybe_screenshot "04-accent-purple"

# -- Navigation --
echo ""
echo -e "${BOLD}[Navigation]${RESET}"
assert_call "navigate" "navigate" '{"route":"uikit"}' '.status == "ok"'
sleep 1
maybe_screenshot "05-uikit-grid"

# -- Grid: tab select --
echo ""
echo -e "${BOLD}[Grid]${RESET}"
assert_call "grid.tab.select (by id)" "grid.tab.select" '{"id":"feed"}' '.status == "ok" and .selectedTab.id == "feed"'
sleep 0.3
assert_call "grid.tab.select (by index)" "grid.tab.select" '{"index":1}' '.status == "ok" and .selectedTab.id == "items"'
sleep 0.3
remo_call "grid.tab.select" '{"index":0}' >/dev/null; sleep 0.3

assert_call "grid.feed.append" "grid.feed.append" '{"title":"E2E Card","subtitle":"automated"}' '.status == "ok" and .tab == "feed"'
sleep 0.3
maybe_screenshot "06-feed-appended"

# Scroll tests on items tab — 20 items guarantees visible scroll range
remo_call "grid.tab.select" '{"id":"items"}' >/dev/null; sleep 0.3
assert_call "grid.scroll.vertical (bottom)" "grid.scroll.vertical" '{"position":"bottom"}' '.status == "ok" and .position == "bottom" and .tab == "items"'
sleep 0.5
maybe_screenshot "07-scrolled-bottom"

assert_call "grid.scroll.vertical (top)" "grid.scroll.vertical" '{"position":"top"}' '.status == "ok" and .position == "top" and .tab == "items"'
sleep 0.3
remo_call "grid.tab.select" '{"id":"feed"}' >/dev/null; sleep 0.3

assert_call "grid.feed.reset" "grid.feed.reset" '{}' '.status == "ok" and .tab == "feed"'
sleep 0.3
maybe_screenshot "08-feed-reset"

assert_call "grid.scroll.horizontal (next)" "grid.scroll.horizontal" '{"direction":"next"}' '.status == "ok" and .selectedTab.id == "items"'
sleep 0.3
assert_call "grid.scroll.horizontal (previous)" "grid.scroll.horizontal" '{"direction":"previous"}' '.status == "ok" and .selectedTab.id == "feed"'
sleep 0.3

assert_call "grid.visible" "grid.visible" '{}' '.status == "ok" and .tab == "feed"'
sleep 0.3
maybe_screenshot "09-visible"

# -- Capability cleanup --
echo ""
echo -e "${BOLD}[Capabilities]${RESET}"
CAPABILITIES_OUTPUT=$(remo capabilities -a "$ADDR" 2>/dev/null | sed 's/\x1b\[[0-9;]*m//g' || true)
if echo "$CAPABILITIES_OUTPUT" | grep -Eq 'items\.(add|remove|clear)'; then
    fail "legacy items.* capabilities are not exposed"
else
    pass "legacy items.* capabilities are not exposed"
fi

# -- State round-trip --
echo ""
echo -e "${BOLD}[State]${RESET}"
assert_call "state.set" "state.set" '{"key":"username","value":"E2E User"}' '.status == "ok"'
sleep 0.3
assert_call "state.get" "state.get" '{"key":"username"}' '.value == "E2E User"'

# -- Remaining tabs (screenshots only, navigate already tested) --
remo_call "navigate" '{"route":"activity"}' >/dev/null; sleep 0.5
maybe_screenshot "10-activity"
remo_call "navigate" '{"route":"settings"}' >/dev/null; sleep 0.5
maybe_screenshot "11-settings"

# -- Screenshot capability --
echo ""
echo -e "${BOLD}[Screenshot]${RESET}"
SCREENSHOT_RESULT=$(remo screenshot -a "$ADDR" -o "$ARTIFACTS_DIR/test-screenshot.jpg" --format jpeg --quality 0.8 2>&1 || true)
if [ -f "$ARTIFACTS_DIR/test-screenshot.jpg" ] && [ -s "$ARTIFACTS_DIR/test-screenshot.jpg" ]; then
    pass "remo screenshot produces valid file"
else
    fail "remo screenshot — file missing or empty: $SCREENSHOT_RESULT"
fi

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------

echo ""
echo "==========================================="
TOTAL=$((PASS_COUNT + FAIL_COUNT))
echo -e " ${BOLD}Results: $TOTAL tests${RESET}"
echo -e "   ${GREEN}PASS: $PASS_COUNT${RESET}"
if [ "$FAIL_COUNT" -gt 0 ]; then
    echo -e "   ${RED}FAIL: $FAIL_COUNT${RESET}"
else
    echo -e "   FAIL: 0"
fi
echo "==========================================="

if [ "$OPT_SCREENSHOTS" = true ]; then
    echo -e " Artifacts: $ARTIFACTS_DIR"
    ls -1 "$ARTIFACTS_DIR" 2>/dev/null | sed 's/^/   /'
fi

if [ "$FAIL_COUNT" -gt 0 ]; then
    exit 1
fi
