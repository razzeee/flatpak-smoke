# VLC screenshot initialization

## Reproduction and cause

flathub-health runs `39761057053` and `39756523147` report successful VLC verify
but screenshot initialization fails with `Could not focus selected window`,
before any recipe step. The consumer pins v0.4.1; this change is on the v0.4.2
preparation baseline.

The first run's artifacts were downloaded again locally. The released v0.4.1
screenshot image (`d6fe40e7ef8e`) reproduces the exact error with:

- VLC 3.0.23, app commit
  `5e38b439b73601ed51c3a911b04c6b233d9fa6fdfd8ad1381585c37af3f79f7d`;
- KDE 5.15-25.08 and the host's installed runtimes mounted read-only;
- the recipe and video from flathub-health
  `d7359aad16e3ec2840fadb7d629ad12799438290`;
- GNOME Shell/Mutter 48.7, Xwayland, non-root UID 65534, software rendering.

Instrumentation distinguished two states at the old 100 ms focus check:

1. Selected ID 2, `VLC media player`, visible and unminimized; focused ID 1,
   `Privacy and Network Access Policy`, also visible and unminimized.
   The privacy window has type 4 (`MODAL_DIALOG`) but **no transient parent**.
2. Both windows still hidden, with no focused window. Activating before mapping
   can have no effect. This independently reproduces the readiness race.

Disabling only the privacy prompt allowed the original media recipe to reach
step 0. Its five-second media/OCR wait subsequently timed out. Adding repeat
also exposed the recipe's exact-title selection race as playback changes the
title; that experiment did not establish a working media recipe.

## Focused solution

The helper observes focus for at most two seconds, checking every 50 ms rather
than sleeping once for 100 ms. It activates once, and re-activates only if the
selected window transitions from hidden to visible. It does not retry resize,
input, capture, recipe steps, or runs. Existing caller deadlines still bound the
helper RPC. Window disappearance and desktop modal checks remain failures.

Focus must still equal the exact selected window. A visible application-modal
window also blocks its transient parent; an unparented modal blocks another
window with the same nonempty sandbox app ID, except its own transient
descendants (including nested dialogs). This matters for VLC: X11 can
temporarily grant focus to its main window even while Qt's application modal
prevents useful input. A recipe can explicitly select the dialog itself.

Focus failures now report selected/focused window identities and all managed
windows, including app ID, PID, type, transient-parent ID, hidden/minimized state,
and desktop modal count. This distinguishes selection, modal and readiness
problems without relaxing ownership checks or silently choosing another window.

The compatibility recipe explicitly uses:

```yaml
launch:
  args:
    - --no-one-instance
    - --no-qt-privacy-ask
    - --no-metadata-network-access
```

These are recipe choices, not runner-injected VLC policy. The consumer must make
an equivalent first-run choice; its unchanged recipe remains blocked by the
privacy dialog. A longer delay cannot resolve that modal.

## Validation and limitations

- `node --test tests/gnome-capture.mjs` exercises the actual extension handler:
  delayed focus, mapping transition, bounded timeout, window disappearance,
  VLC's unparented modal (including focus temporarily on the main window),
  nested dialog targeting, wrong-app rejection, desktop modals, and capture
  stream cleanup. The readiness/modal diagnostic regressions fail against the
  original extension; the nested-dialog regression fails before the ancestor
  exemption was added.
- `tests/compatibility/vlc_focus.py` passed against real VLC: the unconfigured
  profile fails before recipe steps with modal diagnostics; the configured
  profile produces validated initial and playlist PNGs after keyboard input.
- The configured VLC matrix passed 3 serial + 2 overlapping concurrent runs,
  with consistent geometry, unique workspaces and no detected remaining app
  processes. Evidence: `/tmp/opencode/vlc-focus/matrix/report.json`; the paired
  regression evidence is in `/tmp/opencode/vlc-focus/regression4/`.
- The full four-app matrix and hosted Docker CI were not rerun locally. Rootless
  Podman required a clean `/run` mount and hiding privileged GPU bind mounts to
  match the hosted software-rendering environment. Runtime provenance differs
  from a fully pinned hosted-image build; the app commit matches exactly.
- Video playback, the consumer's short-video OCR condition, and its changing
  title selection remain outside this initialization regression. No downstream
  pin or recipe was changed.
