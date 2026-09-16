"""Resolve real Flathub apps once and export immutable inputs for a matrix run."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess


def command(*args):
    return subprocess.check_output(args, text=True).strip()


def file_hash(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    catalog = json.loads(Path(__file__).with_name("apps.json").read_text())
    output = args.output
    output.mkdir(parents=True, exist_ok=True)
    subprocess.run(["flatpak", "remote-add", "--system", "--if-not-exists", "flathub",
                    "https://flathub.org/repo/flathub.flatpakrepo"], check=True)
    subprocess.run(["flatpak", "install", "--system", "--noninteractive", "flathub",
                    *[app["app_id"] for app in catalog["apps"]]], check=True)
    versions = dict(line.split("\t", 1) for line in command(
        "flatpak", "list", "--system", "--app", "--columns=application,version"
    ).splitlines() if "\t" in line)
    resolved = []
    for app in catalog["apps"]:
        app_id = app["app_id"]
        ref = command("flatpak", "info", "--system", "--show-ref", app_id)
        _, _, arch, branch = ref.split("/")
        bundle = output / f"{app_id}.flatpak"
        subprocess.run(["flatpak", "build-bundle", f"--arch={arch}",
                        "/var/lib/flatpak/repo", str(bundle), app_id, branch], check=True)
        runtime = command("flatpak", "info", "--system", "--show-runtime", app_id)
        resolved.append({
            **app, "ref": ref, "version": versions.get(app_id),
            "commit": command("flatpak", "info", "--system", "--show-commit", ref),
            "runtime_ref": "runtime/" + runtime.removeprefix("runtime/"),
            "runtime_commit": command("flatpak", "info", "--system", "--show-commit", runtime),
            "bundle": bundle.name, "bundle_sha256": file_hash(bundle),
        })
    provenance = {
        "schema_version": 1,
        "apps": resolved,
        "flatpak_refs": command("flatpak", "list", "--system", "--columns=ref,active").splitlines(),
        "packages": command("dpkg-query", "-W", "-f=${Package}\t${Version}\n").splitlines(),
    }
    (output / "inputs.json").write_text(json.dumps(provenance, indent=2) + "\n")


if __name__ == "__main__":
    main()
