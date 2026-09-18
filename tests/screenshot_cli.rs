use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn invalid_recipe_is_rejected_before_artifact_installation() {
    let temp = tempfile::tempdir().unwrap();
    let recipe = temp.path().join("recipe.yml");
    let output = temp.path().join("output");
    std::fs::write(&recipe, "version: 99\nsteps: []\n").unwrap();
    Command::cargo_bin("flatpak-smoke")
        .unwrap()
        .args(["screenshot-bundle", "missing.flatpak", "--recipe"])
        .arg(&recipe)
        .arg("--output")
        .arg(&output)
        .assert()
        .failure()
        .stderr(predicate::str::contains("unsupported recipe version"));
    assert!(!output.exists());
}

#[test]
fn duplicate_capture_names_are_rejected_before_preparing_output() {
    let temp = tempfile::tempdir().unwrap();
    let recipe = temp.path().join("recipe.yml");
    let output = temp.path().join("output");
    std::fs::write(&recipe, "version: 1\nsteps:\n  - capture: {name: overview, caption: Browse items}\n  - capture: {name: overview, caption: Search items}\n").unwrap();
    Command::cargo_bin("flatpak-smoke")
        .unwrap()
        .args(["screenshot-bundle", "missing.flatpak", "--recipe"])
        .arg(&recipe)
        .arg("--output")
        .arg(&output)
        .assert()
        .failure()
        .stderr(predicate::str::contains("duplicate capture name"));
    assert!(!output.exists());
}

#[test]
fn missing_desktop_dependencies_produce_both_failure_manifests() {
    let temp = tempfile::tempdir().unwrap();
    let recipe = temp.path().join("recipe.yml");
    let bundle = temp.path().join("fixture.flatpak");
    let output = temp.path().join("output");
    std::fs::write(
        &recipe,
        "version: 1\nsteps:\n  - capture: {name: overview, caption: Browse items}\n",
    )
    .unwrap();
    std::fs::write(&bundle, "dependency checking must precede installation").unwrap();
    Command::cargo_bin("flatpak-smoke")
        .unwrap()
        .env("PATH", temp.path())
        .arg("screenshot-bundle")
        .arg(bundle)
        .arg("--recipe")
        .arg(recipe)
        .arg("--output")
        .arg(&output)
        .assert()
        .failure();
    let result: serde_json::Value =
        serde_json::from_slice(&std::fs::read(output.join("result.json")).unwrap()).unwrap();
    assert_eq!(result["status"], "failed");
    assert_eq!(result["failure"]["reason"], "dependency_failed");
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(output.join("screenshots.json")).unwrap()).unwrap();
    assert_eq!(manifest["schema_version"], 1);
    assert_eq!(manifest["failed_step_index"], serde_json::Value::Null);
    assert_eq!(manifest["captures"], serde_json::json!([]));
}

#[test]
fn malformed_recipes_fail_before_any_output_is_created() {
    let cases = [
        (
            "desktop: kde\nsteps: [{capture: {name: one, caption: Main view}}]",
            "unsupported desktop",
        ),
        (
            "window: {width: 1001}\nsteps: [{capture: {name: one, caption: Main view}}]",
            "1000x700",
        ),
        (
            "steps: [{capture: {name: '../outside', caption: Main view}}]",
            "capture name",
        ),
        (
            "steps: [{capture: {name: one, caption: 'Main view.'}}]",
            "trailing full stop",
        ),
        (
            "steps: [{capture: {name: one, caption: Main view}, key: Return}]",
            "exactly one action",
        ),
        (
            "steps: [{key: Ctrl+Bogus}, {capture: {name: one, caption: Main view}}]",
            "unsupported key",
        ),
        (
            "steps: [{wait_text: Ready, timeout: 18446744073709551615m}, {capture: {name: one, caption: Main view}}]",
            "duration is too large",
        ),
        (
            "steps: [{capture: {name: one, caption: Main view, unknown: true}}]",
            "unknown field",
        ),
    ];
    for (text, error) in cases {
        let temp = tempfile::tempdir().unwrap();
        let recipe = temp.path().join("recipe.yml");
        let output = temp.path().join("output");
        std::fs::write(&recipe, format!("version: 1\n{text}\n")).unwrap();
        Command::cargo_bin("flatpak-smoke")
            .unwrap()
            .args(["screenshot-bundle", "missing.flatpak", "--recipe"])
            .arg(recipe)
            .arg("--output")
            .arg(&output)
            .assert()
            .failure()
            .stderr(predicate::str::contains(error));
        assert!(!output.exists());
    }
}

#[test]
fn screenshot_manifest_is_protected_without_force() {
    let temp = tempfile::tempdir().unwrap();
    let recipe = temp.path().join("recipe.yml");
    let bundle = temp.path().join("fixture.flatpak");
    let output = temp.path().join("output");
    std::fs::create_dir(&output).unwrap();
    std::fs::write(output.join("screenshots.json"), "existing manifest").unwrap();
    std::fs::write(
        &recipe,
        "version: 1\nsteps: [{capture: {name: overview, caption: Main view}}]\n",
    )
    .unwrap();
    std::fs::write(&bundle, "placeholder artifact").unwrap();
    Command::cargo_bin("flatpak-smoke")
        .unwrap()
        .arg("screenshot-bundle")
        .arg(bundle)
        .arg("--recipe")
        .arg(recipe)
        .arg("--output")
        .arg(&output)
        .assert()
        .failure()
        .stderr(predicate::str::contains("--force"));
    assert_eq!(
        std::fs::read_to_string(output.join("screenshots.json")).unwrap(),
        "existing manifest"
    );
}

#[test]
fn invalid_setup_paths_and_launch_placeholders_are_rejected_before_installation() {
    for (settings, message) in [
        (
            "setup: {files: [{source: '../sample', destination: 'data/sample'}]}",
            "source must",
        ),
        (
            "setup: {files: [{source: 'nested/../../sample', destination: 'data/sample'}]}",
            "source must",
        ),
        (
            "setup: {files: [{source: '/etc/passwd', destination: 'data/sample'}]}",
            "source must",
        ),
        (
            "setup: {files: [{source: '', destination: 'data/sample'}]}",
            "source must",
        ),
        (
            "setup: {files: [{source: sample, destination: '../outside'}]}",
            "destination must",
        ),
        (
            "setup: {files: [{source: sample, destination: '/data/file'}]}",
            "destination must",
        ),
        (
            "setup: {files: [{source: sample, destination: 'data/../../outside'}]}",
            "destination must",
        ),
        (
            "setup: {files: [{source: sample, destination: 'cache/file'}]}",
            "destination must",
        ),
        (
            "launch: {args: ['${HOME}/document']}",
            "unsupported launch placeholder",
        ),
        (
            "launch: {args: ['${APP_DATA']}",
            "unterminated launch placeholder",
        ),
        ("launch: {args: [\"bad\\0argument\"]}", "NUL"),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let recipe = temp.path().join("recipe.yml");
        let bundle = temp.path().join("fixture.flatpak");
        let output = temp.path().join("output");
        std::fs::write(&bundle, "validation must precede installation").unwrap();
        std::fs::write(
            &recipe,
            format!(
                "version: 1\n{settings}\nsteps: [{{capture: {{name: main, caption: Main view}}}}]\n"
            ),
        )
        .unwrap();
        Command::cargo_bin("flatpak-smoke")
            .unwrap()
            .env("PATH", temp.path())
            .arg("screenshot-bundle")
            .arg(bundle)
            .arg("--recipe")
            .arg(recipe)
            .arg("--output")
            .arg(&output)
            .assert()
            .failure()
            .stderr(predicate::str::contains(message));
        let result: serde_json::Value =
            serde_json::from_slice(&std::fs::read(output.join("result.json")).unwrap()).unwrap();
        assert_eq!(result["status"], "failed");
        assert!(
            result["failure"]["message"]
                .as_str()
                .unwrap()
                .contains(message)
        );
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(output.join("screenshots.json")).unwrap())
                .unwrap();
        assert_eq!(manifest["captures"], serde_json::json!([]));
        assert!(result["app_ref"].is_null());
    }
}
