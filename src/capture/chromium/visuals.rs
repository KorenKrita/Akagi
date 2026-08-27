use std::sync::Arc;

use anyhow::{Context, Result};
use chromiumoxide::page::Page;
use serde::Serialize;
use tokio::{
    sync::{broadcast, Mutex},
    task::JoinHandle,
};
use tracing::{debug, warn};

use crate::analysis::{result::AnalysisResult, runner::AnalysisCache, Tile34};
use crate::autoplay::majsoul::coords::{get_pai_coord, TILES};
use crate::bot::BotResponse;
use crate::bridge::majsoul::tile::compare_pai;
use crate::event_bus::{AnalysisBus, BotResponseBus};
use crate::game_state::GameTracker;
use crate::schema::MjaiEvent;

const SCRIPT: &str = include_str!("visuals.js");

#[derive(Clone)]
pub struct VisualContext {
    pub show_danger: bool,
    pub show_recommendation: bool,
    pub bot_response_bus: BotResponseBus,
    pub analysis_bus: AnalysisBus,
    pub analysis_cache: AnalysisCache,
    pub game_tracker: Arc<Mutex<GameTracker>>,
}

#[derive(Clone, Debug)]
struct Slot {
    tile: String,
    x: f64,
    y: f64,
    drawn: bool,
}

#[derive(Serialize)]
struct RiskSlot {
    tile: String,
    risk: f64,
}

#[derive(Serialize)]
struct Recommendation<'a> {
    x: f64,
    y: f64,
    label: &'a str,
}

pub async fn install(page: &Page, ctx: VisualContext) -> Result<JoinHandle<()>> {
    page.evaluate_on_new_document(SCRIPT)
        .await
        .context("install game visuals for new documents")?;
    page.evaluate(SCRIPT)
        .await
        .context("install game visuals in current document")?;

    let analyses = ctx.analysis_bus.subscribe();
    let responses = ctx.bot_response_bus.subscribe();
    if ctx.show_danger {
        if let Some(analysis) = ctx.analysis_cache.read().await.clone() {
            update_risk(page, &ctx, &analysis).await;
        }
    }

    let page = page.clone();
    Ok(tokio::spawn(async move {
        let mut analyses = analyses;
        let mut responses = responses;
        let mut analysis_open = ctx.show_danger;
        let mut response_open = ctx.show_recommendation;

        while analysis_open || response_open {
            tokio::select! {
                result = analyses.recv(), if analysis_open => match result {
                    Ok(analysis) => update_risk(&page, &ctx, &analysis).await,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => analysis_open = false,
                },
                result = responses.recv(), if response_open => match result {
                    Ok(response) => update_recommendation(&page, &ctx, &response).await,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => response_open = false,
                },
            }
        }
    }))
}

pub async fn clear_recommendation(page: &Page) {
    if let Err(e) = page
        .evaluate("window.__akagiGameVisuals?.clearRecommendation()")
        .await
    {
        debug!("game visuals: failed to clear recommendation: {e:#}");
    }
}

pub async fn clear_risk(page: &Page) {
    if let Err(e) = page
        .evaluate("window.__akagiGameVisuals?.clearRisk()")
        .await
    {
        debug!("game visuals: failed to clear risk: {e:#}");
    }
}

async fn update_risk(page: &Page, ctx: &VisualContext, analysis: &AnalysisResult) {
    let slots = current_slots(ctx).await;
    let payload: Vec<RiskSlot> = slots
        .into_iter()
        .filter_map(|slot| {
            let idx = Tile34::from_mjai(&slot.tile)?.idx() as usize;
            Some(RiskSlot {
                tile: slot.tile,
                risk: *analysis.mixed_risk.get(idx)?,
            })
        })
        .collect();
    evaluate_json(page, "setRisk", &payload).await;
}

async fn update_recommendation(page: &Page, ctx: &VisualContext, response: &BotResponse) {
    let tile = match &response.action {
        MjaiEvent::Dahai { pai, .. } => Some(pai.as_str()),
        MjaiEvent::Reach { pai: Some(pai), .. } => Some(pai.as_str()),
        _ => None,
    };
    let Some(tile) = tile else {
        clear_recommendation(page).await;
        return;
    };
    let slots = current_slots(ctx).await;
    if let Some(slot) = recommendation_slot(&slots, tile) {
        evaluate_json(
            page,
            "setRecommendation",
            &Recommendation {
                x: slot.x,
                y: slot.y,
                label: "AI",
            },
        )
        .await;
    }
}

async fn current_slots(ctx: &VisualContext) -> Vec<Slot> {
    let snapshot = ctx.game_tracker.lock().await.snapshot();
    let Some(snapshot) = snapshot else {
        return Vec::new();
    };
    let Some(seat) = snapshot.our_seat else {
        return Vec::new();
    };
    let Some(player) = snapshot.players.get(seat as usize) else {
        return Vec::new();
    };
    let dealer_first = snapshot.oya == seat
        && player.tehai.len() == 14
        && player.river.is_empty()
        && player.melds.is_empty()
        && player.kita_tiles.is_empty();
    hand_slots(&player.tehai, player.drawn_tile.as_deref(), dealer_first)
}

fn hand_slots(hand: &[String], drawn_tile: Option<&str>, dealer_first: bool) -> Vec<Slot> {
    let mut tiles = hand.to_vec();
    tiles.sort_by(|a, b| compare_pai(a, b));

    let has_drawn_slot = !dealer_first && matches!(hand.len(), 14 | 11 | 8 | 5 | 2);
    let drawn = drawn_tile.filter(|_| has_drawn_slot).and_then(|tile| {
        let idx = tiles.iter().rposition(|candidate| candidate == tile)?;
        Some(tiles.remove(idx))
    });
    let closed_len = tiles.len();
    let mut slots: Vec<Slot> = tiles
        .into_iter()
        .zip(TILES)
        .map(|(tile, (x, y))| Slot {
            tile,
            x,
            y,
            drawn: false,
        })
        .collect();
    if let Some(tile) = drawn {
        let (x, y) = get_pai_coord(13, closed_len);
        slots.push(Slot {
            tile,
            x,
            y,
            drawn: true,
        });
    }
    slots
}

fn recommendation_slot<'a>(slots: &'a [Slot], tile: &str) -> Option<&'a Slot> {
    slots
        .iter()
        .filter(|slot| same_tile(&slot.tile, tile))
        .max_by_key(|slot| (slot.tile == tile, slot.drawn))
}

fn same_tile(a: &str, b: &str) -> bool {
    a == b
        || Tile34::from_mjai(a)
            .zip(Tile34::from_mjai(b))
            .is_some_and(|(a, b)| a == b)
}

async fn evaluate_json<T: Serialize>(page: &Page, method: &str, value: &T) {
    let Ok(json) = serde_json::to_string(value) else {
        return;
    };
    let script = format!("window.__akagiGameVisuals?.{method}({json})");
    if let Err(e) = page.evaluate(script).await {
        warn!("game visuals: {method} failed: {e:#}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drawn_duplicate_uses_separate_slot_and_is_recommended() {
        let hand = [
            "1m", "2m", "3m", "4m", "5m", "5m", "6m", "7m", "8m", "9m", "1p", "2p", "3p", "5m",
        ]
        .map(str::to_string);
        let slots = hand_slots(&hand, Some("5m"), false);
        let recommended = recommendation_slot(&slots, "5m").unwrap();
        assert!(recommended.drawn);
        assert_eq!((recommended.x, recommended.y), get_pai_coord(13, 13));
    }

    #[test]
    fn dealer_opening_hand_has_no_draw_gap() {
        let hand = [
            "1m", "2m", "3m", "4m", "5m", "6m", "7m", "8m", "9m", "1p", "2p", "3p", "4p", "5p",
        ]
        .map(str::to_string);
        let slots = hand_slots(&hand, Some("5p"), true);
        assert_eq!((slots[13].x, slots[13].y), TILES[13]);
        assert!(!slots[13].drawn);
    }

    #[test]
    fn red_five_uses_normal_five_risk_index() {
        assert_eq!(
            Tile34::from_mjai("5mr").unwrap().idx(),
            Tile34::from_mjai("5m").unwrap().idx()
        );
    }

    #[test]
    fn script_hooks_webgl_material_colour() {
        for required in [
            "getContext",
            "useProgram",
            "bindTexture",
            "drawElements",
            "_MainTex_ST",
            "_Tint",
        ] {
            assert!(SCRIPT.contains(required), "missing {required}");
        }
        assert!(SCRIPT.contains("Math.abs(st[0] - .1)"));
        assert!(SCRIPT.contains("Math.abs(st[1] - .25)"));
        assert!(!SCRIPT.contains("getUniformLocation(program, '_Color')"));
        assert!(
            SCRIPT.find("if (hi < 0)").unwrap() < SCRIPT.find("if (hi === 0)").unwrap(),
            "over-range risks must clamp to red before the zero-risk branch"
        );
        assert!(
            !SCRIPT.contains("border: `4px"),
            "risk must not use overlay boxes"
        );
    }
}
