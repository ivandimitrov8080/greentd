#!/usr/bin/env bash
#
# A corrupted balance table must abort startup, and the message must name the
# field (`found-007`, third acceptance box; the loader is `found-004`).
#
# The corruption is a *validation* failure, not a parse failure: `hp_base: 0.0`
# is a perfectly good `f32`, and only the rule that a scaling base must be
# positive rejects it. That is the interesting case, because a parse failure
# would be caught by `ron` and would say nothing about field paths.
#
# Corrupting a copy rather than the working tree means this can run beside a
# build, in the same job, with no cleanup and no ordering constraint.
#
#   GREENTD_BIN   the binary to run   [target/debug/greentd]
#   GREENTD_PORT  the port to bind    [5297]

set -euo pipefail

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
bin=${GREENTD_BIN:-$root/target/debug/greentd}
port=${GREENTD_PORT:-5297}
expected='waves.scaling.hp_base'

if [[ ! -x "$bin" ]]; then
    echo "balance: $bin is not executable; run \`cargo build\` first" >&2
    exit 1
fi

tmp=$(mktemp -d)
out=$(mktemp)
trap 'rm -rf -- "$tmp" "$out"' EXIT

cp -r -- "$root/assets/balance" "$tmp/balance"
sed -i -E 's/(hp_base: )[0-9]+\.?[0-9]*/\10.0/' -- "$tmp/balance/waves.ron"

# A stale `sed` would make this test vacuous -- it would start a server on a
# table it never touched and report success. So the corruption is asserted
# before it is relied on.
if ! grep -q 'hp_base: 0.0' -- "$tmp/balance/waves.ron"; then
    echo "balance: could not corrupt hp_base in waves.ron; is the file's shape unchanged?" >&2
    exit 1
fi

status=0
NO_COLOR=1 "$bin" server --balance-dir "$tmp/balance" --log-dir "" \
    --bind "127.0.0.1:$port" >"$out" 2>&1 || status=$?

if [[ "$status" -eq 0 ]]; then
    echo "balance: a table with a zero hp_base started a match; the loader is not validating" >&2
    sed 's/^/  /' -- "$out" >&2
    exit 1
fi
if ! grep -qF -- "$expected" "$out"; then
    echo "balance: refused with exit $status but never named $expected" >&2
    sed 's/^/  /' -- "$out" >&2
    exit 1
fi

echo "balance: OK (refused with exit $status, named $expected)"
