#!/usr/bin/env bash
#
# The headless smoke test (`found-007`).
#
# What it proves, in order, and why each one needs a real process:
#
#   1. A dedicated server *starts with no display*. `DISPLAY`, `WAYLAND_DISPLAY`
#      and `XDG_RUNTIME_DIR` are unset for the child, so a server that had grown
#      a window again (`found-001`, D15) would die here with a winit error
#      instead of binding a socket.
#
#      "No display" and "no GPU" are different claims. This script can only
#      make the first one, because it cannot take the GPU away from the child.
#
#   2. It reaches the listening state: the ready line the CI job in
#      `.github/workflows/ci.yml` waits for is
#      `greentd::net: listening on <addr> (server)`.
#
#   3. It actually *steps the simulation*, rather than binding a socket and
#      idling. The observable is `greentd::sim: wave 1 started`, which the sim
#      logs once `match_rules.first_wave_delay` seconds of fixed time have
#      elapsed -- about 90 ticks at the default 30 Hz. Wall-clock and sim time
#      are the same clock here on purpose (`found-001`: a server's frame *is*
#      the sim's tick).
#
#   4. It exits on SIGTERM without a panic. A *graceful* shutdown, draining the
#      link and flushing state, is `net-007`'s and is deliberately not asserted
#      here; what is asserted is that the process dies when it is asked to, so a
#      CI job cannot hang on it.
#
# Run it from anywhere: `ci/smoke-server.sh`. It needs a built binary, so
# `cargo build` first. Overridable from the environment:
#
#   GREENTD_BIN           the binary to run        [target/debug/greentd]
#   GREENTD_SMOKE_PORT    the port to bind         [5199]
#   GREENTD_READY_TIMEOUT seconds to reach listening [30]
#   GREENTD_WAVE_TIMEOUT  seconds to step one wave  [30]
#   GREENTD_TERM_TIMEOUT  seconds to die on SIGTERM [10]

set -euo pipefail

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
bin=${GREENTD_BIN:-$root/target/debug/greentd}
port=${GREENTD_SMOKE_PORT:-5199}
ready_timeout=${GREENTD_READY_TIMEOUT:-30}
wave_timeout=${GREENTD_WAVE_TIMEOUT:-30}
term_timeout=${GREENTD_TERM_TIMEOUT:-10}
addr="127.0.0.1:$port"
output=$(mktemp)
# Matches run against the output with ANSI colour stripped. tracing colours its
# output even when stdout is not a terminal (that is what Bevy's `LogPlugin`
# asks for), so a plain `grep -F "greentd::net: listening"` would miss the line
# by the escapes between the target and the colon. `NO_COLOR=1` is set on the
# child as well, for a readable CI log; this is what makes the matching correct
# rather than merely conventional.
plain=$(mktemp)

strip_ansi() {
    sed -e $'s/\033\\[[0-9;]*[A-Za-z]//g' -- "$output" >"$plain"
}

pid=""
cleanup() {
    if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
        kill -KILL "$pid" 2>/dev/null || true
        wait "$pid" 2>/dev/null || true
    fi
    rm -f -- "$output" "$plain"
}
trap cleanup EXIT

fail() {
    echo "smoke: FAIL: $*" >&2
    echo "--- server output ---" >&2
    sed 's/^/  /' -- "$plain" >&2
    echo "---------------------" >&2
    exit 1
}

# Wait for a literal pattern to appear in the server's output, or give up.
wait_for() {
    local pattern=$1 timeout=$2 what=$3 waited=0
    while true; do
        strip_ansi
        grep -qF -- "$pattern" "$plain" && return 0
        if ! kill -0 "$pid" 2>/dev/null; then
            fail "the server exited before $what"
        fi
        if (( waited >= timeout * 10 )); then
            fail "$what did not happen within ${timeout}s (looked for $pattern)"
        fi
        sleep 0.1
        waited=$((waited + 1))
    done
}

if [[ ! -x "$bin" ]]; then
    echo "smoke: $bin is not executable; run \`cargo build\` first" >&2
    exit 1
fi
if [[ -n "$(command -v ss || true)" ]] && ss -lun 2>/dev/null | grep -q ":$port "; then
    echo "smoke: $addr is already in use; set GREENTD_SMOKE_PORT" >&2
    exit 1
fi

echo "smoke: starting $bin server --bind $addr (no display)"

# `env -u` rather than `VAR= cmd`: an empty DISPLAY is still a DISPLAY to winit,
# and the point of the test is that the server never asks for one.
env -u DISPLAY -u WAYLAND_DISPLAY -u XDG_RUNTIME_DIR NO_COLOR=1 \
    "$bin" server --bind "$addr" --log-dir "" >"$output" 2>&1 &
pid=$!

wait_for "greentd::net: listening on $addr (server)" "$ready_timeout" "the server listened"
echo "smoke: listening"

wait_for "greentd::sim: wave 1 started" "$wave_timeout" "the sim stepped a wave"
echo "smoke: the sim stepped (wave 1)"

echo "smoke: sending SIGTERM"
kill -TERM "$pid"
status=0
for (( i = 0; i < term_timeout * 10; i++ )); do
    if ! kill -0 "$pid" 2>/dev/null; then
        break
    fi
    sleep 0.1
done
if kill -0 "$pid" 2>/dev/null; then
    fail "still running ${term_timeout}s after SIGTERM"
fi
# 143 is 128 + SIGTERM, which is what a process that takes the default action
# exits with. 0 is a future graceful path that cleaned up first. Anything else
# is a signal the process did not ask for.
wait "$pid" || status=$?
pid=""
if [[ "$status" != 0 && "$status" != 143 ]]; then
    fail "exited $status on SIGTERM (expected 0 or 143)"
fi
if grep -qiE "panicked|RUST_BACKTRACE" -- "$plain"; then
    fail "the server panicked"
fi

echo "smoke: OK (bound $addr, stepped, exited $status)"
