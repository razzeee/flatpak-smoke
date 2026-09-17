# flatpak-smoke

Check that your Flatpak installs and shows a window, or generate
native-window screenshots for your app listing. Both workflows run headlessly
in CI.

## Before you start

Use a **Linux runner with Docker and support for privileged containers**.
Run the examples from your app's checkout, after building a
`.flatpak` bundle. Replace `build/org.example.App.flatpak` with your bundle path.

The commands below build the tool's container images with their dependencies.
For CI, pin the Git build URL to a commit using `.git#<full-commit-sha>`.
The images run as root by default. For apps that reject root, such as VSCodium,
use the [non-root container examples](docs/reference.md#non-root-containers).

## Smoke-test your app

```sh
docker build -t flatpak-smoke -f Containerfile \
  https://github.com/razzeee/flatpak-smoke.git

docker run --rm --privileged \
  -v "$PWD:/workspace" -w /workspace \
  flatpak-smoke verify-bundle build/org.example.App.flatpak \
  --output artifacts/smoke --allow-network-remotes --overall-timeout 5m
```

The command fails if installation or startup fails, the app exits during the
test, no visible content appears, or OCR detects a known fatal error message.
Find the result in `artifacts/smoke/result.json`, with screenshots and logs
alongside it.

To also click a button and check the resulting screen, append
`--screenshot-after-click "Log In"`, replacing the label with one in your app.

## Capture app-listing screenshots

Create `screenshots.yml` in your app's repository:

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

Choose a size your app supports and write a caption describing its content.
Add [recipe actions](docs/reference.md#writing-a-recipe) to navigate, enter text,
wait for content, and capture additional views.

```sh
docker build -t flatpak-smoke-screenshots -f Containerfile.screenshots \
  https://github.com/razzeee/flatpak-smoke.git

docker run --rm --privileged --tmpfs /run --tmpfs /dev/dri \
  -v "$PWD:/workspace" -w /workspace \
  flatpak-smoke-screenshots \
  flatpak-smoke screenshot-bundle build/org.example.App.flatpak \
  --recipe screenshots.yml --output artifacts/capture --allow-network-remotes
```

Captures include native window decorations and transparency, without the
desktop background or pointer. They use a fresh app profile, English, and
1× scale.

Your images are in `artifacts/capture/screenshots/`. The accompanying
`screenshots.json` lists their order, captions, and dimensions. Review the images
before publishing them; fresh profiles may need recipe steps to populate a view.

## Add it to CI

Run either set of commands after your Flatpak build. A nonzero exit status fails
the job. Keep `--allow-network-remotes` when the container needs to download
missing runtimes from Flathub.

Upload the output even when a check fails. For GitHub Actions:

```yaml
- name: Upload Flatpak results
  if: always()
  uses: actions/upload-artifact@v7
  with:
    name: flatpak-results
    path: artifacts/
```

For repeated local runs, add `--force` to replace the previous output.

## More options

- [CLI reference](docs/reference.md): local OSTree repos, timeouts, result format,
  host installation, and troubleshooting.
- [Recipe actions and window selection](docs/reference.md#writing-a-recipe).
- [Tested app compatibility](docs/screenshot-compatibility.md).
