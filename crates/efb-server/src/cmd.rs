//! `cmd` frames the server answers itself through a provider.
//!
//! Requests come in as `<area>.<verb>` and are answered with a broadcast
//! `cmd` to every client (`origin: None`): a request carries no reply
//! address, and every open EFB wants the result anyway. Anything not handled
//! here is relayed to the other clients untouched.

use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value, json};
use tracing::warn;

use crate::protocol::ClientId;
use crate::providers::{MAX_VIEWPORT, MIN_VIEWPORT, ProviderError};
use crate::server::Ctx;

const DEFAULT_VIEWPORT: u32 = 800;

pub(crate) fn handle(ctx: &Ctx, client: ClientId, name: String, data: Value) {
    let state = ctx.shared.state.clone();
    let p = &ctx.providers;

    // Unwired provider: behave like any other cmd.
    macro_rules! provider {
        ($field:ident) => {
            match p.$field.clone() {
                Some(provider) => provider,
                None => return state.forward_cmd(name, data, Some(client)),
            }
        };
    }

    match name.as_str() {
        "doc.list" => {
            let docs = provider!(docs);
            ctx.spawn(async move {
                let list = docs.list().await.unwrap_or_else(|e| {
                    warn!(error = %e, "efb-server: doc.list failed");
                    Vec::new()
                });
                state.forward_cmd("doc.catalog", json!({ "docs": list }), None);
            });
        }
        "doc.open" => {
            let docs = provider!(docs);
            let Some(id) = str_field(&data, "id") else {
                return;
            };
            ctx.spawn(async move {
                state.forward_cmd(
                    "doc.status",
                    json!({ "id": id, "state": "downloading" }),
                    None,
                );
                let status = match docs.ensure(&id).await {
                    Ok(pages) => json!({ "id": id, "state": "ready", "pages": pages }),
                    Err(e) => {
                        warn!(id, error = %e, "efb-server: doc.open failed");
                        json!({ "id": id, "state": "error" })
                    }
                };
                state.forward_cmd("doc.status", status, None);
            });
        }
        "fpl.status" => {
            let fpl = provider!(fpl);
            ctx.spawn(async move {
                state.forward_cmd("fpl.pilotId", fpl.status().await, None);
            });
        }
        "fpl.setPilotId" => {
            let fpl = provider!(fpl);
            let pilot_id = str_field(&data, "pilot_id").filter(|s| !s.is_empty());
            ctx.spawn(async move {
                let result = fpl.set_pilot_id(pilot_id).await;
                state.forward_cmd("fpl.result", op_result("setPilotId", &result), None);
                if result.is_ok() {
                    state.forward_cmd("fpl.pilotId", fpl.status().await, None);
                }
            });
        }
        "fpl.fetch" => {
            let fpl = provider!(fpl);
            ctx.spawn(async move {
                let result = fpl.fetch().await.map(|mut plan| {
                    if let Some(obj) = plan.as_object_mut() {
                        obj.insert("fetched_at".into(), json!(now_millis()));
                    }
                    plan
                });
                let result = match result {
                    Ok(plan) => {
                        state.apply_ui_delta(one("fplPlan", plan), None).await;
                        Ok(())
                    }
                    Err(e) => {
                        warn!(code = e.code, "efb-server: fpl.fetch failed");
                        Err(e)
                    }
                };
                state.forward_cmd("fpl.result", op_result("fetch", &result), None);
            });
        }
        "wx.fetch" => {
            let wx = provider!(wx);
            let ids: Vec<String> = data
                .get("ids")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(|s| s.trim().to_uppercase())
                        .filter(|s| !s.is_empty())
                        .collect()
                })
                .unwrap_or_default();
            if ids.is_empty() {
                let err = ProviderError::new("no_stations", "Add at least one station first.");
                return state.forward_cmd("wx.result", result_json(&Err(err)), None);
            }
            ctx.spawn(async move {
                let result = match wx.fetch(ids.clone()).await {
                    Ok(stations) => {
                        let blob =
                            json!({ "ids": ids, "stations": stations, "fetched_at": now_millis() });
                        state.apply_ui_delta(one("wxData", blob), None).await;
                        Ok(())
                    }
                    Err(e) => {
                        warn!(code = e.code, "efb-server: wx.fetch failed");
                        Err(e)
                    }
                };
                state.forward_cmd("wx.result", result_json(&result), None);
            });
        }
        "ctl.load" => {
            let ctl = provider!(ctl);
            ctx.spawn(async move {
                state.forward_cmd("ctl.stored", ctl.load().await, None);
            });
        }
        "ctl.save" => {
            let ctl = provider!(ctl);
            ctx.spawn(async move {
                let result = ctl.save(data).await;
                if let Err(e) = &result {
                    warn!(code = e.code, "efb-server: ctl.save failed");
                }
                state.forward_cmd("ctl.result", op_result("save", &result), None);
            });
        }
        "nav.status" => {
            let nav = provider!(nav);
            ctx.spawn(async move {
                state.forward_cmd("nav.info", nav.status().await, None);
            });
        }
        "nav.charts" => {
            let nav = provider!(nav);
            let Some(icao) = str_field(&data, "icao")
                .map(|s| s.to_uppercase())
                .filter(|s| !s.is_empty())
            else {
                return;
            };
            ctx.spawn(async move {
                let reply = match nav.charts_index(&icao).await {
                    Ok(index) => json!({
                        "icao": icao, "ok": true, "error": null,
                        "charts": index.get("charts").cloned().unwrap_or_else(|| json!([])),
                    }),
                    Err(e) => {
                        warn!(icao, code = e.code, "efb-server: nav.charts failed");
                        json!({ "icao": icao, "ok": false, "charts": [], "error": e.to_json() })
                    }
                };
                state.forward_cmd("nav.chartsIndex", reply, None);
            });
        }
        // Every browser frame must name its session: `cmd` has no other
        // addressing, so the session id is all that says which screen it is
        // for. A frame without one is dropped, not guessed at.
        "browser.open" => {
            let browser = provider!(browser);
            let Some(session) = session(&data) else {
                return;
            };
            let url = str_field(&data, "url").unwrap_or_else(|| "about:blank".into());
            let (w, h) = (viewport(&data, "w"), viewport(&data, "h"));
            ctx.spawn(async move { browser.open(&session, &url, w, h).await });
        }
        "browser.nav" => {
            let browser = provider!(browser);
            let Some(session) = session(&data) else {
                return;
            };
            // Alternatives on one cmd; url wins, so a frame carrying both
            // cannot silently do the other thing.
            if let Some(url) = str_field(&data, "url") {
                ctx.spawn(async move { browser.navigate(&session, &url).await });
            } else if let Some(action) = str_field(&data, "action") {
                ctx.spawn(async move { browser.action(&session, &action).await });
            }
        }
        "browser.input" => {
            let browser = provider!(browser);
            if let Some(session) = session(&data) {
                // Inline, never spawned: see `BrowserProvider::input`.
                browser.input(&session, data);
            }
        }
        "browser.close" => {
            let browser = provider!(browser);
            if let Some(session) = session(&data) {
                ctx.spawn(async move { browser.close(&session).await });
            }
        }
        _ => state.forward_cmd(name, data, Some(client)),
    }
}

fn str_field(data: &Value, key: &str) -> Option<String> {
    data.get(key)
        .and_then(Value::as_str)
        .map(|s| s.trim().to_owned())
}

fn one(key: &str, value: Value) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert(key.to_owned(), value);
    m
}

fn result_json(result: &Result<(), ProviderError>) -> Value {
    match result {
        Ok(()) => json!({ "ok": true, "error": null }),
        Err(e) => json!({ "ok": false, "error": e.to_json() }),
    }
}

fn op_result(op: &str, result: &Result<(), ProviderError>) -> Value {
    let mut v = result_json(result);
    if let Some(obj) = v.as_object_mut() {
        obj.insert("op".into(), json!(op));
    }
    v
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// Bounded and plain, since it comes from a paired client and keys host-side
/// sessions.
fn session(data: &Value) -> Option<String> {
    str_field(data, "session").filter(|s| {
        !s.is_empty()
            && s.len() <= 64
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    })
}

/// Clamped: a value this far out means the client measured before layout.
fn viewport(data: &Value, key: &str) -> u32 {
    data.get(key)
        .and_then(Value::as_f64)
        .filter(|v| v.is_finite() && *v > 0.0)
        .map_or(DEFAULT_VIEWPORT, |v| {
            // Saturating float-to-int cast, then clamped.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let px = v.round() as u32;
            px.clamp(MIN_VIEWPORT, MAX_VIEWPORT)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn results_keep_the_v3_shape() {
        assert_eq!(
            op_result("fetch", &Ok(())),
            json!({ "op": "fetch", "ok": true, "error": null })
        );
        assert_eq!(
            op_result("save", &Err(ProviderError::new("internal", "disk"))),
            json!({ "op": "save", "ok": false, "error": { "code": "internal", "message": "disk" } })
        );
    }

    #[test]
    fn browser_sessions_and_viewports_are_bounded() {
        assert_eq!(
            session(&json!({ "session": " web-1 " })).as_deref(),
            Some("web-1")
        );
        assert_eq!(session(&json!({ "session": "../x" })), None);
        assert_eq!(session(&json!({ "session": "a".repeat(65) })), None);
        assert_eq!(viewport(&json!({ "w": 1e9 }), "w"), MAX_VIEWPORT);
        assert_eq!(viewport(&json!({ "w": 3 }), "w"), MIN_VIEWPORT);
        assert_eq!(viewport(&json!({}), "w"), DEFAULT_VIEWPORT);
    }
}
