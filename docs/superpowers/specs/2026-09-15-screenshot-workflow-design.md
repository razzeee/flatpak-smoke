# Recipe-driven application screenshots

## Status

Approved for implementation. The user selected app-specific recipes, a
GNOME-first screenshot session, and a replaceable desktop backend that can
support KDE later, then approved this interface and its acceptance criteria.

Base revision: `a6a6e92` on `main`.

## Goal

Use flatpak-smoke to produce an ordered set of application-window screenshots
that authors can use in Flathub metadata. Run a repeatable recipe against a
Flatpak build in an isolated desktop session. Begin with fresh application data
and provide a preparation step where a future data importer can restore content
before launch.

The tool produces images and metadata for an author to select and publish.
Successful capture means the requested steps completed and the images passed
technical checks. It does not certify every Flathub listing-quality guideline.
In particular, fresh application data can produce an empty state that is not
suitable for a content-oriented app's listing.

## Approved decisions

- Navigation is described in an app-specific recipe with explicit capture points.
- Screenshot mode uses an isolated, headless GNOME session first. A larger
  screenshot container is acceptable.
- The desktop backend owns window discovery, focus, sizing, input, and native
  capture. Recipes and output metadata do not depend on GNOME APIs.
- A future KDE backend can provide Plasma defaults and support desktop-specific
  integrations. The Flatpak runtime does not select the desktop implicitly.
- Version one starts with fresh app data. User-session export and bundle import
  are follow-up work.

## Flathub requirements that shape the implementation

The linked quality guidelines call for:

- Native Linux window screenshots, including decorations, shadows, and rounded
  corners, without the desktop wallpaper or promotional additions.
- Default platform appearance and at least one screenshot in English.
- Unmaximized windows, generally at most 1000 by 700 logical pixels, or twice
  those image dimensions at a scale factor of two.
- An ordered set of screenshots with the strongest image first, and a short
  caption for each image.
- Useful, current content rather than empty states where the app displays content.

The initial profile uses English, default GNOME appearance, and scale factor one.
It captures the native window image directly. It does not remove a desktop
background by color, add shadows, or resize the exported PNG after capture.
The author chooses capture order and captions in the recipe.

## Proposed command interface

Add commands parallel to the existing artifact inputs:

```sh
flatpak-smoke screenshot-bundle ./org.example.App.flatpak \
  --recipe ./screenshots.yml \
  --output ./capture-output \
  --allow-network-remotes

flatpak-smoke screenshot-repo ./repo app/org.example.App/x86_64/stable \
  --recipe ./screenshots.yml \
  --output ./capture-output
```

Both commands require a recipe and output directory. They support the existing
output protection, `--force`, network-remotes policy, and deadline handling.
The screenshot workflow defaults to a five-minute overall budget, with ten
seconds for individual recipe actions and thirty seconds for initial app readiness.
An action may specify its own timeout, bounded by the remaining overall budget.

Existing verification commands continue to use their current Weston workflow
and result contract. Installation, owned-process cleanup, cancellation, and
liveness checks are shared wherever their behavior is the same.

Extend `doctor` with `--desktop gnome` to check screenshot dependencies and the
native capture helper. The ordinary smoke-test toolchain check remains available.

## Proposed recipe format

Use versioned YAML with strict field validation. A recipe contains presentation
settings and a sequential list of actions. All actions run in one app session.

```yaml
version: 1
desktop: gnome
language: en
window:
  width: 900
  height: 600

steps:
  - capture:
      name: overview
      caption: Browse the available items

  - click_text: Search
  - type_text: example
  - key: Return
  - wait_text: Search results
    timeout: 15s
  - capture:
      name: search
      caption: Find items by name

  - key: Escape
  - key: Ctrl+comma
  - select_window:
      title: Preferences
  - capture:
      name: preferences
      caption: Choose how the app behaves
```

This is an illustrative recipe; its labels and shortcuts must match the target app.

### Action semantics

- `click_text`: find a unique visible label in the selected window using OCR,
  then deliver a click. Ambiguous labels fail with candidate locations recorded.
  Matching and any visual fallback must use fresh diagnostic images.
- `type_text`: enter literal UTF-8 text into the focused control using the
  backend's input mechanism. Unsupported input must produce an explicit failure.
- `key`: deliver a key or supported key chord. The parser validates key names
  and modifiers before launching the app.
- `wait_text`: poll OCR on the selected window until the requested text appears
  or the action deadline expires. Match normalized text, not arbitrary regular
  expressions, in version one.
- `select_window`: select an app-owned top-level window by exact title. Zero
  matches wait until the action deadline; multiple matches fail. The selected
  window remains identified by its session-local window handle even if its title
  changes afterward.
- `capture`: save the selected native window to a named PNG and register its
  caption and order in the screenshot manifest.

The initial window selection waits for one principal app-owned window. If several
windows are eligible, the recipe must supply an initial `window.title` selector.
Ownership must be established using compositor and launched-application identity;
focus alone and an assumed equality between Flatpak ID and Wayland app ID are
insufficient.

`window.width` and `window.height` request the initial main window's logical frame
size, excluding shadow extents. Their defaults are 900 by 600. Both values must be
positive and at most 1000 by 700 for this first windowed profile. A backend waits
for the resize to take effect and records the actual geometry. A window with a
larger minimum size fails with its actual dimensions reported; the image is not
silently scaled or cropped. Secondary windows retain their natural size and are
checked against the same logical-size limits.

The first version accepts `desktop: gnome` and `language: en`, with those defaults
when omitted. Other values fail during validation. Later backends and language
profiles can extend these values without making recipes desktop-specific.

Validate recipes before installing or launching anything: supported version and
actions, exactly one action per step, known fields, valid timeouts, at least one
capture, unique filename-safe capture names, and nonempty captions. Caption text
must be a single line without a trailing full stop. Captions are metadata and
are never painted onto the image.

Only explicit `capture` actions produce listing assets. Navigation and waits can
produce diagnostic images under `logs/`, but those images are not listing entries.

## Desktop backend

The backend provides these operations to the shared recipe runner:

1. Start and stop an owned desktop session.
2. Discover app-owned windows and return session-local handles and geometry.
3. Focus and resize a selected window, reporting the observed result.
4. Deliver pointer and keyboard input relative to the selected window.
5. Capture a diagnostic view for OCR and a native window PNG for publication.
6. Report app, desktop, and helper liveness throughout operations.

Diagnostic images include the coordinate mapping needed for input. Listing PNGs
can have different origins and dimensions because they include shadows and alpha.
Do not interpret listing-image coordinates directly as desktop coordinates.

Implement a GNOME backend using a private GNOME Shell session and a small,
versioned session helper. Use compositor window APIs for discovery and sizing,
and native window capture for the PNG. The helper's supported operations are the
ones above; recipe files contain no GNOME Shell JavaScript or arbitrary commands.

The backend must address the actual permission and caller requirements of the
selected GNOME version. The existence of a D-Bus screenshot method does not prove
that an arbitrary `gdbus` invocation can call it unattended.

KDE support will implement the same operations with KDE facilities. No KDE stub
or desktop guessing is needed in version one. A missing capability is reported
explicitly rather than producing a whole-desktop image as a substitute.

## Workspace and future app data

The screenshot workspace owns the desktop settings, session bus, Flatpak user
installation, and application home/data locations. The current runner redirects
XDG paths but does not set `HOME`; that alone is not a sufficient fresh-data
contract for this feature. Tests must verify the data paths actually seen by a
Flatpak app, including its per-app `~/.var/app` storage.

The lifecycle is:

```text
validate recipe and artifact
→ prepare owned workspace and install artifact
→ prepare application data
→ start desktop and app
→ select and size the initial window
→ observe readiness
→ execute recipe and capture windows
→ finalize results and clean up
```

In version one, application-data preparation creates a fresh profile. Future
import runs at this step while the app is stopped. It must not require changes
to navigation or capture semantics.

The later export/import design should cover selected app configuration and data,
external documents needed by that data, restore destinations, app identity,
compatibility information, and a consistent snapshot from a normal user session.
Do not define an archive format or expose an import flag in this version. An
exported profile should not be assumed portable merely because it is a copy of
an application's directory.

## Readiness, failure handling, and outputs

Reuse persistent-content observation and absolute deadlines. Apply readiness to
the selected window, and allow continuously changing content. Recipe authors use
`wait_text` to establish an app-specific state before a capture. Pixel stability
alone does not prove that a loading screen has finished.

Monitor app, desktop, and helper processes during native capture, OCR, waits,
input, and before final success. Cancellation and failures use owned cleanup.
Report the failed step and preserve completed listing images and diagnostics.

Write:

```text
capture-output/
  result.json
  screenshots.json
  screenshots/
    000-overview.png
    001-search.png
    002-preferences.png
  logs/
    runner.log
    app.stdout.log
    app.stderr.log
    desktop.stdout.log
    desktop.stderr.log
    last-candidate.png
```

`result.json` uses the existing run-result structure for status, artifact, app
ref, timings, failure, and artifact paths. Additional screenshot-workflow context
belongs in `screenshots.json`, which has its own `schema_version: 1` and contains:

- Desktop name and version, language, scale, recipe version, and app ref.
- `failed_step_index`: the zero-based index of the active step on failure, or
  null on success and for failures outside recipe execution.
- An ordered list of completed captures with name, relative path, caption,
  language, actual PNG dimensions, and logical window-frame dimensions.

The first manifest entry is the default screenshot. Captures enter the manifest
only after a complete PNG has been written and validated. On failure, previously
completed entries remain usable and the run status is failed. Once the output
directory is prepared, write failure results for dependency and session errors
as well as action errors. Extend output protection to cover `screenshots.json`.

Generate native PNGs with their original alpha. Check that they decode, have
nonzero dimensions, and contain app content. Verify shadows and transparency on
reference fixtures known to have them; do not assume every application's window
has the same shape. Hosting images, editing MetaInfo XML, full-screen profiles,
and automated judgments of content quality are follow-up work.

## Implementation sequence and acceptance

### 1. Prove native GNOME capture

Before implementing the full recipe runner, demonstrate unattended window
capture inside the screenshot container. Use one GTK/libadwaita fixture and one
Qt/KDE fixture, each with a secondary window.

Verify native decoration and shadow extents, alpha around rounded corners,
default appearance, unmaximized requested sizing, window selection, and no
desktop background or cursor in the listing image. Inspect the PNGs visually
and check their dimensions and alpha data. If the selected GNOME capture path
cannot produce these images, revisit it before proceeding.

### 2. Implement shared recipes and the screenshot commands

Build parsing and validation, the ordered action runner, the GNOME backend,
output manifests, dependency checks, and deadline/liveness integration. Add a
dedicated screenshot container definition with a documented GNOME version.
Keep desktop-specific code outside the growing Weston/VNC session module.

### 3. Verify behavior and document author usage

Acceptance coverage must include:

- At least three ordered captures from a fixture recipe, with matching captions.
- GTK and Qt native window images, including selection of a secondary window.
- Keyboard input, literal text entry, OCR-based clicks, and text waits.
- Delayed rendering and an animated view.
- Missing and ambiguous labels or windows, a closed selected window, and an
  action that times out.
- App, desktop, and helper exits during recipe execution, preserving earlier
  captures and identifying the failed step.
- Cancellation and isolation between two runs, including fresh app data and no
  access to a caller's existing application profile through inherited home paths.
- Existing smoke-verification tests and result-format tests.

Run Rust formatting, build checks, tests, and Clippy with the CI toolchain.
Run native-capture end-to-end checks in the new screenshot image and retain
images and manifests as CI artifacts. Document recipe authoring, capture order,
captions, expected limitations of fresh data, and the future restore point.

## Sources and evidence

- [Flathub screenshot quality guidelines](https://docs.flathub.org/docs/for-app-authors/metainfo-guidelines/quality-guidelines#screenshots)
- [GNOME Shell screenshot D-Bus interface](https://gitlab.gnome.org/GNOME/gnome-shell/-/blob/main/data/dbus-interfaces/org.gnome.Shell.Screenshot.xml)
- [GNOME Shell native screenshot implementation](https://gitlab.gnome.org/GNOME/gnome-shell/-/blob/main/src/shell-screenshot.c)
- Existing `src/session.rs`, `src/process.rs`, `src/verify.rs`, and fixture CI.

The GNOME source provides a native window-actor capture path with alpha. These
source links track upstream main. The implementation uses Debian trixie's GNOME
Shell 48, verified with 48.7. `tests/native_capture.py` demonstrated unattended
native capture, input, resizing, and secondary windows for GTK/libadwaita and Qt;
the PNGs were inspected visually and checked for frame dimensions and alpha.
Its evidence is written under `target/native-capture/`.
