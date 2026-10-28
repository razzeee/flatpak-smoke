# Screenshot compatibility

Screenshot recipes have been tested with these x86_64 app builds on GNOME Shell
48.7, using software rendering, English, 1× scale, and a non-root app process:

| App | Version | Runtime | Tested actions |
| --- | --- | --- | --- |
| GNOME Calculator | 51.0 | GNOME 50 | Initial view and keyboard calculation |
| KCalc | 26.04.3 | KDE 6.10 | Initial view and keyboard calculation |
| VLC | 3.0.23 | KDE 5.15-25.08 | Initial view and opening the playlist by keyboard |
| VSCodium | 1.135.06055 | Freedesktop SDK 25.08 | Package README and Settings opened by keyboard |

These results cover the listed builds and actions, not every GTK, Qt, or Electron
app. VSCodium's accessible text entry and VLC video playback are not covered.
Review captured images before publishing; successful runs do not guarantee
pixel-identical results or listing quality.

## App-specific setup

**GNOME Calculator:** request a window at least 616 pixels tall. The tested recipe
uses 400×650. A size below the app's minimum fails window readiness.

**VSCodium:** run as a non-root user. In the GitHub Action, set `user: nobody`.
See [non-root containers](reference.md#non-root-containers) for local runs.

**VLC:** a fresh profile opens a modal **Privacy and Network Access Policy** dialog
that blocks input to the main window. Select and handle the dialog in your recipe,
or disable it explicitly:

```yaml
launch:
  args:
    - --no-one-instance
    - --no-qt-privacy-ask
    - --no-metadata-network-access
```

These arguments are recipe choices; the runner does not add them. A longer wait
does not dismiss a modal dialog. For playback recipes, wait for the intended media
state and account for window-title changes when selecting a window.
