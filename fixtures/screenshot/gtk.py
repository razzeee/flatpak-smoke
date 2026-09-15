#!/usr/bin/python3
import gi
import os
from pathlib import Path

gi.require_version("Gtk", "4.0")
gi.require_version("Adw", "1")
from gi.repository import Adw, Gio, GLib, Gtk


class Fixture(Adw.Application):
    def do_activate(self):
        self.preferences_windows = []
        quit_action = Gio.SimpleAction.new("quit", None)
        quit_action.connect("activate", lambda *args: self.quit())
        self.add_action(quit_action)
        self.set_accels_for_action("app.quit", ["<Primary>q"])
        preferences = Gio.SimpleAction.new("preferences", None)
        preferences.connect("activate", lambda *args: self.preferences(self.window))
        self.add_action(preferences)
        self.set_accels_for_action("app.preferences", ["<Primary>comma"])
        window = Adw.ApplicationWindow(application=self, title="GTK Screenshot Fixture")
        window.set_default_size(600, 400)
        box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=24)
        box.append(Adw.HeaderBar())
        config = Path(os.environ.get("XDG_CONFIG_HOME", str(Path.home() / ".config")))
        config.mkdir(parents=True, exist_ok=True)
        marker = config / "screenshot-fixture-marker"
        label = Gtk.Label(label="Reused profile" if marker.exists() else "Ready to capture")
        marker.write_text("created by fixture")
        print(f"fixture config: {config}", flush=True)
        entry = Gtk.Entry(placeholder_text="Search")
        entry.set_halign(Gtk.Align.CENTER)
        def search(entry):
            text = entry.get_text()
            print("submitted text: " + text, flush=True)
            if text == "delay":
                label.set_text("Loading")
                GLib.timeout_add(700, lambda: (label.set_text("Results for delay"), False)[1])
            else:
                label.set_text("Results for " + text)
            if text == "duplicates":
                box.append(Gtk.Button(label="Duplicate", halign=Gtk.Align.CENTER))
                box.append(Gtk.Button(label="Duplicate", halign=Gtk.Align.CENTER))
            if text == "blank":
                empty = Gtk.Box(orientation=Gtk.Orientation.VERTICAL)
                empty.append(Adw.HeaderBar())
                empty.append(Gtk.DrawingArea(vexpand=True))
                window.set_content(empty)
        entry.connect("activate", search)
        box.append(label)
        box.append(entry)
        button = Gtk.Button(label="Preferences", halign=Gtk.Align.CENTER)
        button.connect("clicked", lambda button: self.preferences(window))
        box.append(button)
        progress = Gtk.ProgressBar(halign=Gtk.Align.CENTER)
        progress.set_size_request(300, -1)
        box.append(progress)
        def animate():
            progress.set_fraction((progress.get_fraction() + 0.1) % 1.0)
            return True
        GLib.timeout_add(80, animate)
        window.set_content(box)
        window.present()
        self.window = window

    def preferences(self, parent):
        window = Adw.Window(title="Preferences", transient_for=parent)
        window.set_default_size(400, 300)
        box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL)
        box.append(Adw.HeaderBar())
        box.append(Gtk.Label(label="Choose how the app behaves", vexpand=True))
        window.set_content(box)
        window.present()
        self.preferences_windows.append(window)


Fixture(application_id="org.example.ScreenshotGtk").run(None)
