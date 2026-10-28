#!/usr/bin/python3
"""Two-version persistence fixture; the broken candidate loses a database record."""
import argparse
import configparser
import os
from pathlib import Path
import sqlite3

import gi
gi.require_version("Gtk", "4.0")
from gi.repository import Gio, Gtk

parser = argparse.ArgumentParser()
parser.add_argument("--probe-path", type=Path, required=True)
parser.add_argument("--refuse-quit", action="store_true")
options = parser.parse_args()
assert not options.probe_path.exists(), "caller profile is visible"
try:
    options.probe_path.write_text("must not write caller data")
except OSError:
    pass
else:
    raise AssertionError("caller profile is writable")

version = Path("/app/version").read_text().strip()
data = Path(os.environ["XDG_DATA_HOME"])
config = Path(os.environ["XDG_CONFIG_HOME"])
data.mkdir(parents=True, exist_ok=True)
config.mkdir(parents=True, exist_ok=True)
document = data / "document.txt"
settings = config / "settings.ini"
database = sqlite3.connect(data / "records.sqlite")
database.execute("CREATE TABLE IF NOT EXISTS records (id INTEGER PRIMARY KEY, title TEXT)")
if version != "old":
    assert database.execute("PRAGMA user_version").fetchone()[0] == 1
    database.execute("ALTER TABLE records ADD COLUMN migrated INTEGER DEFAULT 1")
    if version == "broken":
        database.execute("DELETE FROM records")
    database.execute("PRAGMA user_version = 2")
    database.commit()


class Fixture(Gtk.Application):
    def do_activate(self):
        window = Gtk.ApplicationWindow(application=self, title="Update fixture")
        window.set_default_size(700, 500)
        box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=20)
        box.set_margin_top(30)
        box.set_margin_bottom(30)
        window.set_child(box)
        box.append(Gtk.Label(label="Version " + version))
        box.append(Gtk.Label(label="Caller profile isolated"))
        self.entry = Gtk.Entry()
        self.entry.set_text(document.read_text() if document.exists() else "")
        box.append(self.entry)
        prefs = configparser.ConfigParser()
        prefs.read(settings)
        box.append(Gtk.Label(label="Theme " + prefs.get("preferences", "theme", fallback="default")))
        row = database.execute("SELECT title FROM records WHERE id = 1").fetchone()
        box.append(Gtk.Label(label="Record " + (row[0] if row else "missing")))
        self.status = Gtk.Label(label="Ready")
        box.append(self.status)
        for name, callback, shortcut in [("save", self.save, "<Primary>s"), ("quit", self.stop, "<Primary>q")]:
            action = Gio.SimpleAction.new(name, None)
            action.connect("activate", callback)
            self.add_action(action)
            self.set_accels_for_action("app." + name, [shortcut])
        window.present()
        self.entry.grab_focus()

    def save(self, *args):
        document.write_text(self.entry.get_text())
        database.execute("INSERT OR REPLACE INTO records (id, title) VALUES (1, ?)", (self.entry.get_text(),))
        database.execute("PRAGMA user_version = " + ("1" if version == "old" else "2"))
        database.commit()
        self.status.set_text("Saved changes")

    def stop(self, *args):
        if options.refuse_quit:
            return
        # This setting intentionally only flushes on graceful shutdown.
        settings.write_text("[preferences]\ntheme=ocean\n")
        database.close()
        self.quit()


Fixture(application_id="org.example.UpdateFixture").run([])
