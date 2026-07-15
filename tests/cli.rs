use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn help_mentions_commands() {
    let mut cmd = Command::cargo_bin("flatpak-smoke").unwrap();
    cmd.arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("verify-bundle"))
        .stdout(predicate::str::contains("verify-repo"))
        .stdout(predicate::str::contains("doctor"));
}

#[test]
fn verify_bundle_requires_existing_bundle() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("out");
    let mut cmd = Command::cargo_bin("flatpak-smoke").unwrap();
    cmd.args([
        "verify-bundle",
        "missing.flatpak",
        "--output",
        output.to_str().unwrap(),
    ])
    .assert()
    .failure()
    .stderr(predicate::str::contains("does not exist"));
}
