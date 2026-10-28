use super::*;
use std::{process::Command, time::Duration};

fn repository_files(root: &Path) -> std::collections::BTreeMap<std::path::PathBuf, Vec<u8>> {
    let mut files = std::collections::BTreeMap::new();
    for entry in fs::read_dir(root).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if entry.file_type().unwrap().is_symlink() {
            files.insert(
                path.clone(),
                fs::read_link(path)
                    .unwrap()
                    .as_os_str()
                    .as_encoded_bytes()
                    .to_vec(),
            );
        } else if path.is_dir() {
            files.extend(repository_files(&path));
        } else {
            files.insert(path.clone(), fs::read(path).unwrap());
        }
    }
    files
}

fn checked(args: &[&str]) {
    let output = Command::new(args[0]).args(&args[1..]).output().unwrap();
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[ignore = "requires flatpak"]
fn bundle_update_preserves_profile_and_deploys_exact_commit() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let build = root.join("build");
    let repo = root.join("repo");
    fs::create_dir_all(build.join("usr")).unwrap();
    fs::create_dir_all(build.join("files")).unwrap();
    fs::create_dir_all(build.join("export")).unwrap();
    fs::write(
        build.join("metadata"),
        "[Runtime]\nname=org.example.Platform\n",
    )
    .unwrap();
    checked(&[
        "flatpak",
        "build-export",
        "--runtime",
        "--arch=x86_64",
        repo.to_str().unwrap(),
        build.to_str().unwrap(),
        "stable",
    ]);
    fs::write(build.join("metadata"), "[Application]\nname=org.example.Update\nruntime=org.example.Platform/x86_64/stable\nsdk=org.example.Platform/x86_64/stable\ncommand=test\n").unwrap();
    for version in ["old", "new"] {
        fs::write(build.join("files/payload"), version).unwrap();
        checked(&[
            "flatpak",
            "build-export",
            "--arch=x86_64",
            repo.to_str().unwrap(),
            build.to_str().unwrap(),
            "stable",
        ]);
        checked(&[
            "flatpak",
            "build-bundle",
            repo.to_str().unwrap(),
            root.join(format!("{version}.flatpak")).to_str().unwrap(),
            "org.example.Update",
            "stable",
        ]);
    }
    let home = root.join("home");
    let runner = CommandRunner::new(root.join("log"))
        .with_clean_env()
        .with_env("PATH", std::env::var_os("PATH").unwrap())
        .with_env("HOME", &home)
        .with_env("XDG_DATA_HOME", home.join("data"))
        .with_env("XDG_CONFIG_HOME", home.join("config"))
        .with_env("XDG_CACHE_HOME", home.join("cache"))
        .with_env("FLATPAK_SYSTEM_DIR", home.join("system"))
        .with_env("FLATPAK_CONFIG_DIR", home.join("flatpak-config"));
    let installer = ArtifactInstaller::new(&runner, Instant::now() + Duration::from_secs(60));
    let old = installer
        .inspect_bundle(&root.join("old.flatpak"), &root.join("inspect-old"))
        .unwrap();
    let new = installer
        .inspect_bundle(&root.join("new.flatpak"), &root.join("inspect-new"))
        .unwrap();
    assert_eq!(old.reference, new.reference);
    assert_ne!(old.commit, new.commit);
    installer.add_dependency_repo(Some(&repo)).unwrap();
    installer.install_bundle(&root.join("old.flatpak")).unwrap();
    assert_eq!(installer.phase_refs(&old.reference).0, old);
    let profile = home.join(".var/app/org.example.Update/data");
    fs::create_dir_all(&profile).unwrap();
    fs::write(profile.join("document"), "saved by old app").unwrap();
    installer.update_bundle(&root.join("new.flatpak")).unwrap();
    assert_eq!(installer.phase_refs(&new.reference).0, new);
    assert_eq!(
        fs::read_to_string(profile.join("document")).unwrap(),
        "saved by old app"
    );
    assert_eq!(installer.list_installed_app_refs().unwrap().len(), 1);
}

// Exercises the portal's second `flatpak run`, which does not pass --user.
// Run with a published runtime repo containing a runtime with flatpak-spawn.
#[test]
#[ignore = "requires Flatpak sandboxing, dbus-run-session and FLATPAK_SMOKE_TEST_DEPENDENCY_REPO"]
fn dependency_isolation_supports_sandboxed_subprocesses() {
    let dependencies = std::env::var_os("FLATPAK_SMOKE_TEST_DEPENDENCY_REPO").unwrap();
    let runtime = std::env::var("FLATPAK_SMOKE_TEST_RUNTIME")
        .unwrap_or_else(|_| "org.gnome.Platform/x86_64/49".into());
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let build = root.join("build");
    fs::create_dir_all(build.join("files")).unwrap();
    fs::create_dir_all(build.join("export")).unwrap();
    fs::write(build.join("metadata"), format!(
        "[Application]\nname=org.example.SpawnProbe\nruntime={runtime}\nsdk={runtime}\ncommand=sh\n"
    )).unwrap();
    let apps = root.join("apps");
    checked(&[
        "flatpak",
        "build-export",
        apps.to_str().unwrap(),
        build.to_str().unwrap(),
    ]);
    let mut env = vec![
        ("HOME".into(), root.join("home").into_os_string()),
        ("XDG_DATA_HOME".into(), root.join("data").into_os_string()),
        (
            "XDG_CONFIG_HOME".into(),
            root.join("config").into_os_string(),
        ),
        ("XDG_CACHE_HOME".into(), root.join("cache").into_os_string()),
        ("XDG_RUNTIME_DIR".into(), root.join("run").into_os_string()),
        (
            "FLATPAK_USER_DIR".into(),
            root.join("user-flatpak").into_os_string(),
        ),
        // This shell-only probe needs no graphics extensions.
        ("FLATPAK_GL_DRIVERS".into(), "none".into()),
    ];
    for (_, value) in &env[..6] {
        fs::create_dir_all(Path::new(value)).unwrap();
    }
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(root.join("run"), fs::Permissions::from_mode(0o700)).unwrap();
    isolate_system_installations(
        root,
        &mut env,
        &root.join("log"),
        Instant::now() + Duration::from_secs(10),
    )
    .unwrap();
    let runner = env.into_iter().fold(
        CommandRunner::new(root.join("log")),
        |runner, (key, value)| runner.with_env(key, value),
    );
    let deadline = Instant::now() + Duration::from_secs(120);
    let installer = ArtifactInstaller::new(&runner, deadline);
    installer
        .add_dependency_repo(Some(Path::new(&dependencies)))
        .unwrap();
    installer
        .install_repo_ref(&apps, "app/org.example.SpawnProbe/x86_64/master")
        .unwrap();
    let output = runner.run("dbus-run-session", [
        "--", "flatpak", "run", "--user", "--command=sh", "org.example.SpawnProbe",
        "-ec", "flatpak-spawn --sandbox --watch-bus sh -ec 'grep -qx sandbox=true /.flatpak-info; echo SUBSANDBOX_OK'",
    ], deadline).unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        output.stdout,
        output.stderr
    );
    assert_eq!(output.stdout.trim(), "SUBSANDBOX_OK");
    // Initialization must not populate the private system installation or
    // expose the caller's system refs/remotes as a fallback.
    for command in ["list", "remotes"] {
        let output = runner
            .run("flatpak", [command, "--system"], deadline)
            .unwrap();
        assert!(output.status.success(), "{}", output.stderr);
        assert!(output.stdout.trim().is_empty(), "{}", output.stdout);
    }
}

// Real OSTree/Flatpak transactions without requiring a desktop or network access.
#[test]
#[ignore = "requires flatpak and ostree"]
fn local_dependencies_are_reusable_and_private() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let repo = root.join("dependencies");
    let app_repo = root.join("apps");
    let build = root.join("build");
    fs::create_dir_all(build.join("files")).unwrap();
    fs::create_dir_all(build.join("usr")).unwrap();
    fs::write(build.join("files/payload"), vec![42; 1024 * 1024]).unwrap();
    fs::write(build.join("usr/payload"), vec![42; 1024 * 1024]).unwrap();
    for metadata in [
        "[Runtime]\nname=org.example.Platform.Extra\n[ExtensionOf]\nref=runtime/org.example.Platform/x86_64/stable\n",
        "[Runtime]\nname=org.example.Platform\n[Extension org.example.Platform.Extra]\ndirectory=extra\nversion=stable\n",
    ] {
        fs::write(build.join("metadata"), metadata).unwrap();
        checked(&[
            "flatpak",
            "build-export",
            "--runtime",
            "--arch=x86_64",
            repo.to_str().unwrap(),
            build.to_str().unwrap(),
            "stable",
        ]);
    }
    fs::write(build.join("metadata"), "[Application]\nname=org.example.App\nruntime=org.example.Platform/x86_64/stable\nsdk=org.example.Platform/x86_64/stable\ncommand=test\n").unwrap();
    fs::create_dir_all(build.join("export")).unwrap();
    checked(&[
        "flatpak",
        "build-export",
        "--arch=x86_64",
        app_repo.to_str().unwrap(),
        build.to_str().unwrap(),
        "stable",
    ]);
    let bundle = root.join("app.flatpak");
    checked(&[
        "flatpak",
        "build-bundle",
        app_repo.to_str().unwrap(),
        bundle.to_str().unwrap(),
        "org.example.App",
        "stable",
    ]);

    let install = |name: &str, bundle_input: bool| {
        let workspace = root.join(name);
        fs::create_dir_all(&workspace).unwrap();
        let mut env = vec![
            ("HOME".into(), workspace.clone().into_os_string()),
            (
                "XDG_DATA_HOME".into(),
                workspace.join("data").into_os_string(),
            ),
            (
                "FLATPAK_USER_DIR".into(),
                workspace.join("data/flatpak").into_os_string(),
            ),
        ];
        isolate_system_installations(
            &workspace,
            &mut env,
            &workspace.join("log"),
            Instant::now() + Duration::from_secs(10),
        )
        .unwrap();
        let runner = env.into_iter().fold(
            CommandRunner::new(workspace.join("log")).with_env(
                "FLATPAK_SYSTEM_DIR",
                root.join("cold/data/flatpak").into_os_string(),
            ),
            |runner, (key, value)| runner.with_env(key, value),
        );
        let installer = ArtifactInstaller::new(&runner, Instant::now() + Duration::from_secs(60));
        if name == "cold" {
            assert_eq!(
                installer.install_bundle(&bundle).unwrap_err().reason,
                FailureReason::DependencyFailed
            );
        }
        let start = Instant::now();
        installer.add_dependency_repo(Some(&repo)).unwrap();
        if bundle_input {
            installer.install_bundle(&bundle).unwrap();
        } else {
            installer
                .install_repo_ref(&app_repo, "app/org.example.App/x86_64/stable")
                .unwrap();
        }
        let refs = installer.phase_refs("app/org.example.App/x86_64/stable");
        eprintln!("{name}: {} ms, {refs:?}", start.elapsed().as_millis());
        assert!(!workspace.join(".var/app").exists());
        assert_eq!(refs.1.as_ref().unwrap().len(), 2);
        assert!(
            refs.1
                .as_ref()
                .unwrap()
                .iter()
                .all(|reference| reference.commit.is_some())
        );
        refs
    };
    let cold = install("cold", true);
    let exported = root.join("exported");
    checked(&[
        "python3",
        "action/prepare-dependencies.py",
        "--installation",
        root.join("cold/data/flatpak").to_str().unwrap(),
        "--output",
        exported.to_str().unwrap(),
    ]);
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(exported.join("inputs.json")).unwrap()).unwrap();
    for reference in cold.1.as_ref().unwrap() {
        assert_eq!(
            manifest["runtimes"][format!("runtime/{}", reference.reference)],
            reference.commit.as_deref().unwrap()
        );
    }
    // Consume the exported deployed commits, rather than just the fixture's build repo.
    fs::rename(&repo, root.join("original")).unwrap();
    fs::rename(&exported, &repo).unwrap();
    let before = repository_files(&repo);
    // Independent concurrent readers, including both source interfaces.
    std::thread::scope(|scope| {
        let a = scope.spawn(|| install("warm-bundle", true));
        let b = scope.spawn(|| install("warm-repo", false));
        assert_eq!(cold, a.join().unwrap());
        assert_eq!(cold, b.join().unwrap());
    });
    assert_eq!(before, repository_files(&repo));
    // A failed/repeated publication cannot damage the already published input.
    let repeated = Command::new("python3")
        .args(["action/prepare-dependencies.py", "--installation"])
        .arg(root.join("cold/data/flatpak"))
        .arg("--output")
        .arg(&repo)
        .output()
        .unwrap();
    assert!(!repeated.status.success());
    assert_eq!(before, repository_files(&repo));
}

#[test]
#[ignore = "set FLATPAK_SMOKE_BENCHMARK_BUNDLE and FLATPAK_SMOKE_BENCHMARK_REPO"]
fn benchmark_prepared_dependencies() {
    let bundle = std::env::var_os("FLATPAK_SMOKE_BENCHMARK_BUNDLE").unwrap();
    let repo = std::env::var_os("FLATPAK_SMOKE_BENCHMARK_REPO").unwrap();
    let mut previous = None;
    let mut cases = vec!["first-verify", "warm-verify", "warm-screenshots"];
    if std::env::var_os("FLATPAK_SMOKE_BENCHMARK_NETWORK").is_some() {
        cases.insert(0, "network-verify");
    }
    for name in cases {
        let workspace = tempfile::tempdir().unwrap();
        let home = workspace.path();
        let mut env = vec![
            ("HOME".into(), home.as_os_str().to_owned()),
            (
                "FLATPAK_USER_DIR".into(),
                home.join("flatpak").into_os_string(),
            ),
            ("XDG_DATA_HOME".into(), home.join("data").into_os_string()),
            ("XDG_CACHE_HOME".into(), home.join("cache").into_os_string()),
            (
                "XDG_CONFIG_HOME".into(),
                home.join("config").into_os_string(),
            ),
            ("LIBGL_ALWAYS_SOFTWARE".into(), "1".into()),
        ];
        isolate_system_installations(
            home,
            &mut env,
            &home.join("log"),
            Instant::now() + Duration::from_secs(10),
        )
        .unwrap();
        if name.ends_with("screenshots") {
            env.extend([
                ("XDG_CURRENT_DESKTOP".into(), "GNOME".into()),
                ("XDG_SESSION_TYPE".into(), "wayland".into()),
                ("WAYLAND_DISPLAY".into(), "wayland-0".into()),
            ]);
        }
        let runner = env.into_iter().fold(
            CommandRunner::new(home.join("log"))
                .with_clean_env()
                .with_env("PATH", std::env::var_os("PATH").unwrap()),
            |runner, (key, value)| runner.with_env(key, value),
        );
        let installer = ArtifactInstaller::new(&runner, Instant::now() + Duration::from_secs(300));
        let start = Instant::now();
        if name == "network-verify" {
            installer.add_flathub_if_allowed(true).unwrap();
        } else {
            installer
                .add_dependency_repo(Some(Path::new(&repo)))
                .unwrap();
        }
        installer.install_bundle(Path::new(&bundle)).unwrap();
        let app = installer.list_installed_app_refs().unwrap().remove(0);
        let refs = installer.phase_refs(&app);
        eprintln!(
            "{name}: {} ms\n{}",
            start.elapsed().as_millis(),
            serde_json::to_string_pretty(&refs).unwrap()
        );
        if name == "warm-verify" {
            assert_eq!(previous.as_ref().unwrap(), &refs);
        }
        previous = Some(refs);
    }
}
