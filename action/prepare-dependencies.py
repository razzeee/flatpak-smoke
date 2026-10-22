"""Export deployed runtime commits into a new, reusable local Flatpak repository.

The source installation is read only. Publish once, then keep the output immutable
while verification and screenshot runs consume it with --dependency-repo.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile


def command(*args, env=None):
    return subprocess.check_output(args, env=env, text=True).strip()


def prepare(installation, output, selected_refs=None):
    installation = installation.resolve(strict=True)
    output = output.absolute()
    if output.exists():
        raise ValueError("output already exists; publish a new snapshot instead")
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".dependencies-", dir=output.parent) as staging:
        staging = Path(staging)
        # Do not enumerate other configured system installations.
        config = staging / "config"
        config.mkdir()
        env = {**os.environ, "FLATPAK_SYSTEM_DIR": str(installation),
               "FLATPAK_CONFIG_DIR": str(config)}
        refs = selected_refs or command("flatpak", "list", "--system", "--runtime", "--all",
                                        "--columns=ref", env=env).splitlines()
        refs = ["runtime/" + ref.removeprefix("runtime/") for ref in refs]
        if any(len(ref.split("/")) != 4 for ref in refs):
            raise ValueError("refs must be runtime/ID/ARCH/BRANCH")
        if not refs:
            raise ValueError("source installation has no deployed runtimes")
        commits = {ref: command("flatpak", "info", "--system", "--show-commit", ref, env=env)
                   for ref in sorted(refs)}
        repo = staging / "repo"
        command("ostree", f"--repo={repo}", "init", "--mode=bare-user-only")
        for ref, commit in commits.items():
            # Pull by the deployed checksum, not a potentially newer remote ref.
            try:
                command("ostree", f"--repo={repo}", "pull-local", str(installation / "repo"), commit)
            except subprocess.CalledProcessError as error:
                raise ValueError(f"cannot export complete {ref} at {commit}; the source may be "
                                 "a partial/subpath deployment or have been pruned") from error
            command("ostree", f"--repo={repo}", "refs", f"--create={ref}", commit)
        command("flatpak", "build-update-repo", str(repo))
        (repo / "inputs.json").write_text(json.dumps({
            "schema_version": 1, "runtimes": commits,
            "flatpak_version": command("flatpak", "--version"),
        }, indent=2) + "\n")
        # rename refuses an existing nonempty snapshot; consumers see only complete exports.
        repo.rename(output)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--installation", type=Path, default=Path("/var/lib/flatpak"))
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--ref", action="append", help="Export only these runtime refs (repeatable)")
    args = parser.parse_args()
    try:
        prepare(args.installation, args.output, args.ref)
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"prepare-dependencies: {error}\n")


if __name__ == "__main__":
    main()
