# Author a recipe interactively

`author-bundle` runs one recipe action at a time in an isolated GNOME session.
It saves each successful step to version-1 YAML. You choose the actions and
expected outcomes. The `observe` command saves a window screenshot and prints
OCR text and the app's window titles.

Build the screenshot image from this checkout to use the command:

```sh
docker build -f Containerfile.screenshots -t flatpak-smoke-author .
```

## Start a draft

Create the recipe in your app's repository. This layout
keeps the recipe and sample files together:

```text
your-app/
  ci/
    screenshots.yml
    fixtures/                 # optional sample files the app needs
      document.txt
      settings.ini
  build/
    org.example.App.flatpak    # your built app
  artifacts/                  # generated screenshots and logs
```

Create `ci/screenshots.yml` with this content:

```yaml
version: 1
window:
  width: 900
  height: 600
steps: []
```

If the app needs sample content, put those files in `ci/fixtures/`. Add `setup`
and `launch` to the recipe before starting. For example:

```yaml
setup:
  files:
    - source: fixtures/document.txt
      destination: data/document.txt
    - source: fixtures/settings.ini
      destination: config/settings.ini
launch:
  args:
    - --open
    - "${APP_DATA}/document.txt"
```

Use your app's actual file formats, configuration paths, and launch arguments.
`--open` is only an example. Omit `setup` and `launch` if the app needs neither.

The source `fixtures/document.txt` means `ci/fixtures/document.txt` because source
paths are relative to the recipe. The runner copies it into the app's fresh
profile before launch. The saved recipe keeps `${APP_DATA}` as a placeholder, so
it works in another checkout or in CI. See the
[fixture and launch argument rules](reference.md#seeding-data-and-launch-arguments)
for details. Keep the recipe and fixtures outside `artifacts/`.

From the app checkout, run:

```sh
docker run --rm -it --privileged --tmpfs /run --tmpfs /dev/dri \
  -v "$PWD:/workspace" -w /workspace flatpak-smoke-author \
  flatpak-smoke author-bundle build/org.example.App.flatpak \
  --recipe ci/screenshots.yml --output artifacts/author-1 \
  --allow-network-remotes --overall-timeout 30m
```

`--recipe ci/screenshots.yml` names both the input draft and the file the command
updates. Each successful step goes into that file. There is no separate export
file to find after `finish`.

`--output artifacts/author-1` names the directory for screenshots, logs, and
validation results. The Docker mount writes both the recipe and those results
back to your app checkout on the host.

The app runs headlessly. Open the observation PNG in an image viewer.
The command prints its path inside the container. Replace `/workspace` with
your app checkout's path to find the image on the host.

## Enter steps

When you see `author>`, type a command and press Enter. For example, to click a
button labeled "Search", type:

```text
click_text: Search
```

The tool runs the command, saves the step if it succeeds, and prompts for the next
command. Type `help` to see examples or `observe` to inspect the app's current window.

Here are more examples. Enter each command on its own line:

```yaml
wait_text: Ready to capture
type_text: example
key: Return
{wait_text: Results for example, timeout: 15s}
capture: {name: search, caption: Find items by name}
click_text: Preferences
select_window: {title: Preferences}
```

Use your app's actual labels and shortcuts. Quote YAML-sensitive values, for
example `type_text: "true"`, `type_text: "a: b"`, or
`type_text: "first line\nsecond line"`. All six
[recipe actions](reference.md#writing-a-recipe) have the same behavior during
authoring and replay. `click_text` requires a unique OCR match. `type_text`
requires a focused control with an accessible editable-text interface.

Commands:

- `observe` refreshes `logs/observation.png` for this session and prints both OCR
  passes and window titles. It adds no recipe step.
- `save` writes the draft. The command also saves after every successful action.
  Each save replaces the file atomically. It keeps action values and key chords,
  rewrites formatting, and removes comments.
- `restart` reloads the draft from disk and replays its steps in a fresh profile
  with the original fixtures. To change earlier steps or setup, edit the draft
  in another editor while at the prompt, then restart. The command refuses to
  save over external edits.
- `finish` requires at least one `wait_text` assertion and one named capture.
  It saves the recipe, closes the session, and replays the recipe in a fresh session.
- `quit` or end of input keeps the saved draft and exits without validation.
- `help` shows examples and commands.

Use `wait_text` to check the result of an action, such as a search result or a
loaded document title. It checks normalized OCR text. `capture` checks that the
window has visible content and saves an image. It does not compare pixels against
a baseline. Review each assertion to check that it tests the intended behavior.

The command leaves failed actions out of the draft. An action may change the app
before it fails, so you must restart before adding steps or finishing.
You can still observe, save, or quit. If a saved step fails during replay, the
command exits and keeps the diagnostics. Edit the draft and rerun the command.

## What to commit

After `finish` reports a successful replay, review and commit `ci/screenshots.yml`
with every file or directory referenced by `setup.files`. Keep their relative
paths unchanged if you move them into another checkout.

Do not commit the generated `artifacts/` directory or Flatpak bundle for replay.
CI builds the bundle and uses the checked-in recipe and fixtures. Keep screenshots
separately if you plan to publish them.

To run the saved recipe again from your app checkout:

```sh
docker run --rm --privileged --tmpfs /run --tmpfs /dev/dri \
  -v "$PWD:/workspace" -w /workspace flatpak-smoke-author \
  flatpak-smoke screenshot-bundle build/org.example.App.flatpak \
  --recipe ci/screenshots.yml --output artifacts/replay-1 \
  --allow-network-remotes
```

For GitHub Actions, use screenshot mode with `recipe: ci/screenshots.yml`.
See the [GitHub Action reference](reference.md#github-action) for the other inputs.
The extra assertion requirement applies only to `finish`. Existing recipes keep
their validation rules.

## Screenshots and logs

In this example, `artifacts/author-1/` contains:

```text
author.json
session-001/
  result.json
  screenshots.json
  logs/observation.png
  screenshots/...
session-002/...             # when restarted
validation/
  result.json
  screenshots.json
  screenshots/...
```

`author.json` has schema version 1. Its status starts as `draft` and changes to
`validated` after a fresh replay succeeds. It then points to
`validation/result.json`, which records the recipe hash and fixture-input digest.
If you edit the recipe or fixtures afterward, replay them again.

Each session has its own result file. A failed action stays in that session's
diagnostics even if a later session succeeds. Only the validation run checks the
completed recipe in a fresh profile.

If validation fails, the command exits nonzero and keeps the recipe and diagnostics.
On interruption or timeout, it keeps saved steps and cleans up the app and desktop.
The overall timeout includes time at the prompt and restarts. Validation gets a
separate overall-timeout budget. Per-step timeouts work as they do during replay.
Use a new output directory to keep earlier attempts. Use `--force` to replace
reused session artifacts and reset validation status.
