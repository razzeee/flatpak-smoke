# flatpak-smoke

Headless smoke testing for Flatpak application builds.

`flatpak-smoke` installs a Flatpak artifact into an isolated temporary user installation, starts it inside a headless Weston Wayland session, waits for the app to draw a visible frame, captures screenshots, OCR-checks for common fatal error screens, and writes a CI-friendly `result.json`.

It is intended for build pipelines that need a fast answer to: "does this freshly built Flatpak install and show a real application window?"

It also captures native application-window screenshots using an author-written
recipe in a headless GNOME session. These commands produce ordered PNGs and
captions for app listings. See [Screenshot recipes](#screenshot-recipes).

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
6. Starts a headless Weston Wayland compositor with a local VNC backend for screenshots and pointer input.
7. Launches the app through `dbus-run-session` and `flatpak run`.
8. Waits for visible app content to persist across at least three samples spanning at least 750ms, comparing each sample with the compositor background. A blank sample restarts observation; animation is allowed.
9. Captures screenshots and OCR-checks them for fatal error markers.
10. Writes `result.json`.

## Failure Conditions

A run fails when any of these happen:

- Required tools are missing.
- The artifact path or app ref is invalid.
- The app cannot be installed.
- The headless Wayland compositor cannot start.
- The app exits at any point before verification finishes, including during OCR or after a click. This reports `early_exit`.
- Weston exits after startup. This reports `display_exited`.
- Visible app content does not persist for the observation period before `--window-timeout`.
- Screenshot OCR finds fatal markers such as `secret portal error`, `unexpected error`, `fatal error`, or `unhandled exception`.

## Screenshots

By default, a successful run captures one screenshot:

```text
screenshots/000-window-visible.png
```

Every requested screenshot must contain visible app content. A run fails if a screenshot file is captured but only contains a solid compositor background.

The runner checks app and compositor liveness while waiting on image tools, VNC
I/O, and interaction delays, and once more before reporting success. A short
observation period cannot prove that startup has finished or distinguish a
long-lived splash screen from the main window.

Interaction screenshots can be requested with `--screenshot-after-click <BUTTON_LABEL>`. The label is located in the previous screenshot with OCR, with a matching primary-action visual fallback for small inverse button labels, clicked through the local Weston VNC backend, and captured as a required follow-up artifact:

```sh
cargo run -- verify-bundle ./build/org.example.App.flatpak \
  --output ./flatpak-smoke-output \
  --screenshot-after-click "Log In"
```

The first requested click writes `screenshots/001-after-click-log-in.png`; later clicks increment the prefix. A run fails if the click text cannot be found, the pointer event cannot be delivered, the follow-up screenshot is blank, or the screenshot does not visibly change before `--screenshot-timeout`.

## Output Directory

Each run writes:

```text
flatpak-smoke-output/
  result.json
  screenshots/
    000-window-visible.png
    001-after-click-log-in.png
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

Failures retain earlier captured screenshots in `screenshots`. If a complete
candidate frame was captured, the list also includes `logs/last-candidate.png`,
even when that frame did not satisfy readiness or post-click checks. This
diagnostic image may contain only the compositor background.

## Timeouts

Default timeouts:

| Option | Default | Meaning |
| --- | ---: | --- |
| `--display-timeout` | `10s` | Time allowed for Weston to become usable. |
| `--window-timeout` | `30s` | Time allowed for visible app content to pass the observation period. |
| `--screenshot-timeout` | `10s` | Time allowed for each screenshot capture. |
| `--overall-timeout` | `60s` | Total run budget. |

Durations support plain seconds or `ms`, `s`, and `m` suffixes.

Screenshot conversion, image comparisons, OCR, and VNC reads and writes share the
remaining stage and overall timeout budgets. Receiving partial VNC data does not
restart the deadline. Cleanup can extend the run slightly beyond its timeout:
each owned process group gets up to 200ms to stop before forced termination.

`SIGINT` and `SIGTERM` cancel the run through the normal failure path. The runner
cleans up its app, compositor, and helper process groups, including background
children whose parent has already exited. The daemonized keyring unlock service
is cleaned up separately using the run's private runtime directory.

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
- `tesseract`
- `xdg-desktop-portal`
- `xdg-desktop-portal-gtk`
- ImageMagick `compare`, `convert`, and `identify`
- GNOME Keyring portal descriptor at `/usr/share/xdg-desktop-portal/portals/gnome-keyring.portal`
- Weston VNC PAM service at `/etc/pam.d/weston-remote-access`

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

## Readiness regression fixtures

The Wayland fixture accepts `FLATPAK_SMOKE_FIXTURE_MODE` with `normal`, `delayed`,
`never-draw`, `crash-after-frame`, `animated`, or `exit-after-click`. The default
is `normal`. These modes exercise delayed rendering, window timeouts, crashes
during observation, continuously changing content, and exits during interaction.

After building the container and fixture bundle, run the mode checks with:

```sh
python3 tests/fixture_modes.py
```

CI runs these checks on pull requests and retains each mode's result and images
under `target/fixture-modes/`.

## Screenshot recipes

`screenshot-bundle` and `screenshot-repo` capture Wayland app windows in an
isolated GNOME session. They include native decorations, shadows, rounded
corners, and transparency. Listing images contain neither the desktop wallpaper
nor the pointer. The tool saves native pixels without adding shadows or scaling
the exported PNG.

```sh
flatpak-smoke screenshot-bundle ./org.example.App.flatpak \
  --recipe ./screenshots.yml --output ./capture-output \
  --allow-network-remotes

flatpak-smoke screenshot-repo ./repo app/org.example.App/x86_64/stable \
  --recipe ./screenshots.yml --output ./capture-output
```

The first backend supports GNOME Shell **48**, tested with 48.7. The dedicated
image supplies its dependencies, English locale, and platform-default appearance:

```sh
docker build -t flatpak-smoke-screenshots -f Containerfile.screenshots .
docker run --rm --privileged --tmpfs /run \
  -v "$PWD:/workspace" -w /workspace \
  flatpak-smoke-screenshots \
  flatpak-smoke screenshot-bundle ./org.example.App.flatpak \
  --recipe ./screenshots.yml --output ./capture-output \
  --allow-network-remotes
```

Use `--tmpfs /run` for the container's private runtime state and system bus. The
entrypoint starts that system bus, then executes the supplied command. A host
installation needs the same GNOME version and dependencies; check them with:

```sh
flatpak-smoke doctor --desktop gnome
```

### Writing a recipe

```yaml
version: 1
desktop: gnome
language: en
window:
  width: 900
  height: 600
steps:
  - wait_text: Ready to capture
  - capture:
      name: overview
      caption: Browse the initial view
  - type_text: example
  - key: Return
  - wait_text: Results for example
    timeout: 15s
  - capture:
      name: search
      caption: Find items by name
  - click_text: Preferences
  - select_window:
      title: Preferences
  - capture:
      name: preferences
      caption: Choose how the app behaves
```

This example runs against the GTK and Qt screenshot fixtures in `fixtures/screenshot/`.
Adapt labels, shortcuts, and capture points to your app. Each step contains
exactly one action and an optional `timeout`:

| Action | Behavior |
| --- | --- |
| `click_text` | Click a unique OCR label in the selected window. Missing or ambiguous labels fail with candidate locations logged. |
| `type_text` | Insert literal UTF-8 text through the focused control's accessibility interface, preserving its caret/selection and verifying the resulting text. Controls without an accessible editable-text interface fail explicitly. |
| `key` | Send a key or chord, such as `Return`, `Escape`, `Tab`, `Alt+F4`, or `Ctrl+comma`. |
| `wait_text` | Poll the selected window until normalized text appears. Use this to confirm navigation or text entry. |
| `select_window` | Wait for an app-owned window with an exact `title`, then retain its window handle. Multiple matches fail. |
| `capture` | Observe visible content, then save a named native-window image with its caption. |

Supported modifiers are `Ctrl`, `Shift`, `Alt`, and `Super`. Keys include printable
ASCII characters, `space`, `comma`, `plus`, `Return`, `Escape`, `Tab`, `BackSpace`,
`Delete`, arrow keys, `Home`, `End`, `PageUp`, `PageDown`, and `F1` through `F12`.

Capture names must be unique and contain only ASCII letters, digits, hyphens, or
underscores. Captions must be nonempty single lines without a trailing full stop.
The first capture is the default listing image; arrange the recipe accordingly.

The initial window defaults to 900×600 logical pixels and is unmaximized before
capture. Set `window.title` when the app has several principal windows. Requested
and secondary-window sizes must fit within 1000×700 logical pixels. Shadows can
extend the PNG beyond the frame dimensions. Version one supports English and
scale factor one. The desktop is selected explicitly, not inferred from the
Flatpak runtime; a future KDE backend can use the same recipe actions.

Screenshot commands default to a five-minute overall timeout, 30 seconds for
desktop startup, 30 seconds for initial window readiness, and ten seconds per
recipe step. Override these with `--overall-timeout`, `--display-timeout`,
`--window-timeout`, and `--screenshot-timeout`. Per-step timeouts are bounded by
the overall deadline. Readiness permits animation and requires at least three
visible samples spanning 750ms.

The content check measures the window interior, excluding shadows, an eight-pixel
edge inset, and the upper header region, up to 64 logical pixels. This prevents
decorations alone from qualifying a blank client area. It is a heuristic; recipes
for apps whose meaningful content is confined to the header need a populated
view before capture. The exported image still contains the complete native window.

### Images and metadata

The output directory contains `result.json`, `screenshots.json`, listing PNGs
under `screenshots/`, and diagnostics under `logs/`. For the example above:

```text
screenshots/000-overview.png
screenshots/001-search.png
screenshots/002-preferences.png
```

`screenshots.json` has its own schema version and records desktop/version,
language, scale, app ref, recipe version, and ordered `captures`. Each capture
contains its name, relative path, caption, PNG dimensions, and logical window
dimensions. `failed_step_index` is zero-based, or null on success and for failures
outside recipe execution. Earlier completed captures remain available on failure.
`result.json` uses the existing run-result schema.

OCR uses separate enlarged diagnostic images, including a contrast pass for small
button labels. These copies and their TSV output stay under `logs/`; they never
replace listing PNGs. Failed runs also retain the last complete candidate image.
Use `--force` to replace an existing capture output, including its screenshot manifest.

### Content and future data bundles

Every screenshot run starts with its own home, desktop configuration, Flatpak
installation, and per-app data. It does not reuse the caller's application profile,
theme, or language settings. This also makes repeated recipes start from the same
fresh state.

The private session enables accessibility for confirmed text entry. `doctor
--desktop gnome` starts a temporary headless session and contacts the native
helper and accessibility bus, in addition to checking installed tools.

Authors still select useful content and review the images against the
[Flathub quality guidelines](https://docs.flathub.org/docs/for-app-authors/metainfo-guidelines/quality-guidelines#screenshots).
Default data can leave a content-oriented app in an empty state. Passing a recipe
does not certify the editorial quality of its screenshots or publish them.

The data-preparation step runs after installation and before desktop/app launch.
A future importer can restore author-provided data there. Export from a normal
user session, external-document mapping, and a portable bundle format are future
work; version one has no data import/export command.
