#!/bin/sh
set -eu
if [ ! -S /run/dbus/system_bus_socket ]; then
    mkdir -p /run/dbus
    dbus-daemon --system --fork --nopidfile
fi
exec "$@"
