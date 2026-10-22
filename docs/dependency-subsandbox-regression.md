# Dependency reuse and sandboxed subprocesses (#43)

## Root cause

`isolate_system_installations` supplied a private `FLATPAK_SYSTEM_DIR` containing
only an empty directory. The initial `flatpak run --user` could launch the app,
but `flatpak-spawn` asks `org.freedesktop.portal.Flatpak` to launch another
`flatpak run`. That command does not specify `--user`: Flatpak 1.16.6 opens all
configured installations before selecting the app. Opening the uninitialized
private system repository fails, and the subprocess exits 1.

Relevant Flatpak 1.16.6 sources:

- [Portal Spawn handler](https://github.com/flatpak/flatpak/blob/1.16.6/portal/flatpak-portal.c)
- [Run installation selection](https://github.com/flatpak/flatpak/blob/1.16.6/app/flatpak-builtins-run.c)
- [Repository initialization/opening](https://github.com/flatpak/flatpak/blob/1.16.6/common/flatpak-dir.c)

The fix initializes the private empty installation using Flatpak itself:
`FLATPAK_USER_DIR=<private-system-dir> flatpak remotes --user`. The override is
limited to that command. Using user mode avoids a system-helper/polkit operation.
Both isolation variables still apply to installation, app launch and portal
activation. No remotes or refs are added to the private system installation.
Initialization shares the phase deadline, logs through `CommandRunner`, and a
failure is classified as `dependency_failed` in both workflows.

`action/prepare-dependencies.py` exports the deployed runtime checksums into an
immutable repository. It does not create the per-run empty system installation;
no exporter or remote-priority change is needed for this failure.

## Regression evidence (2026-10-22)

A rootless privileged Podman container running the existing screenshot image
`ghcr.io/razzeee/flatpak-smoke-screenshots:v0.4.1` provided Flatpak 1.16.6 and a
working nested Flatpak sandbox. Docker's socket was inaccessible. A tiny
`org.example.SpawnProbe` app ran:

```sh
flatpak-spawn --sandbox --watch-bus sh -ec \
  'grep -qx sandbox=true /.flatpak-info; echo SUBSANDBOX_OK'
```

The same installed app and runtime and the same dependency remote were used
throughout this one-variable-at-a-time comparison:

| Private installation configuration | Result |
| --- | --- |
| Neither isolation variable | child succeeds |
| Only `FLATPAK_CONFIG_DIR` | child succeeds |
| Only `FLATPAK_SYSTEM_DIR`, empty directory | child exits 1 |
| Both variables, empty system directory | child exits 1 |
| Both variables, nonexistent system directory | child exits 1 |
| Both variables, initialized empty system repository | child succeeds |

Failure diagnostic:

```text
error: While opening repository /probe/system/repo: opening repo: opendir(/probe/system/repo): No such file or directory
```

The committed Rust regression uses the actual isolation helper, installer and
D-Bus-activated portal. Before the fix, the container test failed with the same
error for `<workspace>/system-flatpak/repo` (0 passed, 1 failed). After the fix,
it passed (1 passed, 0 failed), asserting successful child exit, its output and
`sandbox=true`, rather than merely accepting a visible window. It also checks
that the private system installation has no refs or remotes.

The final regression (including those empty-installation assertions) also passed
in the **exact pinned v0.5.0 verifier image** used by the failing consumer run:
`ghcr.io/razzeee/flatpak-smoke@sha256:3d40bd3cefd93ba8f06ee27fbd3db244ef18a4dcc90b74b9f68f37950c2be532`.
It reported Flatpak 1.16.6 and `1 passed; 0 failed` in 39.08 seconds. Only the
test executable was supplied from the candidate source; the image was unchanged.

The local runtime was `org.kde.Platform/x86_64/5.15-25.08` at
`1e9b0aa4623015cebd20350b36dc515124567b0f43c2235bd4ea8259fc2b18de` from the saved
dependency export. `FLATPAK_GL_DRIVERS=none` is limited to this shell-only test,
which does not need graphics extensions. These are mechanism-level regression
results, not a new GIMP/VSCodium readiness or screenshot comparison.

Host Flatpak 1.18.4 passed the regression even before this fix; testing only the
host would miss the supported container-version failure. CI therefore runs the
test inside the project's Debian verifier image as an unprivileged user, using
the fixture's exported GNOME runtime.

To run against a prepared runtime repository in an environment with working
Flatpak sandboxing and system/session D-Bus support:

```sh
FLATPAK_SMOKE_TEST_DEPENDENCY_REPO=/absolute/path/to/dependencies \
FLATPAK_SMOKE_TEST_RUNTIME=org.gnome.Platform/x86_64/49 \
cargo test --lib dependency_isolation_supports_sandboxed_subprocesses -- --ignored --nocapture
```

## Consumer exception removal gate

The existing consumer comparisons remain the full-application evidence:

- [39851320733](https://github.com/razzeee/flathub-health/actions/runs/39851320733):
  dependency reuse fails GIMP and VSCodium.
- [39853806251](https://github.com/razzeee/flathub-health/actions/runs/39853806251):
  removing only dependency reuse passes both phases with identical observed
  app/runtime/extension commits and pinned images.
- [39855463739](https://github.com/razzeee/flathub-health/actions/runs/39855463739):
  the consumer mitigation passes all seven apps, with reviewed editor captures.

Before removing the exceptions tracked by `flathub-health#48` and `#49`, test
the released/pinned upstream candidate with dependency reuse restored for both
apps. Require both verify and screenshot recipes to pass their existing
readiness assertions; compare all observed app/runtime/extension commits with
the controls; visually check GIMP's editor, icons and brush previews and
VSCodium's README editor; and require the other five apps to remain green.
Record the candidate image digests and run URL. The subprocess regression alone
does not meet that consumer gate. Do not use the globally debug-enabled run
39852795132 as provenance evidence.
