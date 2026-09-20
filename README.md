# flatpak-smoke

Check that your Flatpak installs and shows a window, or capture native-window
screenshots for your app listing. Both workflows run headlessly in CI.

## Before you start

Build a `.flatpak` bundle in your app's repository. The examples below run after
that build on a **Linux amd64 runner with Docker and privileged-container support**.
Replace `build/org.example.App.flatpak` with your bundle path.

The examples target **v0.1.2**. Its action tag and GHCR images
must be published before these versioned examples can run. Until then, use the
[source-build instructions](docs/reference.md#building-from-source) and the
action's `image` override. Publication happens through CI; see the
[release checklist](docs/releases.md).

## Smoke-test your app in GitHub Actions

Add these steps after checkout and your Flatpak build in an `ubuntu-latest` job:

```yaml
- name: Check that the app starts
  uses: razzeee/flatpak-smoke@v0.1.2
  with:
    bundle: build/org.example.App.flatpak
    output: artifacts/smoke
    allow-network-remotes: "true"
    args: --overall-timeout 5m

- name: Upload results, including failures
  if: always()
  uses: actions/upload-artifact@v7
  with:
    name: flatpak-smoke
    path: artifacts/smoke
```

The action fails if installation or startup fails, the app exits during the test,
no visible content appears, or OCR detects a known fatal error message. It saves
`result.json`, screenshots, and logs in the output directory.

`allow-network-remotes` lets Flatpak download missing runtimes from Flathub. For
apps that reject root, add `user: nobody`. To click a button and check the next
screen, add `--screenshot-after-click "Log In"` to `args`.

## Capture app-listing screenshots

### 1. Write a recipe

Create `ci/screenshots.yml` in your app's repository:

```yaml
version: 1
window:
  width: 900
  height: 650
steps:
  - capture:
      name: overview
      caption: Explore the main window
```

Choose a size your app supports and describe the content in the caption. Recipes
can also click buttons, enter text, wait for content, and select another window.
See the [recipe actions](docs/reference.md#writing-a-recipe).

### 2. Add sample content if your app needs it

Check in a small example document or project and, optionally, app settings:

```text
ci/
  screenshots.yml
  fixtures/
    example-project/
      document.txt
    settings.ini
```

Add `setup` and `launch` to the recipe:

```yaml
version: 1
setup:
  files:
    - source: fixtures/example-project
      destination: data/example-project
    - source: fixtures/settings.ini
      destination: config/settings.ini
launch:
  args:
    - --open
    - "${APP_DATA}/example-project/document.txt"
window:
  width: 900
  height: 650
steps:
  - wait_text: Example document
  - capture:
      name: overview
      caption: Edit an example document
```

Use your app's actual file formats, configuration paths, command-line arguments,
and visible text. `--open` is illustrative; apps that accept a positional filename
need only the path argument. Remove the settings entry if you don't need it.

Source paths are **relative to the recipe file**. The runner copies them into the
app's fresh Flatpak data/config directories before launch. `${APP_DATA}` and
`${APP_CONFIG}` resolve to those directories inside the sandbox. Every run starts
from the checked-in files, even if the app edits them. See
[seeding data and launch arguments](docs/reference.md#seeding-data-and-launch-arguments)
for the copy rules.

### 3. Run the recipe in CI

```yaml
- name: Capture listing screenshots
  uses: razzeee/flatpak-smoke@v0.1.2
  with:
    mode: screenshots
    bundle: build/org.example.App.flatpak
    recipe: ci/screenshots.yml
    output: artifacts/capture
    allow-network-remotes: "true"
    args: --overall-timeout 10m

- name: Upload screenshots and diagnostics
  if: always()
  uses: actions/upload-artifact@v7
  with:
    name: listing-screenshots
    path: artifacts/capture
```

Action paths are relative to your checkout. Find the images in
`artifacts/capture/screenshots/`; `screenshots.json` lists their order, captions,
and dimensions. Captures include native window decorations and transparency,
without the desktop background or pointer. They use English and 1× scale.
Review the images before publishing them.

## Run locally with Docker

Run these commands from your app's checkout, using the same bundle and recipe:

```sh
docker pull ghcr.io/razzeee/flatpak-smoke:v0.1.2
docker run --rm --privileged \
  -v "$PWD:/workspace" -w /workspace \
  ghcr.io/razzeee/flatpak-smoke:v0.1.2 \
  verify-bundle build/org.example.App.flatpak \
  --output artifacts/smoke --allow-network-remotes --overall-timeout 5m

docker pull ghcr.io/razzeee/flatpak-smoke-screenshots:v0.1.2
docker run --rm --privileged --tmpfs /run --tmpfs /dev/dri \
  -v "$PWD:/workspace" -w /workspace \
  ghcr.io/razzeee/flatpak-smoke-screenshots:v0.1.2 \
  flatpak-smoke screenshot-bundle build/org.example.App.flatpak \
  --recipe ci/screenshots.yml --output artifacts/capture --allow-network-remotes
```

For repeated runs, add `--force` to replace previous results. Docker images run as
root by default; see the [non-root examples](docs/reference.md#non-root-containers)
for apps that require another user.

## More options

- [Action inputs, outputs, and version pinning](docs/reference.md#github-action).
- [CLI reference](docs/reference.md): local OSTree repos, timeouts, results, host
  installation, and troubleshooting.
- [Tested app compatibility](docs/screenshot-compatibility.md).
