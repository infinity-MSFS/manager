//! Telling the EFB when the remote page wants text (`browser.focus`).
//!
//! A probe, not a listener. The obvious design is a page script reporting
//! focus through `Runtime.addBinding`, but a binding needs the Runtime domain
//! enabled to survive navigations, and `Runtime.enable` is a well-known
//! automation tell. So focus is read, with one `Runtime.evaluate`, at the
//! moments it can change from the crew's side: after a click, after
//! Tab/Enter, after a page load.

use std::sync::{Arc, Mutex};

use infinity_efb_server::BrowserEmitter;
use serde_json::{Value, json};
use tracing::debug;

use super::cdp::Cdp;
use super::lock;

/// Longest field value sent to seed the EFB's edit buffer. Past this the
/// field is `partial` and the EFB appends and backspaces blind: a rich-text
/// editor's `innerText` can be a whole document.
const MAX_SEED_CHARS: usize = 4000;

/// Why a probe is taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProbeCause {
    /// Report the field even if unchanged: after a click (tapping the field
    /// brings the keyboard back) and after Enter/Tab (the page may have
    /// rewritten it, and the EFB's copy must be re-seeded).
    Announce,
    /// Report only a change. The crew may be typing, and re-seeding from a
    /// value read a round trip ago would fight their keystrokes.
    Settle,
}

/// What a probe result means for the EFB.
#[derive(Debug, Clone, PartialEq)]
enum Report {
    Nothing,
    /// Nothing editable is focused any more.
    Cleared,
    Field(Value),
}

/// Decides what to say given the last field reported (`last`, updated) and
/// the probe's result.
fn report(last: &mut Option<String>, found: &Value, cause: ProbeCause) -> Report {
    if found.get("editable").and_then(Value::as_bool) != Some(true) {
        return if last.take().is_some() {
            Report::Cleared
        } else {
            Report::Nothing
        };
    }
    let sig = found
        .get("sig")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let changed = last.as_deref() != Some(sig.as_str());
    *last = Some(sig);
    if !changed && cause == ProbeCause::Settle {
        return Report::Nothing;
    }
    let field = |k: &str| found.get(k).cloned().unwrap_or(Value::Null);
    Report::Field(json!({
        "editable": true,
        "value": field("value"),
        "partial": field("partial"),
        "password": field("password"),
        "multiline": field("multiline"),
        "label": field("label"),
    }))
}

pub(crate) struct Focus {
    cdp: Arc<Cdp>,
    page: String,
    session_id: String,
    emit: BrowserEmitter,
    /// Signature of the field last reported; `None` when nothing editable was.
    last: Mutex<Option<String>>,
}

impl Focus {
    pub(crate) fn new(
        cdp: Arc<Cdp>,
        page: String,
        session_id: String,
        emit: BrowserEmitter,
    ) -> Self {
        Self {
            cdp,
            page,
            session_id,
            emit,
            last: Mutex::new(None),
        }
    }

    /// The document went away under the field (a main-frame navigation).
    pub(crate) fn clear(&self) {
        if lock(&self.last).take().is_some() {
            self.send(Report::Cleared);
        }
    }

    /// Forget what was reported, so the next probe reports whatever it finds;
    /// for a client re-attaching with none of this state.
    pub(crate) fn forget(&self) {
        *lock(&self.last) = None;
    }

    pub(crate) async fn probe(&self, cause: ProbeCause) {
        let expression = format!("({PROBE_SCRIPT})({MAX_SEED_CHARS})");
        let found = match self
            .cdp
            .call(
                Some(&self.page),
                "Runtime.evaluate",
                json!({ "expression": expression, "returnByValue": true, "silent": true }),
            )
            .await
        {
            Ok(v) => v.pointer("/result/value").cloned(),
            Err(e) => {
                debug!(error = %e, "efb-browser: focus probe failed");
                return;
            }
        };
        let Some(found) = found else { return };
        let r = report(&mut lock(&self.last), &found, cause);
        self.send(r);
    }

    fn send(&self, report: Report) {
        let mut data = match report {
            Report::Nothing => return,
            Report::Cleared => json!({ "editable": false }),
            Report::Field(v) => v,
        };
        data["session"] = json!(self.session_id);
        (self.emit)("browser.focus".to_owned(), data);
    }
}

/// Evaluated in the page's main world with the seed cap; returns a plain
/// object by value.
///
/// It descends through same-origin frames to the real focused element. A
/// cross-origin frame's document is unreadable, so a field inside one reports
/// as not editable and the EFB's blind keyboard is the fallback.
///
/// It also reveals the field: the EFB's keyboard covers the lower part of
/// the viewport, so a field focused below the middle is scrolled up.
const PROBE_SCRIPT: &str = r#"function(cap){
  var el = document.activeElement, doc = document;
  try {
    while (el && (el.tagName === 'IFRAME' || el.tagName === 'FRAME') && el.contentDocument) {
      doc = el.contentDocument; el = doc.activeElement;
    }
  } catch (e) {}
  if (!el) return { editable: false };
  var tag = el.tagName, type = '', text = false;
  if (el.isContentEditable) text = true;
  else if (tag === 'TEXTAREA') text = true;
  else if (tag === 'INPUT') {
    type = (el.getAttribute('type') || 'text').toLowerCase();
    text = ['text','search','email','url','tel','password','number'].indexOf(type) !== -1;
  }
  if (!text || el.disabled || el.readOnly) return { editable: false };

  if (doc === document) {
    try {
      var r = el.getBoundingClientRect();
      if (r.top < 0 || r.bottom > window.innerHeight * 0.5) {
        el.scrollIntoView({ block: 'start' });
        window.scrollBy(0, -Math.round(window.innerHeight * 0.15));
      }
    } catch (e) {}
  }

  var value = el.isContentEditable ? String(el.innerText || '') : String(el.value == null ? '' : el.value);
  var password = type === 'password';
  var partial = value.length > cap;
  var label = el.getAttribute('aria-label') || el.getAttribute('placeholder') ||
    el.getAttribute('title') || el.getAttribute('name') || '';
  return {
    editable: true,
    sig: [tag, type, el.id || '', el.getAttribute('name') || '', label, doc === document ? 'top' : 'frame'].join('|'),
    value: (password || partial) ? '' : value,
    partial: partial || (password && value.length > 0),
    password: password,
    multiline: tag === 'TEXTAREA' || !!el.isContentEditable,
    label: String(label).slice(0, 80)
  };
}"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn field(sig: &str, value: &str) -> Value {
        json!({
            "editable": true, "sig": sig, "value": value, "partial": false,
            "password": false, "multiline": false, "label": "Search",
        })
    }

    #[test]
    fn a_new_field_is_reported_with_its_seed() {
        let mut last = None;
        let Report::Field(v) = report(&mut last, &field("INPUT|q", "hello"), ProbeCause::Settle)
        else {
            unreachable!("expected a field report");
        };
        assert_eq!(v["value"], "hello");
        assert_eq!(v["label"], "Search");
        assert!(
            v.get("sig").is_none(),
            "the signature is ours, not the wire's"
        );
        assert_eq!(last.as_deref(), Some("INPUT|q"));
    }

    #[test]
    fn settle_stays_quiet_on_the_same_field_but_announce_does_not() {
        let mut last = Some("INPUT|q".to_owned());
        assert_eq!(
            report(&mut last, &field("INPUT|q", "typed"), ProbeCause::Settle),
            Report::Nothing
        );
        assert!(matches!(
            report(&mut last, &field("INPUT|q", "typed"), ProbeCause::Announce),
            Report::Field(_)
        ));
        assert!(matches!(
            report(&mut last, &field("TEXTAREA|m", ""), ProbeCause::Settle),
            Report::Field(_)
        ));
    }

    #[test]
    fn losing_focus_is_said_once() {
        let mut last = Some("INPUT|q".to_owned());
        let none = json!({ "editable": false });
        assert_eq!(
            report(&mut last, &none, ProbeCause::Settle),
            Report::Cleared
        );
        assert_eq!(
            report(&mut last, &none, ProbeCause::Announce),
            Report::Nothing
        );
        assert_eq!(
            report(&mut last, &json!(null), ProbeCause::Settle),
            Report::Nothing
        );
    }
}
