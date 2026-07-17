# flatpak-smoke

`flatpak-smoke` is a CI-friendly verifier for freshly built Flatpak applications. It installs a Flatpak artifact into an isolated temporary user installation, starts the app in a headless graphical session, waits for a visible window, captures a screenshot, OCR-checks it for fatal error-screen markers, and writes a machine-readable result.

Runs fail if the first visible window is titled `Error`, or if screenshot OCR finds conservative fatal markers such as `secret portal error`, `unexpected error`, or `unhandled exception`.

The isolated session writes an `xdg-desktop-portal` config that uses the GTK portal backend by default and GNOME Keyring for `org.freedesktop.impl.portal.Secret`, then unlocks an empty per-run keyring before launching the app. This lets apps that need the Secret portal launch in the reference container without using host secrets.

## Usage

```sh
flatpak-smoke verify-bundle ./build/org.example.App.flatpak --output ./flatpak-smoke-output
flatpak-smoke verify-repo ./repo app/org.example.App/x86_64/stable --output ./flatpak-smoke-output
flatpak-smoke doctor
```

The output directory contains `result.json`, `screenshots/`, and `logs/`. Existing verifier artifacts cause the command to fail unless `--force` is provided.

To capture additional interaction screenshots, pass `--screenshot-after-click <BUTTON_LABEL>` one or more times. The verifier locates the matching button in the latest screenshot, clicks the button center, and captures another screenshot such as `001-after-click-preferences.png`.

Timeouts default to `--display-timeout 10s`, `--window-timeout 30s`, `--screenshot-timeout 10s`, and `--overall-timeout 60s`.

## Runtime Tools

The current backend uses an isolated D-Bus session plus an Xvfb display. The supported container/runtime needs:

- `flatpak`
- `dbus-run-session`
- `gnome-keyring-daemon`
- `Xvfb`
- `openbox`
- `tesseract`
- `xdg-desktop-portal`
- `xdg-desktop-portal-gtk`
- `xdotool`
- ImageMagick `import` and `convert`

The Secret portal also requires the GNOME Keyring portal descriptor at `/usr/share/xdg-desktop-portal/portals/gnome-keyring.portal`.

`flatpak-smoke doctor` checks this tool set.

## Container

Build the reference image with:

```sh
podman build -t flatpak-smoke -f Containerfile .
```

Flatpak commonly requires namespace support inside CI containers. A known-good starting point is:

```sh
podman run --rm --privileged \
  -v "$PWD:/workspace:Z" \
  -w /workspace \
  flatpak-smoke \
  verify-bundle ./build/org.example.App.flatpak --output ./flatpak-smoke-output
```

Tighten privileges for your runner once Flatpak sandboxing and user namespaces are confirmed to work.
