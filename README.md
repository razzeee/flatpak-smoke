# flatpak-smoke

Headless smoke testing for Flatpak application builds.

`flatpak-smoke` installs a Flatpak artifact into an isolated temporary user installation, starts it inside a headless Weston Wayland session, waits for the app to draw a visible frame, captures screenshots, OCR-checks for common fatal error screens, and writes a CI-friendly `result.json`.

It is intended for build pipelines that need a fast answer to: "does this freshly built Flatpak install and show a real application window?"

## Basic Usage

From a source checkout, run the CLI through Cargo. The `--` separates Cargo arguments from `flatpak-smoke` arguments.

```sh
cargo run -- verify-bundle ./build/org.example.App.flatpak --output ./flatpak-smoke-output
```

If the binary is installed, run it directly:

```sh
flatpak-smoke verify-bundle ./build/org.example.App.flatpak --output ./flatpak-smoke-output
```

The output directory is protected. If it already contains `flatpak-smoke` artifacts, pass `--force` to replace them.

```sh
cargo run -- verify-bundle ./build/org.example.App.flatpak \
  --output ./flatpak-smoke-output \
  --force
```

## Commands

### Verify A Bundle

Use `verify-bundle` for a `.flatpak` file produced by `flatpak build-bundle`.

```sh
cargo run -- verify-bundle ./build/org.example.App.flatpak \
  --output ./flatpak-smoke-output
```

If the bundle requires runtimes or extensions from remotes such as Flathub, allow network remotes:

```sh
cargo run -- verify-bundle ./build/org.example.App.flatpak \
  --output ./flatpak-smoke-output \
  --allow-network-remotes
```

### Verify A Local Repo Ref

Use `verify-repo` for an app ref from a local Flatpak OSTree repository.

```sh
cargo run -- verify-repo ./repo app/org.example.App/x86_64/stable \
  --output ./flatpak-smoke-output
```

### Check The Environment

Use `doctor` to check whether the normal verification toolchain is available.

```sh
cargo run -- doctor
```

## What A Run Does

For each verification run, `flatpak-smoke`:

1. Prepares the output directory.
2. Checks required runtime tools.
3. Creates an isolated temporary Flatpak/XDG user environment.
4. Writes an `xdg-desktop-portal` config that prefers the GTK portal backend and GNOME Keyring for the Secret portal.
5. Installs the bundle or repo ref.
6. Starts a headless Weston Wayland compositor.
7. Launches the app through `dbus-run-session` and `flatpak run`.
8. Waits for a visible Wayland frame by comparing compositor screenshots before and after launch.
9. Captures screenshots and OCR-checks them for fatal error markers.
10. Writes `result.json`.

## Failure Conditions

A run fails when any of these happen:

- Required tools are missing.
- The artifact path or app ref is invalid.
- The app cannot be installed.
- The headless Wayland compositor cannot start.
- The app exits before a visible Wayland frame appears.
- No visible Wayland frame appears before `--window-timeout`.
- Screenshot OCR finds fatal markers such as `secret portal error`, `unexpected error`, `fatal error`, or `unhandled exception`.

## Screenshots

By default, a successful run captures one screenshot:

```text
screenshots/000-window-visible.png
```

Every requested screenshot must contain visible app content. A run fails if a screenshot file is captured but only contains a solid compositor background.

Interaction screenshots can be requested with `--screenshot-after-click <BUTTON_LABEL>`. The Wayland-only backend does not currently have a generic pointer injection mechanism, so requested interaction screenshots fail explicitly instead of passing with missing artifacts.

## Output Directory

Each run writes:

```text
flatpak-smoke-output/
  result.json
  screenshots/
    000-window-visible.png
  logs/
    app.stderr.log
    app.stdout.log
    runner.log
    wayland-baseline.png
    wayland-readiness.png
    wayland-window-detection.png
    weston.stderr.log
    weston.stdout.log
```

`result.json` is stable and intended for CI parsing.

```json
{
  "schema_version": 1,
  "status": "passed",
  "app_ref": "app/org.example.App/x86_64/stable",
  "artifact": {
    "kind": "bundle",
    "path": "./build/org.example.App.flatpak"
  },
  "timings_ms": {
    "install": 1200,
    "launch_to_window": 2400,
    "total": 4100
  },
  "screenshots": [
    "screenshots/000-window-visible.png"
  ],
  "failure": null
}
```

On failure, `status` is `failed` and `failure` contains a machine-readable reason plus a human-readable message.

## Timeouts

Default timeouts:

| Option | Default | Meaning |
| --- | ---: | --- |
| `--display-timeout` | `10s` | Time allowed for Weston to become usable. |
| `--window-timeout` | `30s` | Time allowed for the app to draw a visible Wayland frame. |
| `--screenshot-timeout` | `10s` | Time allowed for each screenshot capture. |
| `--overall-timeout` | `60s` | Total run budget. |

Durations support plain seconds or `ms`, `s`, and `m` suffixes.

```sh
cargo run -- verify-bundle ./build/org.example.App.flatpak \
  --output ./flatpak-smoke-output \
  --overall-timeout 5m \
  --window-timeout 45s
```

## Runtime Requirements

Normal verification requires:

- `flatpak`
- `dbus-run-session`
- `gnome-keyring-daemon`
- `weston`
- `weston-screenshooter`
- `tesseract`
- `xdg-desktop-portal`
- `xdg-desktop-portal-gtk`
- ImageMagick `compare` and `identify`
- GNOME Keyring portal descriptor at `/usr/share/xdg-desktop-portal/portals/gnome-keyring.portal`

Check the normal toolchain with:

```sh
cargo run -- doctor
```

## Container Usage

Build the reference image:

```sh
podman build -t flatpak-smoke -f Containerfile .
```

Run verification inside the container:

```sh
podman run --rm --privileged \
  -v "$PWD:/workspace:Z" \
  -w /workspace \
  flatpak-smoke \
  verify-bundle ./build/org.example.App.flatpak \
  --output ./flatpak-smoke-output
```

Flatpak often needs namespace support inside CI containers. `--privileged` is a known-good starting point; tighten privileges for your runner once Flatpak sandboxing and user namespaces are confirmed to work.

## Troubleshooting

### The Output Directory Already Exists

Use `--force` to replace prior `result.json`, `screenshots/`, and `logs/` artifacts.

### Runtime Dependencies Are Missing

Use `--allow-network-remotes` if the bundle needs runtimes or extensions that are not available in the isolated Flatpak user installation.

### The App Needs More Time

Increase `--overall-timeout` and `--window-timeout` for slow installs or first launches.
