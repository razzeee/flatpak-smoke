# VLC GUI startup: controlled diagnosis

## Evidence

- Hosted failure: [flathub-health run 39398900795](https://github.com/razzeee/flathub-health/actions/runs/39398900795),
  flatpak-smoke v0.1.2, revision `7b4e5db4c134098f41fd7b21b0eaa606a970580f`.
- [VLC manifest 144698b854c1](https://github.com/flathub/org.videolan.VLC/blob/144698b854c18c4a4164738f4dfeb2688701df49/org.videolan.VLC.yaml)
  grants `x11`, not `wayland`. The verifier discarded `DISPLAY` and forced Wayland
  toolkit settings. Preparation succeeded; verification failed with `early_exit`
  and “no private Flatpak instance remains.”
- Actual downloaded logs under
  `/tmp/opencode/flathub-health-run-39398900795/health-org.videolan.VLC/verify/logs/`
  show “no suitable interface module,” skins2 OSFactory failure, CLI fallback,
  then shutdown. PulseAudio refusal is also present.

## Controlled local comparison (2026-10-20)

Docker daemon access is denied at `/var/run/docker.sock`. Rootless Podman works,
including nested Flatpak namespaces with container `--privileged` (no host root).
Tests used a Debian trixie environment with Weston **14.0.2**, matching the hosted
compositor, and a read-only mount of the installed system Flatpaks. A private
system bus was started before dropping to UID 1000 for verifier runs.

Local VLC is **3.0.23**, runtime `org.kde.Platform//5.15-25.08`, app commit
`5e38b439b73601ed51c3a911b04c6b233d9fa6fdfd8ad1381585c37af3f79f7d`, whose
commit subject identifies the same manifest revision `144698b854c1`.
`flatpak build-bundle /var/lib/flatpak/repo target/org.videolan.VLC.flatpak org.videolan.VLC stable`
produced the input for both verifier runs.

1. A four-second headless Weston/Flatpak probe reproduced the exact interface
   errors and CLI shutdown with no X11 display. Adding Weston's `--xwayland`
   and its allocated `DISPLAY` produced persistent “VLC media player” and
   “Privacy and Network Access Policy” windows, confirmed with `xwininfo`.
   The PulseAudio refusal remained. Repeating on Weston 14.0.2 confirmed this.
2. Keeping **all original Wayland toolkit flags** while providing Xwayland also
   produced those persistent windows. Thus the missing usable display is causal;
   Qt forcing was not independently proven causal for VLC. Toolkit selection is
   still made consistent with display capabilities for other apps.
3. The pristine pinned verifier, rebuilt from `git archive HEAD` in a separate
   temporary directory, was run against that bundle in the same environment:

   ```text
   /baseline/target/debug/flatpak-smoke verify-bundle target/org.videolan.VLC.flatpak --output target/vlc-wayland-baseline --overall-timeout 60s
   status: failed; reason: early_exit
   message: app exited during verification; no private Flatpak instance remains
   total: 12108 ms
   ```

4. The patched verifier, with no VLC-specific launch changes, passed:

   ```text
   target/debug/flatpak-smoke verify-bundle target/org.videolan.VLC.flatpak --output target/vlc-xwayland --overall-timeout 60s
   selected app display backend: Xwayland
   Weston Xwayland display ready: :0
   status: passed; total: 13495 ms
   screenshots: screenshots/000-window-visible.png
   ```

   Both runs used a fresh private installation/profile/runtime directory. The
   passing run still logged PulseAudio connection refusal. The normal persistent
   content, screenshot and OCR checks ran over the same Weston VNC connection.
   Local evidence is in `target/vlc-wayland-baseline/` and `target/vlc-xwayland/`.

An exploratory concurrent probe once hit an unrelated Flatpak GL-extension
`invalid merge-dirs` error; the serial repetition passed. This was not treated as
an app failure requiring retries. Weston 14 also requires `/tmp/.X11-unix` to exist;
the verifier creates it when missing without changing an existing directory's
permissions or deleting other runs' sockets/locks.

## Policy and validation

Use effective socket permissions, not app IDs or stderr heuristics. Prefer
Wayland when granted; select Xwayland for `x11` / `fallback-x11` without Wayland.
Do not override Flatpak socket permissions or relaunch failed apps. This addresses
the confirmed mismatch, not every possible application GUI initialization bug.

Rust regression tests cover permission selection (including both sockets,
`fallback-x11`, negative permissions and irrelevant sections), querying the
private installation, launch arguments and environment isolation, owned-display
parsing and bounded discovery. The toolkit regression was observed failing before
the fix with `cargo test xwayland_launch_does_not_force_unavailable_wayland_toolkits`.

`tests/xwayland.py` exercises real fixture Flatpaks: native Wayland, X11-only
capture and clicks, two concurrent Xwayland sessions, preserved `early_exit`,
cancellation/process cleanup and pre-launch readiness failure. It also checks
private workspace and X11 socket/lock cleanup. Run inside the smoke e2e image
with both fixture bundles built and their runtimes installed system-wide:

```sh
python3 tests/xwayland.py --binary /usr/local/bin/flatpak-smoke
```

CI builds both manifests and installs GNOME Platform 49 in the e2e image's own
system installation. Local fixtures were compiled from the same C source against the host's
installed GTK and packaged with available GNOME Platform 51; CI uses the manifests'
GNOME Platform 49. This difference must not be mistaken for hosted validation.

Local validation passed: all 83 non-ignored Rust tests, the separately invoked
real-ImageMagick content regression, clippy with warnings denied, formatting,
17 Python unit tests and actionlint 1.7.7. All seven real display/lifecycle cases
passed under rootless Podman; final development-binary artifacts are in
`target/xwayland-v5/`. Cancellation also checks that Xwayland is rootless and
does not expose the corresponding TCP port (rather than assuming Weston passes
an explicit `-nolisten tcp` flag).

The actual `Containerfile` also built successfully with:

```sh
podman build --target e2e-tests -t localhost/flatpak-smoke-xwayland-e2e -f Containerfile .
```

The initial build command exceeded its two-minute tool deadline during package
downloads; rerunning without that tool limit completed. The resulting image,
`fa6c72d431339db2c3bada5b35f1ca885765f7ed39ff2fc1fb0e9f9fed697993`, contains
the production release binary and runtime plus Python for tests. As UID 1000,
`doctor`, all seven display/lifecycle cases, and the real VLC bundle verification
passed using that image's release binary. Artifacts are in
`target/xwayland-container/` and `target/vlc-xwayland-container/`.

The source container build and execution were validated with rootless Podman;
Docker daemon access remains denied locally. Hosted PR validation is described
below; no manual workflow dispatch was used. No release was
published and no downstream runner pin was changed. OCR deadline handling on
`fix/verification-ocr-deadline` is outside this change.

## Hosted CI follow-up

PR #27's first hosted run, `39408151263`, passed Cargo checks, the existing smoke
fixtures/lifecycle tests and native screenshot recipes. The added display suite
failed before its first Wayland app started: `bwrap: Can't find source path
/proc/self/fd/16: Permission denied`. That suite uniquely remapped the runner's
user Flatpak installation as `/var/lib/flatpak` inside a rootful container. A local
rootless Podman probe of a user-runtime mount did not reproduce the failure.

The e2e image now installs its own system GNOME Platform 49 runtime, matching the
existing screenshot test image's provisioning pattern; CI no longer remaps a host
user installation as a system installation. This is a test-environment correction,
not an app-failure retry or a change to production display selection. The corrected
image (`88044127c1ca68b4c5cfe074a1e7fcd9153e6bc991580e0bd546afd37cbf3f08`)
built locally. All seven cases passed as root without a host runtime mount, using
the image's system GNOME Platform 49 and locally compiled fixtures packaged for
that runtime; artifacts are in `target/xwayland-system-gnome49/`. Hosted
revalidation is still needed to confirm the rootful Docker correction.
