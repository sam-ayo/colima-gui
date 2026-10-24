//! The Colima profiles for the sidebar.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use gpui::{Context, EventEmitter, Task};

use crate::colima::{self, Machine, MachineAction, Settings};
use crate::docker as engine;
use crate::runtime::Backend;

/// A profile can start or stop in a terminal too. A slow poll finds that.
const REFRESH: Duration = Duration::from_secs(10);

pub enum MachineEvent {
    Error(String),
    /// The engine of this profile answers. `colima start` can still run.
    Ready(String),
    /// The result of a DNS check in the VM of this profile. See
    /// `colima::vm_dns_fails`.
    Dns { profile: String, works: bool },
}

pub struct MachineStore {
    pub machines: Vec<Machine>,
    /// The profiles that show a start or a stop in progress.
    pub busy: HashSet<String>,
    /// The profiles that have a `colima` command in progress. A start
    /// leaves `busy` when the engine answers, but stays here until the
    /// command ends, so a second toggle cannot interrupt it.
    running_cli: HashSet<String>,
    _poll: Task<()>,
}

impl EventEmitter<MachineEvent> for MachineStore {}

impl MachineStore {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let poll = cx.spawn(async move |this, cx| {
            loop {
                if this.update(cx, |this, cx| this.refresh(cx)).is_err() {
                    break;
                }
                cx.background_executor().timer(REFRESH).await;
            }
        });
        MachineStore {
            machines: Vec::new(),
            busy: HashSet::new(),
            running_cli: HashSet::new(),
            _poll: poll,
        }
    }

    /// True when the list has machines and none of them runs. The Docker
    /// sections are then disabled.
    pub fn all_off(&self) -> bool {
        !self.machines.is_empty() && !self.machines.iter().any(Machine::is_running)
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let rx = Backend::global(cx).spawn(async { tokio::task::spawn_blocking(colima::list).await });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(machines)) = rx.await else { return };
            let _ = this.update(cx, |this, cx| {
                // A new profile can be missing from the list until
                // `colima start` writes its files. Keep its row.
                let mut machines = machines;
                for m in &this.machines {
                    if this.running_cli.contains(&m.name) && !machines.iter().any(|n| n.name == m.name) {
                        machines.push(m.clone());
                    }
                }
                if this.machines != machines {
                    this.machines = machines;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Starts a profile that is not running. Returns false if the list does
    /// not have a stopped profile with this name.
    pub fn start(&mut self, name: &str, cx: &mut Context<Self>) -> bool {
        if !self.machines.iter().any(|m| m.name == name && !m.is_running()) {
            return false;
        }
        self.run(name, MachineAction::Start, cx);
        true
    }

    /// Runs a `colima` command on a profile. A profile runs one command at
    /// a time. A second action while a command runs has no effect.
    pub fn run(&mut self, name: &str, action: MachineAction, cx: &mut Context<Self>) {
        let profile = name.to_string();
        let job = move || colima::run_action(&profile, action);
        self.run_job(name, action == MachineAction::Start, action.leaves_engine_running(), job, cx);
    }

    /// Makes a new profile with `settings` and starts it. The sidebar shows
    /// its row with a spinner until the command ends.
    pub fn create(&mut self, name: &str, settings: Settings, cx: &mut Context<Self>) {
        if self.machines.iter().any(|m| m.name == name) {
            cx.emit(MachineEvent::Error(format!("A machine with the name “{name}” exists already")));
            return;
        }
        self.machines.push(Machine { name: name.to_string(), status: "Stopped".into() });
        let profile = name.to_string();
        self.run_job(name, true, true, move || colima::create(&profile, &settings), cx);
    }

    /// Changes the settings of a profile. A running machine restarts.
    pub fn configure(&mut self, name: &str, old: Settings, new: Settings, cx: &mut Context<Self>) {
        let Some(running) = self.machines.iter().find(|m| m.name == name).map(Machine::is_running) else {
            return;
        };
        if running && let Some(m) = self.machines.iter_mut().find(|m| m.name == name) {
            // The engine stops first. The status changes back when the
            // engine answers again.
            m.status = "Stopped".into();
        }
        let profile = name.to_string();
        self.run_job(name, true, true, move || colima::configure(&profile, &old, &new, running), cx);
    }

    /// Tells the app if the VM of a running profile can find hosts.
    fn check_dns(&mut self, profile: String, cx: &mut Context<Self>) {
        let rx = Backend::global(cx).spawn({
            let profile = profile.clone();
            async move { tokio::task::spawn_blocking(move || colima::vm_dns_fails(&profile)).await }
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(fails)) = rx.await {
                let _ = this.update(cx, |_, cx| cx.emit(MachineEvent::Dns { profile, works: !fails }));
            }
        })
        .detach();
    }

    /// Runs `job` for a profile. `starts` tells that the engine answers
    /// before the job ends, as with `colima start`. `leaves_running` tells
    /// that the engine runs when the job ends.
    fn run_job(
        &mut self,
        name: &str,
        starts: bool,
        leaves_running: bool,
        job: impl FnOnce() -> anyhow::Result<()> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        if !self.running_cli.insert(name.to_string()) {
            return;
        }
        self.busy.insert(name.to_string());
        let profile = name.to_string();
        let done = Arc::new(AtomicBool::new(false));
        let backend = Backend::global(cx);
        let rx = backend.spawn({
            let done = done.clone();
            async move {
                let result = tokio::task::spawn_blocking(job)
                    .await
                    .map_err(anyhow::Error::from)
                    .and_then(|r| r);
                done.store(true, Ordering::Relaxed);
                result
            }
        });
        // A start can take minutes, but the engine can answer before the
        // command ends. For the other actions, the engine can answer before
        // the command stops it, so only the end of the command counts.
        if starts {
            let ready = backend.spawn(engine::wait_until_ready(engine::profile_socket(&profile), done));
            let profile = profile.clone();
            cx.spawn(async move |this, cx| {
                if ready.await != Ok(true) {
                    return;
                }
                let _ = this.update(cx, |this, cx| {
                    if !this.busy.remove(&profile) {
                        return;
                    }
                    if let Some(m) = this.machines.iter_mut().find(|m| m.name == profile) {
                        m.status = "Running".into();
                    }
                    cx.emit(MachineEvent::Ready(profile.clone()));
                    cx.notify();
                });
            })
            .detach();
        }
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = rx.await.unwrap_or_else(|_| Err(anyhow::anyhow!("cancelled")));
            let _ = this.update(cx, |this, cx| {
                this.busy.remove(&profile);
                this.running_cli.remove(&profile);
                match result {
                    Err(e) => cx.emit(MachineEvent::Error(e.to_string())),
                    Ok(()) if leaves_running => {
                        cx.emit(MachineEvent::Ready(profile.clone()));
                        this.check_dns(profile.clone(), cx);
                    }
                    Ok(()) => {}
                }
                this.refresh(cx);
                cx.notify();
            });
        })
        .detach();
    }
}
