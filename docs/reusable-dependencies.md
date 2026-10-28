# Reuse runtime downloads

Use `--dependency-repo PATH` to install runtimes and extensions from a local
Flatpak/OSTree repository instead of downloading them for each run. The GitHub
Action accepts the workspace-relative `dependency-repo` input and mounts it
read-only. Use v0.5.1 or newer for support for sandboxed subprocesses.

The input must be a published repository containing a `summary`, not a Flatpak
installation or writable download cache. Each run still creates its own
installation, app profile, and desktop session. Reuse avoids downloads, not
per-run deployment.

## Prepare a repository

From a flatpak-smoke source checkout, export an installation that already has the
required runtimes and extensions:

```sh
python3 action/prepare-dependencies.py \
  --installation /var/lib/flatpak --output inputs/dependencies
```

The exporter requires Python 3.9+, Flatpak, and OSTree. It exports deployed commit
checksums and writes `inputs.json` with their refs, checksums, and Flatpak version.
It refuses to replace an existing output. Keep the source installation available
during export.

By default, all deployed runtimes and extensions are exported. Use repeated
`--ref runtime/ID/ARCH/BRANCH` arguments to select a subset. Selected commits must
be complete: partially downloaded Locale or other subpath deployments can fail
export. Provision those refs in full first; export does not fetch missing objects.

Preserve permissions, symlinks, and extended attributes when transporting the
repository. Use a container image layer or an archive that preserves them.

## Use it in both workflows

```sh
flatpak-smoke verify-bundle inputs/app.flatpak \
  --dependency-repo inputs/dependencies --output results/verify
flatpak-smoke screenshot-bundle inputs/app.flatpak \
  --dependency-repo inputs/dependencies --recipe recipe.yml --output results/screenshots
```

In the GitHub Action, prepare the repository within your app's checkout and pass
the same path to both steps:

```yaml
- uses: razzeee/flatpak-smoke@v0.5.1
  with:
    bundle: inputs/app.flatpak
    dependency-repo: inputs/dependencies
    output: results/verify

- uses: razzeee/flatpak-smoke@v0.5.1
  with:
    mode: screenshots
    bundle: inputs/app.flatpak
    dependency-repo: inputs/dependencies
    recipe: ci/screenshots.yml
    output: results/screenshots
```

The option also works with `verify-repo`, `screenshot-repo`, and the action's
`repo` / `app-ref` inputs. An installation's `/var/lib/flatpak/repo` is not a
drop-in published repository: it may lack a summary or use remote-prefixed refs.

## Keep inputs reproducible

- Keep the repository unchanged while runs read it. Concurrent readers are
  supported; use distinct output directories outside the input repository.
- Include dependencies needed by both desktops. For example, VLC's screenshot
  environment also selects `org.kde.WaylandDecoration.QAdwaitaDecorations`.
- Omit `--allow-network-remotes` to avoid resolving missing dependencies from
  Flathub. Enabling it allows each phase to resolve missing refs independently.
- Compare `phase_metadata.runtimes` in each run's `result.json` with your expected
  refs and commits. `inputs.json` lists what is available, not what was selected.
  Flatpak may skip missing optional extensions without failing installation.
- Pin runner images, architecture, and app commits alongside runtime inputs.
  A moving app ref is not a commit pin.

With `--dependency-repo`, host and image runtimes cannot silently satisfy missing
dependencies. Local repositories are trusted inputs: GPG verification is disabled.
This option and `--allow-network-remotes` are not network sandboxes; apps and
bundle-provided origins may still use the network. Enforce network isolation
externally if required.
