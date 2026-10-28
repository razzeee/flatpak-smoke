//! Run recipe actions from terminal input and save successful steps to YAML.
use super::{
    Source,
    manifest::Manifest,
    recipe::{Action, Recipe, Step},
    run_session,
    workflow::{Workflow, bounded},
};
use crate::{
    cli::{ScreenshotArgs, ScreenshotBundleArgs},
    process::remaining,
};
use anyhow::{Context, ensure};
use std::{
    fs,
    io::{self, BufRead, Write},
    path::Path,
    sync::mpsc::{self, Receiver, RecvTimeoutError},
    time::{Duration, Instant},
};

const HELP: &str = "Enter one YAML step per line, for example:\n  wait_text: Ready to capture\n  type_text: example\n  key: Return\n  {wait_text: Results for example, timeout: 15s}\n  capture: {name: search, caption: Find items by name}\nCommands: observe, save, restart, finish, quit, help\nThe command saves each successful step. Use restart to reload the draft and replay\nit in a fresh profile with the original fixtures. Use finish to validate it in a\nfresh session. The recipe must contain a wait_text assertion and a capture.";

#[derive(Clone, Copy, PartialEq)]
enum Decision {
    Quit,
    Restart,
    Finish,
}

pub(super) struct Session {
    input: Receiver<io::Result<String>>,
    pub deadline: Instant,
    decision: Decision,
    pub failed_action: Option<anyhow::Error>,
}

pub fn bundle(args: ScreenshotBundleArgs) -> anyhow::Result<()> {
    let common = args.common;
    // Validate before creating output or starting a reader thread.
    let recipe_path = common
        .recipe
        .canonicalize()
        .context("opening authoring draft")?;
    let draft = Recipe::parse_draft(&fs::read(&recipe_path)?)?;
    draft.validate_setup()?;
    fs::create_dir_all(&common.output)?;
    let root = common.output.canonicalize()?;
    ensure!(
        !recipe_path.starts_with(&root),
        "keep the recipe and fixtures outside the authoring output directory"
    );
    // Reset validation status before starting, even when replacing an attempt.
    let marker = root.join("author.json");
    let marker_file = fs::OpenOptions::new()
        .write(true)
        .create(common.force)
        .truncate(common.force)
        .create_new(!common.force)
        .open(&marker)
        .context(
            "authoring output already used; choose a new --output directory or pass --force",
        )?;
    serde_json::to_writer_pretty(
        marker_file,
        &serde_json::json!({
            "schema_version": 1, "status": "draft", "recipe": recipe_path,
        }),
    )?;
    let (sender, input) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        for line in io::stdin().lock().lines() {
            if sender.send(line).is_err() {
                break;
            }
        }
    });
    let mut session = Session {
        input,
        deadline: Instant::now()
            .checked_add(common.overall_timeout)
            .context("overall timeout is too large")?,
        decision: Decision::Quit,
        failed_action: None,
    };
    let source = Source::Bundle(args.bundle);
    println!("{HELP}");
    for index in 1.. {
        let mut attempt = common.clone();
        attempt.recipe = recipe_path.clone();
        attempt.output = root.join(format!("session-{index:03}"));
        run_session(&attempt, &source, Some(&mut session))?;
        match session.decision {
            Decision::Restart => continue,
            Decision::Quit => {
                println!(
                    "The draft is saved at {}. It has not been validated.",
                    recipe_path.display()
                );
                return Ok(());
            }
            Decision::Finish => break,
        }
    }
    let mut replay = common;
    replay.recipe = recipe_path.clone();
    replay.output = root.join("validation");
    println!("Validating in a fresh session...");
    run_session(&replay, &source, None)
        .context("fresh replay failed. The draft and diagnostics remain available. The recipe has not been validated")?;
    fs::write(
        &marker,
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema_version": 1, "status": "validated", "recipe": recipe_path,
            "result": "validation/result.json",
        }))?,
    )?;
    println!("Validated recipe: {}", recipe_path.display());
    Ok(())
}

impl Session {
    pub fn interact(
        &mut self,
        workflow: &Workflow<'_>,
        recipe: &mut Recipe,
        original: &[u8],
        mut selected: u64,
        common: &ScreenshotArgs,
        manifest: &mut Manifest,
    ) -> anyhow::Result<()> {
        let mut saved = original.to_vec();
        let mut dirty = false;
        self.failed_action = None;
        println!(
            "Replayed {} saved steps. Use observe to inspect the selected window.",
            recipe.steps.len()
        );
        loop {
            print!("author> ");
            io::stdout().flush()?;
            let line = loop {
                remaining(self.deadline)?;
                workflow.desktop.check(self.deadline)?;
                match self.input.recv_timeout(Duration::from_millis(100)) {
                    Ok(line) => break Some(line?),
                    Err(RecvTimeoutError::Disconnected) => break None,
                    Err(RecvTimeoutError::Timeout) => {}
                }
            };
            let Some(line) = line else {
                self.decision = Decision::Quit;
                return Ok(());
            };
            let line = line.trim();
            match line {
                "" => continue,
                "help" => println!("{HELP}"),
                "quit" => {
                    self.decision = Decision::Quit;
                    return Ok(());
                }
                "restart" => {
                    self.decision = Decision::Restart;
                    return Ok(());
                }
                "save" => {
                    saved = save(&common.recipe, recipe, &saved)?;
                    println!("The draft is saved. It has not been validated.");
                }
                "observe" => {
                    let deadline = bounded(self.deadline, common.screenshot_timeout);
                    if let Err(error) = workflow.inspect(selected, deadline) {
                        println!("Observation failed: {error:#}");
                    }
                }
                "finish" if !dirty => {
                    if let Err(error) = validate_completion(recipe) {
                        println!("Cannot finish: {error:#}");
                        continue;
                    }
                    save(&common.recipe, recipe, &saved)?;
                    self.decision = Decision::Finish;
                    return Ok(());
                }
                _ if dirty => println!(
                    "An action failed; restart from the saved draft before adding steps or finishing"
                ),
                _ => {
                    let step = match parse_step(line, recipe) {
                        Ok(step) => step,
                        Err(error) => {
                            println!("Invalid step: {error:#}");
                            continue;
                        }
                    };
                    match workflow.execute_step(
                        &step,
                        recipe.steps.len(),
                        &mut selected,
                        common.screenshot_timeout,
                        manifest,
                    ) {
                        Ok(()) => {
                            recipe.steps.push(step);
                            saved = save(&common.recipe, recipe, &saved)?;
                            println!("Saved step {}", recipe.steps.len());
                        }
                        Err(error) => {
                            workflow
                                .layout
                                .append_runner_log(format!("authoring action failed: {error:#}"))?;
                            println!(
                                "Action failed: {error:#}\nStep not saved. Use restart before continuing."
                            );
                            dirty = true;
                            self.failed_action = Some(error);
                        }
                    }
                }
            }
        }
    }
}

fn parse_step(line: &str, recipe: &Recipe) -> anyhow::Result<Step> {
    let step: Step = serde_yaml_ng::from_str(line).context("expected one YAML step mapping")?;
    if let Action::Capture(capture) = &step.action {
        ensure!(
            !recipe.steps.iter().any(|step| matches!(&step.action,
            Action::Capture(previous) if previous.name == capture.name)),
            "duplicate capture name '{}'",
            capture.name
        );
    }
    Ok(step)
}

fn validate_completion(recipe: &Recipe) -> anyhow::Result<()> {
    Recipe::parse(serde_yaml_ng::to_string(recipe)?.as_bytes())?;
    ensure!(
        recipe
            .steps
            .iter()
            .any(|step| matches!(step.action, Action::WaitText(_))),
        "add a wait_text assertion for the expected outcome"
    );
    recipe.validate_setup()
}

fn save(path: &Path, recipe: &Recipe, previous: &[u8]) -> anyhow::Result<Vec<u8>> {
    ensure!(
        fs::read(path)? == previous,
        "draft changed externally; restart to reload it before saving"
    );
    let contents = serde_yaml_ng::to_string(recipe)?.into_bytes();
    Recipe::parse_draft(&contents)?.validate_setup()?;
    let mut file =
        tempfile::NamedTempFile::new_in(path.parent().context("recipe has no parent directory")?)?;
    file.as_file()
        .set_permissions(fs::metadata(path)?.permissions())?;
    file.write_all(&contents)?;
    file.as_file().sync_all()?;
    file.persist(path).context("saving authoring draft")?;
    Ok(contents)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        output::OutputLayout,
        process::HealthCheck,
        screenshot::{
            desktop::{Desktop, Input, Window},
            images::Images,
            workspace::Workspace,
        },
    };
    use std::cell::RefCell;

    #[derive(Default)]
    struct InputDesktop(RefCell<Vec<Vec<u32>>>);

    impl Desktop for InputDesktop {
        fn version(&self) -> &str {
            "test"
        }
        fn health(&self) -> HealthCheck {
            HealthCheck::default()
        }
        fn check(&self, deadline: Instant) -> anyhow::Result<()> {
            Ok(remaining(deadline).map(|_| ())?)
        }
        fn windows(&self, _: Instant) -> anyhow::Result<Vec<Window>> {
            Ok(Vec::new())
        }
        fn resize(&self, _: u64, _: u32, _: u32, _: Instant) -> anyhow::Result<Window> {
            unreachable!()
        }
        fn capture(&self, _: u64, _: &Path, _: Instant) -> anyhow::Result<Window> {
            unreachable!()
        }
        fn input(&self, _: u64, input: Input<'_>, _: Instant) -> anyhow::Result<()> {
            let Input::Key(keys) = input else {
                unreachable!()
            };
            self.0.borrow_mut().push(keys.to_vec());
            ensure!(keys != [0xff1b], "injected failure after input");
            Ok(())
        }
    }

    #[test]
    fn failed_action_blocks_further_input_and_finish_until_restart() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("draft.yml");
        let original = b"version: 1\nsteps: []\n";
        fs::write(&path, original).unwrap();
        let mut recipe = Recipe::parse_draft(original).unwrap();
        let common = ScreenshotArgs {
            recipe: path.clone(),
            output: root.path().join("output"),
            force: false,
            dependency_repo: None,
            allow_network_remotes: false,
            overall_timeout: Duration::from_secs(5),
            display_timeout: Duration::from_secs(1),
            window_timeout: Duration::from_secs(1),
            screenshot_timeout: Duration::from_secs(1),
        };
        let layout = OutputLayout::prepare(&common.output, false).unwrap();
        let workspace = Workspace::prepare().unwrap();
        let desktop = InputDesktop::default();
        let deadline = Instant::now() + Duration::from_secs(5);
        let workflow = Workflow {
            desktop: &desktop,
            images: Images {
                workspace: &workspace,
                logs: &layout.logs_dir,
                health: desktop.health(),
            },
            layout: &layout,
            overall: deadline,
        };
        let (sender, input) = mpsc::channel();
        for line in [
            "key: Return",
            "key: Escape",
            "key: Tab",
            "finish",
            "restart",
        ] {
            sender.send(Ok(line.to_string())).unwrap();
        }
        let mut session = Session {
            input,
            deadline,
            decision: Decision::Quit,
            failed_action: None,
        };
        let mut manifest = Manifest::new();
        session
            .interact(&workflow, &mut recipe, original, 1, &common, &mut manifest)
            .unwrap();
        assert!(session.decision == Decision::Restart);
        assert!(session.failed_action.is_some());
        assert_eq!(*desktop.0.borrow(), vec![vec![0xff0d], vec![0xff1b]]);
        let saved = fs::read(&path).unwrap();
        let draft = Recipe::parse_draft(&saved).unwrap();
        assert_eq!(draft.steps.len(), 1);
        assert_eq!(manifest.failed_step_index, Some(1));

        // An idle prompt must observe its deadline without needing stdin to close.
        session.deadline = Instant::now() + Duration::from_millis(10);
        assert!(
            session
                .interact(&workflow, &mut recipe, &saved, 1, &common, &mut manifest)
                .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), saved);
        drop(sender);
    }

    #[test]
    fn draft_completion_requires_capture_and_explicit_assertion() {
        let mut draft = Recipe::parse_draft(b"version: 1\nsteps: []\n").unwrap();
        assert!(Recipe::parse(b"version: 1\nsteps: []\n").is_err());
        assert!(validate_completion(&draft).is_err());
        draft
            .steps
            .push(parse_step("capture: {name: main, caption: Main view}", &draft).unwrap());
        assert!(
            validate_completion(&draft)
                .unwrap_err()
                .to_string()
                .contains("wait_text")
        );
        assert!(parse_step("capture: {name: main, caption: Another view}", &draft).is_err());
        draft
            .steps
            .push(parse_step("wait_text: Expected outcome", &draft).unwrap());
        validate_completion(&draft).unwrap();
    }

    #[test]
    fn saved_recipe_round_trips_literals_keys_timeouts_and_fixture_paths() {
        let original = br#"version: 1
setup:
  files:
    - source: fixtures/sample document
      destination: data/sample document
launch:
  args: ['${APP_DATA}/sample document']
steps:
  - click_text: 'Search: all'
  - type_text: "true\n# literal YAML: text"
  - key: Ctrl+Shift+plus
    timeout: 250ms
  - key: Ctrl+comma
    timeout: 18446744073709551615s
  - wait_text: 'yes'
  - select_window: {title: Preferences}
  - capture: {name: main, caption: Main view}
"#;
        let recipe = Recipe::parse(original).unwrap();
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("recipe.yml");
        fs::write(&path, original).unwrap();
        let exported = save(&path, &recipe, original).unwrap();
        let reparsed = Recipe::parse(&exported).unwrap();
        assert_eq!(format!("{recipe:?}"), format!("{reparsed:?}"));
        assert!(
            String::from_utf8(exported.clone())
                .unwrap()
                .contains("key: Ctrl+Shift+plus")
        );
        assert_eq!(fs::read(&path).unwrap(), exported);
        fs::write(&path, "external edit").unwrap();
        assert!(save(&path, &recipe, &exported).is_err());
        assert_eq!(fs::read_to_string(path).unwrap(), "external edit");
    }
}
