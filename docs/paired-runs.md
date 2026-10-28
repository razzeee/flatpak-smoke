# Paired screenshot runs

`action/compare.py` runs a published baseline bundle and a candidate with one
recipe, fixture tree, runtime snapshot and screenshot image. It produces evidence
of changed recipe behavior, **not proof that an application change caused it**.
Two attempts per artifact run sequentially in baseline/candidate/candidate/baseline
order. Every attempt has a fresh container, installation, profile and desktop.

## Run

Use Linux amd64, Python 3.9+, and Docker with privileged-container support. Build
the screenshot image from this checkout: older published images lack the backend
and failed-action metadata required for conclusive comparisons.

```sh
docker build -t paired-screenshots -f Containerfile.screenshots .
```

Prepare a dedicated input directory, for example:

```text
comparison-inputs/
  published.flatpak
  candidate.flatpak
  recipe.yml
  fixtures/
  dependencies/
```

Use [prepare-dependencies.py](reusable-dependencies.md) to export a complete frozen
dependency repository including required extensions. Obtain the published bundle
and its full app ref/OSTree commit from the release source. Publication provenance
is caller supplied; this runner verifies that the installed baseline matches that
ref/commit, but does not authenticate a release service or infer publication from
a filename. Remote download/discovery is a separate preparation step.

```sh
python3 action/compare.py \
  --inputs comparison-inputs \
  --baseline published.flatpak --candidate candidate.flatpak \
  --recipe recipe.yml --dependency-repo dependencies \
  --baseline-source https://dl.flathub.org/repo/ \
  --baseline-ref app/org.example.App/x86_64/stable \
  --baseline-commit "$PUBLISHED_APP_COMMIT" \
  --image paired-screenshots --user nobody \
  --output artifacts/comparison
```

All artifact/recipe/dependency paths are relative to `--inputs`; fixture paths
remain relative to the recipe. Output must be new and must not overlap inputs.
The entire dedicated input tree is copied into `OUTPUT/inputs` (including modes,
symlinks and available extended attributes), so budget disk space for the runtime
repository. Do not use the project root as the input directory. Keep inputs stable
during preparation. The prepared copy is fingerprinted before and after execution
and mounted read-only; only each attempt's output is writable. No app data is
carried between attempts. This tests clean starts, not upgrades.

The image must already exist locally; its tag is resolved once to a content ID.
All attempts run on the same Docker daemon with `--network none`, the same image,
user and CLI defaults. Network remotes are disabled. Applications requiring live
services will need offline fixtures or future explicit service support. Privileged
containers are the existing execution model, not a security boundary for hostile
apps. Wall-clock time, scheduling and app-internal randomness are not frozen.

`--execution-timeout` (default 900 seconds) and `--cleanup-timeout` (30 seconds)
apply to each attempt. Failures do not stop the other artifact from being tested;
cancellation stops further attempts after bounded cleanup. There are no adaptive
retries and no majority vote.

## Evidence and classification

`comparison.json` schema version 1 is separate from single-run `result.json` v1.
It contains the verdict, structured reason codes, caller-supplied baseline source,
bundle SHA-256 checksums, snapshot fingerprint, and the ordered attempts. Each
attempt retains its role/path, process exit code, execution record, result and
screenshot manifest, plus image ID/OS/architecture, Docker daemon ID/kernel/server
version, user, network policy and execution timeout settings. CLI defaults and
tools are fixed by the image. `comparison.md` links the original outputs and
lists per-attempt timings; artifacts stay in `baseline/1`, `candidate/1`,
`candidate/2`, and `baseline/2`. Execution records are siblings of those directories.
Referenced screenshots are checked for existence and recorded with byte size and
SHA-256 checksum; missing artifacts make evidence inconclusive. Those checksums
support review and do not turn pixel differences into failures.

Conclusive evidence requires:

- Known installed app commits, stable within each artifact's repetitions, and
  the same app ID/architecture. Baseline and candidate branches may differ.
- Identical actual runtime **and extension** ref/commit sets. Ordering and the
  optional `runtime/` prefix are normalized. Null, empty, duplicate or incomplete
  runtime provenance is inconclusive. Available snapshot refs alone are not
  evidence of which dependencies were selected.
- Matching recognized `v1:sha256` recipe input digests. This covers exact recipe
  bytes and copied fixtures. Legacy `content_hash` describes only recipe bytes
  and cannot substitute for a missing input digest. Changing fixtures requires
  rerunning both artifacts; formatting-only recipe changes also change identity.
- Matching image/daemon/environment and desktop/version, backend, runner version,
  language and scale. A Wayland/Xwayland switch or a runtime migration is reported
  as inconclusive for this app-only comparison.
- Completed, consistent execution/result/manifest records and successful cleanup.
  Missing fields, nulls, malformed records and unsupported schemas are not equal
  evidence, even when both attempts lack the same field.
- Repeatedly identical outcomes within each artifact. Two agreeing attempts are
  a bounded repeatability check, not a statistical claim that the app is non-flaky.

Verdicts:

- `new_regression_signal`: baseline passes; candidate reproducibly fails an
  explicitly categorized recipe expectation.
- `existing_failure`: both reproducibly fail the same expectation signature.
- `improvement`: baseline reproducibly fails an expectation; candidate passes.
- `unchanged_pass`: both pass.
- `inconclusive`: failed comparability checks, infrastructure/uncategorized errors,
  differing failure signatures, interrupted runs, or mixed outcomes. Different
  outcomes from the same installed app commit are also inconclusive.

The expectation signature is input digest + zero-based failed step index + action
kind + failure reason. The digest/index identifies the action's exact arguments.
Messages are diagnostic, not identifiers. Initial-window failures without a
recipe-step identity, and currently uncategorized failures (including some text
wait timeouts), remain inconclusive. An existing failure says the same observation
was present, not that both artifacts have the same root cause. Later steps are
unobserved, so it does not establish that there are no additional regressions.

Screenshots, dimensions and timing changes are supporting evidence only. No pixel
threshold or timing delta determines a verdict, and passing recipes do not prove
pixel equality or general application correctness.

## CI policy and integration

Default exit status is zero after a completed report, including regression signals
and inconclusive results. `--fail-on-regression` returns 1 only for
`new_regression_signal`. Invalid inputs/orchestration errors return 2; cancellation
returns 130. A container startup failure or timeout captured as an attempt is
reported as inconclusive. Upload results even when the command exits nonzero.

Upload `comparison.json`, `comparison.md`, attempt directories and their sibling
execution records; omit `inputs/` unless you want the full reproducibility bundle.
Future Vorarbeiter integration can consume the versioned report and apply its own
policy without parsing Markdown or changing the individual runner result format.

Validation: `python3 -m unittest discover -s tests -p 'test_*.py'` covers the policy
matrix and mocked execution boundary. `tests/paired_runs.py` runs real pass/pass
and pass/expectation-failure pairs using the existing GTK fixture in screenshot CI.
