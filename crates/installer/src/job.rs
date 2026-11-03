use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use tokio::sync::watch;

use crate::error::{Error, Result, Stop};
use crate::extract::Progress;

pub(crate) type StopRx = watch::Receiver<Option<Stop>>;

/// Resolves once the job is asked to stop. A dropped sender (the app
/// shutting down) counts as a pause, keeping the download for next time.
pub(crate) async fn stopped(rx: &mut StopRx) -> Stop {
    loop {
        if let Some(stop) = *rx.borrow_and_update() {
            return stop;
        }
        if rx.changed().await.is_err() {
            return Stop::Pause;
        }
    }
}

const REPORT_EVERY: Duration = Duration::from_millis(150);

/// Runs blocking work that reports through [`Progress`], relaying its
/// progress every [`REPORT_EVERY`] and raising its stop flag on request.
pub(crate) async fn blocking<T: Send + 'static>(
    stop: &mut StopRx,
    report: &mut (dyn FnMut(u64, Option<u64>) + Send),
    work: impl FnOnce(&Progress) -> Result<Option<T>> + Send + 'static,
) -> Result<T> {
    let progress = Arc::new(Progress::default());
    let shared = progress.clone();
    let mut task = tokio::task::spawn_blocking(move || work(&shared));
    let mut ticker = tokio::time::interval(REPORT_EVERY);
    let mut requested = None;
    let outcome = loop {
        tokio::select! {
            joined = &mut task => break joined,
            _ = ticker.tick() => {
                let total = progress.total.load(Ordering::Relaxed);
                report(progress.done.load(Ordering::Relaxed), (total > 0).then_some(total));
            }
            stop = stopped(stop), if requested.is_none() => {
                requested = Some(stop);
                progress.stop.store(true, Ordering::Relaxed);
            }
        }
    };
    let result = outcome.map_err(|e| Error::Internal(format!("worker thread: {e}")))??;
    match (result, requested) {
        // Finished just as the stop arrived: keep the work.
        (Some(value), _) => Ok(value),
        (None, stop) => Err(Error::Stopped(stop.unwrap_or(Stop::Pause))),
    }
}
