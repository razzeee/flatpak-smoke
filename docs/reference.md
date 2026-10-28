# CLI reference

For app-author setup and CI integration, start with the [quick start](../README.md).
Commands below that use Cargo or local Containerfiles assume you are
in a checkout of the flatpak-smoke repository.

`flatpak-smoke` installs a Flatpak artifact into an isolated temporary user installation, starts it inside a headless Weston Wayland session, waits for the app to draw a visible frame, captures screenshots, OCR-checks for common fatal error screens, and writes a CI-friendly `result.json`.

It also captures native application-window screenshots using an author-written
recipe in a headless GNOME session. These commands produce ordered PNGs and
captions for app listings. See [Screenshot recipes](#screenshot-recipes).

## Basic usage

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

### Verify a bundle

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

### Verify a local repo ref

Use `verify-repo` for an app ref from a local Flatpak OSTree repository.

```sh
cargo run -- verify-repo ./repo app/org.example.App/x86_64/stable \
  --output ./flatpak-smoke-output
```

### Check the environment

Use `doctor` to check whether the normal verification toolchain is available.

```sh
cargo run -- doctor
```

## Run isolation

Each run starts with a fresh temporary home, Flatpak user installation, and app
profile, including when using the host CLI. Verification waits for visible app
content across at least three samples spanning 750ms before capturing it.
Animation is allowed; a long-lived splash screen can still pass this check.

## Failure conditions

A run fails when any of these happen:

- Required tools are missing.
- The artifact path or app ref is invalid.
- The app cannot be installed.
- The headless Wayland compositor cannot start.
- The app exits at any point before verification finishes, including during OCR or after a click. This reports `early_exit`. A successful launcher exit is allowed while a matching Flatpak instance remains in the run's private runtime directory; an unsuccessful launcher exit still fails.
- Weston exits after startup. This reports `display_exited`.
- Visible app content does not persist for the observation period before `--window-timeout`.
- Screenshot OCR finds fatal markers such as `secret portal error`, `unexpected error`, `fatal error`, or `unhandled exception`.

## Screenshots

By default, a successful run captures one screenshot:

```text
screenshots/000-window-visible.png
```

Every requested screenshot must contain visible app content. A solid compositor
background or a blank window does not qualify. The verifier estimates window
regions from connected pixel differences against the pre-launch background. It
checks each region separately, excluding a 32-pixel edge margin for shadows and
borders and an additional header strip of up to 64 pixels. This is a heuristic,
not native window geometry: tiny windows, overlapping windows, unusual shadows,
or meaningful content confined to the header can defeat this estimate. The
saved screenshot retains the full desktop pixels.

Interaction screenshots can be requested with `--screenshot-after-click <BUTTON_LABEL>`. The label is located in the previous screenshot with OCR, with a matching primary-action visual fallback for small inverse button labels, clicked through the local Weston VNC backend, and captured as a required follow-up artifact:

```sh
cargo run -- verify-bundle ./build/org.example.App.flatpak \
  --output ./flatpak-smoke-output \
  --screenshot-after-click "Log In"
```

The first requested click writes `screenshots/001-after-click-log-in.png`; later clicks increment the prefix. A run fails if the click text cannot be found, the pointer event cannot be delivered, the follow-up screenshot is blank, or the screenshot does not visibly change before `--screenshot-timeout`.

## Output directory

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
  "phase_metadata": {
    "app": { "ref": "app/org.example.App/x86_64/stable", "commit": "0123456789abcdef" },
    "runtimes": [
      { "ref": "runtime/org.freedesktop.Platform/x86_64/24.08", "commit": "abcdef0123456789" }
    ],
    "recipe": null
  },
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

`phase_metadata` in `result.json` (schema version 1)
reports what the verifier or screenshot runner found in its own private Flatpak
installation after installation: `app` is the installed app, and `runtimes` is
the list of installed runtime refs. Each entry has a Flatpak `ref` and a
`commit`; the commit is `null` when Flatpak could not report it. `runtimes` is
`null` when Flatpak could not enumerate them, and an empty array when the query
succeeded but found none. Before installation, app and runtime metadata are
`null`. Screenshot recipe metadata is
hashed before dependency installation, so it can be populated even when a later
dependency check fails; only app metadata remains `null` before installation.
For screenshot runs,
`recipe.content_hash` is the `md5:`-prefixed
MD5 of the exact recipe file bytes; it is `null` when recipe content cannot be
read. Verification runs have `recipe: null`. These fields supplement, rather
than replace, `app_ref` and `screenshots.json` metadata.

Screenshot `phase_metadata.recipe.input_digest` is a nullable field
in schema version 1. It has format `v1:sha256:<64 lowercase hex digits>` and
covers the exact parsed YAML bytes plus every referenced `setup.files` file or
directory tree. Unlike the YAML-only `content_hash`, it detects
fixture-only changes. It is populated only after setup and launch-argument
preparation succeed, before the app starts; earlier failures leave it `null`.
Older results may omit it. Consumers should compare digests of the same version
and treat missing/null values as unknown, not as an unchanged input set.

The digest includes file names and empty directories, but excludes absolute
checkout paths, timestamps, permissions, and app-generated data. Moving an
identical recipe tree preserves it. Keep source files unchanged during setup;
copying is not an atomic snapshot of concurrent edits.

On failure, `status` is `failed` and `failure` contains a machine-readable reason plus a human-readable message.

Screenshot runs can additionally report `failure.category`:

| Category | Meaning |
| --- | --- |
| `infrastructure` | A known capture mechanism failure: helper disconnect/transport/invalid JSON, snapshot storage/cleanup, screenshot publication, or an otherwise unclassified final helper query failure. |
| `expectation` | An unmet observable expectation: no matching window before its deadline, or a click-text target that is missing or ambiguous. This does not establish whether the app or recipe is at fault. |

For example, a disconnected native helper reports
`{"reason":"screenshot_failed","category":"infrastructure","message":"native capture helper disconnected"}`.
Category is optional. Missing or null category means **unclassified**, not
recipe failure. Older runners, verification runs, and capture paths without a
known classification omit it. In particular, `screenshot_failed` alone does not
distinguish infrastructure from app/recipe behavior. Consumers should tolerate
unknown categories and retain their own verdict/presentation policy.

The category describes the primary reported failure, not every secondary cleanup
error in the message. Concrete process/helper health failures take precedence
over a step error; a bare deadline check does not erase that step's classified
failure. Final helper checks report specific reasons such as
`display_exited` and `early_exit`. `screenshots.json.failed_step_index` locates
recipe-step failures; a null index can also indicate a final helper-check failure.
Preflight and output failures that prevent result publication can still leave no
`result.json`.

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
children whose parent has already exited. The private D-Bus session remains alive
after a successful launcher handoff. Cleanup also stops matching Flatpak instances
that detached from the launcher process group, with up to two seconds for the
instance cleanup command. The daemonized keyring unlock service is cleaned up
separately using the run's private runtime directory.

```sh
cargo run -- verify-bundle ./build/org.example.App.flatpak \
  --output ./flatpak-smoke-output \
  --overall-timeout 5m \
  --window-timeout 45s
```

## Runtime requirements

Normal verification requires:

- `flatpak`
- `dbus-run-session`
- `gnome-keyring-daemon`
- `weston`
- `Xwayland` and `xdpyinfo` (from `xwayland` and `x11-utils` on Debian), for X11-only apps
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

### X11 compatibility in verification

Verification prefers Wayland when the app permits it. Apps with `x11` or
`fallback-x11` but no Wayland socket use Xwayland inside the headless Weston
session. The runner uses the app's existing permissions; it does not grant extra
display sockets. Capture, OCR, and clicking work in either mode.

`doctor` checks both display capabilities. Native Wayland verification does not
require the Xwayland-specific tools.

## Container usage

Published images are `ghcr.io/razzeee/flatpak-smoke` for verification and
`ghcr.io/razzeee/flatpak-smoke-screenshots` for native screenshots. They support
Linux amd64 and contain the tool and desktop dependencies, but not app runtimes.
Pass `--allow-network-remotes` when those need downloading from Flathub.
Use a published release tag or `sha-<full-commit-sha>`; digest pinning is supported.
The action runs Docker with `--privileged`; use it only on trusted runners and
workflows. See [security and image maintenance](security.md) before adopting it.

### Building from source

Build the reference image:

```sh
podman build -t flatpak-smoke -f Containerfile .
```

Or build both images from Git without checking out this repository:

```sh
docker build -t flatpak-smoke -f Containerfile \
  https://github.com/razzeee/flatpak-smoke.git
docker build -t flatpak-smoke-screenshots -f Containerfile.screenshots \
  https://github.com/razzeee/flatpak-smoke.git
```

Pin the Git build URL with `.git#<full-commit-sha>` in CI. For unreleased work,
build from a checkout containing that work, using `.` as the build context.
The action accepts these local names through its `image` input.

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

### Non-root containers

For apps that reject root, use the image's existing `nobody` account, UID/GID
65534. These examples assume you built the images from source and are
running from your app's checkout. Use fresh output directories.

For smoke testing, start the system bus before dropping privileges. Set `USER`
so the Weston VNC client authenticates as the same user as the compositor:

```sh
docker run --rm --privileged --tmpfs /run --entrypoint sh \
  -v "$PWD:/workspace" -w /workspace flatpak-smoke -c '
    set -eu
    mkdir -p /run/dbus
    dbus-daemon --system --fork --nopidfile
    install -d -o 65534 -g 65534 artifacts/smoke-user
    exec setpriv --reuid=65534 --regid=65534 --clear-groups \
      env HOME=/tmp USER=nobody \
      flatpak-smoke verify-bundle build/org.example.App.flatpak \
      --output artifacts/smoke-user --allow-network-remotes --overall-timeout 5m
  '
```

The screenshot image's entrypoint already starts the system bus:

```sh
docker run --rm --privileged --tmpfs /run --tmpfs /dev/dri \
  -v "$PWD:/workspace" -w /workspace flatpak-smoke-screenshots sh -c '
    set -eu
    install -d -o 65534 -g 65534 artifacts/capture-user
    exec setpriv --reuid=65534 --regid=65534 --clear-groups \
      flatpak-smoke screenshot-bundle build/org.example.App.flatpak \
      --recipe screenshots.yml --output artifacts/capture-user --allow-network-remotes
  '
```

The output files belong to UID 65534. Simply passing your host UID with Docker's
`--user` is insufficient if that UID has no account in the image. Screenshot
mode also needs the system bus started before switching users.

In the GitHub Action, set `user: nobody`. The action starts the bus, drops app
privileges, and restores artifact ownership to the runner after either success or
failure. It also handles replacement of previous non-root outputs with `--force`.

## GitHub Action

Run the composite action in a Linux amd64 job with Docker and privileged-container
support. GitHub-hosted `ubuntu-latest` is the tested runner. Checkout your app and
build its bundle or OSTree repository first. Python 3.9 or newer must be available
on self-hosted runners. No Rust toolchain is required.

| Input | Default | Meaning |
| --- | --- | --- |
| `mode` | `verify` | `verify` or `screenshots`. |
| `bundle` | None | Workspace-relative `.flatpak` bundle. Mutually exclusive with `repo` and `app-ref`. |
| `repo` | None | Workspace-relative OSTree repository, used instead of `bundle`. |
| `app-ref` | None | Full `app/ID/ARCH/BRANCH` ref, required with `repo`. |
| `dependency-repo` | None | Workspace-relative published runtime repository, mounted read-only. See [runtime reuse](reusable-dependencies.md). |
| `recipe` | None | Workspace-relative recipe file, required for `screenshots`; invalid for `verify`. |
| `output` | `artifacts/flatpak-smoke` | Output directory beneath the workspace. Existing artifacts require `args: --force`. |
| `allow-network-remotes` | `false` | Set to `"true"` to download missing runtimes from Flathub. |
| `args` | Empty | Extra CLI arguments, with quoted values and multiline input supported. |
| `execution-timeout` | `900` | Docker execution budget in integer seconds (1–86400), including image pulls and CLI execution. |
| `cleanup-timeout` | `30` | Additional shared stop/removal budget in integer seconds (1–86400). |
| `image` | Version recorded in `action/image-version` | Override the mode-specific image with a local name, release tag, SHA tag, or digest. |
| `user` | `root` | `root` or `nobody`. Use `nobody` for apps that reject root. |

Example using a local repository and longer timeouts:

```yaml
- uses: razzeee/flatpak-smoke@v0.5.1
  id: capture
  with:
    mode: screenshots
    repo: build/repo
    app-ref: app/org.example.App/x86_64/master
    recipe: ci/screenshots.yml
    output: artifacts/capture
    allow-network-remotes: "true"
    user: nobody
    args: |
      --overall-timeout 10m
      --window-timeout 60s
```

The action's `output` output is the workspace-relative artifact directory. The
wrapper sets it before invoking Docker, including when the CLI later fails.
Input-validation failures happen earlier and do not produce output artifacts.
The CLI exit status fails the step. Upload results in a separate
`actions/upload-artifact` step with `if: always()`, as shown in the README.

### Wrapper deadlines and diagnostics

The CLI's `--overall-timeout` remains independent and should be shorter than
`execution-timeout`, leaving time for image pulls and CLI result publication.
For example, use `args: --overall-timeout 5m`, `execution-timeout: '420'`, and
`cleanup-timeout: '30'` for a five-minute run with time for image pulls and cleanup.
Keep an outer process/job deadline longer than their sum (for example 480 seconds)
to allow Python startup and artifact publication and protect against runner failure.

The `execution` action output names `<output>.execution.json`, a **sibling** of
the artifact directory so it remains host-writable while the container owns that
directory. Upload this file alongside the artifact directory. It is atomically
published before Docker starts and updated after cleanup. Reusing an output path
replaces this wrapper record; use distinct output paths to retain execution history.
The wrapper never modifies the CLI's `result.json`: consumers should retain its
more-specific reason and also inspect wrapper/cleanup diagnostics.

Schema version 1 records `container`, configured budgets, `exit_code`, `signal`,
`elapsed_seconds` (including cleanup), and `status`: `running`, `completed`,
`docker_start_failed`, `timeout`, or `cancelled`. `completed` means Docker returned,
not that the CLI passed. Docker exit codes 125/126/127 indicate startup/command
execution failure; ordinary CLI exit codes are passed through. Wrapper timeout
returns 124; SIGTERM/SIGINT cancellation returns 130. Docker launch OS errors
return 1 and include `error`; Docker's own launch messages remain in the step log.

`cleanup` records `stop` (on cancellation/timeout) and `remove`, each with status
`ok`, `absent`, `failed`, or `timeout`, plus exit code/stderr or OS error where
available. Half the shared cleanup budget is reserved for graceful stop, allowing
the CLI to publish cancellation artifacts and the container shell to restore
ownership; remaining time is available for forced removal. Repeated TERM/INT is
ignored during cleanup. Cleanup failures do not replace the original exit status
or CLI result; consumers must check the cleanup record separately.

SIGKILL, job/host termination, an unresponsive Docker daemon, or exhaustion of the
grace period can prevent final diagnostics, ownership restoration, or removal.
A surviving `running` record is incomplete evidence, not success. Forced removal
cannot guarantee CLI artifacts or ownership; the early record's container name
supports external diagnosis/removal. Invalid inputs or an unwritable record
destination can fail before Docker starts with only stderr diagnostics.

`args` uses shell-style quoting only. No shell commands, environment variables,
globs, or command substitutions are evaluated. For example,
`--screenshot-after-click "Log In"` passes the label as one argument. Use the named
inputs for `--output`, `--recipe`, and `--allow-network-remotes`; overriding those
through `args` is rejected. App launch arguments belong in the recipe, not here.

The workspace is mounted at `/workspace`. Keep the bundle, recipe, and fixture
files within it; action input paths cannot escape it. The action manages Docker's
privileged mode, tmpfs mounts, system bus, and artifact ownership.

### Pinning versions

Each action release records its matching image release in `action/image-version`.
It does not follow `latest`. Main-branch builds publish `sha-<full-commit-sha>`
images after CI passes; `v*` tags publish the matching version after the same
checks. The action currently targets `v0.5.1`.

For immutable CI inputs, pin `uses: razzeee/flatpak-smoke@<full-action-commit-sha>`
and set `image: ghcr.io/razzeee/flatpak-smoke-screenshots@sha256:<digest>` for
screenshots, or the corresponding smoke image for verification. Obtain the digest
from the published package or `docker image inspect` after pulling. When testing
unreleased action changes, set `image` to the matching main-build SHA tag or a
locally built image. Forks publishing to another owner must override `image`.

## Troubleshooting

### The output directory already exists

Use `--force` to replace prior `result.json`, `screenshots/`, and `logs/` artifacts.

### Runtime dependencies are missing

Use `--allow-network-remotes` if the bundle needs runtimes or extensions that are not available in the isolated Flatpak user installation.

### The app needs more time

Increase `--overall-timeout` and `--window-timeout` for slow installs or first launches.

### Seeded files are missing or the app cannot open a document

Resolve `setup.files[].source` relative to the recipe directory, not the checkout.
For example, `ci/screenshots.yml` with `source: fixtures/sample.txt` reads
`ci/fixtures/sample.txt`. Ensure CI checks out those files and that directories
contain regular files rather than symlinks.

Use `${APP_DATA}` or `${APP_CONFIG}` in recipe launch arguments, rather than a
host path such as `/workspace/ci/fixtures/sample.txt`. Copying files into the
container does not itself grant the Flatpak sandbox access. Check that the app
supports the supplied argument and file format. Inspect `result.json` and
`logs/app.stderr.log` when launching fails.

## Screenshot recipes

`screenshot-bundle` and `screenshot-repo` capture app windows in an
isolated GNOME session. They include native decorations, shadows, rounded
corners, and transparency. Listing images contain neither the desktop wallpaper
nor the pointer. The tool saves native pixels without adding shadows or scaling
the exported PNG.

X11-only apps use GNOME's authenticated Xwayland compatibility path. Existing
`/tmp/.X11-unix` directories must have appropriate ownership and mode 1777;
the runner creates the directory if missing but does not change an existing one.

```sh
flatpak-smoke screenshot-bundle ./org.example.App.flatpak \
  --recipe ./screenshots.yml --output ./capture-output \
  --allow-network-remotes

flatpak-smoke screenshot-repo ./repo app/org.example.App/x86_64/stable \
  --recipe ./screenshots.yml --output ./capture-output
```

Screenshot capture supports GNOME Shell **48**, tested with 48.7. The dedicated
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

Source builds include `author-bundle`. See [interactive recipe authoring](recipe-authoring.md)
to write a recipe one step at a time and check it in a fresh session.

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
Flatpak runtime.

See [screenshot compatibility](screenshot-compatibility.md) for tested builds
and app-specific setup.

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
`result.json` uses the same schema as verification runs.

OCR uses separate enlarged diagnostic images, including a contrast pass for small
button labels. These copies and their TSV output stay under `logs/`; they never
replace listing PNGs. Failed runs also retain the last complete candidate image.
Use `--force` to replace an existing capture output, including its screenshot manifest.

### Seeding data and launch arguments

Every screenshot run starts with its own home, desktop configuration, Flatpak
installation, and per-app data. It does not reuse the caller's application profile,
theme, or language settings. This also makes repeated recipes start from the same
fresh state.

Use optional `setup.files` entries to copy checked-in files or directories into
that profile, and `launch.args` to open a document or select an app mode:

```yaml
version: 1
setup:
  files:
    - source: fixtures/project
      destination: data/example-project
    - source: fixtures/settings.ini
      destination: config/my-app/settings.ini
launch:
  args:
    - --open
    - "${APP_DATA}/example-project/document.txt"
steps:
  - wait_text: Example document
  - capture:
      name: document
      caption: Edit an example document
```

Adapt `--open`, the configuration layout, and the visible text to your app. Existing
version 1 recipes without these fields still launch with an empty profile and no
extra arguments.

Copy rules:

- Sources are nonempty relative paths beneath the recipe directory. Absolute
  paths and `..` source components are rejected; keep fixtures alongside the
  recipe or in a subdirectory.
- Destinations must name a path beneath `data/` or `config/`. They map to the
  installed app's private XDG data/config directories, not the desktop's settings.
- A file copies to the exact destination name. A directory copies its contents
  into the destination directory, including nested directories and hidden files.
- Source symlinks, including symlinked parent paths, and special files are rejected.
  Absolute destinations and `..` destination components are rejected too.
- Directories may merge, but two files cannot occupy the same destination. Existing
  destination files are never silently overwritten. Missing sources and conflicts
  fail before app launch and appear in the run's result.
- Copies create ordinary writable files under the current runner user; source
  ownership and executable bits are not preserved. Sources remain unchanged.
- Setup runs after installation and before desktop/app launch, within the overall
  timeout. The temporary profile is removed during normal run cleanup.

`launch.args` is an array of strings appended after the app ID in `flatpak run`.
Each element stays one argument, including spaces or an empty string. Two
placeholders are supported anywhere within an argument:

| Placeholder | Expansion |
| --- | --- |
| `${APP_DATA}` | The app's sandbox-visible XDG data directory. |
| `${APP_CONFIG}` | The app's sandbox-visible XDG configuration directory. |

Unknown or unterminated `${...}` placeholders and NUL characters are errors.
There is no shell evaluation or arbitrary environment expansion. These settings
do not alter Flatpak permissions or choose a different executable. General app
environment overrides and profile export/import commands are not provided.

The private session enables accessibility for confirmed text entry. `doctor
--desktop gnome` starts a temporary headless session and contacts the native
helper and accessibility bus, in addition to checking installed tools.

Authors still select useful content and review the images against the
[Flathub quality guidelines](https://docs.flathub.org/docs/for-app-authors/metainfo-guidelines/quality-guidelines#screenshots).
Default data can leave a content-oriented app in an empty state. Passing a recipe
does not certify the editorial quality of its screenshots or publish them.
