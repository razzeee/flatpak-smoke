# Screenshot compatibility

The real-app matrix tests GNOME Calculator, KCalc, and VSCodium using their
Flathub bundles. Each app runs three times serially and twice concurrently, with
two captures per run. Failed runs remain failures; the matrix does not retry them.

## Tested configuration

The local matrix passed all 15 runs with these builds:

| App | Version | Runtime | Recipe coverage |
| --- | --- | --- | --- |
| GNOME Calculator | 51.0 | GNOME 50 | Initial view and keyboard calculation |
| KCalc | 26.04.3 | KDE 6.10 | Initial view and keyboard calculation |
| VSCodium | 1.135.06055 | Freedesktop SDK 25.08 | Package README and Settings opened by keyboard |

These results cover x86_64, GNOME Shell 48.7 on Debian trixie, software rendering,
English, scale one, and a non-root app process. They establish compatibility for
these recipes and builds, not every app built with GTK, Qt, or Electron.
VSCodium's accessible text-entry support is not covered by this matrix.

Calculator needs a window taller than its 616-pixel minimum. Its recipe requests
400×650. A request below an app's minimum size fails window readiness.

Run VSCodium as a non-root user. The matrix starts the system bus as root, then
uses `setpriv` to run the matrix and apps as UID/GID 65534. No extra Chromium
sandbox flags are added. The capture runner follows private Flatpak instances
when a launcher exits successfully, and stops those instances during cleanup.
It starts and unlocks one private keyring daemon before launching the app.
Desktop modal dialogs block capture/input rather than silently receiving input
intended for an app.

## Run the matrix

From the repository root:

```sh
docker build -t flatpak-smoke-screenshots -f Containerfile.screenshots .
docker build -t screenshot-compatibility -f Containerfile.compatibility .
image_id=$(docker image inspect screenshot-compatibility --format '{{.Id}}')
mkdir -p target
docker run --rm --privileged --tmpfs /run --tmpfs /dev/dri \
  -v "$PWD:/workspace" -w /workspace "$image_id" sh -c '
    install -d -o 65534 -g 65534 target/compatibility
    exec setpriv --reuid=65534 --regid=65534 --clear-groups \
      python3 tests/compatibility/matrix.py \
      --output target/compatibility/run --image-id "$1"
  ' compatibility "$image_id"
```

Choose a fresh output directory for each invocation. For a diagnostic run, add
`--app vscodium --repeats 1 --concurrent-runs 0`; the report labels reduced
coverage as `probe`. Increase `--repeats` and `--concurrent-runs` for longer runs.
`--concurrency` controls the number of simultaneous runs of the same app.

The image build resolves Flathub apps and runtimes once and exports bundles.
Every repetition uses those bundles and the same installed runtimes. Rebuilding
without cached provisioning layers can resolve newer upstream commits.
Keep the built image to repeat a particular environment. Its local image ID
identifies that build but is not a registry download URL.

Scheduled and manually dispatched CI runs execute this matrix and upload
`target/compatibility/**` even when a run fails. PR CI also checks the report
validator and exercises the separate GTK/Qt fixture recipes. Both suites check
that cancellation invokes cleanup within the run's private runtime directory
and terminates the observed sandbox processes and their descendants. The tests
record PID start times to distinguish surviving processes from reused PIDs.
The scheduled suite requires VSCodium's cancellation case to include processes
outside the launcher's process group. Process snapshots and survivor lists are
retained with the cancellation evidence.

The fixture suite also makes a detached launcher's liveness probe intentionally
slow and checks that the probe is stopped at the recipe action's deadline.
Liveness polling shares the active action/overall deadline.

## Read the evidence

`report.json` records app/runtime commits, bundle and recipe hashes, the runner
binary hash, image ID, installed packages, run timings, and individual outcomes.
Each run retains its normal screenshot manifest, PNGs, and diagnostic logs.

A passing matrix requires successful CLI results, ordered captures, matching
app refs and artifact lists, decodable PNGs with accurate dimensions, and the
English/scale-one profile. It also checks consistent geometry, distinct
workspaces, overlapping concurrent invocations, and surviving processes whose
`HOME` belongs to a capture workspace. That process check is limited to processes
whose environment the matrix user can read.

Decoded pixel hashes and alpha ranges are observations. Different pixel hashes
can reflect caret blinking or other animation and do not fail the matrix.
Successful smoke coverage does not promise byte-identical images or certify
Flathub's editorial screenshot quality. Review the images before publishing.
