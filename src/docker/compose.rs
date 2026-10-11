//! Docker Compose commands. The Engine API has no Compose calls, so the app
//! runs the `docker compose` CLI.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context as _, Result, bail};
use bollard::Docker;
use bollard::query_parameters::ListContainersOptions;

use super::containers::{COMPOSE_FILES_LABEL, COMPOSE_PROJECT_LABEL};

/// The folder of the project. Compose reads relative paths and the `.env`
/// file from it.
pub const WORKING_DIR_LABEL: &str = "com.docker.compose.project.working_dir";
/// The `--env-file` values of the project, separated by commas.
pub const ENV_FILES_LABEL: &str = "com.docker.compose.project.environment_file";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ComposeAction {
    /// Creates and starts the services. Compose recreates the containers
    /// of the services that changed.
    Up,
    /// Pulls the images, then does an `Up`.
    PullUp,
    /// Builds the images, then does an `Up`.
    BuildUp,
    /// Stops and removes the containers and networks. The volumes and the
    /// images stay.
    Down,
}

impl ComposeAction {
    /// The commands, in sequence. A pull ignores the services that Compose
    /// builds, because their images are not in a registry.
    fn steps(self) -> &'static [&'static [&'static str]] {
        match self {
            ComposeAction::Up => &[&["up", "--detach"]],
            ComposeAction::PullUp => &[&["pull", "--ignore-buildable"], &["up", "--detach"]],
            ComposeAction::BuildUp => &[&["up", "--detach", "--build"]],
            ComposeAction::Down => &[&["down"]],
        }
    }

    /// The menu label.
    pub fn label(self) -> &'static str {
        match self {
            ComposeAction::Up => "Compose Up",
            ComposeAction::PullUp => "Pull and Up",
            ComposeAction::BuildUp => "Build and Up",
            ComposeAction::Down => "Compose Down…",
        }
    }

    /// The text while the action runs.
    pub fn pending_label(self) -> &'static str {
        match self {
            ComposeAction::Up => "Starting…",
            ComposeAction::PullUp => "Pulling…",
            ComposeAction::BuildUp => "Building…",
            ComposeAction::Down => "Removing…",
        }
    }

    /// The text when the action ends, as in "Started “shop”".
    pub fn done_label(self) -> &'static str {
        match self {
            ComposeAction::Up | ComposeAction::PullUp | ComposeAction::BuildUp => "Started",
            ComposeAction::Down => "Removed",
        }
    }

    /// The verb for a message, as in "Could not start “shop”".
    pub fn verb(self) -> &'static str {
        match self {
            ComposeAction::Up => "start",
            ComposeAction::PullUp => "pull",
            ComposeAction::BuildUp => "build",
            ComposeAction::Down => "remove",
        }
    }

    /// True if the action needs the Compose files. A `Down` needs only the
    /// project name, because Compose finds the containers by their labels.
    pub fn needs_files(self) -> bool {
        self != ComposeAction::Down
    }
}

/// The values that select a project for the CLI.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Project {
    /// `None` lets Compose make the name from the folder.
    pub name: Option<String>,
    pub files: Vec<String>,
    pub dir: Option<String>,
    pub env_files: Vec<String>,
}

impl Project {
    /// A new project from one Compose file.
    pub fn from_file(path: &Path) -> Project {
        Project {
            name: None,
            files: vec![path.display().to_string()],
            dir: path.parent().map(|d| d.display().to_string()),
            env_files: Vec::new(),
        }
    }

    /// Reads the project from the labels of a container.
    fn from_labels(name: &str, labels: &HashMap<String, String>) -> Project {
        let list = |key: &str| -> Vec<String> {
            labels
                .get(key)
                .map(|v| v.split(',').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect())
                .unwrap_or_default()
        };
        Project {
            name: Some(name.to_string()),
            files: list(COMPOSE_FILES_LABEL),
            dir: labels.get(WORKING_DIR_LABEL).filter(|d| !d.is_empty()).cloned(),
            env_files: list(ENV_FILES_LABEL),
        }
    }

    /// The global arguments of `docker compose`.
    fn args(&self) -> Vec<String> {
        let mut args: Vec<String> = ["--ansi", "never", "--progress", "plain"].map(String::from).into();
        if let Some(name) = &self.name {
            args.extend(["--project-name".into(), name.clone()]);
        }
        for file in &self.files {
            args.extend(["--file".into(), file.clone()]);
        }
        if let Some(dir) = &self.dir {
            args.extend(["--project-directory".into(), dir.clone()]);
        }
        for file in &self.env_files {
            args.extend(["--env-file".into(), file.clone()]);
        }
        args
    }
}

/// Reads a project from the labels of one of its containers.
pub async fn project(docker: &Docker, name: &str) -> Result<Project> {
    let opts = ListContainersOptions {
        all: true,
        filters: Some(HashMap::from([(
            "label".to_string(),
            vec![format!("{COMPOSE_PROJECT_LABEL}={name}")],
        )])),
        ..Default::default()
    };
    let list = docker.list_containers(Some(opts)).await?;
    let labels = list
        .into_iter()
        .find_map(|c| c.labels)
        .with_context(|| format!("no container of “{name}” exists"))?;
    Ok(Project::from_labels(name, &labels))
}

/// Runs an action on the engine at `socket`, and waits for it to end.
/// `on_progress` gets each progress line. Call this on a blocking thread.
pub fn run(socket: &str, project: &Project, action: ComposeAction, mut on_progress: impl FnMut(&str)) -> Result<()> {
    if action.needs_files() && project.files.is_empty() {
        bail!("the containers do not tell which Compose files they came from");
    }
    let (program, prefix) = cli()?;
    for step in action.steps() {
        let mut cmd = Command::new(program);
        cmd.args(prefix)
            .args(project.args())
            .args(*step)
            .env("PATH", crate::colima::search_path())
            .env("DOCKER_HOST", socket)
            // A context would select a different engine than `DOCKER_HOST`.
            .env_remove("DOCKER_CONTEXT")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().with_context(|| format!("could not run `{program}`"))?;
        // Compose writes the progress and the errors to stderr. The last
        // line tells the cause of a failure.
        let mut last = String::new();
        if let Some(stderr) = child.stderr.take() {
            for line in BufReader::new(stderr).lines() {
                let Ok(line) = line else { break };
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                if let Some(text) = progress(line) {
                    on_progress(text);
                }
                last = line.to_string();
            }
        }
        if !child.wait()?.success() {
            bail!("{}", clean_error(&last));
        }
    }
    Ok(())
}

/// Finds the Compose CLI: the `docker compose` plugin, else the
/// standalone `docker-compose`.
fn cli() -> Result<(&'static str, &'static [&'static str])> {
    let works = |program: &str, args: &[&str]| {
        Command::new(program)
            .args(args)
            .env("PATH", crate::colima::search_path())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    };
    if works("docker", &["compose", "version"]) {
        return Ok(("docker", &["compose"]));
    }
    if works("docker-compose", &["version"]) {
        return Ok(("docker-compose", &[]));
    }
    bail!("Docker Compose is not installed. Install it with `brew install docker-compose`")
}

/// Returns the text of a line for the user, or `None` for a line about
/// one image layer ("1efec2485f45 Downloading 1.049MB").
fn progress(line: &str) -> Option<&str> {
    let first = line.split_whitespace().next()?;
    let layer = first.len() == 12 && first.bytes().all(|b| b.is_ascii_hexdigit());
    (!layer).then_some(line)
}

fn clean_error(line: &str) -> String {
    let line = line.strip_prefix("Error response from daemon: ").unwrap_or(line);
    if line.is_empty() { "Compose stopped with an error".into() } else { line.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_project_from_labels() {
        let labels = HashMap::from([
            (COMPOSE_FILES_LABEL.to_string(), "/p/compose.yaml, /p/compose.dev.yaml".to_string()),
            (WORKING_DIR_LABEL.to_string(), "/p".to_string()),
            (ENV_FILES_LABEL.to_string(), String::new()),
        ]);
        let project = Project::from_labels("shop", &labels);
        assert_eq!(project.files, ["/p/compose.yaml", "/p/compose.dev.yaml"]);
        assert_eq!(project.dir.as_deref(), Some("/p"));
        assert!(project.env_files.is_empty());
        assert_eq!(
            project.args(),
            [
                "--ansi", "never", "--progress", "plain", "--project-name", "shop", "--file",
                "/p/compose.yaml", "--file", "/p/compose.dev.yaml", "--project-directory", "/p",
            ]
        );
    }

    #[test]
    fn a_new_file_has_no_project_name() {
        let project = Project::from_file(Path::new("/src/shop/compose.yaml"));
        assert_eq!(project.name, None);
        assert_eq!(project.dir.as_deref(), Some("/src/shop"));
    }

    #[test]
    fn skips_the_layer_lines() {
        assert_eq!(progress("1efec2485f45 Downloading 1.049MB"), None);
        assert_eq!(progress("Image nginx:alpine Pulling"), Some("Image nginx:alpine Pulling"));
        assert_eq!(progress("Container shop-web-1 Started"), Some("Container shop-web-1 Started"));
    }

    #[test]
    fn keeps_only_the_daemon_message() {
        assert_eq!(clean_error("Error response from daemon: pull access denied"), "pull access denied");
        assert_eq!(clean_error(""), "Compose stopped with an error");
    }
}

/// Tests against the real engine. They make their own project.
/// Run them with `cargo test -- --ignored`.
#[cfg(test)]
mod live_tests {
    use super::*;
    use crate::docker::{connect, discover_socket};

    #[tokio::test]
    #[ignore = "needs a running Docker engine"]
    async fn up_reads_labels_and_down_removes() {
        let dir = std::env::temp_dir().join("cgui-compose-live");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("compose.yaml");
        std::fs::write(&file, "name: cgui-compose-live\nservices:\n  web:\n    image: nginx:alpine\n").unwrap();
        let socket = discover_socket();
        let (docker, _) = connect(&socket).await;
        let docker = docker.unwrap();

        let (s, p) = (socket.clone(), Project::from_file(&file));
        let mut lines = Vec::new();
        tokio::task::spawn_blocking(move || {
            run(&s, &p, ComposeAction::Up, |l| lines.push(l.to_string())).map(|()| lines)
        })
        .await
        .unwrap()
        .map(|lines| assert!(lines.iter().any(|l| l.contains("Started")), "{lines:?}"))
        .unwrap();

        // The labels give the same files, so a second action finds them.
        let project = project(&docker, "cgui-compose-live").await.unwrap();
        assert_eq!(project.files, [file.display().to_string()]);
        let s = socket.clone();
        let p = project.clone();
        tokio::task::spawn_blocking(move || run(&s, &p, ComposeAction::PullUp, |_| {})).await.unwrap().unwrap();

        let p = Project { name: Some("cgui-compose-live".into()), ..Default::default() };
        tokio::task::spawn_blocking(move || run(&socket, &p, ComposeAction::Down, |_| {})).await.unwrap().unwrap();
        assert!(!project_exists(&docker).await);
    }

    #[tokio::test]
    #[ignore = "needs a running Docker engine"]
    async fn a_failure_gives_the_cause() {
        let dir = std::env::temp_dir().join("cgui-compose-bad");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("compose.yaml");
        std::fs::write(&file, "services:\n  bad:\n    image: cgui-no-such/image:1\n").unwrap();
        let socket = discover_socket();
        let p = Project::from_file(&file);
        let err = tokio::task::spawn_blocking(move || run(&socket, &p, ComposeAction::Up, |_| {}))
            .await
            .unwrap()
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("pull access denied"), "{msg}");
        assert!(!msg.starts_with("Error response"), "{msg}");
    }

    async fn project_exists(docker: &Docker) -> bool {
        project(docker, "cgui-compose-live").await.is_ok()
    }
}
