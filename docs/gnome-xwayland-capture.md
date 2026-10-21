# GNOME Xwayland capture startup in v0.4.0

## Pinned evidence

Both flathub-health runs
[39484207867](https://github.com/razzeee/flathub-health/actions/runs/39484207867) and
[39514153322](https://github.com/razzeee/flathub-health/actions/runs/39514153322)
failed in screenshot desktop initialization, before app launch:

```text
selected screenshot display backend: Xwayland
libmutter-ERROR: Failed to start X Wayland: Directory "/tmp/.X11-unix" is not writable
GNOME exited during capture with exit status: 133
```

The downloaded `desktop.stderr.log` identifies Mutter 48.7. The `libmutter-ERROR`
is a fatal initialization error (133 is 128 + SIGTRAP), not evidence of an app
crash, normal session teardown, or a C assertion. No app launcher was recorded.
Neither hosted failure tested the VLC recipe or produced a screenshot.

The v0.4.0 tag resolves exactly to
`a84805caeaa4ae5c3deb80d76128c22139f77bf2`. Reproduction uses the hosted screenshot
image by immutable digest:

```text
ghcr.io/razzeee/flatpak-smoke-screenshots@sha256:7b29b3d7516046720b56be7a96059f30abdf05bfe0e094689ad51bf2ce20753f
```

Local VLC and its KDE runtime match the first hosted run:

- App: `org.videolan.VLC//stable`, commit
  `5e38b439b73601ed51c3a911b04c6b233d9fa6fdfd8ad1381585c37af3f79f7d`.
- Runtime: `org.kde.Platform//5.15-25.08`, commit
  `1e9b0aa4623015cebd20350b36dc515124567b0f43c2235bd4ea8259fc2b18de`.
- Recipe fetched at flathub-health revision
  `527cc2756b0fcee4e7b2124b4d797ed2fb48acd0`, MD5
  `afd3a2238b098be5cdd43350b4e4aa29`, with its original 179225-byte video.

## Controlled diagnosis

The exact image, bundle, recipe and video reproduce `display_exited` / 133 as
UID 65534 with umask 022. `/tmp/.X11-unix` is initially absent; after failure it
is **1755, owned by 65534:65534**. Ownership is correct, but permissions are not.

[Mutter 48.7's socket setup](https://github.com/GNOME/mutter/blob/48.7/src/wayland/meta-xwayland.c)
calls `mkdir(..., 01777)` without overriding umask. Its later existing-directory
check requires group/other write bits and the sticky bit. GNOME reserves both
public and service X11 displays, so the second check rejects the directory made
by the first under umask 022.

This is an incomplete integration in the Xwayland screenshot path added by
`ef774c1`, not a regression in the Weston startup verifier from PR #27. v0.4.0
implements Xwayland capture selection; its startup failure is therefore a runner
bug, not an expected "X11 apps unsupported" limitation.

Changing only directory preparation to root-owned 1777 removes the trap, but
reveals a second prerequisite: v0.4.0 probes hard-coded `:0` without Xauthority.
GNOME's authenticated Xwayland server rejects that connection. The runner must
use the display and authority file established by its own GNOME session, not a
caller display or a sandbox-only `--env=DISPLAY` override.

## Fix and regression coverage

- Reuse the existing X11 socket-directory initializer before GNOME Xwayland
  startup. Newly created directories are explicitly set to 1777, independent of
  umask; no container-root ownership change is required. Existing shared
  directories and unrelated files are not chmoded, re-owned or removed.
- The private GNOME helper reports its display and Xauthority path. Validate a
  local display and an authority file inside the run's private runtime directory;
  use them for the bounded readiness query and as parent environment for Flatpak's
  normal socket/authentication mapping. Do not disable X11 authentication.
- Ask the owning GNOME session to quit cooperatively before forced process-group
  cleanup. Killing mandatory Xwayland at the same instant as GNOME can otherwise
  skip Mutter's socket/lock cleanup. The cooperative phase is bounded to one second,
  works after cancellation, and retains forced cleanup as a fallback. Cleanup
  never replaces the original failure or retries an application.
- Preserve GNOME/Wayland as the compositor and native-window capture/input path.
  Wayland-only sessions still use `--no-x11` and do not initialize Xwayland.

The ignored real-GNOME regression
`screenshot::gnome::tests::xwayland_session_starts_with_authenticated_readiness`
was observed failing in approximately two seconds with the exact 133 diagnostic
before the fix. It now starts two simultaneous sessions and verifies distinct
displays/authority files, so a hard-coded `:0` is not sufficient to pass.

Unit tests cover socket-directory mode and preservation of existing contents,
display/auth propagation and rejection of missing, remote or foreign credentials.
`tests/xwayland_capture.py` runs as non-root with umask 022 in a fresh screenshot
container. It checks native Wayland, X11 native captures and keyboard input, two
simultaneous X11 captures, private workspace/process cleanup, and detached-app
cancellation with socket/lock cleanup. The corresponding CI step builds both
minimal fixture manifests using GNOME Platform 49.

Local loops and downloaded evidence are under
`/tmp/opencode/vlc-39484207867-repro/` and
`/tmp/opencode/flathub-health-run-{39484207867,39514153322}/`.
The full released-image loop is `python3 /tmp/opencode/vlc-39484207867-repro/loop.py baseline`;
the minimized real-GNOME test driver is `session-test.py` in the same directory.
Local fixture capture evidence is in `target/xwayland-capture-local/run4/`.
Docker daemon access remains denied locally; rootless Podman is the feasible path.
Its image `/run` copy-up and privileged GPU bind mounts were hidden in the local
harness to match hosted surfaceless Docker, not changed in production code.

## What this does not establish

After fixing startup/authentication, the **unchanged** hosted VLC recipe reaches
app launch and fails ambiguous-window selection (two matching windows in the
fresh profile). A diagnostic-only recipe suppressing VLC's privacy prompt and
repeating the five-second media file separately hits the existing strict focus
guard while the VLC window is hidden. These remain real failures; no focus retry,
VLC-specific launch workaround, or window-selection relaxation is included here.
The downstream recipe and its actual-media screenshot completion check remain
unverified. No release is published and no downstream pin or recipe is changed.
