# Security and image maintenance

## Trust boundary

The GitHub Action starts Docker with `--privileged` because Flatpak's nested
namespace and sandbox setup is not reliable with a narrower capability set on
all supported CI runners. The action is intended for Linux amd64 runners. Treat
the bundle, repository, recipe, and fixture files as executable/untrusted input:
the app is launched inside the privileged container.

- Prefer ephemeral, GitHub-hosted runners. Do not run untrusted pull-request
  artifacts on persistent self-hosted runners.
- Do not make secrets or sensitive host files available to the job that runs the
  action. The workspace is bind-mounted into the container.
- Keep the job's `GITHUB_TOKEN` permissions minimal. The action itself does not
  need repository write permission.
- For self-hosted environments, evaluate Flatpak namespace support and reduce
  Docker privileges before use; the project does not currently provide a
  supported unprivileged mode.

The `user: nobody` option drops the application process to UID 65534. It is
defense in depth, not a replacement for the container/runner trust boundary:
the action wrapper and system services still start as root.

## Build inputs and updates

The Rust builder and Debian runtime base images in both production Containerfiles
are pinned by OCI manifest digest. Dependabot checks Docker dependencies weekly;
review base-image digest updates rather than replacing the pins with floating
tags. Keep the human-readable tag beside each digest so the intended release
line remains clear.

APT packages are intentionally installed from the current Debian Trixie
repositories at image build time, rather than pinned to old package versions.
This means rebuilds are not bit-for-bit reproducible, but can receive Debian
security fixes without waiting for a package-version edit. A base-image digest
pin alone does not freeze those APT package versions.

Before publishing a new image/action release:

1. Review Dependabot updates and rebuild both production images from the pinned
   base digests, which resolves current Debian packages.
2. Run the full CI suite, including fixture and screenshot end-to-end jobs.
3. Publish the matching versioned images and action tag through the release
   workflow, then verify the published image digest and architecture.
4. Record the resulting image digest for deployments that need immutable image
   selection. The action's version tag is convenient but is not an immutable
   supply-chain pin; set its `image` input to `...@sha256:<digest>` when that is
   required. Pin the action itself to a full commit SHA as well.

The published images and CI build currently target `linux/amd64` only. Other
architectures are not tested or supported; adding them requires native or
emulated end-to-end coverage for Flatpak, Xwayland, and screenshot capture.

Report security issues privately to the repository owner rather than opening a
public issue with exploit details.
