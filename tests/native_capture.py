"""Native GNOME capture contract. Run inside Containerfile.screenshots native-tests."""
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]


def process_group_running(group):
    for path in Path("/proc").iterdir():
        if not path.name.isdecimal():
            continue
        try:
            fields = (path / "stat").read_text().rsplit(")", 1)[1].split()
        except (FileNotFoundError, ProcessLookupError):
            continue
        if int(fields[2]) == group and fields[0] not in ("Z", "X"):
            return True
    return False


def stop_process_group(child):
    # The leader can exit before its descendants finish writing session state.
    # Reap it, but wait for the whole group before removing the temporary home.
    for sig, grace in ((signal.SIGTERM, .2), (signal.SIGKILL, 3)):
        try:
            os.killpg(child.pid, sig)
        except ProcessLookupError:
            pass
        deadline = time.monotonic() + grace
        while time.monotonic() < deadline:
            child.poll()
            if not process_group_running(child.pid):
                child.wait(timeout=3)
                return
            time.sleep(.01)
    raise RuntimeError(f"process group {child.pid} survived cleanup")


def run():
    output = ROOT / "target/native-capture"
    output.mkdir(parents=True, exist_ok=True)
    children = []
    with tempfile.TemporaryDirectory() as directory:
        home = Path(directory)
        env = dict(os.environ, HOME=str(home), XDG_CONFIG_HOME=str(home / "config"),
                   XDG_DATA_HOME=str(home / "data"), XDG_CACHE_HOME=str(home / "cache"),
                   XDG_RUNTIME_DIR=str(home / "runtime"), XDG_CURRENT_DESKTOP="GNOME",
                   XDG_SESSION_TYPE="wayland", GSETTINGS_BACKEND="keyfile",
                   LIBGL_ALWAYS_SOFTWARE="1", GSK_RENDERER="cairo", GTK_A11Y="none",
                   QT_QPA_PLATFORM="wayland", WAYLAND_DISPLAY="wayland-0")
        for name in ["DISPLAY", "DBUS_SESSION_BUS_ADDRESS"]:
            env.pop(name, None)
        (home / "runtime").mkdir(mode=0o700)
        extension = home / "data/gnome-shell/extensions/capture@flatpak-smoke"
        shutil.copytree(ROOT / "desktop/gnome", extension)
        subprocess.run(["gsettings", "set", "org.gnome.shell", "enabled-extensions",
                        "['capture@flatpak-smoke']"], env=env, check=True)
        bus_file = home / "bus"
        log = (output / "desktop.log").open("w")
        shell = subprocess.Popen([
            "dbus-run-session", "--", "sh", "-c",
            'printf %s "$DBUS_SESSION_BUS_ADDRESS" > "$1"; exec gnome-shell --headless --wayland --virtual-monitor=1280x900 --no-x11',
            "session", str(bus_file),
        ], env=env, stdout=log, stderr=log, start_new_session=True)
        children.append(shell)

        def call(request):
            reply = subprocess.run(["python3", str(ROOT / "desktop/gnome/client.py"),
                                    json.dumps(request), "2000"], env=env,
                                   capture_output=True, text=True, timeout=3)
            if reply.returncode:
                raise RuntimeError(reply.stderr)
            response = json.loads(reply.stdout)
            if not response["ok"]:
                raise RuntimeError(response["error"])
            return response["result"]

        def until(operation, timeout=20):
            deadline = time.monotonic() + timeout
            last_error = None
            while time.monotonic() < deadline:
                if shell.poll() is not None:
                    raise RuntimeError("GNOME exited; see " + str(output / "desktop.log"))
                try:
                    result = operation()
                    if result:
                        return result
                except (RuntimeError, FileNotFoundError) as error:
                    last_error = error
                time.sleep(0.1)
            raise RuntimeError(f"Wait timed out: {last_error}")

        try:
            until(lambda: bus_file.exists())
            env["DBUS_SESSION_BUS_ADDRESS"] = bus_file.read_text()
            print(until(lambda: call({"action": "ping"})), flush=True)
            for toolkit in ["gtk", "qt"]:
                child = subprocess.Popen(["python3", str(ROOT / f"fixtures/screenshot/{toolkit}.py")],
                                         env=env, stdout=log, stderr=log, start_new_session=True)
                children.append(child)
                windows = until(lambda: [w for w in call({"action": "windows"}) if w["pid"] == child.pid])
                window = windows[0]
                window = call({"action": "resize", "window": window["id"], "width": 900, "height": 600})
                time.sleep(0.5)
                image = output / f"{toolkit}.png"
                info = call({"action": "capture", "window": window["id"], "path": str(image)})
                assert info["frame"]["width"] == 900 and info["frame"]["height"] == 600, info
                alpha = subprocess.check_output(["convert", str(image), "-alpha", "extract", "-format", "%[fx:minima] %[fx:maxima]", "info:"], text=True).split()
                assert float(alpha[0]) < 0.1 and float(alpha[1]) == 1, alpha
                print(toolkit, info, "alpha", alpha, flush=True)
                # Tab navigation exercises input without relying on a particular button color.
                diagnostic = output / f"{toolkit}-ocr.png"
                subprocess.run(["convert", str(image), "-background", "white", "-alpha", "remove", "-alpha", "off", "-resize", "200%", "-colorspace", "Gray", "-threshold", "60%", str(diagnostic)], check=True)
                text = subprocess.check_output(["tesseract", str(diagnostic), "stdout", "--psm", "11", "tsv"], text=True)
                words = [line.split("\t") for line in text.splitlines()[1:]]
                match = next(row for row in words if len(row) >= 12 and row[11].strip("()[]|") == "Preferences")
                call({"action": "click", "window": window["id"],
                      "x": (int(match[6]) + int(match[8]) // 2) / 2,
                      "y": (int(match[7]) + int(match[9]) // 2) / 2})
                dialog = until(lambda: next((w for w in call({"action": "windows"})
                                             if w["pid"] == child.pid and w["title"] == "Preferences"), None))
                call({"action": "capture", "window": dialog["id"], "path": str(output / f"{toolkit}-preferences.png")})
                stop_process_group(child)
        finally:
            for child in reversed(children):
                stop_process_group(child)
            subprocess.run(["fusermount3", "-uz", str(home / "runtime/doc")], capture_output=True, timeout=3)
            log.close()


if __name__ == "__main__":
    run()
