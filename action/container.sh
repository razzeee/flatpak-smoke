#!/bin/sh
# Invoked as root to start the system bus, then optionally drop app privileges.
set -eu
user=$1
owner_uid=$2
owner_gid=$3
output=$4
shift 4
mkdir -p /run/dbus
dbus-daemon --system --fork --nopidfile
if [ "$user" = nobody ]; then
    install -d -o 65534 -g 65534 "$output"
    chown -hR 65534:65534 "$output"
else
    mkdir -p "$output"
fi
set +e
if [ "$user" = nobody ]; then
    setpriv --reuid=65534 --regid=65534 --clear-groups env HOME=/tmp USER=nobody "$@"
else
    "$@"
fi
status=$?
# Host runners must be able to upload logs and remove artifacts after failures.
if ! chown -hR "$owner_uid:$owner_gid" "$output"; then
    printf '%s\n' 'flatpak-smoke action: failed to restore output ownership' >&2
    if [ "$status" -eq 0 ]; then
        status=1
    fi
fi
exit "$status"
