use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn invalid_update_recipes_fail_with_phase_evidence_before_installation() {
    let temp = tempfile::tempdir().unwrap();
    let prepare = temp.path().join("prepare.yml");
    let verify = temp.path().join("verify.yml");
    let output = temp.path().join("output");
    std::fs::write(
        &prepare,
        "version: 1\nsteps: [{capture: {name: old, caption: Old}}]\n",
    )
    .unwrap();
    std::fs::write(
        &verify,
        "version: 1\nsteps: [{wait_text: Saved}, {capture: {name: new, caption: New}}]\n",
    )
    .unwrap();
    let mut command = Command::new(assert_cmd::cargo::cargo_bin!("flatpak-smoke"));
    command
        .args([
            "verify-update-bundle",
            "missing-candidate.flatpak",
            "--from",
            "missing-old.flatpak",
            "--prepare-recipe",
        ])
        .arg(&prepare)
        .arg("--verify-recipe")
        .arg(&verify)
        .arg("--output")
        .arg(&output);
    command
        .assert()
        .failure()
        .stderr(predicate::str::contains("preparation recipe requires quit"));
    let result: serde_json::Value =
        serde_json::from_slice(&std::fs::read(output.join("result.json")).unwrap()).unwrap();
    assert_eq!(result["update"]["failed_phase"], "validate");
    assert!(!output.join("phases").exists());
}

#[test]
fn phase_evidence_is_protected_without_force() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(temp.path().join("phases/prepare")).unwrap();
    let evidence = temp.path().join("phases/prepare/saved.png");
    std::fs::write(&evidence, "existing evidence").unwrap();
    Command::new(assert_cmd::cargo::cargo_bin!("flatpak-smoke"))
        .args([
            "verify-update-bundle",
            "new.flatpak",
            "--from",
            "old.flatpak",
            "--prepare-recipe",
            "prepare.yml",
            "--verify-recipe",
            "verify.yml",
            "--output",
        ])
        .arg(temp.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("--force"));
    assert_eq!(
        std::fs::read_to_string(evidence).unwrap(),
        "existing evidence"
    );
}
