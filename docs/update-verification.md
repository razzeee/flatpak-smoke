# Verify updates with existing app data

`verify-update-bundle` installs an old bundle, runs a preparation recipe, waits
for a graceful app exit, updates to a candidate bundle, and runs verification
against the same disposable profile. It uses the GNOME screenshot toolchain,
including `ostree` for inspecting bundle identities before installation.

```sh
flatpak-smoke verify-update-bundle candidate.flatpak \
  --from old.flatpak \
  --prepare-recipe ci/prepare-update.yml \
  --verify-recipe ci/verify-update.yml \
  --dependency-repo dependencies \
  --output artifacts/update
```

Use the screenshot container built from this checkout. For example:

```sh
docker build -t flatpak-smoke-update -f Containerfile.screenshots .
docker run --rm --privileged --tmpfs /run --tmpfs /dev/dri --tmpfs /tmp:rw,size=4g \
  -v "$PWD:/workspace" -w /workspace flatpak-smoke-update \
  flatpak-smoke verify-update-bundle candidate.flatpak --from old.flatpak \
  --prepare-recipe ci/prepare-update.yml --verify-recipe ci/verify-update.yml \
  --dependency-repo dependencies --output artifacts/update
```

Both bundles must have the same app ID, architecture and branch, and different
commits. `--from` declares which build is older; the runner does not infer release
ordering from version strings. Normal forward updates are supported. A candidate
is installed with `flatpak install --or-update`, without uninstalling the old app.
The deployed commit must match the candidate bundle exactly.

The example places temporary installations on tmpfs, reducing container
writable-layer I/O. Allow enough memory for the required runtimes.

## Preparation recipe

Use the existing [recipe actions](reference.md#writing-a-recipe) to create and
save meaningful data. Optional `setup.files` is copied once before the old app
starts. All normal steps must complete with the app still running. Then the
required `quit` section sends the application's normal quit shortcut:

```yaml
version: 1
steps:
  - type_text: Persistent document
  - key: Ctrl+s
  - wait_text: Saved changes
  - capture:
      name: prepared
      caption: Saved data in the old version
quit:
  key: Ctrl+q
  timeout: 10s
```

Adapt the steps to the app, including save dialogs. `quit` targets the last
selected window. Success requires a successful launcher exit and no remaining
private Flatpak instance. A timeout or failed exit stops the test before updating;
forced cleanup is not accepted as graceful shutdown.

## Verification recipe

Verification uses the same app data, config and cache, with a newly started desktop
session. Its `launch.args` can reopen saved documents using `${APP_DATA}` and
`${APP_CONFIG}`. `setup.files` and `quit` are rejected here.

```yaml
version: 1
steps:
  - wait_text: Persistent document
  - wait_text: Theme ocean
  - wait_text: Record Persistent document
  - capture:
      name: preserved
      caption: Saved content after the update
```

At least one `wait_text` is required. Choose distinctive text from saved content,
settings, or records—not a generic welcome screen. Check edit/save interactions
too when testing continued usability. These are OCR/UI assertions, not direct
database integrity checks; a capture alone does not establish persistence.

## Isolation, output and failure handling

Every invocation owns one temporary home and private Flatpak installation for
both versions. Caller environment variables are cleared, system installations
are hidden, and app filesystem grants are reset with `--nofilesystem=host:reset`.
Documents must live in the disposable app profile; use recipe fixtures to seed
them. Caller profiles are never imported. Runtime dependencies come from
`--dependency-repo` or, when explicitly enabled, `--allow-network-remotes`.

`result.json` contains `update.before` and `update.after` metadata (app/runtime
commits and recipe hashes), the old bundle path, and `update.failed_phase`.
Phases are `validate`, `install_old`, `prepare`, `stop_old`, `update`, and `verify`.
Preparation and verification each write their own `screenshots.json`, captures,
and logs under `phases/prepare/` and `phases/verify/`. Earlier evidence survives a
later failure. The temporary installation/profile is removed after process
cleanup, including on failure and handled cancellation.

The overall timeout defaults to ten minutes. Display/window/screenshot timeouts
use the same options as screenshot recipes. Use `--force` to replace an existing
output directory's artifacts, including phase outputs.

This initial command is available directly through the CLI/container. The
GitHub Action does not yet expose an update mode.
