use std::time::Duration;

use infinity_ipc::dto::{Sim, SimStatus};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System};
use tokio::sync::watch;

const EXECUTABLES: &[(&str, Sim)] = &[
    ("flightsimulator2024.exe", Sim::Msfs2024),
    ("flightsimulator.exe", Sim::Msfs2020),
];

pub const DEFAULT_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
pub struct SimWatch {
    rx: watch::Receiver<SimStatus>,
}

impl SimWatch {
    pub fn new(interval: Duration) -> (Self, impl Future<Output = ()> + Send + 'static) {
        let (tx, rx) = watch::channel(SimStatus::default());
        (Self { rx }, poll(tx, interval))
    }

    pub fn status(&self) -> SimStatus {
        *self.rx.borrow()
    }

    pub fn subscribe(&self) -> watch::Receiver<SimStatus> {
        self.rx.clone()
    }
}

async fn poll(tx: watch::Sender<SimStatus>, interval: Duration) {
    let mut system = System::new_with_specifics(
        RefreshKind::nothing().with_processes(ProcessRefreshKind::nothing()),
    );
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        ticker.tick().await;
        system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing(),
        );
        let status = SimStatus {
            running: detect(
                system
                    .processes()
                    .values()
                    .map(|p| p.name().to_string_lossy()),
            ),
        };
        tx.send_if_modified(|current| {
            let changed = *current != status;
            if changed {
                tracing::info!(sim = ?status.running, "simulator status changed");
                *current = status;
            }
            changed
        });
        if tx.is_closed() {
            return;
        }
    }
}

fn detect<S: AsRef<str>>(names: impl Iterator<Item = S>) -> Option<Sim> {
    let mut found = None;
    for name in names {
        let name = name.as_ref();
        for (exe, sim) in EXECUTABLES {
            if name.eq_ignore_ascii_case(exe) {
                if found != Some(Sim::Msfs2024) {
                    found = Some(*sim);
                }
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_by_executable_name() {
        assert_eq!(
            detect(["explorer.exe", "FlightSimulator2024.exe"].iter()),
            Some(Sim::Msfs2024)
        );
        assert_eq!(detect(["FLIGHTSIMULATOR.EXE"].iter()), Some(Sim::Msfs2020));
        assert_eq!(
            detect(["FlightSimulator2024.exe", "FlightSimulator.exe"].iter()),
            Some(Sim::Msfs2024)
        );
        assert_eq!(detect(["steam.exe"].iter()), None);
    }
}
