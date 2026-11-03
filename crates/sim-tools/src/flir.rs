//! The bridge to the MD-10 HUD's FLIR gauge.
//!
//! Wire protocol (`flir_stream`, the crate both ends use):
//! - Gauge -> manager: bridge event `flir.state` (`StateReport`) about 15 Hz
//!   while the HUD is up. `active: true` keeps the engine developing;
//!   silence or `active: false` stops it.
//! - Manager -> gauge: `flir.chunk` commands (`Chunk`), acked, one per piece
//!   of a gray frame packed with the delta + deflate codec.
//!
//! The names on the 10-systems side: `display-md-hud`'s `wire` (CommBus
//! events) and `coherent/bridge/Main.res` (the relay to
//! `ws://127.0.0.1:9878/md10_flir`).
//!
//! Pacing: latest-wins frames from the engine, at most [`FRAMES_IN_FLIGHT`]
//! frames being delivered at once, one attempt per chunk (a late frame is
//! better dropped than retried: the next is already coming).

use std::sync::Arc;
use std::time::Duration;

use flir_stream::{CHUNK_CHARS, CHUNK_COMMAND, Codec, STATE_EVENT, StateReport};
use infinity_bridge::{BridgeKind, BridgeManager, BridgeSpec};
use infinity_flir_engine::FlirEngine;
use tokio::sync::Semaphore;
use tokio::sync::broadcast::error::RecvError;
use tracing::{debug, warn};

/// Must match the relay in `10-systems/coherent/bridge/Main.res`.
const PORT: u16 = 9878;
const NAMESPACE: &str = "md10_flir";

/// Per-chunk ack budget: under the relay's own timeout, so a stalled sim
/// frame costs one frame, not a backlog.
const CHUNK_TIMEOUT: Duration = Duration::from_secs(2);
const FRAMES_IN_FLIGHT: usize = 2;

pub(crate) fn serve(engine: &FlirEngine, bridges: &BridgeManager) {
    let spec = BridgeSpec {
        id: "flir".into(),
        kind: BridgeKind::TauriWsBridge,
        port: PORT,
        namespace: NAMESPACE.into(),
    };
    let mgr = bridges.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(e) = mgr.ensure_started(spec).await {
            warn!(error = %e, port = PORT, "flir bridge did not start");
        }
    });

    // State reports drive the engine.
    {
        let engine = engine.clone();
        let mut rx = bridges.subscribe();
        tauri::async_runtime::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(evt) if evt.name == STATE_EVENT => {
                        match serde_json::from_value::<StateReport>(evt.data) {
                            Ok(report) if report.active => engine.request(report),
                            Ok(_) => engine.withdraw(),
                            Err(e) => debug!(error = %e, "flir: bad state report"),
                        }
                    }
                    Ok(_) => {}
                    Err(RecvError::Lagged(n)) => warn!(dropped = n, "flir handler lagged"),
                    Err(RecvError::Closed) => break,
                }
            }
        });
    }

    // Frames go out as they are developed.
    let engine = engine.clone();
    let mgr = bridges.clone();
    let mut frames = engine.frames();
    let in_flight = Arc::new(Semaphore::new(FRAMES_IN_FLIGHT));
    tauri::async_runtime::spawn(async move {
        while frames.changed().await.is_ok() {
            if frames.borrow_and_update().is_none() {
                continue;
            }
            // Latest wins: wait for a slot, then send whatever is newest.
            let Ok(permit) = Arc::clone(&in_flight).acquire_owned().await else {
                break;
            };
            let Some(frame) = frames.borrow_and_update().clone() else {
                continue;
            };
            let quant = engine.settings().quant;
            let (mgr, engine) = (mgr.clone(), engine.clone());
            tauri::async_runtime::spawn(async move {
                let _permit = permit;
                let chunks = flir_stream::chunks(&frame, Codec::Gray8Delta, quant, CHUNK_CHARS);
                let wire_bytes: usize = chunks.iter().map(|c| c.data.len()).sum();
                let sends = chunks.into_iter().map(|c| {
                    let mgr = mgr.clone();
                    async move {
                        let payload = serde_json::to_value(&c).unwrap_or_default();
                        mgr.command_with_attempts(
                            NAMESPACE,
                            CHUNK_COMMAND,
                            payload,
                            CHUNK_TIMEOUT,
                            1,
                        )
                        .await
                    }
                });
                let results = futures_util::future::join_all(sends).await;
                let failed = results.iter().find_map(|r| r.as_ref().err());
                if let Some(e) = failed {
                    debug!(error = %e, seq = frame.seq, "flir: frame not delivered");
                }
                engine.note_sent(failed.is_none(), wire_bytes as u64);
            });
        }
    });
}
