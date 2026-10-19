"""Reject a release tag that would select different images in the action."""
import os
from pathlib import Path


def main():
    tag = os.environ["RELEASE_TAG"]
    version = Path("action/image-version").read_text().strip()
    if tag != version:
        raise SystemExit(
            f"Release tag {tag!r} does not match action/image-version {version!r}. "
            "Prepare a new version commit and use an unused tag; do not move existing tags."
        )


if __name__ == "__main__":
    main()
