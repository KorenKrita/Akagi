//! Riichi City outbound (client → server) frame builder.
//!
//! Gameplay actions ride `"req_game_action"` with the same numeric action
//! codes the server broadcasts back. Every shape below is verified against
//! the client's own sender (the game logic ships as Lua:
//! `lua_models_game` `ReqOutCard`/`ReqGameOpt`, and the offer-to-button
//! construction in `lua_procestates_game_components`), cross-checked with
//! recorded uplink frames from live play.

use crate::schema::MjaiEvent;
use serde_json::{json, Value};

use super::packet::WPacket;

const CMD_GAME_ACTION: &str = "req_game_action";

/// The WPacket binary command every request rides (0 = heartbeat, 1 =
/// auth).
const BIN_CMD_REQUEST: u16 = 6;

/// Action codes shared with the inbound `cmd_game_action_brc`. Verified
/// against the client's `EMjActionType` enum and uplink recordings: 1
/// pass, 2/3/4 chi variants (claimed tile low/middle/high in the run),
/// 5 pon, 6 daiminkan, 7 ron, 8 ankan, 9 kakan, 10 tsumo, 11 dahai,
/// 13 kita.
mod action {
    pub const PASS: i64 = 1;
    pub const RON: i64 = 7;
    pub const TSUMO: i64 = 10;
    pub const DAHAI: i64 = 11;
    pub const KITA: i64 = 13;
    pub const PON: i64 = 5;
    pub const DAIMINKAN: i64 = 6;
    pub const ANKAN: i64 = 8;
    pub const KAKAN: i64 = 9;
}

/// The chi variant code: where the claimed tile sits in the run — 2 = low
/// end, 3 = middle, 4 = high end. A wrong (or unoffered) variant gets a
/// `code 0` ack but is never applied; the call window just runs out.
fn chi_action_code(claimed: u32, consumed: &[u32; 2]) -> Option<i64> {
    let rank = |c: u32| c & 0x0f;
    let mut run = [rank(claimed), rank(consumed[0]), rank(consumed[1])];
    run.sort_unstable();
    if run[0] == rank(claimed) {
        Some(2)
    } else if run[1] == rank(claimed) {
        Some(3)
    } else {
        Some(4)
    }
}

/// Positions of `tiles` within `hand` (the tracker's tehai, which mirrors
/// the server's hand order — that is what the wire positions index).
/// Each hand tile is consumed at most once. `None` when a tile is absent
/// or no hand was given: the caller then omits `move_cards_pos`.
fn hand_positions(hand: Option<&[String]>, tiles: &[String]) -> Option<Vec<u32>> {
    let hand = hand?;
    let mut used = vec![false; hand.len()];
    let mut out = Vec::with_capacity(tiles.len());
    'next: for t in tiles {
        for (i, h) in hand.iter().enumerate() {
            if !used[i] && h == t {
                used[i] = true;
                out.push(i as u32);
                continue 'next;
            }
        }
        return None;
    }
    Some(out)
}

/// Attach `move_cards_pos` when the positions are computable; the client
/// omits the field, so we do too rather than send a null.
fn with_pos(mut d: Value, hand: Option<&[String]>, tiles: &[String]) -> Value {
    if let Some(p) = hand_positions(hand, tiles) {
        d["move_cards_pos"] = json!(p);
    }
    d
}

/// mjai tile string → Riichi City card code (inverse of `consts::card_to_mjai`).
/// Returns `None` for `"?"` and anything malformed — encoding a hidden tile
/// means the caller's state is wrong, and the right move is to skip the action.
pub fn mjai_to_card(pai: &str) -> Option<u32> {
    let code = match pai {
        "?" => return None,
        "1p" => 0x01,
        "2p" => 0x02,
        "3p" => 0x03,
        "4p" => 0x04,
        "5p" => 0x05,
        "6p" => 0x06,
        "7p" => 0x07,
        "8p" => 0x08,
        "9p" => 0x09,
        "1s" => 0x11,
        "2s" => 0x12,
        "3s" => 0x13,
        "4s" => 0x14,
        "5s" => 0x15,
        "6s" => 0x16,
        "7s" => 0x17,
        "8s" => 0x18,
        "9s" => 0x19,
        "1m" => 0x21,
        "2m" => 0x22,
        "3m" => 0x23,
        "4m" => 0x24,
        "5m" => 0x25,
        "6m" => 0x26,
        "7m" => 0x27,
        "8m" => 0x28,
        "9m" => 0x29,
        "E" => 0x31,
        "S" => 0x41,
        "W" => 0x51,
        "N" => 0x61,
        "P" => 0x71,
        "F" => 0x81,
        "C" => 0x91,
        "5pr" => 0x105,
        "5sr" => 0x115,
        "5mr" => 0x125,
        _ => return None,
    };
    Some(code)
}

fn cards(pais: impl IntoIterator<Item = impl AsRef<str>>) -> Option<Vec<u32>> {
    pais.into_iter().map(|p| mjai_to_card(p.as_ref())).collect()
}

/// Encode one bot decision as the client frame that performs it, without
/// table context (see [`encode_action_with`] for the context-aware form).
pub fn encode_action(ev: &MjaiEvent) -> Option<Vec<u8>> {
    encode_action_with(ev, None, None, None)
}

/// [`encode_action`] plus the table state some actions need: the tile we
/// just drew (a tsumo's `card` is the winning tile), the most recent
/// discard (a ron's `card` is the claimed tile), and our tehai (call
/// requests carry `move_cards_pos` — the consumed tiles' positions in the
/// server-ordered hand). The autoplay planner supplies all three from its
/// `ActionContext`.
pub fn encode_action_with(
    ev: &MjaiEvent,
    tsumo_pai: Option<&str>,
    last_discard: Option<&str>,
    hand: Option<&[String]>,
) -> Option<Vec<u8>> {
    let data: Value = match ev {
        MjaiEvent::Dahai { pai, tsumogiri, .. } => dahai_data(pai, *tsumogiri, false, hand)?,
        MjaiEvent::Reach { pai: Some(pai), .. } => dahai_data(pai, false, true, hand)?,
        // Discard-claims (chi/pon) are bare `{action, card}` on the wire:
        // the client's own claim buttons send nothing else — the server
        // derives the meld from the variant code plus our hand.
        MjaiEvent::Chi { pai, consumed, .. } => {
            let card = mjai_to_card(pai)?;
            let group = cards(consumed)?;
            json!({
                "action": chi_action_code(card, &[group[0], group[1]])?,
                "card": card,
            })
        }
        MjaiEvent::Pon { pai, .. } => json!({
            "action": action::PON,
            "card": mjai_to_card(pai)?,
        }),
        // Daiminkan is the one claim that does carry the consumed tiles:
        // `card` names the claimed discard, `group_cards` the three
        // matching tiles from our hand, `move_cards_pos` their positions.
        MjaiEvent::Daiminkan { pai, consumed, .. } => with_pos(
            json!({
                "action": action::DAIMINKAN,
                "card": mjai_to_card(pai)?,
                "group_cards": cards(consumed)?,
            }),
            hand,
            consumed,
        ),
        // Ankan (from the client's offer builder): `card` is one of the
        // four, `group_cards` carries three ("the server only needs
        // three" — its own comment), and `move_cards_pos` the positions
        // of all four.
        MjaiEvent::Ankan { consumed, .. } => {
            let group = cards(consumed)?;
            with_pos(
                json!({
                    "action": action::ANKAN,
                    "card": group.first().copied()?,
                    "group_cards": &group[1..],
                }),
                hand,
                consumed,
            )
        }
        // Kakan (client's `ActionBuGang` branch): the promoted tile and
        // its hand position, and no `group_cards`.
        MjaiEvent::Kakan { pai, .. } => with_pos(
            json!({
                "action": action::KAKAN,
                "card": mjai_to_card(pai)?,
            }),
            hand,
            std::slice::from_ref(pai),
        ),
        // Kita (client's `ActionPullNorth`): the north tile, bare.
        MjaiEvent::Kita { pai, .. } => json!({
            "action": action::KITA,
            "card": mjai_to_card(pai.as_deref().unwrap_or("N"))?,
        }),
        // Tsumo names its winning tile (`{"action":10,"card":6}` for a 6p
        // self-draw win — recorded). A ron mirrors it with the claimed
        // discard, per the client's claim-button construction.
        MjaiEvent::Hora { target, actor, .. } => {
            let mut d =
                json!({ "action": if target == actor { action::TSUMO } else { action::RON } });
            if let Some(pai) = if target == actor {
                tsumo_pai
            } else {
                last_discard
            } {
                d["card"] = json!(mjai_to_card(pai)?);
            }
            d
        }
        // Verified: declining a call window is a bare `{"action":1}`.
        MjaiEvent::None => json!({ "action": action::PASS }),
        _ => return None,
    };
    Some(action_frame(&data))
}

/// A discard request. Riichi is the same request with `is_li_zhi: true`
/// (verified: the recorded riichi discard carries exactly these fields).
///
/// `move_cards_pos` is client display metadata, never validated: the
/// client computes `[index, toIndex]` as animation hints (which rack slot
/// the tile slides from) and the server merely relays them in the
/// broadcast so other clients can animate. `index` is the tile's 1-based
/// position in the hand — the drawn tile sits last (`CheckOutCardIndex`).
/// `toIndex` depends on the player's chosen tile-sort order
/// (`GetMoveIndexByOutCard`); the client's own fallback branches return
/// `hand.len() - 1`, which is what we send. Without a hand we fall back
/// to the recorded placeholder shapes.
fn dahai_data(pai: &str, tsumogiri: bool, riichi: bool, hand: Option<&[String]>) -> Option<Value> {
    let pos: Value = match hand {
        Some(hand) if !hand.is_empty() => {
            let to = (hand.len() - 1).max(1) as u32;
            let from = if tsumogiri {
                hand.len() as u32
            } else {
                // First matching tile, 1-based — mirrors the client's
                // card-object lookup well enough for an animation hint.
                hand.iter()
                    .position(|h| h == pai)
                    .map(|i| i as u32 + 1)
                    .unwrap_or(to)
            };
            json!([from, to])
        }
        // No hand: the recorded placeholder shapes.
        _ => json!([if tsumogiri { 14 } else { 13 }, 13]),
    };
    Some(json!({
        "action": action::DAHAI,
        "card": mjai_to_card(pai)?,
        "is_li_zhi": riichi,
        // Double-riichi flag. TODO: set when the bot's reach is declared on
        // its first uninterrupted turn (junme <= 1 with no calls).
        "is_xuan_gao_ting": false,
        "li_zhi_operate": 0,
        "move_cards_pos": pos,
    }))
}

fn action_frame(data: &Value) -> Vec<u8> {
    WPacket::encode_request(
        BIN_CMD_REQUEST,
        &json!({ "cmd": CMD_GAME_ACTION, "data": data }),
    )
}

/// The round-advance press ("OK" on the scoring screen — the second OK; the
/// first is client-local only). Verified: one bare `req_user_prepare` per
/// round end advances to the next round (`rsp_user_prepare code 0` +
/// `cmd_user_prepare` broadcast; a duplicate gets `code 2`, harmlessly).
pub fn user_prepare() -> Vec<u8> {
    WPacket::encode_request(BIN_CMD_REQUEST, &json!({ "cmd": "req_user_prepare" }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every frame we build must survive the inbound parser unchanged —
    /// whatever the real uplink shapes turn out to be, this invariant is
    /// how captures get compared against these templates.
    #[test]
    fn frames_round_trip_through_the_parser() {
        let cases = vec![
            MjaiEvent::Dahai {
                actor: 0,
                pai: "3m".into(),
                tsumogiri: true,
            },
            MjaiEvent::Dahai {
                actor: 0,
                pai: "5sr".into(),
                tsumogiri: false,
            },
            MjaiEvent::Reach {
                actor: 0,
                pai: Some("1p".into()),
            },
            MjaiEvent::Chi {
                actor: 0,
                target: 3,
                pai: "2s".into(),
                consumed: ["1s".into(), "3s".into()],
            },
            MjaiEvent::Pon {
                actor: 0,
                target: 1,
                pai: "N".into(),
                consumed: ["N".into(), "N".into()],
            },
            MjaiEvent::Daiminkan {
                actor: 0,
                target: 2,
                pai: "E".into(),
                consumed: ["E".into(), "E".into(), "E".into()],
            },
            MjaiEvent::Ankan {
                actor: 0,
                consumed: ["5mr".into(), "5m".into(), "5m".into(), "5m".into()],
            },
            MjaiEvent::Kakan {
                actor: 0,
                pai: "5pr".into(),
                consumed: ["5p".into(), "5p".into(), "5p".into()],
            },
            MjaiEvent::Kita {
                actor: 0,
                pai: Some("N".into()),
            },
            MjaiEvent::Hora {
                actor: 0,
                target: 0,
                deltas: None,
                ura_markers: None,
            },
            MjaiEvent::Hora {
                actor: 0,
                target: 1,
                deltas: None,
                ura_markers: None,
            },
            MjaiEvent::None,
        ];
        for ev in cases {
            let frame = encode_action_with(&ev, Some("6p"), Some("7s"), None)
                .unwrap_or_else(|| panic!("no frame for {ev:?}"));
            let pkts = WPacket::parse_frame(&frame);
            assert_eq!(pkts.len(), 1, "one packet per frame for {ev:?}");
            assert_eq!(pkts[0].body["cmd"], CMD_GAME_ACTION);
        }
    }

    /// The chi variant code must say where the claimed tile sits in the run
    /// — a blanket wrong code gets acked but silently never applied.
    #[test]
    fn chi_variant_codes_match_the_offered_action_list() {
        // Claiming 6m keeping 7m8m → 6m is the low tile → 2 (verified manual).
        let low = MjaiEvent::Chi {
            actor: 0,
            target: 3,
            pai: "6m".into(),
            consumed: ["7m".into(), "8m".into()],
        };
        // Claiming 6m keeping 5m7m → middle → 3 (verified manual).
        let mid = MjaiEvent::Chi {
            actor: 0,
            target: 3,
            pai: "6m".into(),
            consumed: ["5m".into(), "7m".into()],
        };
        // Claiming 8p keeping 6p7p → high tile → 4.
        let high = MjaiEvent::Chi {
            actor: 0,
            target: 3,
            pai: "8p".into(),
            consumed: ["6p".into(), "7p".into()],
        };
        // Red five counts as its plain rank: claiming 3p keeping 4p5pr.
        let red = MjaiEvent::Chi {
            actor: 0,
            target: 3,
            pai: "3p".into(),
            consumed: ["4p".into(), "5pr".into()],
        };
        let code = |ev: &MjaiEvent| {
            WPacket::parse_frame(&encode_action(ev).unwrap())[0].body["data"]["action"].clone()
        };
        assert_eq!(code(&low), 2);
        assert_eq!(code(&mid), 3);
        assert_eq!(code(&high), 4);
        assert_eq!(code(&red), 2, "the incident case from the live game");

        // The full three-way case: claiming 5m holding 3m4m5m6m7m — all
        // three variants are offered and each pick maps to its own code.
        let pick = |a: &str, b: &str| MjaiEvent::Chi {
            actor: 0,
            target: 3,
            pai: "5m".into(),
            consumed: [a.into(), b.into()],
        };
        assert_eq!(code(&pick("3m", "4m")), 4, "3-4-5: claimed is high");
        assert_eq!(code(&pick("4m", "6m")), 3, "4-5-6: claimed is middle");
        assert_eq!(code(&pick("6m", "7m")), 2, "5-6-7: claimed is low");
    }

    /// Kan requests carry the consumed tiles' positions in the server-
    /// ordered hand; unresolvable positions omit the field instead of
    /// sending nonsense. Chi and pon carry nothing but the variant code
    /// and the claimed tile — the client's own claim buttons send exactly
    /// that, and the server derives the meld from our hand.
    #[test]
    fn calls_carry_hand_positions_when_known() {
        let hand: Vec<String> = [
            "1m", "2m", "4p", "5pr", "6s", "7s", "E", "E", "E", "W", "N", "P", "F",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let daiminkan = MjaiEvent::Daiminkan {
            actor: 0,
            target: 1,
            pai: "E".into(),
            consumed: ["E".into(), "E".into(), "E".into()],
        };
        let pkt = &WPacket::parse_frame(
            &encode_action_with(&daiminkan, None, None, Some(&hand)).unwrap(),
        )[0];
        assert_eq!(pkt.body["data"]["move_cards_pos"], json!([6, 7, 8]));
        assert_eq!(pkt.body["data"]["group_cards"], json!([0x31, 0x31, 0x31]));

        // Chi and pon stay bare even with a hand available.
        let pon = MjaiEvent::Pon {
            actor: 0,
            target: 1,
            pai: "E".into(),
            consumed: ["E".into(), "E".into()],
        };
        let pkt =
            &WPacket::parse_frame(&encode_action_with(&pon, None, None, Some(&hand)).unwrap())[0];
        assert_eq!(pkt.body["data"].as_object().unwrap().len(), 2);
        assert_eq!(pkt.body["data"]["action"], 5);
        assert_eq!(pkt.body["data"]["card"], 0x31);

        let chi = MjaiEvent::Chi {
            actor: 0,
            target: 1,
            pai: "3p".into(),
            consumed: ["4p".into(), "5pr".into()],
        };
        let unknown: Vec<String> = vec!["9s".to_string(); 13];
        let pkt =
            &WPacket::parse_frame(&encode_action_with(&chi, None, None, Some(&unknown)).unwrap())
                [0];
        assert!(pkt.body["data"].get("move_cards_pos").is_none());
        assert!(pkt.body["data"].get("group_cards").is_none());
    }

    /// Ankan carries three `group_cards` (the client's own comment: "the
    /// server only needs three") with the positions of all four, and the
    /// promoted-tile kakan carries only the tile and its position.
    #[test]
    fn kan_shapes_match_the_client_sender() {
        let hand: Vec<String> = [
            "1m", "2m", "5m", "5mr", "5m", "5m", "6s", "7s", "E", "S", "W", "N", "P",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let ankan = MjaiEvent::Ankan {
            actor: 0,
            consumed: ["5mr".into(), "5m".into(), "5m".into(), "5m".into()],
        };
        let pkt =
            &WPacket::parse_frame(&encode_action_with(&ankan, None, None, Some(&hand)).unwrap())[0];
        assert_eq!(pkt.body["data"]["action"], 8);
        assert_eq!(pkt.body["data"]["card"], 0x125);
        assert_eq!(pkt.body["data"]["group_cards"], json!([0x25, 0x25, 0x25]));
        // Positions follow the consumed list's order (the red five is
        // listed first and sits at hand index 3).
        assert_eq!(pkt.body["data"]["move_cards_pos"], json!([3, 2, 4, 5]));

        let kakan = MjaiEvent::Kakan {
            actor: 0,
            pai: "5p".into(),
            consumed: ["5p".into(), "5p".into(), "5p".into()],
        };
        let hand2: Vec<String> = vec!["5p".to_string(); 14];
        let pkt =
            &WPacket::parse_frame(&encode_action_with(&kakan, None, None, Some(&hand2)).unwrap())
                [0];
        assert_eq!(pkt.body["data"]["action"], 9);
        assert_eq!(pkt.body["data"]["card"], 0x05);
        assert!(pkt.body["data"].get("group_cards").is_none());
        assert_eq!(pkt.body["data"]["move_cards_pos"], json!([0]));
    }

    /// Kita is the bare north tile, per the client's `ActionPullNorth`
    /// offer construction.
    #[test]
    fn kita_is_bare() {
        let kita = MjaiEvent::Kita {
            actor: 0,
            pai: Some("N".into()),
        };
        let pkt = &WPacket::parse_frame(&encode_action(&kita).unwrap())[0];
        assert_eq!(pkt.body["data"].as_object().unwrap().len(), 2);
        assert_eq!(pkt.body["data"]["action"], 13);
        assert_eq!(pkt.body["data"]["card"], 0x61);
    }

    /// A ron names the claimed discard, mirroring the tsumo's winning
    /// tile — the client's claim buttons send `{action, card}`.
    #[test]
    fn ron_names_the_claimed_discard() {
        let ron = MjaiEvent::Hora {
            actor: 0,
            target: 2,
            deltas: None,
            ura_markers: None,
        };
        let pkt =
            &WPacket::parse_frame(&encode_action_with(&ron, None, Some("7s"), None).unwrap())[0];
        assert_eq!(pkt.body["data"]["action"], 7);
        assert_eq!(pkt.body["data"]["card"], 0x17);
        assert_eq!(pkt.body["data"].as_object().unwrap().len(), 2);
    }

    /// Verbatim from the recording: a 6p self-draw win went out as
    /// `{"action":10,"card":6}`; pass was a bare `{"action":1}`.
    #[test]
    fn tsumo_and_pass_match_the_recorded_shapes() {
        let tsumo = MjaiEvent::Hora {
            actor: 3,
            target: 3,
            deltas: None,
            ura_markers: None,
        };
        let pkt =
            &WPacket::parse_frame(&encode_action_with(&tsumo, Some("6p"), None, None).unwrap())[0];
        // Regression (2026-08-20 live test): frames built with binary cmd 0
        // are heartbeats — the server silently dropped every injected
        // action. Gameplay requests ride binary cmd 6.
        assert_eq!(pkt.cmd, 6, "gameplay requests must ride binary cmd 6");
        assert_eq!(pkt.body["data"]["action"], 10);
        assert_eq!(pkt.body["data"]["card"], 6);

        let pkt = &WPacket::parse_frame(&encode_action(&MjaiEvent::None).unwrap())[0];
        assert_eq!(pkt.body["data"]["action"], 1);
        assert_eq!(
            pkt.body["data"].as_object().unwrap().len(),
            1,
            "pass is bare"
        );
    }

    /// Verbatim from the recording: the riichi discard carried the full
    /// field set with `is_li_zhi: true`, and a tsumogiri used
    /// `move_cards_pos: [14,13]`.
    #[test]
    fn discard_shapes_match_the_recording() {
        let riichi = MjaiEvent::Reach {
            actor: 3,
            pai: Some("7m".into()),
        };
        let pkt = &WPacket::parse_frame(&encode_action(&riichi).unwrap())[0];
        assert_eq!(pkt.body["data"]["action"], 11);
        assert_eq!(pkt.body["data"]["is_li_zhi"], true);
        assert_eq!(pkt.body["data"]["is_xuan_gao_ting"], false);
        assert_eq!(pkt.body["data"]["li_zhi_operate"], 0);

        let tsumogiri = MjaiEvent::Dahai {
            actor: 3,
            pai: "N".into(),
            tsumogiri: true,
        };
        let pkt = &WPacket::parse_frame(&encode_action(&tsumogiri).unwrap())[0];
        assert_eq!(pkt.body["data"]["move_cards_pos"][0], 14);
        assert_eq!(pkt.body["data"]["move_cards_pos"][1], 13);
        assert_eq!(pkt.body["data"]["is_li_zhi"], false);
    }

    /// With the hand available, `move_cards_pos` follows the client's
    /// formula: the tile's 1-based hand position (the drawn tile sits
    /// last), and the client's own `hand.len() - 1` fallback for the
    /// animation target slot.
    #[test]
    fn discard_positions_follow_the_client_formula() {
        let hand: Vec<String> = [
            "1m", "2m", "3m", "4p", "5p", "6s", "7s", "8s", "9s", "E", "S", "W", "N", "F",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        // Tedashi of the 4p (0-based index 3 → 1-based 4).
        let tedashi = MjaiEvent::Dahai {
            actor: 0,
            pai: "4p".into(),
            tsumogiri: false,
        };
        let pkt =
            &WPacket::parse_frame(&encode_action_with(&tedashi, None, None, Some(&hand)).unwrap())
                [0];
        assert_eq!(pkt.body["data"]["move_cards_pos"], json!([4, 13]));

        // Tsumogiri always names the last slot.
        let tsumogiri = MjaiEvent::Dahai {
            actor: 0,
            pai: "F".into(),
            tsumogiri: true,
        };
        let pkt = &WPacket::parse_frame(
            &encode_action_with(&tsumogiri, None, None, Some(&hand)).unwrap(),
        )[0];
        assert_eq!(pkt.body["data"]["move_cards_pos"], json!([14, 13]));
    }

    /// The round-advance press must be a bare req_user_prepare on binary
    /// cmd 6, matching what the client sends after each scoring screen.
    #[test]
    fn user_prepare_matches_the_recorded_shape() {
        let pkt = &WPacket::parse_frame(&user_prepare())[0];
        assert_eq!(pkt.cmd, 6);
        assert_eq!(pkt.body["cmd"], "req_user_prepare");
        assert!(pkt.body.get("data").is_none(), "no data payload");
    }

    #[test]
    fn hidden_tile_is_refused() {
        let ev = MjaiEvent::Dahai {
            actor: 0,
            pai: "?".into(),
            tsumogiri: true,
        };
        assert!(encode_action(&ev).is_none());
    }

    #[test]
    fn non_actions_have_no_frame() {
        assert!(encode_action(&MjaiEvent::EndGame {
            reason: Default::default(),
            final_scores: None,
            final_ranks: None,
        })
        .is_none());
        // A reach without its declaring tile cannot be sent — same contract
        // as the Majsoul/Tenhou planners.
        assert!(encode_action(&MjaiEvent::Reach {
            actor: 0,
            pai: None
        })
        .is_none());
    }

    /// The inverse table must be exact: every code the inbound table maps to
    /// a known tile maps back to that code.
    #[test]
    fn tile_inverse_is_total_and_exact() {
        for code in 0u32..=0x125 {
            let m = super::super::consts::card_to_mjai(code);
            if m == "?" {
                continue;
            }
            assert_eq!(mjai_to_card(&m), Some(code), "round trip failed for {m}");
        }
    }
}
