"""Call the private desktop helper and return its JSON response verbatim."""
import json
import os
from pathlib import Path
import sys
import gi

gi.require_version("Gio", "2.0")
from gi.repository import Gio, GLib


def owns_accessibility_proxy(proxy_pid, app_pid):
    # A Flatpak accessibility connection belongs to its dbus proxy, which can
    # outlive/reparent away from the launcher. Identify the listening socket
    # actually mounted in the selected app, using kernel inode information.
    try:
        mounted = Path(f"/proc/{app_pid}/root/run/flatpak/at-spi-bus").stat()
        runtime = Path(os.environ["XDG_RUNTIME_DIR"])
        sockets = set()
        for line in Path("/proc/net/unix").read_text().splitlines()[1:]:
            fields = line.split(maxsplit=7)
            if len(fields) != 8 or not Path(fields[7]).is_relative_to(runtime):
                continue
            try:
                host = Path(fields[7]).stat()
            except OSError:
                continue
            if (host.st_dev, host.st_ino) == (mounted.st_dev, mounted.st_ino):
                sockets.add(f"socket:[{fields[6]}]")
        for fd in Path(f"/proc/{proxy_pid}/fd").iterdir():
            try:
                if os.readlink(fd) in sockets:
                    return True
            except OSError:
                continue
        return False
    except (OSError, KeyError):
        return False


def enter_text(pid, text):
    """Use an acknowledged editable-text operation, preserving caret/selection."""
    gi.require_version("Atspi", "2.0")
    from gi.repository import Atspi
    desktop = Atspi.get_desktop(0)
    matches = []
    for index in range(desktop.get_child_count()):
        app = desktop.get_child_at_index(index)
        owner = app.get_process_id()
        if owner != pid and not owns_accessibility_proxy(owner, pid):
            continue
        pending = [app]
        visited = 0
        while pending:
            node = pending.pop()
            visited += 1
            if visited > 10000:
                raise RuntimeError("Accessibility tree exceeds traversal limit")
            states = node.get_state_set()
            if states.contains(Atspi.StateType.FOCUSED) and states.contains(Atspi.StateType.EDITABLE):
                if node.get_editable_text_iface() and node.get_text_iface():
                    matches.append(node)
            for child in range(node.get_child_count()):
                pending.append(node.get_child_at_index(child))
    if len(matches) != 1:
        raise RuntimeError(f"type_text requires one focused accessible editable control; found {len(matches)}")
    node = matches[0]
    readable = node.get_text_iface()
    editable = node.get_editable_text_iface()
    before = Atspi.Text.get_text(readable, 0, -1)
    start = end = Atspi.Text.get_caret_offset(readable)
    if Atspi.Text.get_n_selections(readable):
        selection = Atspi.Text.get_selection(readable, 0)
        start, end = selection.start_offset, selection.end_offset
    expected = before[:start] + text + before[end:]
    if not Atspi.EditableText.set_text_contents(editable, expected) or Atspi.Text.get_text(readable, 0, -1) != expected:
        raise RuntimeError("Focused control did not accept the requested text")
    if not Atspi.Text.set_caret_offset(readable, start + len(text)):
        raise RuntimeError("Could not restore caret after text entry")


def main():
    request = json.loads(sys.argv[1])
    connection = Gio.bus_get_sync(Gio.BusType.SESSION, None)
    if request["action"] == "keyring_ready":
        # Check ownership first so this probe cannot auto-start a competing daemon.
        owner = connection.call_sync(
            "org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus",
            "NameHasOwner", GLib.Variant("(s)", ("org.freedesktop.secrets",)),
            GLib.VariantType.new("(b)"), Gio.DBusCallFlags.NONE, int(sys.argv[2]), None,
        ).unpack()[0]
        ready = False
        if owner:
            try:
                locked = connection.call_sync(
                    "org.freedesktop.secrets", "/org/freedesktop/secrets/collection/login",
                    "org.freedesktop.DBus.Properties", "Get",
                    GLib.Variant("(ss)", ("org.freedesktop.Secret.Collection", "Locked")),
                    GLib.VariantType.new("(v)"), Gio.DBusCallFlags.NO_AUTO_START,
                    int(sys.argv[2]), None,
                ).unpack()[0]
                ready = locked is False
            except GLib.Error as error:
                # The daemon can own its bus name before exporting the collection.
                if Gio.DBusError.get_remote_error(error) not in (
                    "org.freedesktop.DBus.Error.UnknownObject",
                    "org.freedesktop.DBus.Error.UnknownMethod",
                ):
                    raise
        print(json.dumps({"ok": True, "result": ready}))
        return
    native = {**request, "action": "focus"} if request["action"] == "type_text" else request
    reply = connection.call_sync(
        "org.flatpak.Smoke.Desktop1", "/org/flatpak/Smoke/Desktop1",
        "org.flatpak.Smoke.Desktop1", "Call", GLib.Variant("(s)", (json.dumps(native),)),
        GLib.VariantType.new("(s)"), Gio.DBusCallFlags.NONE, int(sys.argv[2]), None,
    )
    response = json.loads(reply.unpack()[0])
    if response["ok"] and request["action"] == "ping":
        # Also prove that the private session can provide the accessibility bus
        # needed for acknowledged text entry.
        connection.call_sync("org.a11y.Bus", "/org/a11y/bus", "org.a11y.Bus", "GetAddress",
                             None, GLib.VariantType.new("(s)"), Gio.DBusCallFlags.NONE,
                             int(sys.argv[2]), None)
    if response["ok"] and request["action"] == "type_text":
        try:
            enter_text(response["result"]["pid"], request["text"])
        except Exception as error:
            response = {"ok": False, "error": str(error)}
    print(json.dumps(response))


if __name__ == "__main__":
    main()
