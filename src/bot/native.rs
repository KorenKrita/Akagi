//! Built-in, in-process bot runner backed by the pure-Rust `native_bot`
//! crate (a small behavior-cloned CNN run via candle — no Python, no
//! libriichi, no subprocess).
//!
//! Unlike [`crate::bot::runner::SubprocessBot`], this runner keeps a live
//! `native_bot::Engine` in-process: each `react()` feeds the batch through the
//! engine's riichienv-core game state and, at our decision points, runs the net
//! to pick a legal action. Bundled model weights are embedded in the binary, so
//! there is no venv, no `uv sync`, and nothing to install.
//!
//! Two reserved bot names select it: [`NATIVE_4P`] (yonma) and [`NATIVE_3P`]
//! (sanma). `BotManager::spawn_runner` recognises them and constructs this
//! runner directly, bypassing the `bot.py`/registry path.

use crate::bot::api::{ApiClient, Candidate};
use crate::bot::runner::BotRunner;
use crate::bot::types::BotResponse;
use crate::config::NativeApiConfig;
use crate::event_bus::NotifyBus;
use crate::game_state::convert;
use crate::schema::{MjaiEvent, Notification};
use anyhow::Result;
use async_trait::async_trait;
use native_bot::engine::{BotAction, Decision, Engine};
use serde_json::Value;
use tracing::warn;

/// Reserved name for the built-in 4-player bot.
pub const NATIVE_4P: &str = "akagi-native";
/// Reserved name for the built-in 3-player (sanma) bot.
pub const NATIVE_3P: &str = "akagi-native3p";

/// Notification id for the online-API health toasts (degrade / recover). A
/// stable id lets the recovery toast replace the outage one, and the frontend
/// keys its persistent "online API" status indicator off the same channel.
pub const NATIVE_API_HEALTH_ID: &str = "native-api-health";

/// Whether `name` selects the built-in native bot (either mode).
pub fn is_native(name: &str) -> bool {
    name == NATIVE_4P || name == NATIVE_3P
}

/// Display label for a reserved native-bot name, for the Bots UI.
pub fn display_name(name: &str) -> Option<&'static str> {
    match name {
        NATIVE_4P => Some("Akagi (built-in, 4p)"),
        NATIVE_3P => Some("Akagi (built-in, 3p)"),
        _ => None,
    }
}

/// Construct the built-in bot runner for a game of `num_players` seated at
/// `actor_id`. Picks the remote-inference-API path when
/// [`NativeApiConfig::is_active`] (opted in with a server URL + key), otherwise
/// the fully offline local model. The API path still loads the local model as
/// a fallback, so this can never be "less available" than the local one.
pub fn build(
    actor_id: u8,
    num_players: u8,
    api: &NativeApiConfig,
    notify_tx: NotifyBus,
) -> Result<Box<dyn BotRunner>> {
    if api.is_active() {
        Ok(Box::new(ApiNativeBot::new(
            actor_id,
            num_players,
            api,
            notify_tx,
        )?))
    } else {
        Ok(Box::new(NativeBot::new(actor_id, num_players)?))
    }
}

pub struct NativeBot {
    engine: Engine,
    actor_id: u8,
}

impl NativeBot {
    /// Build the in-process bot for a game of `num_players` with our seat at
    /// `actor_id`, loading the bundled default weights for that mode.
    pub fn new(actor_id: u8, num_players: u8) -> Result<Self> {
        let engine = native_bot::defaults::engine(num_players, actor_id)?;
        Ok(Self { engine, actor_id })
    }
}

#[async_trait]
impl BotRunner for NativeBot {
    async fn react(&mut self, events: &[MjaiEvent]) -> Result<BotResponse> {
        for ev in events {
            // Keep our seat current if a start_game tags a (possibly new) seat.
            if let MjaiEvent::StartGame { id: Some(seat), .. } = ev {
                self.actor_id = *seat;
                self.engine.set_seat(*seat);
            }
            if let Some(ri) = convert::to_riichienv(ev)? {
                self.engine.feed(ri);
            }
        }

        let (action, meta) = match self.engine.decide()? {
            Some(d) => {
                let meta = build_show_meta(&d.candidates);
                (bot_action_to_mjai(d.action, self.actor_id), meta)
            }
            None => (MjaiEvent::None, None),
        };
        Ok(BotResponse { action, meta })
    }

    async fn reset(&mut self) -> Result<()> {
        self.engine.reset();
        Ok(())
    }
}

/// Built-in bot that proxies each decision to the remote inference API
/// (`POST /v3/react`, see `native_bot/API.md`) instead of running the embedded
/// model.
///
/// It still keeps a local [`Engine`] for two reasons:
/// - **Gating** — the API's rate limits are low, and calling on every opponent
///   discard "just in case" roughly triples request count. So we run the local
///   model's cheap legal-action check first and only hit the network when our
///   seat genuinely has a move to make.
/// - **Fallback** — if the server is unreachable, rate-limited, or the key is
///   invalid, we play the local model's action so a live game never stalls.
///
/// The remote service is stateless: every call re-uploads the current kyoku's
/// mjai stream (from our seat's censored perspective). We accumulate that
/// stream in [`ApiNativeBot::stream`] and shape it for the API in
/// [`build_api_events`].
pub struct ApiNativeBot {
    engine: Engine,
    client: ApiClient,
    /// Model id to request; empty ⇒ let the server pick its game default.
    model: String,
    seat: u8,
    num_players: u8,
    /// Current-kyoku mjai stream (Akagi schema). Reset on each `start_kyoku`
    /// but keeps the leading `start_game` the API requires as `events[0]`.
    stream: Vec<MjaiEvent>,
    /// Toast channel — used to alert the user when the online API becomes
    /// unavailable (and again when it recovers).
    notify_tx: NotifyBus,
    /// Whether the last API request succeeded. Toasts fire only on a change of
    /// this flag, so a persistently-down server doesn't spam a toast per turn.
    healthy: bool,
}

impl ApiNativeBot {
    /// Build the API-backed bot for a game of `num_players` seated at
    /// `actor_id`, using the server URL / key / model from `api`. The local
    /// model is loaded too (gating + fallback).
    pub fn new(
        actor_id: u8,
        num_players: u8,
        api: &NativeApiConfig,
        notify_tx: NotifyBus,
    ) -> Result<Self> {
        let engine = native_bot::defaults::engine(num_players, actor_id)?;
        let client = ApiClient::new(&api.base_url, &api.key)?;
        Ok(Self {
            engine,
            client,
            model: api.model_for(num_players).trim().to_string(),
            seat: actor_id,
            num_players,
            stream: Vec::new(),
            notify_tx,
            healthy: true,
        })
    }

    /// Record the outcome of an API request and, **only on a health change**,
    /// toast the user: a warning when the server first becomes unreachable
    /// (the bot silently falls back to the local model), and a success when it
    /// recovers. `ok` = the HTTP request itself succeeded (server reachable).
    fn record_health(&mut self, ok: bool, err: Option<&str>) {
        if ok == self.healthy {
            return; // no transition
        }
        self.healthy = ok;
        let note = if ok {
            Notification::success("Online inference restored")
                .body("The built-in bot is using the online API again.")
                .id(NATIVE_API_HEALTH_ID)
        } else {
            let body = match err {
                Some(e) => format!("Falling back to the built-in local model. ({e})"),
                None => "Falling back to the built-in local model.".to_string(),
            };
            Notification::warn("Online inference unavailable")
                .body(body)
                .id(NATIVE_API_HEALTH_ID)
        };
        let _ = self.notify_tx.send(note);
    }

    /// Append one event to the current-kyoku stream, resetting on boundaries so
    /// each request stays small (a kyoku is well under the API's 512-event cap).
    fn accumulate(&mut self, ev: &MjaiEvent) {
        match ev {
            MjaiEvent::StartGame { .. } => {
                self.stream.clear();
                self.stream.push(ev.clone());
            }
            MjaiEvent::StartKyoku { .. } => {
                // Keep the leading start_game (required as events[0]); drop the
                // previous kyoku's tail.
                let start_game = self
                    .stream
                    .first()
                    .filter(|e| matches!(e, MjaiEvent::StartGame { .. }))
                    .cloned();
                self.stream.clear();
                if let Some(sg) = start_game {
                    self.stream.push(sg);
                }
                self.stream.push(ev.clone());
            }
            _ => self.stream.push(ev.clone()),
        }
    }

    /// The `model` argument for a react call: `None` when unset so the server
    /// falls back to its game default.
    fn model_arg(&self) -> Option<&str> {
        if self.model.is_empty() {
            None
        } else {
            Some(self.model.as_str())
        }
    }

    /// Query the server for the move at the current decision point. Falls back
    /// to `local` (the local model's decision) on any error or a `null`
    /// reaction so a live game never stalls.
    async fn remote_decision(&mut self, local: &Decision) -> (MjaiEvent, Option<Value>) {
        let events = build_api_events(&self.stream, self.seat, self.num_players);
        let result = self.client.react(self.model_arg(), self.seat, events).await;
        // A successful HTTP round-trip (even a `null` reaction) means the server
        // is reachable; only a transport/HTTP error counts as "API unavailable".
        match result {
            Ok(resp) => {
                self.record_health(true, None);
                match resp.reaction {
                    Some(reaction) => match self.resolve_reaction(reaction, &resp.candidates).await
                    {
                        Some(pair) => pair,
                        None => local_reply(local, self.seat),
                    },
                    None => {
                        // Server sees no legal action though the local gate found
                        // one — a stream mismatch. Play the local move rather than
                        // silently pass a real turn.
                        warn!(
                            "native API: null reaction though local has a legal move; using local"
                        );
                        local_reply(local, self.seat)
                    }
                }
            }
            Err(e) => {
                let msg = format!("{e:#}");
                warn!("native API react failed ({msg}); falling back to local model");
                self.record_health(false, Some(&msg));
                local_reply(local, self.seat)
            }
        }
    }

    /// Turn a raw mjai reaction from the server into the event to play, filling
    /// the reach two-step (declare → discard) when the reaction is a bare
    /// `reach`. `None` if the reaction can't be parsed (caller falls back).
    async fn resolve_reaction(
        &mut self,
        reaction: Value,
        candidates: &[Candidate],
    ) -> Option<(MjaiEvent, Option<Value>)> {
        let mut ev: MjaiEvent = serde_json::from_value(reaction).ok()?;
        set_actor(&mut ev, self.seat);
        // Majsoul fuses declaring riichi + discarding into one click, and
        // autoplay stalls unless the reach event names the discard. The server
        // returns a bare reach (the discard is a second decision), so resolve it
        // now — ask the server again with the reach appended, or fall back to
        // the local model's predicted riichi discard.
        if let MjaiEvent::Reach { pai: None, .. } = &ev {
            match self.reach_discard().await {
                Some(discard) => {
                    ev = MjaiEvent::Reach {
                        actor: self.seat,
                        pai: Some(discard),
                    };
                }
                // Couldn't resolve the riichi discard (the follow-up call failed
                // AND the local model found no riichi-legal discard). A reach
                // without the discard tile stalls autoplay, so decline the API
                // reaction entirely and let the caller fall back to the full
                // local decision.
                None => return None,
            }
        }
        let meta = build_show_meta_mjai(&ev, candidates);
        Some((ev, meta))
    }

    /// Resolve the post-reach discard: append the reach to the stream and
    /// re-query the server. Falls back to the local model's predicted riichi
    /// discard on any error.
    async fn reach_discard(&mut self) -> Option<String> {
        let mut events = build_api_events(&self.stream, self.seat, self.num_players);
        events.push(serde_json::json!({ "type": "reach", "actor": self.seat }));
        match self.client.react(self.model_arg(), self.seat, events).await {
            Ok(resp) => {
                if let Some(reaction) = resp.reaction {
                    if let Ok(MjaiEvent::Dahai { pai, .. }) =
                        serde_json::from_value::<MjaiEvent>(reaction)
                    {
                        return Some(pai);
                    }
                }
                self.engine.reach_discard()
            }
            Err(e) => {
                warn!("native API reach-discard follow-up failed ({e:#}); using local");
                self.engine.reach_discard()
            }
        }
    }
}

#[async_trait]
impl BotRunner for ApiNativeBot {
    async fn react(&mut self, events: &[MjaiEvent]) -> Result<BotResponse> {
        for ev in events {
            if let MjaiEvent::StartGame { id: Some(seat), .. } = ev {
                self.seat = *seat;
                self.engine.set_seat(*seat);
            }
            self.accumulate(ev);
            if let Some(ri) = convert::to_riichienv(ev)? {
                self.engine.feed(ri);
            }
        }

        // Local gate: no legal action ⇒ don't spend an API call (and don't
        // pester the server with the opponent-discard windows we can't act on).
        let local = match self.engine.decide()? {
            Some(d) => d,
            None => {
                return Ok(BotResponse {
                    action: MjaiEvent::None,
                    meta: None,
                })
            }
        };

        let (action, meta) = self.remote_decision(&local).await;
        Ok(BotResponse { action, meta })
    }

    async fn reset(&mut self) -> Result<()> {
        self.engine.reset();
        self.stream.clear();
        Ok(())
    }
}

/// Build the reply pair (mjai action + HUD card) from the local model's
/// decision — the fallback when the API path is unavailable.
fn local_reply(local: &Decision, seat: u8) -> (MjaiEvent, Option<Value>) {
    let meta = build_show_meta(&local.candidates);
    (bot_action_to_mjai(local.action.clone(), seat), meta)
}

/// Shape the accumulated Akagi mjai stream into the API's expected JSON:
/// censor other seats' hidden info to `?`, pad 3p `start_game`/`start_kyoku`
/// arrays to length 4, strip player-count / predicted-reach extensions.
fn build_api_events(stream: &[MjaiEvent], seat: u8, num_players: u8) -> Vec<Value> {
    stream
        .iter()
        .map(|ev| to_api_event(ev, seat, num_players))
        .collect()
}

fn to_api_event(ev: &MjaiEvent, seat: u8, num_players: u8) -> Value {
    use serde_json::json;
    let three_p = num_players == 3;
    match ev {
        MjaiEvent::StartGame { names, .. } => {
            let mut names = names.clone();
            if three_p {
                while names.len() < 4 {
                    names.push(String::new());
                }
            }
            json!({ "type": "start_game", "names": names })
        }
        MjaiEvent::StartKyoku {
            bakaze,
            dora_marker,
            kyoku,
            honba,
            kyotaku,
            oya,
            scores,
            tehais,
            ..
        } => {
            let mut scores = scores.clone();
            // Reveal only our own hand; every other seat is 13 "?".
            let mut tehais: Vec<Vec<String>> = tehais
                .iter()
                .enumerate()
                .map(|(i, hand)| {
                    if i as u8 == seat {
                        hand.clone()
                    } else {
                        hidden_hand()
                    }
                })
                .collect();
            if three_p {
                // Pad to the length-4 shape the API requires for 3p, with a
                // phantom 4th seat (score 0, 13 "?").
                while scores.len() < 4 {
                    scores.push(0);
                }
                while tehais.len() < 4 {
                    tehais.push(hidden_hand());
                }
            }
            json!({
                "type": "start_kyoku",
                "bakaze": bakaze,
                "dora_marker": dora_marker,
                "kyoku": kyoku,
                "honba": honba,
                "kyotaku": kyotaku,
                "oya": oya,
                "scores": scores,
                "tehais": tehais,
            })
        }
        MjaiEvent::Tsumo { actor, pai } => {
            // We never see another seat's draw.
            let pai = if *actor == seat {
                pai.clone()
            } else {
                "?".to_string()
            };
            json!({ "type": "tsumo", "actor": actor, "pai": pai })
        }
        MjaiEvent::Reach { actor, .. } => {
            // Strip the non-spec predicted `pai`; the API wants a bare reach.
            json!({ "type": "reach", "actor": actor })
        }
        // Everything else is public and already API-shaped.
        other => serde_json::to_value(other).unwrap_or_else(|_| json!({ "type": "none" })),
    }
}

fn hidden_hand() -> Vec<String> {
    vec!["?".to_string(); 13]
}

/// Force an mjai reaction's `actor` to our seat (the server should already set
/// it, but be defensive).
fn set_actor(ev: &mut MjaiEvent, seat: u8) {
    use MjaiEvent as E;
    match ev {
        E::Tsumo { actor, .. }
        | E::Dahai { actor, .. }
        | E::Reach { actor, .. }
        | E::Pon { actor, .. }
        | E::Chi { actor, .. }
        | E::Daiminkan { actor, .. }
        | E::Ankan { actor, .. }
        | E::Kakan { actor, .. }
        | E::Hora { actor, .. }
        | E::Kita { actor, .. } => *actor = seat,
        _ => {}
    }
}

/// Prepend `lead` to `rest`, forming a meld's tile list (called tile first).
fn with_lead(lead: &str, rest: &[String]) -> Vec<String> {
    std::iter::once(lead.to_string())
        .chain(rest.iter().cloned())
        .collect()
}

/// HUD "Bot Show" card for the API path: the first row is the exact chosen move
/// (precise tiles), the rest are the server's runner-up **candidate** labels,
/// each with its probability — so the card shows the model's top-N.
fn build_show_meta_mjai(ev: &MjaiEvent, candidates: &[Candidate]) -> Option<serde_json::Value> {
    let mut items: Vec<Value> = Vec::new();
    // Row 0: the exact reaction (precise tiles), prob from the top candidate.
    if let Some((label, pais)) = label_pais_mjai(ev) {
        let prob = candidates.first().map(|c| c.prob);
        items.push(make_show_item(label, &pais, prob));
    }
    // Remaining rows: the coarse candidate labels. `candidates[0]` is the chosen
    // move, already rendered above with exact tiles, so start from index 1.
    for c in candidates.iter().skip(1) {
        if let Some((label, pais)) = label_pais_candidate(&c.action) {
            items.push(make_show_item(label, &pais, Some(c.prob)));
        }
    }
    wrap_show(items)
}

/// Label + tiles for a resolved mjai reaction (the exact chosen move).
fn label_pais_mjai(ev: &MjaiEvent) -> Option<(&'static str, Vec<String>)> {
    let out = match ev {
        MjaiEvent::Dahai { pai, .. } => ("Discard", vec![pai.clone()]),
        MjaiEvent::Reach { pai, .. } => ("Riichi", pai.clone().into_iter().collect()),
        MjaiEvent::Pon { pai, consumed, .. } => ("Pon", with_lead(pai, consumed)),
        MjaiEvent::Chi { pai, consumed, .. } => ("Chi", with_lead(pai, consumed)),
        MjaiEvent::Daiminkan { pai, consumed, .. } => ("Kan", with_lead(pai, consumed)),
        MjaiEvent::Ankan { consumed, .. } => ("Ankan", consumed.to_vec()),
        MjaiEvent::Kakan { pai, consumed, .. } => ("Kakan", with_lead(pai, consumed)),
        MjaiEvent::Hora { .. } => ("Hora", vec![]),
        MjaiEvent::Ryukyoku { .. } => ("Ryukyoku", vec![]),
        MjaiEvent::Kita { .. } => ("Kita", vec!["N".into()]),
        // Passing IS the chosen move on a call window (pon/chi/kan/ron
        // offered, model declines) — show it, or the card would render only
        // the runner-ups and read as recommending the call it just declined.
        MjaiEvent::None => ("None", vec![]),
        _ => return None,
    };
    Some(out)
}

/// Label + tiles for a coarse candidate action string (see API §8), e.g.
/// `dahai:5p`, `reach`, `chi_mid`, `pon`, `kan`, `nukidora`. `dahai:<pai>`
/// carries the exact tile; the rest are move-type labels only.
fn label_pais_candidate(action: &str) -> Option<(&'static str, Vec<String>)> {
    if let Some(pai) = action.strip_prefix("dahai:") {
        return Some(("Discard", vec![pai.to_string()]));
    }
    let out = match action {
        "reach" => ("Riichi", vec![]),
        "pon" => ("Pon", vec![]),
        "chi_low" | "chi_mid" | "chi_high" => ("Chi", vec![]),
        "kan" => ("Kan", vec![]),
        "hora" => ("Hora", vec![]),
        "ryukyoku" => ("Ryukyoku", vec![]),
        "nukidora" => ("Kita", vec!["N".into()]),
        // On a call window the pass option is half the decision (e.g. pon 55%
        // vs none 45%) — a real ranked row, not noise.
        "none" => ("None", vec![]),
        // Unknown future labels are not shown as a row.
        _ => return None,
    };
    Some(out)
}

/// Build one `show.items` entry: a label, optional tiles, optional probability
/// (rendered as a whole-percent string).
fn make_show_item(label: &str, pais: &[String], prob: Option<f64>) -> Value {
    use serde_json::json;
    let mut item = serde_json::Map::new();
    item.insert("label".into(), json!(label));
    if pais.iter().any(|p| !p.is_empty()) {
        item.insert("pais".into(), json!(pais));
    }
    if let Some(p) = prob {
        item.insert("value".into(), json!(format!("{:.0}%", p * 100.0)));
    }
    Value::Object(item)
}

/// Wrap `items` in the `{ "show": { title, items } }` envelope, or `None` when
/// there is nothing to show.
fn wrap_show(items: Vec<Value>) -> Option<Value> {
    use serde_json::json;
    if items.is_empty() {
        return None;
    }
    Some(json!({ "show": { "title": "Akagi", "items": items } }))
}

fn take_n<const N: usize>(v: Vec<String>) -> [String; N] {
    let mut it = v.into_iter();
    std::array::from_fn(|_| it.next().unwrap_or_default())
}

/// Build the HUD "Bot Show" recommendation card (`meta.show`) from the ranked
/// candidates, so the built-in bot surfaces its **top-N** suggestions (each with
/// its policy probability) like other bots. `None` when the top choice is a pass
/// (nothing to recommend — the previous card stays).
fn build_show_meta(candidates: &[(BotAction, f32)]) -> Option<serde_json::Value> {
    // A leading pass means "no action this turn": leave the previous card up.
    if matches!(candidates.first(), None | Some((BotAction::Pass, _))) {
        return None;
    }
    let items: Vec<Value> = candidates
        .iter()
        .filter_map(|(a, p)| {
            label_pais_bot_action(a)
                .map(|(label, pais)| make_show_item(label, &pais, Some(*p as f64)))
        })
        .collect();
    wrap_show(items)
}

/// Label + tiles for one bot action, or `None` for a pass (not shown as a row).
fn label_pais_bot_action(a: &BotAction) -> Option<(&'static str, Vec<String>)> {
    let out = match a {
        BotAction::Dahai { pai, .. } => ("Discard", vec![pai.clone()]),
        BotAction::Reach { pai } => (
            "Riichi",
            if pai.is_empty() {
                vec![]
            } else {
                vec![pai.clone()]
            },
        ),
        BotAction::Pon { pai, consumed, .. } => ("Pon", with_lead(pai, consumed)),
        BotAction::Chi { pai, consumed, .. } => ("Chi", with_lead(pai, consumed)),
        BotAction::Daiminkan { pai, consumed, .. } => ("Kan", with_lead(pai, consumed)),
        BotAction::Ankan { consumed } => ("Ankan", consumed.clone()),
        BotAction::Kakan { pai, consumed } => ("Kakan", with_lead(pai, consumed)),
        BotAction::Hora { .. } => ("Hora", vec![]),
        BotAction::Kyushu => ("Ryukyoku", vec![]),
        BotAction::Kita => ("Kita", vec!["N".into()]),
        BotAction::Pass => return None,
    };
    Some(out)
}

/// Map a schema-agnostic [`BotAction`] to Akagi's `MjaiEvent` reply.
fn bot_action_to_mjai(a: BotAction, actor: u8) -> MjaiEvent {
    match a {
        BotAction::Dahai { pai, tsumogiri } => MjaiEvent::Dahai {
            actor,
            pai,
            tsumogiri,
        },
        BotAction::Reach { pai } => MjaiEvent::Reach {
            actor,
            pai: Some(pai),
        },
        BotAction::Pon {
            target,
            pai,
            consumed,
        } => MjaiEvent::Pon {
            actor,
            target,
            pai,
            consumed: take_n(consumed),
        },
        BotAction::Chi {
            target,
            pai,
            consumed,
        } => MjaiEvent::Chi {
            actor,
            target,
            pai,
            consumed: take_n(consumed),
        },
        BotAction::Daiminkan {
            target,
            pai,
            consumed,
        } => MjaiEvent::Daiminkan {
            actor,
            target,
            pai,
            consumed: take_n(consumed),
        },
        BotAction::Ankan { consumed } => MjaiEvent::Ankan {
            actor,
            consumed: take_n(consumed),
        },
        BotAction::Kakan { pai, consumed } => MjaiEvent::Kakan {
            actor,
            pai,
            consumed: take_n(consumed),
        },
        BotAction::Hora { target } => MjaiEvent::Hora {
            actor,
            target,
            deltas: None,
            ura_markers: None,
        },
        BotAction::Kyushu => MjaiEvent::Ryukyoku { deltas: None },
        BotAction::Kita => MjaiEvent::Kita {
            actor,
            pai: Some("N".into()),
        },
        BotAction::Pass => MjaiEvent::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn start_game_4p(seat: u8) -> MjaiEvent {
        MjaiEvent::StartGame {
            names: vec!["a".into(), "b".into(), "c".into(), "d".into()],
            kyoku_first: None,
            aka_flag: None,
            id: Some(seat),
            num_players: 4,
        }
    }

    fn start_kyoku_4p() -> MjaiEvent {
        let hand: Vec<String> = [
            "1m", "2m", "3m", "4m", "5m", "6m", "7m", "8m", "9m", "1p", "2p", "3p", "4p",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        MjaiEvent::StartKyoku {
            bakaze: "E".into(),
            dora_marker: "2m".into(),
            kyoku: 1,
            honba: 0,
            kyotaku: 0,
            oya: 0,
            scores: vec![25000, 25000, 25000, 25000],
            tehais: vec![hand.clone(), hand.clone(), hand.clone(), hand],
            num_players: 4,
        }
    }

    #[tokio::test]
    async fn native_bot_returns_legal_discard_on_own_tsumo() {
        let mut bot = NativeBot::new(0, 4).unwrap();
        // Feed the opening up to our first draw in one batch (as the manager would).
        let resp = bot
            .react(&[
                start_game_4p(0),
                start_kyoku_4p(),
                MjaiEvent::Tsumo {
                    actor: 0,
                    pai: "5p".into(),
                },
            ])
            .await
            .unwrap();
        // On our own tsumo we must act — a discard (or riichi/kan/hora), never None.
        assert!(
            !matches!(resp.action, MjaiEvent::None),
            "bot must act on its own tsumo, got None"
        );
        match resp.action {
            MjaiEvent::Dahai { actor, .. } | MjaiEvent::Reach { actor, .. } => {
                assert_eq!(actor, 0)
            }
            MjaiEvent::Ankan { .. } | MjaiEvent::Kakan { .. } | MjaiEvent::Hora { .. } => {}
            other => panic!("unexpected reply on own turn: {other:?}"),
        }
    }

    #[tokio::test]
    async fn native_bot_passes_when_not_its_turn() {
        let mut bot = NativeBot::new(0, 4).unwrap();
        // Opponent (seat 1) draws and discards; we (seat 0) usually can't act.
        let resp = bot
            .react(&[
                start_game_4p(0),
                start_kyoku_4p(),
                MjaiEvent::Tsumo {
                    actor: 1,
                    pai: "9s".into(),
                },
                MjaiEvent::Dahai {
                    actor: 1,
                    pai: "9s".into(),
                    tsumogiri: true,
                },
            ])
            .await
            .unwrap();
        // Either None (nothing to do) or a legal call — must not be one of our
        // own-turn-only actions.
        assert!(
            !matches!(
                resp.action,
                MjaiEvent::Dahai { .. } | MjaiEvent::Reach { .. }
            ),
            "must not discard on someone else's turn: {:?}",
            resp.action
        );
    }

    // ---------- API-backed native bot: request shaping ----------

    fn hand13() -> Vec<String> {
        (0..13).map(|i| format!("{}p", (i % 9) + 1)).collect()
    }

    #[test]
    fn api_event_censors_other_seats_and_strips_num_players() {
        let sk = MjaiEvent::StartKyoku {
            bakaze: "E".into(),
            dora_marker: "2m".into(),
            kyoku: 1,
            honba: 0,
            kyotaku: 0,
            oya: 0,
            scores: vec![25000, 25000, 25000, 25000],
            tehais: vec![hand13(), hand13(), hand13(), hand13()],
            num_players: 4,
        };
        let v = to_api_event(&sk, 2, 4);
        assert_eq!(v["type"], "start_kyoku");
        assert!(
            v.get("num_players").is_none(),
            "num_players must be stripped for the API"
        );
        let tehais = v["tehais"].as_array().unwrap();
        assert_eq!(tehais.len(), 4);
        // Our own seat (2) is revealed; all others are 13 "?".
        assert_ne!(tehais[2][0], "?");
        for i in [0usize, 1, 3] {
            let hand = tehais[i].as_array().unwrap();
            assert_eq!(hand.len(), 13);
            assert!(hand.iter().all(|t| t == "?"), "seat {i} must be hidden");
        }

        // Draws: ours revealed, others censored.
        let mine = to_api_event(
            &MjaiEvent::Tsumo {
                actor: 2,
                pai: "5p".into(),
            },
            2,
            4,
        );
        assert_eq!(mine["pai"], "5p");
        let theirs = to_api_event(
            &MjaiEvent::Tsumo {
                actor: 1,
                pai: "5p".into(),
            },
            2,
            4,
        );
        assert_eq!(theirs["pai"], "?");
    }

    #[test]
    fn api_event_pads_3p_arrays_to_length_four() {
        let sg = MjaiEvent::StartGame {
            names: vec!["a".into(), "b".into(), "c".into()],
            kyoku_first: None,
            aka_flag: None,
            id: Some(0),
            num_players: 3,
        };
        let v = to_api_event(&sg, 0, 3);
        assert_eq!(v["names"].as_array().unwrap().len(), 4);
        assert_eq!(v["names"][3], "");
        assert!(v.get("num_players").is_none());
        assert!(v.get("id").is_none(), "start_game reduced to type + names");

        let sk = MjaiEvent::StartKyoku {
            bakaze: "E".into(),
            dora_marker: "1s".into(),
            kyoku: 1,
            honba: 0,
            kyotaku: 0,
            oya: 0,
            scores: vec![35000, 35000, 35000],
            tehais: vec![hand13(), hand13(), hand13()],
            num_players: 3,
        };
        let v = to_api_event(&sk, 0, 3);
        assert_eq!(v["scores"].as_array().unwrap().len(), 4);
        assert_eq!(v["scores"][3], 0);
        let tehais = v["tehais"].as_array().unwrap();
        assert_eq!(tehais.len(), 4);
        // Phantom 4th seat is a hidden hand.
        assert_eq!(tehais[3].as_array().unwrap().len(), 13);
        assert_eq!(tehais[3][0], "?");
        // Real other seat (1) still censored.
        assert!(tehais[1].as_array().unwrap().iter().all(|t| t == "?"));
    }

    #[test]
    fn api_event_strips_predicted_reach_pai() {
        let r = MjaiEvent::Reach {
            actor: 0,
            pai: Some("5p".into()),
        };
        let v = to_api_event(&r, 0, 4);
        assert_eq!(v["type"], "reach");
        assert_eq!(v["actor"], 0);
        assert!(
            v.get("pai").is_none(),
            "predicted reach pai must be stripped"
        );
    }

    #[test]
    fn show_meta_from_reaction_carries_label_pai_and_prob() {
        let ev = MjaiEvent::Dahai {
            actor: 0,
            pai: "W".into(),
            tsumogiri: false,
        };
        let cands = vec![Candidate {
            action: "dahai:W".into(),
            prob: 0.83,
        }];
        let meta = build_show_meta_mjai(&ev, &cands).unwrap();
        let item = &meta["show"]["items"][0];
        assert_eq!(item["label"], "Discard");
        assert_eq!(item["pais"][0], "W");
        assert_eq!(item["value"], "83%");
    }

    #[test]
    fn local_show_meta_lists_top_candidates_with_probs() {
        let cands = vec![
            (
                BotAction::Dahai {
                    pai: "1m".into(),
                    tsumogiri: false,
                },
                0.6f32,
            ),
            (BotAction::Reach { pai: "2p".into() }, 0.3f32),
            (
                BotAction::Dahai {
                    pai: "9s".into(),
                    tsumogiri: true,
                },
                0.1f32,
            ),
        ];
        let meta = build_show_meta(&cands).unwrap();
        let items = meta["show"]["items"].as_array().unwrap();
        assert_eq!(items.len(), 3, "should surface all three candidates");
        assert_eq!(items[0]["label"], "Discard");
        assert_eq!(items[0]["pais"][0], "1m");
        assert_eq!(items[0]["value"], "60%");
        assert_eq!(items[1]["label"], "Riichi");
        assert_eq!(items[1]["pais"][0], "2p");
        assert_eq!(items[1]["value"], "30%");
        assert_eq!(items[2]["pais"][0], "9s");
        assert_eq!(items[2]["value"], "10%");
    }

    #[test]
    fn local_show_meta_none_when_top_is_pass() {
        let cands = vec![
            (BotAction::Pass, 0.9f32),
            (
                BotAction::Pon {
                    target: 0,
                    pai: "1m".into(),
                    consumed: vec!["1m".into(), "1m".into()],
                },
                0.1f32,
            ),
        ];
        assert!(
            build_show_meta(&cands).is_none(),
            "a leading pass must not replace the card"
        );
    }

    #[test]
    fn api_show_meta_lists_candidates_with_probs() {
        let ev = MjaiEvent::Dahai {
            actor: 0,
            pai: "5p".into(),
            tsumogiri: false,
        };
        let cands = vec![
            Candidate {
                action: "dahai:5p".into(),
                prob: 0.7,
            },
            Candidate {
                action: "reach".into(),
                prob: 0.2,
            },
            Candidate {
                action: "dahai:9m".into(),
                prob: 0.1,
            },
        ];
        let meta = build_show_meta_mjai(&ev, &cands).unwrap();
        let items = meta["show"]["items"].as_array().unwrap();
        assert_eq!(items.len(), 3);
        // Row 0 = exact chosen move; rows 1..= coarse candidate labels.
        assert_eq!(items[0]["label"], "Discard");
        assert_eq!(items[0]["pais"][0], "5p");
        assert_eq!(items[0]["value"], "70%");
        assert_eq!(items[1]["label"], "Riichi");
        assert_eq!(items[1]["value"], "20%");
        assert_eq!(items[2]["label"], "Discard");
        assert_eq!(items[2]["pais"][0], "9m");
        assert_eq!(items[2]["value"], "10%");
    }

    /// A call window where the model declines (pon 35% vs none 65%): the card
    /// must show the chosen pass as row 0 AND keep `none` runner-up rows —
    /// dropping them made the card read as recommending the declined call.
    #[test]
    fn api_show_meta_renders_pass_and_none_rows() {
        // Chosen move is pass: row 0 = "None" with candidates[0]'s prob.
        let cands = vec![
            Candidate {
                action: "none".into(),
                prob: 0.65,
            },
            Candidate {
                action: "pon".into(),
                prob: 0.35,
            },
        ];
        let meta = build_show_meta_mjai(&MjaiEvent::None, &cands).unwrap();
        let items = meta["show"]["items"].as_array().unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["label"], "None");
        assert_eq!(items[0]["value"], "65%");
        assert_eq!(items[1]["label"], "Pon");
        assert_eq!(items[1]["value"], "35%");

        // Chosen move is the call: the none runner-up still gets a row.
        let ev = MjaiEvent::Pon {
            actor: 0,
            target: 3,
            pai: "4m".into(),
            consumed: ["4m".into(), "4m".into()],
        };
        let cands = vec![
            Candidate {
                action: "pon".into(),
                prob: 0.55,
            },
            Candidate {
                action: "none".into(),
                prob: 0.45,
            },
        ];
        let meta = build_show_meta_mjai(&ev, &cands).unwrap();
        let items = meta["show"]["items"].as_array().unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["label"], "Pon");
        assert_eq!(items[0]["value"], "55%");
        assert_eq!(items[1]["label"], "None");
        assert_eq!(items[1]["value"], "45%");
    }

    #[test]
    fn build_constructs_local_and_api_runners_offline() {
        let notify = crate::event_bus::notify_bus();
        // Local path (API inactive).
        let off = NativeApiConfig::default();
        assert!(!off.is_active());
        assert!(build(0, 4, &off, notify.clone()).is_ok());

        // API path constructs without any network I/O (client + local fallback
        // model only). A bogus URL is fine — nothing connects at build time.
        let on = NativeApiConfig {
            enabled: true,
            base_url: "http://127.0.0.1:9".into(),
            key: "k".repeat(32),
            model_4p: "4p-ot2".into(),
            model_3p: String::new(),
        };
        assert!(on.is_active());
        assert!(build(0, 4, &on, notify.clone()).is_ok());
        assert!(build(0, 3, &on, notify).is_ok());
    }

    #[test]
    fn api_health_toasts_only_on_transition() {
        use crate::schema::NotifyLevel;
        let notify = crate::event_bus::notify_bus();
        let mut rx = notify.subscribe();
        let api = NativeApiConfig {
            enabled: true,
            base_url: "http://127.0.0.1:9".into(),
            key: "k".repeat(32),
            model_4p: String::new(),
            model_3p: String::new(),
        };
        let mut bot = ApiNativeBot::new(0, 4, &api, notify.clone()).unwrap();

        // healthy → degraded: one warn toast naming the error.
        bot.record_health(false, Some("boom"));
        let n = rx.try_recv().expect("degrade toast");
        assert_eq!(n.level, NotifyLevel::Warn);
        assert_eq!(n.id.as_deref(), Some(NATIVE_API_HEALTH_ID));
        assert!(n.body.unwrap_or_default().contains("boom"));

        // still degraded: no repeat toast (would spam once per turn).
        bot.record_health(false, Some("boom again"));
        assert!(
            rx.try_recv().is_err(),
            "must not toast without a transition"
        );

        // recovered: one success toast, same id (replaces the warning).
        bot.record_health(true, None);
        let n = rx.try_recv().expect("recover toast");
        assert_eq!(n.level, NotifyLevel::Success);
        assert_eq!(n.id.as_deref(), Some(NATIVE_API_HEALTH_ID));

        // still healthy: silent.
        bot.record_health(true, None);
        assert!(rx.try_recv().is_err());
    }
}
