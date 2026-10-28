#!/usr/bin/env bash
set -euo pipefail

# Only changes the soft limit inherited by this application.
project_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
requested="${NANANEAS_NOFILE:-32768}"
if [[ ! "$requested" =~ ^[1-9][0-9]{0,8}$ ]]; then
    echo 'NANANEAS_NOFILE must be a positive integer (at most 9 digits).' >&2
    exit 1
fi
current="$(ulimit -Sn)"
if [[ "$current" != unlimited ]] && (( 10#$current < 10#$requested )); then
    if ! ulimit -Sn "$requested"; then
        echo "Cannot raise open-file limit to $requested; hard limit: $(ulimit -Hn)." >&2
        exit 1
    fi
fi
binary="$project_dir/target/release/torrent-finder"
if [[ ! -x "$binary" ]]; then
    echo "Executable missing: $binary" >&2
    echo "Build first: cd \"$project_dir\" && cargo build --release" >&2
    exit 1
fi
exec "$binary" "$@"
