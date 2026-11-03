//! The input path: `browser.input` payloads parsed into [`InputEvent`]s,
//! queued per session in arrival order, and translated into CDP calls.
//!
//! Order is the point. A press and its release, a run of characters and the
//! backspaces after them: v3 once spawned a task per event and lost the
//! order, so releases overtook presses (the page kept the button held and the
//! next drag selected text) and typed text arrived shuffled. Now
//! [`BrowserProvider::input`](infinity_efb_server::BrowserProvider::input)
//! enqueues inline from the socket's read loop and one worker per session
//! drains the queue, awaiting each call before the next.
//!
//! Awaiting also makes the queue the backpressure: on a page slow to take
//! input, pointer moves and wheel ticks pile up and the worker folds each run
//! into one event instead of replaying the backlog ([`coalesce`]).

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::mpsc;
use tracing::debug;

use super::cdp::Cdp;
use super::focus::{Focus, ProbeCause};

/// Probes after the one taken when a click lands. A page that focuses a
/// field from a timer (a search icon expanding into a box) has not done it
/// yet when the click returns.
const SETTLE_PROBES: [Duration; 2] = [Duration::from_millis(250), Duration::from_millis(700)];

/// Most backspaces one `edit` may ask for. The EFB only asks for what it
/// typed, so this only bounds a malformed frame.
const MAX_EDIT_DELETE: u32 = 10_000;

/// CDP's modifier bit for Ctrl.
const MODIFIER_CTRL: i64 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MouseKind {
    Down,
    Up,
    Move,
}

/// One `browser.input` payload. Coordinates are page pixels: the viewport
/// renders 1:1, so the client's offsets inside its `<img>` need no scaling.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum InputEvent {
    Mouse {
        kind: MouseKind,
        x: f64,
        y: f64,
    },
    Wheel {
        x: f64,
        y: f64,
        dx: f64,
        dy: f64,
    },
    /// `key`/`char`: text, inserted as one IME-style commit.
    Text(String),
    /// `key`/`press`: a named key (`Enter`, `Tab`, arrows...).
    Key {
        key: String,
        modifiers: i64,
    },
    /// Replace the tail of the focused field: delete `del` characters from
    /// its end, then insert `text`. The EFB keeps a copy of the field and
    /// sends the difference, so this is one edit however it was typed.
    Edit {
        del: u32,
        text: String,
    },
}

impl InputEvent {
    /// Lenient like v3: numbers may be floats or missing (0), an unknown
    /// mouse `kind` is a move and any key `kind` but `char` is a press. An
    /// unknown `type` is dropped.
    pub(crate) fn parse(v: &Value) -> Option<Self> {
        let num = |k: &str| {
            v.get(k)
                .and_then(Value::as_f64)
                .filter(|n| n.is_finite())
                .unwrap_or(0.0)
        };
        let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("");
        Some(match s("type") {
            "mouse" => Self::Mouse {
                kind: match s("kind") {
                    "down" => MouseKind::Down,
                    "up" => MouseKind::Up,
                    _ => MouseKind::Move,
                },
                x: num("x"),
                y: num("y"),
            },
            "wheel" => Self::Wheel {
                x: num("x"),
                y: num("y"),
                dx: num("dx"),
                dy: num("dy"),
            },
            "key" if s("kind") == "char" => Self::Text(s("text").to_owned()),
            "key" => Self::Key {
                key: s("key").to_owned(),
                modifiers: v.get("modifiers").and_then(Value::as_i64).unwrap_or(0),
            },
            "edit" => Self::Edit {
                // Saturating float-to-int cast, then bounded.
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                del: (num("del").max(0.0) as u32).min(MAX_EDIT_DELETE),
                text: s("text").to_owned(),
            },
            _ => return None,
        })
    }

    fn is_wheel(&self) -> bool {
        matches!(self, Self::Wheel { .. })
    }

    fn is_move(&self) -> bool {
        matches!(
            self,
            Self::Mouse {
                kind: MouseKind::Move,
                ..
            }
        )
    }
}

/// Folds what is already queued behind `first`: a run of wheel ticks into one
/// (deltas summed, at the last position) and a run of moves into the last.
/// Only a run of the same kind, since folding across a press would move the
/// press. Returns the event to dispatch and the one that ended the run.
pub(crate) fn coalesce(
    first: InputEvent,
    mut next: impl FnMut() -> Option<InputEvent>,
) -> (InputEvent, Option<InputEvent>) {
    if !first.is_wheel() && !first.is_move() {
        return (first, None);
    }
    let mut merged = first;
    while let Some(event) = next() {
        merged = match (merged, event) {
            (InputEvent::Wheel { dx: ax, dy: ay, .. }, InputEvent::Wheel { x, y, dx, dy }) => {
                InputEvent::Wheel {
                    x,
                    y,
                    dx: ax + dx,
                    dy: ay + dy,
                }
            }
            (prev, event) if prev.is_move() && event.is_move() => event,
            (prev, event) => return (prev, Some(event)),
        };
    }
    (merged, None)
}

/// One CDP call on the page session.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Call {
    pub(crate) method: &'static str,
    pub(crate) params: Value,
}

/// What to look at once an event's calls are done.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FollowUp {
    None,
    /// Focus may have moved or the field been rewritten.
    Probe(ProbeCause),
}

/// The CDP calls for one event, in order.
pub(crate) fn plan(event: &InputEvent) -> (Vec<Call>, FollowUp) {
    match event {
        InputEvent::Mouse { kind, x, y } => {
            let moved = mouse("mouseMoved", *x, *y, 0);
            match kind {
                // Arrive before pressing: a pointer that teleports onto a
                // button and presses is a scripted click to behavioural bot
                // checks, and hover menus never open under it.
                MouseKind::Down => (
                    vec![moved, mouse("mousePressed", *x, *y, 1)],
                    FollowUp::None,
                ),
                MouseKind::Up => (
                    vec![mouse("mouseReleased", *x, *y, 0)],
                    // Announce: tapping the field you are in is how the crew
                    // bring the keyboard back after hiding it.
                    FollowUp::Probe(ProbeCause::Announce),
                ),
                MouseKind::Move => (vec![moved], FollowUp::None),
            }
        }
        InputEvent::Wheel { x, y, dx, dy } => (
            vec![Call {
                method: "Input.dispatchMouseEvent",
                params: json!({
                    "type": "mouseWheel", "x": x, "y": y,
                    "deltaX": dx, "deltaY": dy, "button": "none", "buttons": 0,
                }),
            }],
            FollowUp::None,
        ),
        InputEvent::Text(text) => (insert_text(text).into_iter().collect(), FollowUp::None),
        InputEvent::Key { key, modifiers } => {
            let calls = press(key, *modifiers);
            // Both can move focus or rewrite the field: Tab to the next one,
            // Enter by submitting (a chat box clears on send).
            let follow = if key == "Tab" || key == "Enter" {
                FollowUp::Probe(ProbeCause::Announce)
            } else {
                FollowUp::None
            };
            (calls, follow)
        }
        InputEvent::Edit { del, text } => {
            if *del == 0 && text.is_empty() {
                return (Vec::new(), FollowUp::None);
            }
            // Ctrl+End first: the crew's tap may have left the caret anywhere,
            // and the EFB's copy is edited at its end.
            let mut calls = press("End", MODIFIER_CTRL);
            for _ in 0..*del {
                calls.extend(press("Backspace", 0));
            }
            calls.extend(insert_text(text));
            (calls, FollowUp::None)
        }
    }
}

fn mouse(kind: &str, x: f64, y: f64, buttons: i64) -> Call {
    let moved = kind == "mouseMoved";
    Call {
        method: "Input.dispatchMouseEvent",
        params: json!({
            "type": kind, "x": x, "y": y,
            "button": if moved { "none" } else { "left" },
            "buttons": buttons,
            "clickCount": if moved { 0 } else { 1 },
        }),
    }
}

/// `Input.insertText` is the IME commit path: one atomic insertion the page
/// sees as a normal `beforeinput`/`input` pair, which framework-controlled
/// fields listen for. A `char` event per character is slower and is dropped
/// by fields that only watch `beforeinput`.
fn insert_text(text: &str) -> Option<Call> {
    (!text.is_empty()).then(|| Call {
        method: "Input.insertText",
        params: json!({ "text": text }),
    })
}

/// `(key, code, virtual key, text)`. CDP needs a virtual key code for
/// anything not printable or the page sees a keydown with no identity. Enter
/// carries `"\r"` because implicit form submission hangs off the keypress a
/// text-bearing keydown produces; a textless Enter submits nothing.
fn named_key(key: &str) -> Option<(&'static str, i64, Option<&'static str>)> {
    Some(match key {
        "Enter" => ("Enter", 13, Some("\r")),
        "Tab" => ("Tab", 9, None),
        "Backspace" => ("Backspace", 8, None),
        "Escape" => ("Escape", 27, None),
        "ArrowUp" => ("ArrowUp", 38, None),
        "ArrowDown" => ("ArrowDown", 40, None),
        "ArrowLeft" => ("ArrowLeft", 37, None),
        "ArrowRight" => ("ArrowRight", 39, None),
        "Home" => ("Home", 36, None),
        "End" => ("End", 35, None),
        "PageUp" => ("PageUp", 33, None),
        "PageDown" => ("PageDown", 34, None),
        "Delete" => ("Delete", 46, None),
        _ => return None,
    })
}

/// A down/up pair for a named key; nothing for a key not in the table.
fn press(name: &str, modifiers: i64) -> Vec<Call> {
    let Some((key, vk, text)) = named_key(name) else {
        return Vec::new();
    };
    let mut down = json!({
        "type": if text.is_some() { "keyDown" } else { "rawKeyDown" },
        "key": key, "code": key,
        "windowsVirtualKeyCode": vk, "nativeVirtualKeyCode": vk,
        "modifiers": modifiers,
    });
    if let Some(t) = text {
        down["text"] = json!(t);
        down["unmodifiedText"] = json!(t);
    }
    let up = json!({
        "type": "keyUp",
        "key": key, "code": key,
        "windowsVirtualKeyCode": vk, "nativeVirtualKeyCode": vk,
        "modifiers": modifiers,
    });
    vec![
        Call {
            method: "Input.dispatchKeyEvent",
            params: down,
        },
        Call {
            method: "Input.dispatchKeyEvent",
            params: up,
        },
    ]
}

/// Starts one session's ordered worker. It ends when the session (the only
/// sender) is dropped.
pub(crate) fn spawn_worker(
    cdp: Arc<Cdp>,
    page: String,
    focus: Arc<Focus>,
    mut events: mpsc::UnboundedReceiver<InputEvent>,
) {
    tokio::spawn(async move {
        let mut carry: Option<InputEvent> = None;
        loop {
            let first = match carry.take() {
                Some(e) => e,
                None => match events.recv().await {
                    Some(e) => e,
                    None => break,
                },
            };
            let (event, rest) = coalesce(first, || events.try_recv().ok());
            carry = rest;

            let (calls, follow) = plan(&event);
            for call in calls {
                if let Err(e) = cdp.call(Some(&page), call.method, call.params).await {
                    debug!(method = call.method, error = %e, "efb-browser: input call failed");
                }
            }
            if let FollowUp::Probe(cause) = follow {
                // In the queue, so the value read is the one the next queued
                // edit acts on.
                focus.probe(cause).await;
                let later = focus.clone();
                tokio::spawn(async move {
                    for delay in SETTLE_PROBES {
                        tokio::time::sleep(delay).await;
                        later.probe(ProbeCause::Settle).await;
                    }
                });
            }
        }
    });
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    fn ev(v: Value) -> InputEvent {
        InputEvent::parse(&v).unwrap()
    }

    fn mv(x: f64) -> InputEvent {
        InputEvent::Mouse {
            kind: MouseKind::Move,
            x,
            y: 0.0,
        }
    }

    fn wheel(dy: f64) -> InputEvent {
        InputEvent::Wheel {
            x: 1.0,
            y: 2.0,
            dx: 0.0,
            dy,
        }
    }

    #[test]
    fn every_wire_shape_parses() {
        assert_eq!(
            ev(json!({ "type": "mouse", "kind": "down", "x": 60, "y": 25.4, "button": 0 })),
            InputEvent::Mouse {
                kind: MouseKind::Down,
                x: 60.0,
                y: 25.4
            }
        );
        assert!(ev(json!({ "type": "mouse", "kind": "hover" })).is_move());
        assert_eq!(
            ev(json!({ "type": "wheel", "x": 1, "y": 2, "dx": 0, "dy": -120 })),
            wheel(-120.0)
        );
        assert_eq!(
            ev(json!({ "type": "key", "kind": "char", "text": "é" })),
            InputEvent::Text("é".into())
        );
        assert_eq!(
            ev(json!({ "type": "key", "kind": "press", "key": "Enter" })),
            InputEvent::Key {
                key: "Enter".into(),
                modifiers: 0
            }
        );
        assert_eq!(
            ev(json!({ "type": "edit", "del": 2.0, "text": "ab" })),
            InputEvent::Edit {
                del: 2,
                text: "ab".into()
            }
        );
        assert_eq!(InputEvent::parse(&json!({ "type": "touch" })), None);
        assert_eq!(InputEvent::parse(&json!({})), None);
    }

    #[test]
    fn edit_deletes_are_bounded() {
        assert_eq!(
            ev(json!({ "type": "edit", "del": -5 })),
            InputEvent::Edit {
                del: 0,
                text: String::new()
            }
        );
        assert_eq!(
            ev(json!({ "type": "edit", "del": 1e12 })),
            InputEvent::Edit {
                del: MAX_EDIT_DELETE,
                text: String::new()
            }
        );
    }

    fn run(first: InputEvent, rest: Vec<InputEvent>) -> (InputEvent, Option<InputEvent>, usize) {
        let mut q: VecDeque<_> = rest.into();
        let (e, carry) = coalesce(first, || q.pop_front());
        (e, carry, q.len())
    }

    #[test]
    fn runs_of_moves_and_wheels_fold_but_never_across_a_press() {
        let (e, carry, left) = run(mv(1.0), vec![mv(2.0), mv(3.0)]);
        assert_eq!((e, carry, left), (mv(3.0), None, 0));

        let (e, carry, _) = run(wheel(10.0), vec![wheel(5.0), wheel(-3.0)]);
        assert_eq!(e, wheel(12.0));
        assert_eq!(carry, None);

        let down = InputEvent::Mouse {
            kind: MouseKind::Down,
            x: 9.0,
            y: 9.0,
        };
        let (e, carry, left) = run(mv(1.0), vec![mv(2.0), down.clone(), mv(4.0)]);
        assert_eq!(e, mv(2.0));
        assert_eq!(carry, Some(down.clone()));
        assert_eq!(left, 1, "nothing behind the press is consumed");

        // Moves do not fold into wheels or vice versa.
        let (e, carry, _) = run(wheel(1.0), vec![mv(1.0)]);
        assert_eq!((e, carry), (wheel(1.0), Some(mv(1.0))));

        // Anything else is dispatched alone, without draining the queue.
        let (e, carry, left) = run(down.clone(), vec![mv(1.0)]);
        assert_eq!((e, carry, left), (down, None, 1));
    }

    fn methods(calls: &[Call]) -> Vec<String> {
        calls
            .iter()
            .map(|c| {
                let kind = c.params["type"].as_str().unwrap_or("");
                format!("{}:{kind}", c.method)
            })
            .collect()
    }

    #[test]
    fn a_click_arrives_then_presses_and_the_release_probes_focus() {
        let (calls, follow) = plan(&InputEvent::Mouse {
            kind: MouseKind::Down,
            x: 5.0,
            y: 6.0,
        });
        assert_eq!(
            methods(&calls),
            [
                "Input.dispatchMouseEvent:mouseMoved",
                "Input.dispatchMouseEvent:mousePressed"
            ]
        );
        assert_eq!(calls[1].params["button"], "left");
        assert_eq!(calls[1].params["clickCount"], 1);
        assert_eq!(calls[0].params["button"], "none");
        assert_eq!(follow, FollowUp::None);

        let (calls, follow) = plan(&InputEvent::Mouse {
            kind: MouseKind::Up,
            x: 5.0,
            y: 6.0,
        });
        assert_eq!(methods(&calls), ["Input.dispatchMouseEvent:mouseReleased"]);
        assert_eq!(follow, FollowUp::Probe(ProbeCause::Announce));
    }

    #[test]
    fn enter_carries_text_so_forms_submit() {
        let (calls, follow) = plan(&InputEvent::Key {
            key: "Enter".into(),
            modifiers: 0,
        });
        assert_eq!(
            methods(&calls),
            [
                "Input.dispatchKeyEvent:keyDown",
                "Input.dispatchKeyEvent:keyUp"
            ]
        );
        assert_eq!(calls[0].params["text"], "\r");
        assert_eq!(calls[0].params["windowsVirtualKeyCode"], 13);
        assert_eq!(follow, FollowUp::Probe(ProbeCause::Announce));

        let (calls, follow) = plan(&InputEvent::Key {
            key: "ArrowDown".into(),
            modifiers: 0,
        });
        assert_eq!(calls[0].params["type"], "rawKeyDown");
        assert!(calls[0].params.get("text").is_none());
        assert_eq!(follow, FollowUp::None);

        let (calls, _) = plan(&InputEvent::Key {
            key: "F13".into(),
            modifiers: 0,
        });
        assert!(calls.is_empty(), "unknown keys send nothing");
    }

    #[test]
    fn an_edit_goes_to_the_end_deletes_then_inserts() {
        let (calls, _) = plan(&InputEvent::Edit {
            del: 2,
            text: "xy".into(),
        });
        let m = methods(&calls);
        assert_eq!(m.len(), 2 + 2 * 2 + 1);
        assert_eq!(calls[0].params["key"], "End");
        assert_eq!(calls[0].params["modifiers"], MODIFIER_CTRL);
        assert_eq!(calls[2].params["key"], "Backspace");
        assert_eq!(calls[4].params["key"], "Backspace");
        assert_eq!(calls[6].method, "Input.insertText");
        assert_eq!(calls[6].params["text"], "xy");

        let (calls, _) = plan(&InputEvent::Edit {
            del: 0,
            text: String::new(),
        });
        assert!(calls.is_empty());
        let (calls, _) = plan(&InputEvent::Text(String::new()));
        assert!(calls.is_empty());
    }
}
