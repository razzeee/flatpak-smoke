"""Run real screenshot recipes repeatedly and concurrently without retrying failures."""
import argparse
from concurrent.futures import ThreadPoolExecutor, as_completed
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
import threading
import time

from prepare import file_hash

ROOT = Path(__file__).resolve().parents[2]
RECIPES = Path(__file__).resolve().parent


def require(condition, message):
    if not condition:
        raise ValueError(message)


def inspect_png(path):
    info = subprocess.check_output(["identify", "-format", "%m %w %h", str(path)],
                                   text=True, timeout=15).split()
    require(len(info) == 3 and info[0] == "PNG", f"not one PNG image: {path}")
    width, height = map(int, info[1:])
    require(0 < width <= 2048 and 0 < height <= 2048, f"unreasonable PNG dimensions: {info}")
    pixels = subprocess.check_output(["convert", str(path), "-depth", "8", "rgba:-"], timeout=15)
    require(len(pixels) == width * height * 4, f"incomplete pixel data: {path}")
    alpha = pixels[3::4]
    return {
        "width": width, "height": height,
        "png_sha256": file_hash(path), "pixels_sha256": hashlib.sha256(pixels).hexdigest(),
        "alpha_min": min(alpha), "alpha_max": max(alpha),
    }


def validate_run(output, app, returncode):
    result = json.loads((output / "result.json").read_text())
    require(returncode == 0, f"runner exit {returncode}: {result.get('failure')}")
    require(result["status"] == "passed" and result["failure"] is None, f"unsuccessful result: {result}")
    manifest = json.loads((output / "screenshots.json").read_text())
    require(result["app_ref"] == manifest["app_ref"] == app["ref"], "wrong app ref in capture results")
    require(manifest["failed_step_index"] is None, "successful run has a failed step")
    require(manifest["desktop"] == "gnome" and manifest["desktop_version"], "missing desktop provenance")
    require(manifest["language"] == "en" and manifest["scale"] == 1, "wrong capture profile")
    captures = manifest["captures"]
    require([capture["name"] for capture in captures] == app["captures"], "unexpected capture order or missing capture")
    require(result["screenshots"] == [capture["path"] for capture in captures], "artifact lists disagree")
    inspected = []
    for index, capture in enumerate(captures):
        expected = f"screenshots/{index:03}-{capture['name']}.png"
        require(capture["path"] == expected, "unexpected capture path")
        path = (output / expected).resolve()
        require(path.is_relative_to(output.resolve()), "capture escaped its output directory")
        require(capture["caption"].strip() and capture["language"] == "en", "missing caption or wrong language")
        require(0 < capture["window_width"] <= 1000 and 0 < capture["window_height"] <= 700, "window exceeds profile bounds")
        image = inspect_png(path)
        require((image["width"], image["height"]) == (capture["width"], capture["height"]), "manifest dimensions disagree with PNG")
        inspected.append({**capture, **image})
    return {"desktop_version": manifest["desktop_version"], "captures": inspected}


def invoke(command, log):
    process = subprocess.Popen(command, cwd=ROOT, stdout=log, stderr=log, start_new_session=True)
    try:
        return process.wait(timeout=330)
    except subprocess.TimeoutExpired:
        os.killpg(process.pid, signal.SIGTERM)
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait()
        raise TimeoutError("runner exceeded its external timeout")


def workspace_from(output):
    path = output / "logs/runner.log"
    if path.exists():
        for line in path.read_text().splitlines():
            if line.startswith("capture workspace: "):
                return line.split(": ", 1)[1]
    return None


def run_once(app, phase, index, args, barrier=None):
    output = args.output / app["name"] / f"{phase}-{index:02}"
    output.parent.mkdir(parents=True, exist_ok=True)
    require(not output.exists(), f"run output already exists: {output}")
    record = {"app": app["name"], "phase": phase, "index": index, "output": str(output),
              "status": "failed", "returncode": None}
    if barrier:
        barrier.wait(timeout=10)
    record["started"] = time.monotonic()
    command = [args.binary, "screenshot-bundle", str(args.inputs / app["bundle"]),
               "--recipe", str(RECIPES / app["recipe"]), "--output", str(output),
               "--overall-timeout", "5m"]
    try:
        with output.with_suffix(".log").open("w") as log:
            record["returncode"] = invoke(command, log)
        record["finished"] = time.monotonic()
        record.update(validate_run(output, app, record["returncode"]))
        record["status"] = "passed"
    except Exception as error:
        record["finished"] = time.monotonic()
        record["error"] = str(error)
    record["elapsed_ms"] = round((record["finished"] - record["started"]) * 1000)
    record["workspace"] = workspace_from(output)
    print(f"{app['name']} {phase}-{index:02}: {record['status']} ({record['elapsed_ms']}ms)", flush=True)
    return record


def remaining_processes(workspaces):
    wanted = {str(Path(workspace)): workspace for workspace in workspaces}
    leaked = []
    for process in Path("/proc").iterdir():
        if not process.name.isdecimal():
            continue
        try:
            if "\nState:\tZ" in process.joinpath("status").read_text():
                continue
            environment = process.joinpath("environ").read_bytes().split(b"\0")
            home = next((entry[5:].decode() for entry in environment if entry.startswith(b"HOME=")), None)
            if home in wanted:
                leaked.append({"pid": int(process.name), "workspace": wanted[home]})
        except (OSError, UnicodeError):
            continue
    return leaked


def summary(apps, runs):
    output = []
    for app in apps:
        selected = [run for run in runs if run["app"] == app["name"]]
        passed = [run for run in selected if run["status"] == "passed"]
        geometry = {}
        pixels = {}
        for run in passed:
            for capture in run["captures"]:
                name = capture["name"]
                geometry.setdefault(name, set()).add((capture["width"], capture["height"], capture["window_width"], capture["window_height"]))
                pixels.setdefault(name, set()).add(capture["pixels_sha256"])
        output.append({
            "app": app["name"], "passed": len(passed), "failed": len(selected) - len(passed),
            "mixed_outcomes": 0 < len(passed) < len(selected),
            "geometry_consistent": all(len(values) == 1 for values in geometry.values()),
            "pixel_variants": {name: len(values) for name, values in pixels.items()},
        })
    return output


def write_report(path, report):
    temporary = path.with_suffix(".tmp")
    temporary.write_text(json.dumps(report, indent=2) + "\n")
    temporary.replace(path)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--inputs", type=Path, default=Path("/opt/compatibility/artifacts"))
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--binary", default="/usr/local/bin/flatpak-smoke")
    parser.add_argument("--app", action="append")
    parser.add_argument("--repeats", type=int, default=3)
    parser.add_argument("--concurrency", type=int, default=2)
    parser.add_argument("--concurrent-runs", type=int, default=2)
    parser.add_argument("--image-id")
    args = parser.parse_args()
    require(args.repeats >= 1 and args.concurrency >= 1 and args.concurrent_runs >= 0, "invalid repetition/concurrency limits")
    args.output = args.output.resolve()
    require(not args.output.exists(), "choose a fresh matrix output directory to retain prior evidence")
    provenance = json.loads((args.inputs / "inputs.json").read_text())
    catalog = json.loads(RECIPES.joinpath("apps.json").read_text())["apps"]
    names = {app["name"] for app in catalog}
    require(not args.app or set(args.app) <= names, "unknown app selection")
    prepared = {app["name"]: app for app in provenance["apps"]}
    apps = []
    for app in catalog:
        if args.app and app["name"] not in args.app:
            continue
        source = prepared[app["name"]]
        require(source["app_id"] == app["app_id"], "provisioned application ID changed")
        require(file_hash(args.inputs / source["bundle"]) == source["bundle_sha256"], "bundle hash differs from provisioned input")
        apps.append({**source, **app, "recipe_sha256": file_hash(RECIPES / app["recipe"])})
    args.output.mkdir(parents=True)
    report = {
        "schema_version": 1, "created": datetime.now(timezone.utc).isoformat(),
        "coverage": "full" if len(apps) == len(catalog) and args.repeats >= 3 and args.concurrency >= 2 and args.concurrent_runs >= 2 else "probe",
        "image_id": args.image_id, "runner_sha256": file_hash(Path(args.binary)),
        "inputs": {**provenance, "apps": apps},
        "settings": {"repeats": args.repeats, "concurrency": args.concurrency, "concurrent_runs": args.concurrent_runs},
        "runs": [], "complete": False,
    }
    report_path = args.output / "report.json"
    try:
        for app in apps:
            for index in range(args.repeats):
                report["runs"].append(run_once(app, "serial", index, args))
                write_report(report_path, report)
            if args.concurrent_runs:
                workers = min(args.concurrency, args.concurrent_runs)
                barrier = threading.Barrier(workers)
                with ThreadPoolExecutor(max_workers=workers) as pool:
                    futures = [pool.submit(run_once, app, "concurrent", index, args,
                                           barrier if index < workers else None)
                               for index in range(args.concurrent_runs)]
                    for future in as_completed(futures):
                        report["runs"].append(future.result())
                        write_report(report_path, report)
        report["complete"] = True
    finally:
        report["apps"] = summary(apps, report["runs"])
        workspaces = [run["workspace"] for run in report["runs"] if run["workspace"]]
        report["unique_workspaces"] = len(workspaces) == len(set(workspaces))
        time.sleep(2)
        report["remaining_processes"] = remaining_processes(workspaces)
        report["status"] = "passed" if (report["complete"] and report["unique_workspaces"]
            and not report["remaining_processes"] and all(run["status"] == "passed" for run in report["runs"])
            and all(app["geometry_consistent"] for app in report["apps"])) else "failed"
        write_report(report_path, report)
    if report["status"] != "passed":
        raise SystemExit(f"Compatibility matrix failed; see {report_path}")
    print(f"Compatibility matrix passed; see {report_path}")


if __name__ == "__main__":
    main()
