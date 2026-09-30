//! The Colima profiles for the sidebar.

use std::collections::HashSet;
use std::time::Duration;

use gpui::{Context, EventEmitter, Task};

use crate::colima::{self, Machine};
use crate::runtime::Backend;

/// A profile can start or stop in a terminal too. A slow poll finds that.
const REFRESH: Duration = Duration::from_secs(10);

pub enum MachineEvent {
    Error(String),
}

pub struct MachineStore {
    pub machines: Vec<Machine>,
    /// The profiles that have a start or a stop in progress.
    pub busy: HashSet<String>,
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

    /// Starts a stopped profile, or stops a running profile.
    pub fn toggle(&mut self, name: &str, cx: &mut Context<Self>) {
        let Some(machine) = self.machines.iter().find(|m| m.name == name) else {
            return;
        };
        if !self.busy.insert(name.to_string()) {
            return;
        }
        let start = !machine.is_running();
        let profile = name.to_string();
        let rx = Backend::global(cx).spawn({
            let profile = profile.clone();
            async move {
                tokio::task::spawn_blocking(move || colima::set_running(&profile, start))
                    .await
                    .map_err(anyhow::Error::from)
                    .and_then(|r| r)
            }
        });
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = rx.await.unwrap_or_else(|_| Err(anyhow::anyhow!("cancelled")));
            let _ = this.update(cx, |this, cx| {
                this.busy.remove(&profile);
                if let Err(e) = result {
                    cx.emit(MachineEvent::Error(e.to_string()));
                }
                this.refresh(cx);
                cx.notify();
            });
        })
        .detach();
    }
}
