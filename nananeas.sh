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
binary="$project_dir/torrent-finder"
if [[ ! -x "$binary" ]]; then
    echo "Executable missing: $binary" >&2
    echo "Place the compiled torrent-finder executable alongside nananeas.sh." >&2
    exit 1
fi
cd -- "$project_dir"
exec "$binary" "$@"
