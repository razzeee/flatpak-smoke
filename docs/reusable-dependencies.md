# Reusable runtime inputs

Both bundle and repository commands accept `--dependency-repo PATH`. The action
accepts the workspace-relative `dependency-repo` input and mounts it read-only.
This is a **published local Flatpak/OSTree repository**, not an installation,
home directory, or writable download cache. It must contain a `summary`.

Each invocation still creates its own user installation, app profile, caches,
run directory and desktop processes. Flatpak pulls and deploys the needed refs
from the shared repository. Concurrent readers do not update its refs or share
installation locks. Publish the repository once, keep it immutable during all
runs, and use distinct output directories. Never put outputs inside the input
repository. Local repositories are trusted inputs (GPG verification is disabled).

## Prepare once, consume twice

After provisioning an installation with the required runtimes and extensions:

```sh
python3 action/prepare-dependencies.py \
  --installation /var/lib/flatpak --output inputs/dependencies

flatpak-smoke verify-bundle inputs/app.flatpak \
  --dependency-repo inputs/dependencies --output results/verify
flatpak-smoke screenshot-bundle inputs/app.flatpak \
  --dependency-repo inputs/dependencies --recipe recipe.yml --output results/screenshots
```

The exporter requires Python 3.9+, Flatpak and OSTree. It reads **deployed commit
checksums**, copies those commits into a staged repository, writes `inputs.json`
with the exact runtime refs/checksums and Flatpak version, and publishes by rename.
It refuses to update an existing snapshot. Failed preparation leaves no published
output. Concurrent publishers should use unique output names; a completed snapshot
cannot be overwritten. The source installation must remain available during export.

Use repeated `--ref runtime/ID/ARCH/BRANCH` to export a selected dependency set.
Without `--ref`, all deployed runtimes, including extensions, are selected.
Commits must be complete in the source repository: partially downloaded Locale
or other subpath deployments can fail export. Provision those refs in full first,
or explicitly select the complete refs you need. Export does not fetch missing
objects from the network. Preserve OSTree permissions, symlinks and extended
attributes when transporting a bare-user-only snapshot; a container image layer
or an appropriate archive is preferable to a naive file copier.

For the action, use the same snapshot in both steps:

```yaml
with:
  bundle: inputs/app.flatpak
  dependency-repo: inputs/dependencies
  output: results/verify # use a different output for screenshots
  image: flatpak-smoke-with-dependencies
```

Until this feature is included in a versioned release image, build this checkout's
`Containerfile` (or `Containerfile.screenshots`) and set `image` to that local image.
The existing v0.4.2 images do not accept this new option.

The existing `repo` + `app-ref` input also works with `dependency-repo`; this avoids
app bundle serialization if you already have a **published** app repository.
An installation's `/var/lib/flatpak/repo` is not a drop-in published repository:
it can lack a summary and have deployment/remote-prefixed refs. Keep app refs
frozen as well. A moving local app ref is not a commit pin.

## Reproducibility and backend dependencies

With an explicit dependency repository, flatpak-smoke points both installation
and launch at empty private system/config directories. Thus host/image runtimes
cannot silently satisfy dependencies and disappear from `phase_metadata.runtimes`.
The selected runtime and extension commits are installed in the private user
installation and recorded using the existing per-phase provenance fields.
The repository URL and installation commands are recorded in `logs/runner.log`.
`inputs.json` records the available set; phase metadata records the selected set.

The local dependency remote has higher priority than the optional Flathub remote.
For reproducible dependency resolution, supply a complete frozen input set and
omit `--allow-network-remotes`. Enabling it permits missing dependencies to be
resolved independently from Flathub; compare the resulting per-phase commits.
This flag is not a network sandbox: apps and bundle-provided origins may still
use the network. Enforce network isolation externally if required.

Include the union of dependencies needed by both desktops, not just those
installed by verification. For example, VLC's screenshot environment selects
`org.kde.WaylandDecoration.QAdwaitaDecorations` in addition to the common KDE/GL
refs. Flatpak retains its normal conditional extension selection; we neither
force-install every snapshot ref nor suppress related refs with `--no-related`.
Missing optional extensions can be skipped by Flatpak, so a successful install
alone does not establish snapshot completeness. Compare phase metadata to your
expected backend-specific dependency set. Pin runner images and platform/arch
alongside the snapshot.

## Design choice and measurements

The consumer at `d7359aad16e3ec2840fadb7d629ad12799438290` pins v0.4.1;
this implementation is on the v0.4.2 baseline. Its run 39761057053 spent
405.995 / 564.621 seconds (71.9%) installing across 13 phases. This is summed
phase time, not workflow wall time, and excludes host preparation/image pulls.

`tests/compatibility/prepare.py` already provisions system runtimes into its image.
That avoids runtime redeployment, but implicitly couples runs to mutable/system
installation state and the existing provenance reader lists only user runtimes.
The explicit repository input instead reuses immutable content, keeps deployments
private, and makes runtime provenance complete for this opt-in path. Existing
system-provisioned behavior without the option is unchanged.

Local experiment on 2026-10-22, Flatpak 1.18.4, tmpfs storage, real VLC bundle:

- First fresh installation from the prepared repository: **4.214 s**.
- Second fresh verification installation from the same repository: **4.212 s**.
- Fresh screenshot-environment installation: **4.213 s**.
- Export from the already provisioned host installation: **2.149 s** (initial
  dependency downloads excluded).
- Snapshot size: approximately **1.5 GiB**, containing KDE Platform, GL default, GL extra, and
  the Wayland-decoration extension.
- App commit: `5e38b439b73601ed51c3a911b04c6b233d9fa6fdfd8ad1381585c37af3f79f7d`.
- KDE Platform commit: `1e9b0aa4623015cebd20350b36dc515124567b0f43c2235bd4ea8259fc2b18de`.
- App/common runtime commits matched across all three runs; screenshot provisioning
  additionally selected Wayland-decoration commit
  `5f1d32b236a386ed21a66248c9001b94b8261f46ef03399dc87e376835149887`.

These timings include remote registration, installation and provenance queries.
“First” means an empty private installation, not a flushed OS page cache; the
snapshot was already prepared. “Warm” still deploys into a new installation.
There is effectively no warm deployment saving here: the benefit is avoiding
repeated remote downloads/resolution. These are installation-only measurements,
not desktop launch/capture tests, and cannot be compared directly to hosted CI
timings. Snapshot preparation, storage/transport, app installation and per-run
runtime deployment remain costs. No claimed workflow-wall-time saving.

A subsequent fresh Flathub baseline took **52.227 s**, followed by local runs of
4.212 / 4.211 / 4.212 s. The app and common runtime commits matched, but Flathub
also installed codecs-extra, KStyle.Adwaita and KDE Locale, which the selected
snapshot omitted. **This is not an equivalent-input speedup comparison.** Trying
to export the host's Locale commit confirmed the partial-deployment limitation:
OSTree rejected a missing dirtree object and no snapshot was published.

The same real VLC bundle installed successfully from a read-only snapshot mount
in the v0.4.1 screenshot image (Flatpak 1.16.6), with container networking disabled.
This validates the Flatpak transaction on the consumer's toolchain. A subsequent
CLI build using `rust:1-trixie` also installed from the snapshot in this image.
Full local capture was blocked by the rootless Podman environment: bubblewrap
failed to configure loopback (`Failed RTM_NEWADDR: No child processes`) before
application startup. No successful local desktop capture is claimed.

## Validation and reproduction

```sh
cargo test
python3 -m unittest discover -s tests -p 'test_*.py'
cargo test --lib local_dependencies_are_reusable_and_private -- --ignored --nocapture

FLATPAK_SMOKE_BENCHMARK_BUNDLE=/absolute/app.flatpak \
FLATPAK_SMOKE_BENCHMARK_REPO=/absolute/dependencies \
cargo test --lib benchmark_prepared_dependencies -- --ignored --nocapture
```

Set `FLATPAK_SMOKE_BENCHMARK_NETWORK=1` to prepend a fresh Flathub install for a
network baseline (requires network access; its resolved commits may differ).
The synthetic real-Flatpak test verifies missing-dependency failure, export of
deployed checksums, bundle and local-app-repo installation, extension provenance,
and concurrent independent readers, and checks that readers leave the snapshot
unchanged. CI runs this test explicitly, and its Docker end-to-end jobs use
prepared dependencies for non-root verification and root/non-root screenshots.
The action contract test verifies the read-only mount and input forwarding for
both modes. Hosted consumer benchmarks remain follow-up validation.
