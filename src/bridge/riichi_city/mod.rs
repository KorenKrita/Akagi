//! Riichi City (麻雀一番街) protocol bridge.
//!
//! Wire format: a 15-byte big-endian [`packet`] header + optional JSON body
//! over WebSocket. Gameplay messages carry a string `"cmd"` discriminator
//! inside the JSON (`cmd_enter_room`, `cmd_game_start`, `cmd_game_action_brc`,
//! …); the binary `cmd` field only matters for `CMDAuth`, which carries our
//! `uid`. See [`packet`] for the framing and [`consts`] for the tile table.
//!
//! Faithful Rust port of the observation half of the original Akagi v2 Python
//! Riichi City bridge (`mitm/bridge/riichi_city/bridge.py`). Riichi City has no
//! web client and no autoplay, so this is observe-only: [`build`](Bridge::build)
//! is a no-op and both wire directions are parsed (the `CMDAuth` `uid` packet is
//! client→server while gameplay broadcasts are server→client; client request
//! frames simply don't match any server `cmd_*` and fall through).
//!
//! Two intentional improvements over v2:
//! - **Sanma** events use the native length 3 (no ghost-padding to 4); actor
//!   indices stay in `0..=2`, matching the Tenhou bridge and V3 downstream.
//! - **`cmd_game_action_brc`** iterates every entry in `action_info` instead of
//!   returning after the first (v2 dropped batched actions).
//!
//! Win / draw settlement is decoded from `cmd_game_end`: a `hora` per winner
//! (with deltas + ura-dora) or a `ryukyoku`, followed by `end_kyoku`. Deltas are
//! each player's `point_profit` (+ `extra_profit`) — the per-hand change
//! including honba and collected kyotaku but EXCLUDING that player's own riichi
//! stick. The −1000 for declaring riichi is applied by the mjai consumer at
//! `reach_accepted` (same split as the Tenhou bridge); a `user_point` running
//! diff would double-count it. The action codes 7/10/12 in
//! `cmd_game_action_brc` only flag the end — the scores live in `cmd_game_end`.

pub mod consts;
pub mod packet;
pub mod state;

use super::{Bridge, Direction, ParseResult};
use crate::{
    config::Platform,
    logger::{FlowLogger, Session},
    schema::{MjaiEvent, ParsedFrame},
};
use chrono::Local;
use consts::card_to_mjai;
use packet::{WPacket, CMD_AUTH};
use serde_json::Value as JsonValue;
use state::GameStatus;
use std::sync::Arc;
use tracing::{info, warn};

const LOG: &str = "akagi::bridge::riichi_city";

/// Per-flow Riichi City state. One bridge instance per WebSocket connection.
pub struct RiichiCityBridge {
    /// Our own player id, captured from the `CMDAuth` handshake. `-1` until seen.
    uid: i64,
    status: GameStatus,
    #[allow(dead_code)]
    flow_log: Option<Arc<FlowLogger>>,
    session: Option<Arc<Session>>,
    mjai_log: Option<Arc<FlowLogger>>,
}

impl RiichiCityBridge {
    pub fn new(flow_log: Option<Arc<FlowLogger>>, session: Option<Arc<Session>>) -> Self {
        Self {
            uid: -1,
            status: GameStatus::default(),
            flow_log,
            session,
            mjai_log: None,
        }
    }

    /// Open a fresh `riichi_city_<ts>.mjai.jsonl` per game (Tenhou/Majsoul
    /// rotation pattern). No-op when no session is wired (tests).
    fn rotate_mjai_log(&mut self) {
        let Some(session) = &self.session else {
            return;
        };
        let ts = Local::now().format("%Y%m%d-%H%M%S%.3f").to_string();
        let file_name = format!("riichi_city_{ts}.mjai.jsonl");
        let label = format!("riichi_city mjai {ts}");
        match session.flow_logger(Platform::RiichiCity.subdir(), &file_name, label) {
            Ok(log) => {
                info!(target: LOG, "opened mjai log {file_name}");
                self.mjai_log = Some(log);
            }
            Err(e) => {
                warn!(target: LOG, "failed to open mjai log {file_name}: {e:#}");
                self.mjai_log = None;
            }
        }
    }

    fn write_mjai(&self, events: &[MjaiEvent]) {
        let Some(log) = &self.mjai_log else { return };
        for ev in events {
            match serde_json::to_string(ev) {
                Ok(line) => log.writeln(&line),
                Err(e) => warn!(target: LOG, "failed to serialize MjaiEvent: {e:#}"),
            }
        }
    }

    fn dispatch(&mut self, pkt: &WPacket) -> Vec<MjaiEvent> {
        // The binary auth handshake carries our uid (client → server).
        if pkt.cmd == CMD_AUTH {
            if let Some(uid) = pkt.body.get("uid").and_then(json_i64) {
                self.uid = uid;
                info!(target: LOG, "captured player uid from auth");
            }
            return Vec::new();
        }
        let Some(cmd) = pkt.body.get("cmd").and_then(JsonValue::as_str) else {
            return Vec::new();
        };
        let data = pkt.body.get("data");
        match cmd {
            "cmd_enter_room" => self.on_enter_room(data),
            "cmd_game_start" => self.on_game_start(data),
            "cmd_in_card_brc" => self.on_in_card_brc(data),
            "cmd_send_current_action" => self.on_send_current_action(data),
            "cmd_game_action_brc" => self.on_game_action_brc(data),
            "cmd_gang_bao_brc" => self.on_gang_bao_brc(data),
            "cmd_game_end" => self.on_game_end(data),
            "cmd_room_end" => self.on_room_end(),
            _ => Vec::new(),
        }
    }

    /// `cmd_enter_room` — collect players + table size; emits nothing. Dedups
    /// repeated messages for the same room by `classify_id` (a latent no-op in
    /// v2, where `classify_id` was never stored).
    fn on_enter_room(&mut self, data: Option<&JsonValue>) -> Vec<MjaiEvent> {
        let Some(data) = data else { return Vec::new() };
        let classify_id = data.pointer("/options/classify_id").and_then(json_i64);
        if let (Some(prev), Some(now)) = (self.status.classify_id, classify_id) {
            if prev == now {
                warn!(target: LOG, "duplicate cmd_enter_room for active room, ignored");
                return Vec::new();
            }
        }
        let player_count = data
            .pointer("/options/player_count")
            .and_then(json_i64)
            .unwrap_or(4);
        let is_3p = player_count == 3;
        let mut status = GameStatus {
            game_start: true,
            is_3p,
            num_players: if is_3p { 3 } else { 4 },
            classify_id,
            ..GameStatus::default()
        };
        if let Some(players) = data.get("players").and_then(JsonValue::as_array) {
            for p in players {
                if let Some(uid) = p.pointer("/user/user_id").and_then(json_i64) {
                    status.player_list.push(uid);
                }
            }
        }
        self.status = status;
        Vec::new()
    }

    /// `cmd_game_start` — per kyoku. On the first kyoku also resolves our seat
    /// and emits `start_game`; then `start_kyoku` + the opening `tsumo`.
    fn on_game_start(&mut self, data: Option<&JsonValue>) -> Vec<MjaiEvent> {
        let Some(data) = data else { return Vec::new() };
        let mut events = Vec::new();
        let bakaze = field_card(data, "quan_feng");
        let dora_marker = field_card(data, "bao_pai_card");
        let dealer_pos = field_i64(data, "dealer_pos").unwrap_or(0);
        let n = self.status.num_players as i64;

        if self.status.game_start {
            // Rotate enter-room order so index 0 is the first dealer.
            if !self.status.player_list.is_empty() {
                let mid = dealer_pos.rem_euclid(self.status.player_list.len() as i64) as usize;
                self.status.player_list.rotate_left(mid);
            }
            let seat = self
                .status
                .player_list
                .iter()
                .position(|&id| id == self.uid)
                .unwrap_or_else(|| {
                    warn!(target: LOG, "our uid not found in player_list; defaulting seat 0");
                    0
                }) as u8;
            self.status.seat = seat;
            self.status.shift = dealer_pos;
            self.rotate_mjai_log();
            let names: Vec<String> = (0..self.status.num_players)
                .map(|i| i.to_string())
                .collect();
            events.push(MjaiEvent::StartGame {
                names,
                kyoku_first: None,
                aka_flag: None,
                id: Some(seat),
                num_players: self.status.num_players,
            });
            self.status.game_start = false;
        }

        let rel = (dealer_pos - self.status.shift).rem_euclid(n) as u8;
        let kyoku = rel + 1;
        let oya = rel;
        let honba = field_i64(data, "ben_chang_num").unwrap_or(0).max(0) as u8;
        let kyotaku = field_i64(data, "li_zhi_bang_num").unwrap_or(0).max(0) as u8;

        // Scores from user_info_list, which is in fixed seat order (index i =
        // seat i). Verified against a full 13-kyoku capture with rotating
        // dealers: the list never re-orders with dealer_pos.
        let mut scores = vec![0i32; self.status.num_players as usize];
        if let Some(list) = data.get("user_info_list").and_then(JsonValue::as_array) {
            for (i, p) in list
                .iter()
                .take(self.status.num_players as usize)
                .enumerate()
            {
                scores[i] = field_i64(p, "hand_points").unwrap_or(0) as i32;
            }
        }

        // Our starting hand; opponents are hidden placeholders.
        let hand_cards: Vec<i64> = data
            .get("hand_cards")
            .and_then(JsonValue::as_array)
            .map(|a| a.iter().filter_map(json_i64).collect())
            .unwrap_or_default();
        let (tehai_codes, tsumo_code): (&[i64], Option<i64>) = if hand_cards.len() == 14 {
            (&hand_cards[..13], Some(hand_cards[13]))
        } else {
            (&hand_cards[..], None)
        };
        let my_tehai: Vec<String> = tehai_codes
            .iter()
            .map(|&c| card_to_mjai(c as u32))
            .collect();
        let mut tehais: Vec<Vec<String>> =
            vec![vec!["?".to_string(); 13]; self.status.num_players as usize];
        if (self.status.seat as usize) < tehais.len() && my_tehai.len() == 13 {
            tehais[self.status.seat as usize] = my_tehai;
        }

        events.push(MjaiEvent::StartKyoku {
            bakaze,
            dora_marker,
            kyoku,
            honba,
            kyotaku,
            oya,
            scores,
            tehais,
            num_players: self.status.num_players,
        });

        self.status.pending_dora.clear();
        self.status.pending_reach = None;

        // Opening draw: our own drawn 14th tile if we are the dealer, otherwise
        // the dealer's hidden first draw.
        match tsumo_code {
            Some(code) => events.push(MjaiEvent::Tsumo {
                actor: self.status.seat,
                pai: card_to_mjai(code as u32),
            }),
            None => events.push(MjaiEvent::Tsumo {
                actor: oya,
                pai: "?".to_string(),
            }),
        }
        events
    }

    /// `cmd_in_card_brc` — another player's draw (tile hidden as `?`).
    fn on_in_card_brc(&mut self, data: Option<&JsonValue>) -> Vec<MjaiEvent> {
        let mut events = self.flush_pending_reach();
        let Some(data) = data else { return events };
        let Some(uid) = field_i64(data, "user_id") else {
            return events;
        };
        let Some(actor) = self.status.actor_of(uid) else {
            warn!(target: LOG, "cmd_in_card_brc from unknown user_id");
            return events;
        };
        events.push(MjaiEvent::Tsumo {
            actor,
            pai: field_card(data, "card"),
        });
        events
    }

    /// `cmd_send_current_action` — our own draw (revealed tile).
    fn on_send_current_action(&mut self, data: Option<&JsonValue>) -> Vec<MjaiEvent> {
        let mut events = self.flush_pending_reach();
        let Some(data) = data else { return events };
        let pai = field_card(data, "in_card");
        if pai != "?" {
            events.push(MjaiEvent::Tsumo {
                actor: self.status.seat,
                pai,
            });
        } else {
            warn!(target: LOG, "cmd_send_current_action with unknown in_card");
        }
        events
    }

    /// `cmd_game_action_brc` — chi/pon/kan/dahai/reach/nukidora and end-of-kyoku
    /// triggers. Every entry in `action_info` is processed.
    fn on_game_action_brc(&mut self, data: Option<&JsonValue>) -> Vec<MjaiEvent> {
        let mut events = self.flush_pending_reach();
        let Some(data) = data else { return events };
        let Some(actions) = data.get("action_info").and_then(JsonValue::as_array) else {
            return events;
        };
        let n = self.status.num_players;
        for action in actions {
            let act = field_i64(action, "action").unwrap_or(-1);
            // Win (ron 7 / tsumo 10) and abortive draw (12) are finalized by the
            // cmd_game_end settlement (hora/ryukyoku + end_kyoku), not here.
            if matches!(act, 7 | 10 | 12) {
                continue;
            }
            let Some(uid) = field_i64(action, "user_id") else {
                continue;
            };
            let Some(actor) = self.status.actor_of(uid) else {
                warn!(target: LOG, "game action from unknown user_id");
                continue;
            };
            match act {
                2..=4 => {
                    // Chi always calls from kamicha (previous seat).
                    let target = (actor + n - 1) % n;
                    let consumed = group_cards(action);
                    events.push(MjaiEvent::Chi {
                        actor,
                        target,
                        pai: field_card(action, "card"),
                        consumed: [idx(&consumed, 0), idx(&consumed, 1)],
                    });
                }
                5 => {
                    let consumed = group_cards(action);
                    events.push(MjaiEvent::Pon {
                        actor,
                        target: self.status.last_dahai_actor.unwrap_or(actor),
                        pai: field_card(action, "card"),
                        consumed: [idx(&consumed, 0), idx(&consumed, 1)],
                    });
                }
                6 => {
                    let consumed = group_cards(action);
                    events.push(MjaiEvent::Daiminkan {
                        actor,
                        target: self.status.last_dahai_actor.unwrap_or(actor),
                        pai: field_card(action, "card"),
                        consumed: [idx(&consumed, 0), idx(&consumed, 1), idx(&consumed, 2)],
                    });
                }
                8 => {
                    // Concealed kan: four copies, the first marked red for 5s.
                    let base = field_card(action, "card");
                    let red = red_of(&base);
                    events.push(MjaiEvent::Ankan {
                        actor,
                        consumed: [red, base.clone(), base.clone(), base],
                    });
                }
                9 => {
                    // Added kan: pai is the added tile; the existing pon is three
                    // copies. If the added tile is a plain 5 the pon held the red;
                    // if it is the red 5 the pon held three plain.
                    let pai = field_card(action, "card");
                    let consumed = if is_red(&pai) {
                        let plain = pai[..2].to_string();
                        [plain.clone(), plain.clone(), plain]
                    } else {
                        [red_of(&pai), pai.clone(), pai.clone()]
                    };
                    events.push(MjaiEvent::Kakan {
                        actor,
                        pai,
                        consumed,
                    });
                }
                11 => {
                    let pai = field_card(action, "card");
                    let tsumogiri = match action.get("move_cards_pos").and_then(JsonValue::as_array)
                    {
                        Some(a) if !a.is_empty() => a.first().and_then(json_i64) == Some(14),
                        _ => true,
                    };
                    let is_li_zhi = field_bool(action, "is_li_zhi");
                    if is_li_zhi {
                        events.push(MjaiEvent::Reach { actor, pai: None });
                    }
                    events.push(MjaiEvent::Dahai {
                        actor,
                        pai,
                        tsumogiri,
                    });
                    self.status.last_dahai_actor = Some(actor);
                    if is_li_zhi {
                        // Defer reach_accepted until after we know whether the
                        // discard was called (chi/pon/kan) — flushed at the next
                        // action / draw.
                        self.status.pending_reach = Some(MjaiEvent::ReachAccepted { actor });
                    }
                    // Kan-dora revealed by an earlier cmd_gang_bao_brc surfaces
                    // after this (the kan-caller's) discard.
                    for marker in self.status.pending_dora.drain(..) {
                        events.push(MjaiEvent::Dora {
                            dora_marker: marker,
                        });
                    }
                }
                13 => {
                    // Nukidora (北抜き, sanma) → mjai kita.
                    events.push(MjaiEvent::Kita {
                        actor,
                        pai: Some(field_card(action, "card")),
                    });
                }
                _ => {}
            }
        }
        events
    }

    /// `cmd_gang_bao_brc` — a new kan-dora indicator; deferred until the next
    /// discard.
    fn on_gang_bao_brc(&mut self, data: Option<&JsonValue>) -> Vec<MjaiEvent> {
        if let Some(cards) = data
            .and_then(|d| d.get("cards"))
            .and_then(JsonValue::as_array)
        {
            if let Some(last) = cards.last().and_then(json_i64) {
                self.status.pending_dora.push(card_to_mjai(last as u32));
            }
        }
        Vec::new()
    }

    /// `cmd_game_end` — per-kyoku settlement. Emits a `hora` per winner (with
    /// deltas + ura-dora) or a `ryukyoku`, then `end_kyoku`.
    fn on_game_end(&mut self, data: Option<&JsonValue>) -> Vec<MjaiEvent> {
        let mut events = self.flush_pending_reach();
        let Some(data) = data else {
            events.push(MjaiEvent::EndKyoku);
            return events;
        };
        let end_type = field_i64(data, "end_type").unwrap_or(-1);
        let n = self.status.num_players as usize;

        // Per-actor deltas = point_profit (+ extra_profit for pao / special
        // payments, observed 0 in normal hands). This is the per-hand change
        // including honba and collected kyotaku but EXCLUDING the player's own
        // riichi stick: the −1000 for declaring riichi is applied by the mjai
        // consumer at reach_accepted, so folding it in here (e.g. via a
        // user_point running diff) would double-count it.
        let mut deltas = vec![0i32; n];
        if let Some(list) = data.get("user_profit").and_then(JsonValue::as_array) {
            for up in list {
                let Some(uid) = field_i64(up, "user_id") else {
                    continue;
                };
                let Some(actor) = self.status.actor_of(uid) else {
                    continue;
                };
                let a = actor as usize;
                if a >= n {
                    continue;
                }
                deltas[a] = (field_i64(up, "point_profit").unwrap_or(0)
                    + field_i64(up, "extra_profit").unwrap_or(0))
                    as i32;
            }
        }

        // A win_info entry is a real winner only when its hand has value
        // (all_point > 0). On an exhaustive draw (荒牌流局) win_info instead
        // lists the *tenpai* players with all_point == 0 — not winners — so the
        // presence of win_info alone must NOT be read as a win.
        let winners: Vec<&JsonValue> = data
            .get("win_info")
            .and_then(JsonValue::as_array)
            .map(|ws| {
                ws.iter()
                    .filter(|w| field_i64(w, "all_point").unwrap_or(0) > 0)
                    .collect()
            })
            .unwrap_or_default();

        if winners.is_empty() {
            // Any draw: exhaustive (荒牌流局), 九種九牌, or other abortive. The
            // deltas already carry tenpai/noten payments and riichi-stick moves.
            events.push(MjaiEvent::Ryukyoku {
                deltas: Some(deltas),
            });
        } else {
            // One hora per winner (multiple for double/triple ron).
            let is_tsumo = end_type == 1;
            for win in winners {
                let Some(uid) = field_i64(win, "user_id") else {
                    continue;
                };
                let Some(actor) = self.status.actor_of(uid) else {
                    continue;
                };
                let target = if is_tsumo {
                    actor
                } else {
                    self.status.last_dahai_actor.unwrap_or(actor)
                };
                // li_bao_card = ura-dora indicators (revealed on a riichi win).
                let ura = win
                    .get("li_bao_card")
                    .and_then(JsonValue::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(json_i64)
                            .map(|c| card_to_mjai(c as u32))
                            .collect::<Vec<_>>()
                    })
                    .filter(|v| !v.is_empty());
                events.push(MjaiEvent::Hora {
                    actor,
                    target,
                    deltas: Some(deltas.clone()),
                    ura_markers: ura,
                });
            }
        }

        events.push(MjaiEvent::EndKyoku);
        events
    }

    /// `cmd_room_end` — game over.
    fn on_room_end(&mut self) -> Vec<MjaiEvent> {
        self.status = GameStatus::default();
        vec![MjaiEvent::EndGame]
    }

    fn flush_pending_reach(&mut self) -> Vec<MjaiEvent> {
        match self.status.pending_reach.take() {
            Some(ev) => vec![ev],
            None => Vec::new(),
        }
    }
}

impl Bridge for RiichiCityBridge {
    fn parse(&mut self, _direction: Direction, content: &[u8]) -> ParseResult {
        // Both directions are parsed: the CMDAuth uid is client→server, gameplay
        // is server→client. A frame normally holds one packet.
        let packets = WPacket::parse_frame(content);
        if packets.is_empty() {
            return ParseResult::empty();
        }
        let parsed = Some(ParsedFrame {
            method: packets[0].method_label(),
            args: packets[0].body.clone(),
        });
        let mut events = Vec::new();
        for pkt in &packets {
            events.extend(self.dispatch(pkt));
        }
        self.write_mjai(&events);
        ParseResult { events, parsed }
    }

    fn build(&mut self, _command: &MjaiEvent) -> Option<Vec<u8>> {
        // Observe-only — Riichi City has no autoplay.
        None
    }
}

// ============================================================================
// JSON / tile helpers
// ============================================================================

/// Coerce a JSON value to `i64`, accepting integers and numeric strings.
fn json_i64(v: &JsonValue) -> Option<i64> {
    if let Some(n) = v.as_i64() {
        return Some(n);
    }
    if let Some(n) = v.as_u64() {
        return Some(n as i64);
    }
    v.as_str().and_then(|s| s.parse::<i64>().ok())
}

fn field_i64(data: &JsonValue, key: &str) -> Option<i64> {
    data.get(key).and_then(json_i64)
}

/// Read a tile-code field and convert to its mjai string (`?` when absent).
fn field_card(data: &JsonValue, key: &str) -> String {
    card_to_mjai(field_i64(data, key).unwrap_or(0) as u32)
}

/// Read a flag that may arrive as a JSON bool or a 0/1 integer.
fn field_bool(data: &JsonValue, key: &str) -> bool {
    match data.get(key) {
        Some(JsonValue::Bool(b)) => *b,
        Some(other) => json_i64(other).map(|n| n != 0).unwrap_or(false),
        None => false,
    }
}

/// Decode the `group_cards` array (tiles consumed from hand for a call).
fn group_cards(action: &JsonValue) -> Vec<String> {
    action
        .get("group_cards")
        .and_then(JsonValue::as_array)
        .map(|a| {
            a.iter()
                .map(|c| card_to_mjai(json_i64(c).unwrap_or(0) as u32))
                .collect()
        })
        .unwrap_or_default()
}

/// `v[i]` or `?` — keeps consumed arrays well-formed against short input.
fn idx(v: &[String], i: usize) -> String {
    v.get(i).cloned().unwrap_or_else(|| "?".to_string())
}

/// True for an mjai red-five string (`5mr`/`5pr`/`5sr`).
fn is_red(pai: &str) -> bool {
    matches!(pai, "5mr" | "5pr" | "5sr")
}

/// The red-five variant of a plain `5m`/`5p`/`5s`; otherwise the tile itself.
fn red_of(pai: &str) -> String {
    match pai {
        "5m" | "5p" | "5s" => format!("{pai}r"),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    /// Frame a JSON body as a WPacket and run it through the bridge.
    fn feed(bridge: &mut RiichiCityBridge, cmd: u16, body: Value) -> Vec<MjaiEvent> {
        let json = serde_json::to_vec(&body).unwrap();
        let packet_size = (15 + json.len()) as u32;
        let mut buf = Vec::new();
        buf.extend_from_slice(&packet_size.to_be_bytes());
        buf.extend_from_slice(&[0x00, 0x0f, 0x00, 0x01]);
        buf.extend_from_slice(&0u32.to_be_bytes()); // message_index
        buf.extend_from_slice(&cmd.to_be_bytes());
        buf.push(1);
        buf.extend_from_slice(&json);
        bridge.parse(Direction::Down, &buf).events
    }

    // Fake ids per CLAUDE.md rule 8.
    const ME: i64 = 1001;

    fn auth(b: &mut RiichiCityBridge) {
        feed(b, CMD_AUTH, json!({ "uid": ME.to_string() }));
    }

    fn enter_room_4p(b: &mut RiichiCityBridge) {
        feed(
            b,
            18,
            json!({
                "cmd": "cmd_enter_room",
                "data": {
                    "options": { "classify_id": 555, "player_count": 4 },
                    "players": [
                        {"user": {"user_id": 1001}},
                        {"user": {"user_id": 1002}},
                        {"user": {"user_id": 1003}},
                        {"user": {"user_id": 1004}}
                    ]
                }
            }),
        );
    }

    #[test]
    fn yonma_game_start_emits_start_game_kyoku_tsumo() {
        let mut b = RiichiCityBridge::new(None, None);
        auth(&mut b);
        enter_room_4p(&mut b);
        // dealer_pos 0 → no rotation; ME is index 0 → seat 0 → we are dealer
        // (14-card hand).
        let events = feed(
            &mut b,
            18,
            json!({
                "cmd": "cmd_game_start",
                "data": {
                    "quan_feng": 0x31,        // East
                    "bao_pai_card": 0x21,     // 1m
                    "dealer_pos": 0,
                    "ben_chang_num": 0,
                    "li_zhi_bang_num": 0,
                    "user_info_list": [
                        {"hand_points": 25000}, {"hand_points": 25000},
                        {"hand_points": 25000}, {"hand_points": 25000}
                    ],
                    "hand_cards": [0x21,0x22,0x23,0x24,0x25,0x26,0x27,0x28,0x29,0x01,0x02,0x03,0x04,0x05]
                }
            }),
        );
        assert_eq!(events.len(), 3);
        match &events[0] {
            MjaiEvent::StartGame {
                id,
                num_players,
                names,
                ..
            } => {
                assert_eq!(*id, Some(0));
                assert_eq!(*num_players, 4);
                assert_eq!(names.len(), 4);
            }
            other => panic!("expected StartGame, got {other:?}"),
        }
        match &events[1] {
            MjaiEvent::StartKyoku {
                bakaze,
                dora_marker,
                kyoku,
                oya,
                scores,
                tehais,
                num_players,
                ..
            } => {
                assert_eq!(bakaze, "E");
                assert_eq!(dora_marker, "1m");
                assert_eq!(*kyoku, 1);
                assert_eq!(*oya, 0);
                assert_eq!(*num_players, 4);
                assert_eq!(scores, &vec![25000; 4]);
                assert_eq!(tehais.len(), 4);
                assert_eq!(tehais[0][0], "1m");
                assert_eq!(tehais[0][9], "1p");
                assert_eq!(tehais[1], vec!["?".to_string(); 13]);
            }
            other => panic!("expected StartKyoku, got {other:?}"),
        }
        match &events[2] {
            MjaiEvent::Tsumo { actor, pai } => {
                assert_eq!(*actor, 0);
                assert_eq!(pai, "5p"); // the drawn 14th tile
            }
            other => panic!("expected Tsumo, got {other:?}"),
        }
    }

    #[test]
    fn sanma_uses_native_length_three() {
        let mut b = RiichiCityBridge::new(None, None);
        feed(&mut b, CMD_AUTH, json!({ "uid": "1002" }));
        feed(
            &mut b,
            18,
            json!({
                "cmd": "cmd_enter_room",
                "data": {
                    "options": { "classify_id": 1, "player_count": 3 },
                    "players": [
                        {"user": {"user_id": 1001}},
                        {"user": {"user_id": 1002}},
                        {"user": {"user_id": 1003}}
                    ]
                }
            }),
        );
        // dealer_pos 0; ME=1002 → seat 1; 13-card hand (not dealer).
        let events = feed(
            &mut b,
            18,
            json!({
                "cmd": "cmd_game_start",
                "data": {
                    "quan_feng": 0x31, "bao_pai_card": 0x21, "dealer_pos": 0,
                    "ben_chang_num": 0, "li_zhi_bang_num": 0,
                    "user_info_list": [
                        {"hand_points": 35000}, {"hand_points": 35000}, {"hand_points": 35000}
                    ],
                    "hand_cards": [0x21,0x22,0x23,0x24,0x25,0x26,0x27,0x28,0x29,0x01,0x02,0x03,0x04]
                }
            }),
        );
        let sg = events.iter().find_map(|e| match e {
            MjaiEvent::StartGame {
                id, num_players, ..
            } => Some((*id, *num_players)),
            _ => None,
        });
        assert_eq!(sg, Some((Some(1), 3)));
        let sk = events.iter().find_map(|e| match e {
            MjaiEvent::StartKyoku {
                scores,
                tehais,
                num_players,
                ..
            } => Some((scores.len(), tehais.len(), *num_players)),
            _ => None,
        });
        assert_eq!(sk, Some((3, 3, 3)));
        // Not the dealer → opening tsumo is the dealer's hidden draw.
        let tsumo = events.iter().find_map(|e| match e {
            MjaiEvent::Tsumo { actor, pai } => Some((*actor, pai.clone())),
            _ => None,
        });
        assert_eq!(tsumo, Some((0, "?".to_string())));
    }

    fn started_4p() -> RiichiCityBridge {
        let mut b = RiichiCityBridge::new(None, None);
        auth(&mut b);
        enter_room_4p(&mut b);
        feed(
            &mut b,
            18,
            json!({
                "cmd": "cmd_game_start",
                "data": {
                    "quan_feng": 0x31, "bao_pai_card": 0x21, "dealer_pos": 0,
                    "ben_chang_num": 0, "li_zhi_bang_num": 0,
                    "user_info_list": [
                        {"hand_points": 25000}, {"hand_points": 25000},
                        {"hand_points": 25000}, {"hand_points": 25000}
                    ],
                    "hand_cards": [0x21,0x22,0x23,0x24,0x25,0x26,0x27,0x28,0x29,0x01,0x02,0x03,0x04,0x05]
                }
            }),
        );
        b
    }

    #[test]
    fn other_player_draw_is_hidden() {
        let mut b = started_4p();
        let events = feed(
            &mut b,
            18,
            json!({"cmd": "cmd_in_card_brc", "data": {"user_id": 1002, "card": 0}}),
        );
        assert_eq!(events.len(), 1);
        match &events[0] {
            MjaiEvent::Tsumo { actor, pai } => {
                assert_eq!(*actor, 1);
                assert_eq!(pai, "?");
            }
            other => panic!("expected Tsumo, got {other:?}"),
        }
    }

    #[test]
    fn reach_dahai_defers_reach_accepted_until_next_action() {
        let mut b = started_4p();
        // We discard declaring riichi.
        let e1 = feed(
            &mut b,
            18,
            json!({
                "cmd": "cmd_game_action_brc",
                "data": {"action_info": [
                    {"action": 11, "user_id": 1001, "card": 0x29, "move_cards_pos": [5], "is_li_zhi": true}
                ]}
            }),
        );
        assert!(matches!(e1[0], MjaiEvent::Reach { actor: 0, .. }));
        assert!(
            matches!(&e1[1], MjaiEvent::Dahai { actor: 0, pai, tsumogiri: false } if pai == "9m")
        );
        // The next draw flushes the deferred reach_accepted first.
        let e2 = feed(
            &mut b,
            18,
            json!({"cmd": "cmd_in_card_brc", "data": {"user_id": 1002, "card": 0}}),
        );
        assert!(matches!(e2[0], MjaiEvent::ReachAccepted { actor: 0 }));
        assert!(matches!(e2[1], MjaiEvent::Tsumo { actor: 1, .. }));
    }

    #[test]
    fn pon_targets_last_discarder() {
        let mut b = started_4p();
        // Seat 1 discards (sets last_dahai_actor = 1)…
        feed(
            &mut b,
            18,
            json!({"cmd": "cmd_game_action_brc", "data": {"action_info": [
                {"action": 11, "user_id": 1002, "card": 0x25, "move_cards_pos": [14], "is_li_zhi": false}
            ]}}),
        );
        // …then we pon it.
        let e = feed(
            &mut b,
            18,
            json!({"cmd": "cmd_game_action_brc", "data": {"action_info": [
                {"action": 5, "user_id": 1001, "card": 0x25, "group_cards": [0x25, 0x25]}
            ]}}),
        );
        match &e[0] {
            MjaiEvent::Pon {
                actor,
                target,
                pai,
                consumed,
            } => {
                assert_eq!(*actor, 0);
                assert_eq!(*target, 1);
                assert_eq!(pai, "5m");
                assert_eq!(consumed, &["5m".to_string(), "5m".to_string()]);
            }
            other => panic!("expected Pon, got {other:?}"),
        }
    }

    #[test]
    fn ankan_marks_red_five() {
        let mut b = started_4p();
        let e = feed(
            &mut b,
            18,
            json!({"cmd": "cmd_game_action_brc", "data": {"action_info": [
                {"action": 8, "user_id": 1001, "card": 0x25}
            ]}}),
        );
        match &e[0] {
            MjaiEvent::Ankan { actor, consumed } => {
                assert_eq!(*actor, 0);
                assert_eq!(
                    consumed,
                    &[
                        "5mr".to_string(),
                        "5m".to_string(),
                        "5m".to_string(),
                        "5m".to_string()
                    ]
                );
            }
            other => panic!("expected Ankan, got {other:?}"),
        }
    }

    #[test]
    fn kakan_with_red_added_tile_yields_plain_consumed() {
        let mut b = started_4p();
        let e = feed(
            &mut b,
            18,
            json!({"cmd": "cmd_game_action_brc", "data": {"action_info": [
                {"action": 9, "user_id": 1001, "card": 0x125}
            ]}}),
        );
        match &e[0] {
            MjaiEvent::Kakan {
                actor,
                pai,
                consumed,
            } => {
                assert_eq!(*actor, 0);
                assert_eq!(pai, "5mr");
                assert_eq!(
                    consumed,
                    &["5m".to_string(), "5m".to_string(), "5m".to_string()]
                );
            }
            other => panic!("expected Kakan, got {other:?}"),
        }
    }

    #[test]
    fn kan_dora_flushes_after_next_discard() {
        let mut b = started_4p();
        // Kan-dora indicator arrives (deferred)…
        let none = feed(
            &mut b,
            18,
            json!({"cmd": "cmd_gang_bao_brc", "data": {"cards": [0x22]}}),
        );
        assert!(none.is_empty());
        // …and surfaces right after the next discard.
        let e = feed(
            &mut b,
            18,
            json!({"cmd": "cmd_game_action_brc", "data": {"action_info": [
                {"action": 11, "user_id": 1001, "card": 0x05, "move_cards_pos": [14], "is_li_zhi": false}
            ]}}),
        );
        assert!(matches!(&e[0], MjaiEvent::Dahai { pai, .. } if pai == "5p"));
        assert!(matches!(&e[1], MjaiEvent::Dora { dora_marker } if dora_marker == "2m"));
    }

    #[test]
    fn nukidora_maps_to_kita() {
        let mut b = started_4p();
        let e = feed(
            &mut b,
            18,
            json!({"cmd": "cmd_game_action_brc", "data": {"action_info": [
                {"action": 13, "user_id": 1001, "card": 0x61}
            ]}}),
        );
        match &e[0] {
            MjaiEvent::Kita { actor, pai } => {
                assert_eq!(*actor, 0);
                assert_eq!(pai.as_deref(), Some("N"));
            }
            other => panic!("expected Kita, got {other:?}"),
        }
    }

    #[test]
    fn win_action_alone_does_not_end_kyoku() {
        // Actions 7/10/12 only flag the end; the kyoku closes on cmd_game_end.
        let mut b = started_4p();
        let e = feed(
            &mut b,
            18,
            json!({"cmd": "cmd_game_action_brc", "data": {"action_info": [
                {"action": 7, "user_id": 1001}
            ]}}),
        );
        assert!(e.is_empty(), "win action must not emit end_kyoku by itself");
    }

    #[test]
    fn game_end_ron_emits_hora_with_deltas_and_ura() {
        let mut b = started_4p(); // seat 0 = us (1001), scores all 25000
                                  // Seat 2 (1003) discards → ron target.
        feed(
            &mut b,
            18,
            json!({"cmd": "cmd_game_action_brc", "data": {"action_info": [
                {"action": 11, "user_id": 1003, "card": 0x25, "move_cards_pos": [14], "is_li_zhi": false}
            ]}}),
        );
        // Ron flag (no-op) then the settlement.
        feed(
            &mut b,
            18,
            json!({"cmd": "cmd_game_action_brc", "data": {"action_info": [{"action": 7, "user_id": 1001}]}}),
        );
        let e = feed(
            &mut b,
            18,
            json!({
                "cmd": "cmd_game_end",
                "data": {
                    "end_type": 0,
                    "win_info": [{"user_id": 1001, "all_point": 12000, "li_bao_card": [0x51]}],
                    // Winner riichi'd: point_profit 14000 (12000 + 2 sticks),
                    // loser -12000. user_point is set to a DIFFERENT running diff
                    // (+13000 / -13000) to prove deltas come from point_profit,
                    // not a user_point diff (the −1000 is at reach_accepted).
                    "user_profit": [
                        {"user_id": 1001, "user_point": 38000, "point_profit": 14000, "li_zhi_profit": 1000},
                        {"user_id": 1002, "user_point": 25000, "point_profit": 0, "li_zhi_profit": 0},
                        {"user_id": 1003, "user_point": 12000, "point_profit": -12000, "li_zhi_profit": -1000},
                        {"user_id": 1004, "user_point": 25000, "point_profit": 0, "li_zhi_profit": 0}
                    ]
                }
            }),
        );
        assert_eq!(e.len(), 2);
        match &e[0] {
            MjaiEvent::Hora {
                actor,
                target,
                deltas,
                ura_markers,
            } => {
                assert_eq!(*actor, 0);
                assert_eq!(*target, 2, "ron target is the last discarder");
                assert_eq!(
                    deltas.as_ref().unwrap(),
                    &vec![14000, 0, -12000, 0],
                    "deltas must be point_profit (excludes own riichi stick), not the user_point diff",
                );
                assert_eq!(ura_markers.as_deref(), Some(["W".to_string()].as_slice()));
            }
            other => panic!("expected Hora, got {other:?}"),
        }
        assert!(matches!(e[1], MjaiEvent::EndKyoku));
    }

    #[test]
    fn game_end_tsumo_targets_self_no_ura() {
        let mut b = started_4p();
        let e = feed(
            &mut b,
            18,
            json!({
                "cmd": "cmd_game_end",
                "data": {
                    "end_type": 1,
                    "win_info": [{"user_id": 1003, "all_point": 6000}],
                    "user_profit": [
                        {"user_id": 1001, "point_profit": -2000},
                        {"user_id": 1002, "point_profit": -2000},
                        {"user_id": 1003, "point_profit": 6000},
                        {"user_id": 1004, "point_profit": -2000}
                    ]
                }
            }),
        );
        match &e[0] {
            MjaiEvent::Hora {
                actor,
                target,
                deltas,
                ura_markers,
            } => {
                assert_eq!(*actor, 2);
                assert_eq!(*target, 2, "tsumo target == winner");
                assert_eq!(deltas.as_ref().unwrap(), &vec![-2000, -2000, 6000, -2000]);
                assert!(ura_markers.is_none());
            }
            other => panic!("expected Hora, got {other:?}"),
        }
        assert!(matches!(e[1], MjaiEvent::EndKyoku));
    }

    #[test]
    fn game_end_abortive_draw_emits_ryukyoku() {
        // 九種九牌: end_type 6, empty win_info, no point change.
        let mut b = started_4p();
        let e = feed(
            &mut b,
            18,
            json!({
                "cmd": "cmd_game_end",
                "data": {
                    "end_type": 6,
                    "win_info": [],
                    "user_profit": [
                        {"user_id": 1001, "point_profit": 0},
                        {"user_id": 1002, "point_profit": 0},
                        {"user_id": 1003, "point_profit": 0},
                        {"user_id": 1004, "point_profit": 0}
                    ]
                }
            }),
        );
        assert_eq!(e.len(), 2);
        match &e[0] {
            MjaiEvent::Ryukyoku { deltas } => {
                assert_eq!(deltas.as_ref().unwrap(), &vec![0, 0, 0, 0]);
            }
            other => panic!("expected Ryukyoku, got {other:?}"),
        }
        assert!(matches!(e[1], MjaiEvent::EndKyoku));
    }

    /// Regression (from a real capture): 荒牌流局 arrives as end_type 7 with
    /// a non-empty win_info that lists the *tenpai* players (all_point == 0).
    /// It must become one ryukyoku, not a hora per "winner".
    #[test]
    fn game_end_exhaustive_draw_with_tenpai_list_is_ryukyoku() {
        let mut b = started_4p(); // scores all 25000
        let e = feed(
            &mut b,
            18,
            json!({
                "cmd": "cmd_game_end",
                "data": {
                    "end_type": 7,
                    // Seats 1 and 3 tenpai (revealed), but all_point == 0.
                    "win_info": [
                        {"user_id": 1002, "all_point": 0, "all_fu": 0, "all_fang_num": 0},
                        {"user_id": 1004, "all_point": 0, "all_fu": 0, "all_fang_num": 0}
                    ],
                    // point_profit = tenpai +1500 / noten −1500. Seats 1 and 3
                    // also declared riichi (li_zhi_profit −1000), but that −1000
                    // is applied at reach_accepted, NOT in the ryukyoku deltas.
                    "user_profit": [
                        {"user_id": 1001, "point_profit": -1500, "li_zhi_profit": 0},
                        {"user_id": 1002, "point_profit": 1500, "li_zhi_profit": -1000},
                        {"user_id": 1003, "point_profit": -1500, "li_zhi_profit": 0},
                        {"user_id": 1004, "point_profit": 1500, "li_zhi_profit": -1000}
                    ]
                }
            }),
        );
        assert_eq!(e.len(), 2, "exactly one ryukyoku + end_kyoku, no hora");
        match &e[0] {
            MjaiEvent::Ryukyoku { deltas } => {
                assert_eq!(
                    deltas.as_ref().unwrap(),
                    &vec![-1500, 1500, -1500, 1500],
                    "tenpai payment is +1500; the riichi −1000 is at reach_accepted",
                );
            }
            other => panic!("expected Ryukyoku, got {other:?}"),
        }
        assert!(matches!(e[1], MjaiEvent::EndKyoku));
    }

    #[test]
    fn room_end_emits_end_game() {
        let mut b = started_4p();
        let e = feed(&mut b, 18, json!({"cmd": "cmd_room_end", "data": {}}));
        assert_eq!(e.len(), 1);
        assert!(matches!(e[0], MjaiEvent::EndGame));
    }

    #[test]
    fn unknown_cmd_is_ignored() {
        let mut b = started_4p();
        let e = feed(
            &mut b,
            18,
            json!({"cmd": "cmd_some_future_thing", "data": {}}),
        );
        assert!(e.is_empty());
    }
}
