# How to release

A release uses the same version tag for the GitHub Action and both container
images. CI publishes the images after the tagged revision passes its checks.

## 1. Prepare the version

1. Choose an unused `v*` version tag, such as `v0.2.0`.
2. Set `action/image-version` to that exact tag.
3. Update the versioned examples in `README.md` and `docs/reference.md`, and the
   default-image assertion in `tests/test_action.py`.
4. Commit and merge these changes into `main`, then wait for CI to pass.

## 2. Tag the release

From an up-to-date checkout of `main`, verify that `HEAD` is the intended release
commit. Replace the example version below with the version you prepared:

```sh
VERSION=v0.2.0
git tag -a "$VERSION" -m "Release $VERSION"
git push origin "$VERSION"
```

The tag triggers Cargo checks, action wrapper tests, fixture smoke tests, and
screenshot end-to-end tests. Publication fails if the tag does not match
`action/image-version`.

## 3. Wait for the images

Check that both publishing jobs complete successfully:

- `ghcr.io/razzeee/flatpak-smoke`
- `ghcr.io/razzeee/flatpak-smoke-screenshots`

Each image gets the version tag and a `sha-<full-commit-sha>` tag, with OCI source
and revision labels. The publishing jobs use `GITHUB_TOKEN` with `packages: write`.
Both GHCR packages must permit publication from this repository's workflows.

The two pushes are independent. If either fails, fix the cause and rerun the
failed job before announcing the release. Keep the release tag on its original
commit; code fixes need a new version.

## 4. Verify the release

Ensure both GHCR packages are public. Using a Docker configuration without GHCR
credentials, pull the versioned images:

```sh
docker pull "ghcr.io/razzeee/flatpak-smoke:$VERSION"
docker pull "ghcr.io/razzeee/flatpak-smoke-screenshots:$VERSION"
```

Run the README's smoke-test and screenshot examples with a known fixture bundle.
Also check the versioned GitHub Action in a consuming workflow, including artifact
upload after a failed check.

## 5. Publish release notes

Create a GitHub release for the existing tag. Describe the changes and include
both image names and their digests. Consumers can use the exact action tag or pin
its commit together with an image digest.
