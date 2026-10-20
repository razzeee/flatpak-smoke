# Verification OCR under CPU contention

## Evidence

[Hosted run 39398900795](https://github.com/razzeee/flathub-health/actions/runs/39398900795)
used flatpak-smoke v0.1.2 (`7b4e5db4c134098f41fd7b21b0eaa606a970580f`).
VSCodium installed in 44,852ms, passed persistent-content detection and capture,
then failed with `screenshot_failed` / `failed to run tesseract for screenshot OCR:
deadline elapsed`. Total time was 64,838ms. This is not the earlier bundle-export
failure.

The downstream invocation explicitly supplied `--overall-timeout 5m`, not the
verifier's default 60s. Installation consumes that overall budget; it does not
consume the later screenshot stage's default 10s. Capture, foreground/content
checks and error-text OCR share that screenshot deadline. OCR does not receive a
fresh 10s. The saved logs do not record the exact remaining budget at OCR entry.

The verification image uses OpenMP-enabled Tesseract 5.5.0. Replaying the saved
`verify/screenshots/000-window-visible.png` in that image under a rootless Podman
0.25-CPU quota timed out at 10s with unrestricted OpenMP. Changing only
`OMP_THREAD_LIMIT=1` completed in approximately 5s. With two CPUs, unrestricted OCR
completed in approximately 2s. The saved image itself is readable, including the
VSCodium README, and contains no recognized fatal-error marker.

Recipe OCR already limits OpenMP to one thread; verification's three OCR paths
did not. Applying that existing policy addresses the reproduced CPU-contention
failure without changing deadline scope, timeout defaults or error handling.
The hosted failure is consistent with this mechanism, but its exact CPU load and
remaining screenshot budget cannot be recovered from the artifacts. A hosted
rerun remains necessary to confirm the full VSCodium session.

## Reproduction and regression

No Docker socket, privileged container or running Flatpak is required. Set
`SCREENSHOTS` to the absolute directory containing the downloaded screenshot.
On SELinux hosts, use an appropriate bind-mount label (the examples disable
container labeling only for these read-only diagnostic mounts).

```sh
IMAGE=ghcr.io/razzeee/flatpak-smoke:v0.1.2
# Baseline: exits 124 under the constrained CPU quota.
podman run --rm --cpus 0.25 --security-opt label=disable \
  -v "$SCREENSHOTS:/evidence:ro" --entrypoint timeout "$IMAGE" \
  10 tesseract /evidence/000-window-visible.png stdout --psm 6
# Same input, deadline and quota: finishes with readable text.
podman run --rm --cpus 0.25 --security-opt label=disable \
  -v "$SCREENSHOTS:/evidence:ro" -e OMP_THREAD_LIMIT=1 \
  --entrypoint timeout "$IMAGE" \
  10 tesseract /evidence/000-window-visible.png stdout --psm 6
```

The deterministic regression exercises the real verification OCR entry points
with private executable stubs. It failed before the fix with the hosted error
message in approximately one second:

```sh
cargo test verification_ocr_limits_threads_without_resetting_stage_budget
cargo test hung_ocr_obeys_stage_and_overall_deadlines
cargo test process_exits_interrupt_ocr_before_its_deadline
```

Coverage includes all three OCR paths, overriding inherited OpenMP settings,
retaining fatal-error detection, exhausted deadlines, nonzero tool exits, missing
tools, hung OCR, and app/compositor exits. The optional saved-image test invokes
real Tesseract through the same bounded verification function:

```sh
FLATPAK_SMOKE_OCR_REPLAY="$SCREENSHOTS/000-window-visible.png" \
  cargo test verification_ocr_replays_saved_screenshot -- --ignored --nocapture
```

For the CPU-constrained replay, build the library test executable using
`cargo test --locked --lib --no-run` in a Debian-compatible Rust container, then
run that executable in the released verification image, with the same 0.25-CPU
quota and `FLATPAK_SMOKE_OCR_REPLAY` pointing to the mounted screenshot. The test
uses a 10s screenshot deadline bounded by a separate 30s overall deadline, and
asserts that OCR succeeds without detecting an app-error marker. With the fix
removed from an isolated source copy (and the diff checked), this exact test
failed in 10.02s with the hosted `screenshot_failed` / `deadline elapsed` error.
The fixed executable passed three consecutive replays in 4.87s, 4.97s and 5.53s.
