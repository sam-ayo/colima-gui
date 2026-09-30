//! The Colima profiles for the sidebar.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use gpui::{Context, EventEmitter, Task};

use crate::colima::{self, Machine, MachineAction};
use crate::docker as engine;
use crate::runtime::Backend;

/// A profile can start or stop in a terminal too. A slow poll finds that.
const REFRESH: Duration = Duration::from_secs(10);

pub enum MachineEvent {
    Error(String),
    /// The engine of this profile answers. `colima start` can still run.
    Ready(String),
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

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let rx = Backend::global(cx).spawn(async { tokio::task::spawn_blocking(colima::list).await });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(machines)) = rx.await else { return };
            let _ = this.update(cx, |this, cx| {
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
        if !self.running_cli.insert(name.to_string()) {
            return;
        }
        self.busy.insert(name.to_string());
        let profile = name.to_string();
        let done = Arc::new(AtomicBool::new(false));
        let backend = Backend::global(cx);
        let rx = backend.spawn({
            let profile = profile.clone();
            let done = done.clone();
            async move {
                let result = tokio::task::spawn_blocking(move || colima::run_action(&profile, action))
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
        if action == MachineAction::Start {
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
                    Ok(()) if action.leaves_engine_running() => cx.emit(MachineEvent::Ready(profile.clone())),
                    Ok(()) => {}
                }
                this.refresh(cx);
                cx.notify();
            });
        })
        .detach();
    }
}
