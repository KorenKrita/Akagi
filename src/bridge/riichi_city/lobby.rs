//! Riichi City lobby HTTP client — matchmaking.
//!
//! Queueing does not ride the gameplay WebSocket: the client books ranked
//! matches over HTTPS against the same node its WS uses, authenticated by
//! the `sid` from the WS auth frame (captured on the inject bus). Endpoint
//! shapes are from the shipped Lua: POST `/lobbys/readStageClassifies` (no
//! body), `/lobbys/startStage` `{"classifyID"}`, `/lobbys/cancelStage`
//! `{"matchID"}` (matchIDs arrive as `cmd_stagematch_run` WS pushes).
//!
//! Auth is one `Cookies` header carrying a JSON object. Responses are
//! `{"code", "data"}` or `{"code", "analysis_data"}` with the payload
//! AES-256-encrypted (key literal from the Lua; cipher mode not visible,
//! so ECB then CBC-zero-IV is tried until one yields JSON). We request
//! `datatype: "0"` — a plaintext mode the client itself uses elsewhere —
//! and only decrypt when the server encrypts anyway.

use crate::autoplay::inject::LobbyCredentials;
use crate::config::{RiichiGameType, RiichiRoom};
use aes::cipher::generic_array::GenericArray;
use aes::cipher::{BlockDecrypt, KeyInit};
use anyhow::{anyhow, Context, Result};
use base64::engine::Engine as _;
use serde_json::{json, Value};
use std::path::Path;

/// The web node the client itself races to; debug-overridable via config.
pub const DEFAULT_WEB_BASE: &str = "https://aga-alb.mahjong-jp.net";

const ANALYSIS_KEY: &[u8; 32] = b"idpwepjzsjbg18sdf25as4bsefls944a";

/// One ranked queue class from `readStageClassifies`. Wire names:
/// stageType 1-4 = star/moon/sun/galaxy, round 1 = east-only / 2 = hanchan
/// (both confirmed against a live `cmd_stagematch_run` push).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Classify {
    pub id: String,
    pub stage_type: i64,
    pub round: i64,
    pub player_count: i64,
}

pub fn room_to_stage_type(room: RiichiRoom) -> i64 {
    match room {
        RiichiRoom::Star => 1,
        RiichiRoom::Moon => 2,
        RiichiRoom::Sun => 3,
        RiichiRoom::Galaxy => 4,
    }
}

pub fn game_type_to_round(game_type: RiichiGameType) -> i64 {
    match game_type {
        RiichiGameType::EastOnly => 1,
        RiichiGameType::Hanchan => 2,
    }
}

/// Pick the 4-player queue class for `room` + `game_type`, if offered.
pub fn select_classify(
    classifies: &[Classify],
    room: RiichiRoom,
    game_type: RiichiGameType,
) -> Option<&Classify> {
    classifies.iter().find(|c| {
        c.stage_type == room_to_stage_type(room)
            && c.round == game_type_to_round(game_type)
            && c.player_count == 4
    })
}

/// Human-facing reason for a refused `startStage`. Non-zero codes are
/// never retried: the interesting ones (identity-verification challenge,
/// AI-ban notice, AFK penalty) all mean "a human must act in the client".
pub fn start_stage_failure_reason(code: i64) -> String {
    match code {
        1313 => "the server demanded identity verification — solve the \
                 challenge in the Riichi City client, then start a new session"
            .to_string(),
        code => format!("the server refused the queue request (code {code})"),
    }
}

/// Full identity for the lobby API: the WS auth frame's fields plus the
/// persistent pseudo device id and the distribution channel.
#[derive(Debug, Clone)]
pub struct LobbyAuth {
    pub sid: String,
    pub lang: String,
    pub platform: String,
    pub version: String,
    pub deviceid: String,
    pub channel: String,
}

impl LobbyAuth {
    pub fn from_credentials(
        creds: &LobbyCredentials,
        deviceid: String,
        channel: String,
    ) -> Self {
        Self {
            sid: creds.sid.clone(),
            lang: creds.lang.clone(),
            platform: creds.platform.clone(),
            version: creds.version.clone(),
            deviceid,
            channel,
        }
    }

    /// Auth is one `Cookies` header carrying a JSON object; `datatype:
    /// "0"` asks for plaintext responses.
    pub fn cookies_header(&self) -> String {
        json!({
            "sid": self.sid,
            "datatype": "0",
            "platform": self.platform,
            "version": self.version,
            "lang": self.lang,
            "deviceid": self.deviceid,
            "channel": self.channel,
        })
        .to_string()
    }
}

/// A decoded lobby response.
#[derive(Debug)]
pub struct Envelope {
    pub code: i64,
    pub data: Value,
}

impl Envelope {
    /// Parse a response body, decrypting `analysis_data` when present.
    pub fn parse(body: &str) -> Result<Envelope> {
        let v: Value =
            serde_json::from_str(body).context("lobby response is not JSON")?;
        let code = v.get("code").and_then(Value::as_i64).unwrap_or(0);
        let data = match v.get("data") {
            Some(d) if !d.is_null() => d.clone(),
            _ => {
                let enc = v
                    .get("analysis_data")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        anyhow!("response carries neither data nor analysis_data")
                    })?;
                decrypt_analysis(enc)?
            }
        };
        Ok(Envelope { code, data })
    }
}

/// Decrypt an `analysis_data` payload. The cipher mode lives in native
/// code we cannot read, so try the two standard candidates and keep the
/// one that decrypts to valid JSON. The encrypted blob itself wraps
/// `{"data": …}` (the client re-assigns `ret_data.data = data.data`).
fn decrypt_analysis(b64: &str) -> Result<Value> {
    let ct = base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .context("analysis_data is not base64")?;
    let candidates = [
        aes_ecb_decrypt(&ct, ANALYSIS_KEY),
        aes_cbc_zero_iv_decrypt(&ct, ANALYSIS_KEY),
    ];
    for pt in candidates.into_iter().flatten() {
        if let Ok(v) = serde_json::from_slice::<Value>(&pt) {
            return Ok(v.get("data").cloned().unwrap_or(v));
        }
    }
    Err(anyhow!(
        "analysis_data decrypts under neither ECB nor CBC — {} bytes",
        ct.len()
    ))
}

fn unpad_pkcs7(buf: &mut Vec<u8>) -> bool {
    let Some(&n) = buf.last() else { return false };
    let n = n as usize;
    if n == 0 || n > 16 || n > buf.len() {
        return false;
    }
    if buf[buf.len() - n..].iter().any(|&b| b != n as u8) {
        return false;
    }
    buf.truncate(buf.len() - n);
    true
}

fn aes_ecb_decrypt(ct: &[u8], key: &[u8; 32]) -> Option<Vec<u8>> {
    if ct.is_empty() || !ct.len().is_multiple_of(16) {
        return None;
    }
    let cipher = aes::Aes256::new_from_slice(key).ok()?;
    let mut buf = ct.to_vec();
    for block in buf.as_chunks_mut::<16>().0 {
        cipher.decrypt_block(GenericArray::from_mut_slice(block));
    }
    unpad_pkcs7(&mut buf).then_some(buf)
}

fn aes_cbc_zero_iv_decrypt(ct: &[u8], key: &[u8; 32]) -> Option<Vec<u8>> {
    if ct.is_empty() || !ct.len().is_multiple_of(16) {
        return None;
    }
    let cipher = aes::Aes256::new_from_slice(key).ok()?;
    let snapshot = ct.to_vec();
    let mut buf = ct.to_vec();
    for i in (16..buf.len()).step_by(16) {
        cipher.decrypt_block(GenericArray::from_mut_slice(&mut buf[i..i + 16]));
        for j in 0..16 {
            buf[i + j] ^= snapshot[i - 16 + j];
        }
    }
    // First block's IV is all zeroes, so no XOR after its decryption.
    cipher.decrypt_block(GenericArray::from_mut_slice(&mut buf[0..16]));
    unpad_pkcs7(&mut buf).then_some(buf)
}

/// Load (or create) the persistent pseudo device id sent in the lobby
/// `Cookies` header. Deliberately not the client's real
/// `SystemInfo.deviceUniqueIdentifier` (unreadable) nor its
/// `riichi_key.txt` GUID (serves a separate device-binding purpose): the
/// official clients themselves send free-form ids (WebGL sends
/// `"webgl-<random>"`), so a stable random id reads as "this account
/// logged in from one more device" — the `sid` is the actual credential.
pub fn load_or_create_device_id(dir: &Path) -> Result<String> {
    let path = dir.join("riichi_city_deviceid");
    if let Ok(s) = std::fs::read_to_string(&path) {
        let t = s.trim();
        if t.len() == 32 && t.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Ok(t.to_string());
        }
    }
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut rng = rand::rng();
    let id: String = (0..32)
        .map(|_| {
            let b = rand::Rng::random_range(&mut rng, 0u8..16);
            HEX[b as usize] as char
        })
        .collect();
    std::fs::create_dir_all(dir).ok();
    std::fs::write(&path, &id).context("persist the lobby device id")?;
    Ok(id)
}

/// Thin HTTP client for the three lobby endpoints.
pub struct LobbyClient {
    http: reqwest::Client,
    base: String,
    auth: LobbyAuth,
}

impl LobbyClient {
    pub fn new(base: &str, auth: LobbyAuth) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(15))
                .connect_timeout(std::time::Duration::from_secs(10))
                .build()
                .expect("reqwest client builds"),
            base: base.trim_end_matches('/').to_string(),
            auth,
        }
    }

    async fn post(&self, path: &str, body: Option<Value>) -> Result<Envelope> {
        let mut req = self
            .http
            .post(format!("{}{path}", self.base))
            .header("Cookies", self.auth.cookies_header());
        if let Some(body) = body {
            req = req.json(&body);
        }
        let resp = req.send().await.with_context(|| format!("lobby {path}"))?;
        let status = resp.status();
        let text = resp.text().await.context("read the lobby response body")?;
        if !status.is_success() {
            let snippet: String = text.chars().take(120).collect();
            anyhow::bail!("lobby {path} -> HTTP {status}: {snippet}");
        }
        Envelope::parse(&text)
    }

    /// The ranked queue classes currently offered.
    pub async fn read_classifies(&self) -> Result<Vec<Classify>> {
        let env = self.post("/lobbys/readStageClassifies", None).await?;
        if env.code != 0 {
            anyhow::bail!("readStageClassifies refused (code {})", env.code);
        }
        let arr = env
            .data
            .as_array()
            .ok_or_else(|| anyhow!("readStageClassifies returned no list"))?;
        Ok(arr
            .iter()
            .filter_map(|c| {
                Some(Classify {
                    id: c.get("id")?.as_str()?.to_string(),
                    stage_type: c.get("stageType")?.as_i64()?,
                    round: c.get("round")?.as_i64()?,
                    player_count: c.get("playerCount")?.as_i64()?,
                })
            })
            .collect())
    }

    /// Join the ranked queue for `classify_id`.
    pub async fn start_stage(&self, classify_id: &str) -> Result<Envelope> {
        self.post(
            "/lobbys/startStage",
            Some(json!({ "classifyID": classify_id })),
        )
        .await
    }

    /// Leave the queue booked under `match_id`.
    pub async fn cancel_stage(&self, match_id: &str) -> Result<Envelope> {
        self.post("/lobbys/cancelStage", Some(json!({ "matchID": match_id })))
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aes::cipher::BlockEncrypt;

    fn credentials() -> LobbyCredentials {
        LobbyCredentials {
            sid: "da3p6g8h8t2s5j53ggs03740f8".into(),
            lang: "en".into(),
            platform: "pc".into(),
            version: "2.2.4.95474".into(),
        }
    }

    #[test]
    fn cookies_header_carries_the_auth_json() {
        let auth = LobbyAuth::from_credentials(
            &credentials(),
            "008de44c56a24828858e1e3998d8659c".into(),
            "steam".into(),
        );
        let v: Value = serde_json::from_str(&auth.cookies_header()).unwrap();
        assert_eq!(v["sid"], "da3p6g8h8t2s5j53ggs03740f8");
        assert_eq!(v["datatype"], "0");
        assert_eq!(v["platform"], "pc");
        assert_eq!(v["version"], "2.2.4.95474");
        assert_eq!(v["deviceid"], "008de44c56a24828858e1e3998d8659c");
        assert_eq!(v["channel"], "steam");
    }

    #[test]
    fn plain_envelope_parses() {
        let env =
            Envelope::parse(r#"{"code":0,"data":[{"id":"x","stageType":4}]}"#).unwrap();
        assert_eq!(env.code, 0);
        assert_eq!(env.data[0]["id"], "x");
    }

    fn pad(mut body: Vec<u8>) -> Vec<u8> {
        let n = 16 - body.len() % 16;
        body.extend(std::iter::repeat(n as u8).take(n));
        body
    }

    fn aes_ecb_encrypt(pt: &[u8], key: &[u8; 32]) -> Vec<u8> {
        let cipher = aes::Aes256::new_from_slice(key).unwrap();
        let mut buf = pad(pt.to_vec());
        for block in buf.chunks_exact_mut(16) {
            cipher.encrypt_block(GenericArray::from_mut_slice(block));
        }
        buf
    }

    fn aes_cbc_zero_iv_encrypt(pt: &[u8], key: &[u8; 32]) -> Vec<u8> {
        let cipher = aes::Aes256::new_from_slice(key).unwrap();
        let mut buf = pad(pt.to_vec());
        let mut prev = [0u8; 16]; // zero IV
        for i in (0..buf.len()).step_by(16) {
            for j in 0..16 {
                buf[i + j] ^= prev[j];
            }
            cipher.encrypt_block(GenericArray::from_mut_slice(&mut buf[i..i + 16]));
            prev.copy_from_slice(&buf[i..i + 16]);
        }
        buf
    }

    fn b64(bytes: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    #[test]
    fn encrypted_envelope_parses_under_ecb() {
        let payload = br#"{"data":{"list":[1,2,3]}}"#;
        let enc = aes_ecb_encrypt(payload, ANALYSIS_KEY);
        let body = format!(
            r#"{{"code":0,"analysis_data":"{}"}}"#,
            b64(&enc)
        );
        let env = Envelope::parse(&body).unwrap();
        // The blob's inner `data` is what surfaces.
        assert_eq!(env.data["list"][2], 3);
    }

    #[test]
    fn encrypted_envelope_parses_under_cbc() {
        let payload = br#"{"data":{"ok":true}}"#;
        let enc = aes_cbc_zero_iv_encrypt(payload, ANALYSIS_KEY);
        let body = format!(r#"{{"code":0,"analysis_data":"{}"}}"#, b64(&enc));
        let env = Envelope::parse(&body).unwrap();
        assert_eq!(env.data["ok"], true);
    }

    #[test]
    fn garbage_analysis_data_is_an_error_not_a_panic() {
        let body = r#"{"code":0,"analysis_data":"!!!notbase64!!!"}"#;
        assert!(Envelope::parse(body).is_err());
    }

    #[test]
    fn classify_selection_maps_room_and_game_type() {
        let classifies = vec![
            Classify { id: "star-e".into(), stage_type: 1, round: 1, player_count: 4 },
            Classify { id: "sun-h".into(), stage_type: 3, round: 2, player_count: 4 },
            Classify { id: "galaxy-e".into(), stage_type: 4, round: 1, player_count: 4 },
            Classify { id: "galaxy-h".into(), stage_type: 4, round: 2, player_count: 4 },
            Classify { id: "galaxy-3p".into(), stage_type: 4, round: 1, player_count: 3 },
        ];
        assert_eq!(
            select_classify(&classifies, RiichiRoom::Galaxy, RiichiGameType::EastOnly)
                .unwrap()
                .id,
            "galaxy-e"
        );
        assert_eq!(
            select_classify(&classifies, RiichiRoom::Galaxy, RiichiGameType::Hanchan)
                .unwrap()
                .id,
            "galaxy-h"
        );
        assert_eq!(
            select_classify(&classifies, RiichiRoom::Sun, RiichiGameType::Hanchan)
                .unwrap()
                .id,
            "sun-h"
        );
        // The 3p table must never match a 4p queue.
        assert!(select_classify(&classifies, RiichiRoom::Moon, RiichiGameType::EastOnly)
            .is_none());
    }

    #[test]
    fn failure_reasons_never_suggest_retrying() {
        assert!(start_stage_failure_reason(1313).contains("verification"));
        assert!(start_stage_failure_reason(7001).contains("7001"));
    }

    #[test]
    fn device_id_is_stable_once_created() {
        let dir = tempfile::tempdir().unwrap();
        let a = load_or_create_device_id(dir.path()).unwrap();
        let b = load_or_create_device_id(dir.path()).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.len(), 32);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));

        // Corrupt content is regenerated, not trusted.
        std::fs::write(dir.path().join("riichi_city_deviceid"), "junk").unwrap();
        let c = load_or_create_device_id(dir.path()).unwrap();
        assert_ne!(c, "junk");
        assert_eq!(c.len(), 32);
    }
}
