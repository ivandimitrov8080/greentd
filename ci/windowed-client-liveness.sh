#!/usr/bin/env bash
#
# The windowed-client liveness test (`audit-031`, D33).
#
# What it proves, and why a single-process job cannot:
#
# A client that renders used to stop running its `Update` schedule after one or
# two seconds. The cause is in the *presentation* stack, not in this crate: on
# Wayland, Mesa's Vulkan WSI blocks `vkQueuePresentKHR` in
# `wl_display_dispatch_queue` until the compositor releases a buffer, and a
# window that is not being presented never gets one. The render thread blocks in
# `present`, the main thread blocks behind it in `bevy_render`'s `extract`, and
# the peer stops receiving replication while it is away. Measured with a
# backtrace; see `audit-031`.
#
# The assertion is a *correspondence*, not a line count. The server starts a wave
# every `match_rules.wave_interval` seconds and broadcasts a `ServerNotice` for
# each; the client logs `notice: WaveStarted(N)` when it receives one. So this
# script joins the two logs on the wave number `N` and requires that every wave
# the server started *after the client connected* has a matching client notice,
# and that the notice arrived within a small tolerance of the wave. A stalled
# loop loses the notices that fell inside the stall and delays the rest, so it
# fails both halves of that check.
#
# `LIVENESS_HIDE=1` makes the failure deterministic rather than intermittent, by
# moving the window to the compositor's scratchpad for the middle stretch: the
# window stops being presented at a known instant, so a regression that
# re-introduces the blocking present fails here every time instead of one run in
# five. It needs `swaymsg` and a sway session; the default is off, because
# hiding a window on somebody's desktop is rude.
#
# Not wired into `.github/workflows/ci.yml`. It needs a display *and* a working
# Vulkan driver, and a hosted runner has neither, so it would be skipped there;
# a check that is always skipped is worse than a documented manual one. The
# in-process two-process harness that *would* be CI-able is `test-008` in
# `tasks/12-testing.org`, and it is not written yet.
#
# Run it from anywhere: `ci/windowed-client-liveness.sh`. It needs a built
# binary, so `cargo build` first. Overridable from the environment:
#
#   LIVENESS_BIN        the binary to run             [target/debug/greentd]
#   LIVENESS_PORT       the server port               [5198]
#   LIVENESS_HIDE       1 to hide the window mid-run  [0]
#   LIVENESS_SECONDS    seconds to watch the client   [65]
#   LIVENESS_TOLERANCE  seconds a notice may be late  [4]
#
# The knobs are not `GREENTD_*` on purpose: that prefix is reserved by
# `found-002` for config keys, and an unknown one in a child's environment is a
# startup error, so a knob named `GREENTD_LIVE_SECONDS` would make the very
# process this script starts refuse to run.

set -euo pipefail

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
bin=${LIVENESS_BIN:-$root/target/debug/greentd}
port=${LIVENESS_PORT:-5198}
hide=${LIVENESS_HIDE:-0}
seconds=${LIVENESS_SECONDS:-65}
tolerance=${LIVENESS_TOLERANCE:-4}
addr="127.0.0.1:$port"
client_bind="127.0.0.1:$((port + 100))"
srv_out=$(mktemp)
cli_out=$(mktemp)
srv_plain=$(mktemp)
cli_plain=$(mktemp)
title="Green TD - client"

server_pid=""
client_pid=""
cleanup() {
    for p in "$client_pid" "$server_pid"; do
        if [[ -n "$p" ]] && kill -0 "$p" 2>/dev/null; then
            kill -KILL "$p" 2>/dev/null || true
            wait "$p" 2>/dev/null || true
        fi
    done
    rm -f -- "$srv_out" "$cli_out" "$srv_plain" "$cli_plain"
}
trap cleanup EXIT

strip_ansi() {
    sed -e $'s/\033\\[[0-9;]*[A-Za-z]//g' -- "$1" >"$2"
}

fail() {
    strip_ansi "$cli_out" "$cli_plain" || true
    echo "liveness: FAIL: $*" >&2
    echo "--- client notice lines ---" >&2
    grep -E 'notice: WaveStarted|handshake' "$cli_plain" | tail -20 | sed 's/^/  /' >&2 || true
    echo "--- client output (tail) ---" >&2
    tail -15 "$cli_plain" | sed 's/^/  /' >&2 || true
    echo "----------------------------" >&2
    exit 1
}

# `N seconds` for every wave the server started.
server_ts() {
    grep -E 'wave [0-9]+ started' "$1" \
        | sed -E 's/.*T([0-9]+):([0-9]+):([0-9.]+)Z.*wave ([0-9]+) started.*/\4 \1 \2 \3/' \
        | awk '{ printf "%d %.3f\n", $1, $2 * 3600 + $3 * 60 + $4 }'
}

# `N seconds` for every wave notice the client received.
client_ts() {
    grep -E 'notice: WaveStarted\([0-9]+\)' "$1" \
        | sed -E 's/.*T([0-9]+):([0-9]+):([0-9.]+)Z.*WaveStarted\(([0-9]+)\).*/\4 \1 \2 \3/' \
        | awk '{ printf "%d %.3f\n", $1, $2 * 3600 + $3 * 60 + $4 }'
}

if [[ ! -x "$bin" ]]; then
    echo "liveness: $bin is not executable; run \`cargo build\` first" >&2
    exit 1
fi
if [[ -z "${WAYLAND_DISPLAY:-}${DISPLAY:-}" ]]; then
    echo "liveness: no display; this test needs a windowed peer" >&2
    exit 1
fi

echo "liveness: starting $bin server --bind $addr (headless)"
env -u DISPLAY -u WAYLAND_DISPLAY -u XDG_RUNTIME_DIR NO_COLOR=1 \
    "$bin" server --bind "$addr" --log-dir "" >"$srv_out" 2>&1 &
server_pid=$!
sleep 1

echo "liveness: starting $bin client --server $addr (windowed), watching ${seconds}s"
NO_COLOR=1 RUST_LOG=info,greentd=debug \
    "$bin" client --server "$addr" --bind "$client_bind" --log-dir "" >"$cli_out" 2>&1 &
client_pid=$!

waited=0
while (( waited < seconds )); do
    # The deterministic half: at roughly a third of the way in, take the window
    # away from the compositor, so the present that used to block here does.
    if [[ "$hide" == 1 ]]; then
        if (( waited == seconds / 3 )); then
            if command -v swaymsg >/dev/null 2>&1; then
                echo "liveness: hiding the window (swaymsg move scratchpad)"
                swaymsg "[title=\"$title\"] move scratchpad" >/dev/null 2>&1 || true
            else
                echo "liveness: LIVENESS_HIDE=1 but no swaymsg; skipping the hide" >&2
            fi
        elif (( waited == (seconds * 2) / 3 )); then
            command -v swaymsg >/dev/null 2>&1 \
                && swaymsg "[title=\"$title\"] scratchpad show" >/dev/null 2>&1 || true
        fi
    fi
    if ! kill -0 "$client_pid" 2>/dev/null; then
        wait "$client_pid" 2>/dev/null || true
        fail "the client exited before the run finished"
    fi
    sleep 1
    waited=$((waited + 1))
done

strip_ansi "$srv_out" "$srv_plain"
strip_ansi "$cli_out" "$cli_plain"

if grep -qiE "panicked|RUST_BACKTRACE" -- "$cli_plain"; then
    fail "the client panicked"
fi

# When the client got going: the first timestamped line of its log, which is
# what the handshake was racing. Waves before this are not its to miss.
client_start=$(grep -m1 -E 'T[0-9]+:[0-9]+:[0-9.]+Z' "$cli_plain" \
    | sed -E 's/.*T([0-9]+):([0-9]+):([0-9.]+)Z.*/\1 \2 \3/' \
    | awk '{ printf "%.3f", $1 * 3600 + $2 * 60 + $3 }')
if [[ -z "$client_start" ]]; then
    fail "the client logged nothing with a timestamp"
fi

report=$(awk -v start="$client_start" -v grace=2 -v tol="$tolerance" '
    FNR == NR { srv[$1] = $2; next }
    { cli[$1] = $2 }
    END {
        bad = 0; seen = 0
        for (n in srv) {
            if (srv[n] < start + grace) continue   # the client was not connected yet
            seen++
            if (!(n in cli)) {
                printf "no notice for wave %d (server started it %.0fs in)\n", n, srv[n] - start
                bad = 1
                continue
            }
            d = cli[n] - srv[n]
            if (d < 0) d = -d
            if (d > tol) {
                printf "wave %d notice was %.1fs late (tolerance %s)\n", n, d, tol
                bad = 1
            }
        }
        if (bad) exit 1
        if (seen < 3) { printf "only %d waves while the client was connected\n", seen; exit 1 }
        print seen
    }' <(server_ts "$srv_plain") <(client_ts "$cli_plain")) || fail "$report"

echo "liveness: OK ($report waves delivered, each within ${tolerance}s)"
