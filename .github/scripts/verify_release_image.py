"""Verify public release references against the digest returned by the push."""
import json
import os
import re
import subprocess
import tempfile
from pathlib import Path


def main():
    image = os.environ["RELEASE_IMAGE"]
    tag = os.environ["RELEASE_TAG"]
    revision = os.environ["RELEASE_REVISION"]
    digest = os.environ["RELEASE_DIGEST"]
    if not re.fullmatch(r"sha256:[0-9a-f]{64}", digest):
        raise SystemExit(f"Invalid publication digest: {digest!r}")

    # Do not inherit the publishing job's GHCR credentials.
    with tempfile.TemporaryDirectory() as config:
        # setup-buildx-action can install the CLI plugin in the original config
        # directory. Preserve plugin discovery, but no auths or credential helpers.
        original_config = Path(os.environ.get("DOCKER_CONFIG", Path.home() / ".docker"))
        (Path(config) / "config.json").write_text(json.dumps({
            "cliPluginsExtraDirs": [str(original_config / "cli-plugins")],
        }))
        env = {**os.environ, "DOCKER_CONFIG": config}
        for reference in (f"{image}:{tag}", f"{image}:sha-{revision}", f"{image}@{digest}"):
            try:
                result = subprocess.run(
                    ["docker", "buildx", "imagetools", "inspect", reference,
                     "--format", "{{.Manifest.Digest}}"],
                    env=env, check=True, capture_output=True, text=True,
                )
            except subprocess.CalledProcessError as error:
                raise SystemExit(
                    f"Cannot read public image {reference}: {error.stderr or error}"
                ) from error
            actual = result.stdout.strip()
            if actual != digest:
                raise SystemExit(f"Public image {reference} resolved to {actual!r}, expected {digest}")
            print(f"Verified public image {reference}: {actual}")

    with Path(os.environ["GITHUB_STEP_SUMMARY"]).open("a") as summary:
        summary.write(f"## Published {image}\n\nTag: `{tag}`\n\n"
                      f"Revision: `{revision}`\n\nPublic immutable image: `{image}@{digest}`\n\n")


if __name__ == "__main__":
    main()
