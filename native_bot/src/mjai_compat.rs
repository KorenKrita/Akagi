//! Compatibility fixes applied to incoming mjai events before they reach a
//! riichienv-core game state.
//!
//! Shared by the offline extractor ([`crate::replay`]) and the live engine
//! ([`crate::engine`]), so a log that replays cleanly can also be fed to the
//! engine without panicking.

use riichienv_core::replay::MjaiEvent;

/// Tenhou sanma logs use a 4-seat layout: `start_kyoku`/`hora`/`ryukyoku`
/// carry 4-element `scores`/`tehais`/`delta` (the 4th is a dummy dead seat),
/// which a 3-seat `GameState3P` would index out of bounds. Truncate them to 3.
///
/// A no-op for 4-player events, and for 3-element arrays that are already the
/// right shape — so it is safe to run over every event of a sanma stream.
pub fn sanitize_3p(ev: &mut MjaiEvent) {
    match ev {
        MjaiEvent::StartKyoku { scores, tehais, .. } => {
            scores.truncate(3);
            tehais.truncate(3);
        }
        MjaiEvent::Hora { delta, scores, .. } => {
            if let Some(d) = delta {
                d.truncate(3);
            }
            if let Some(s) = scores {
                s.truncate(3);
            }
        }
        MjaiEvent::Ryukyoku {
            delta,
            scores,
            tehais,
            ..
        } => {
            if let Some(d) = delta {
                d.truncate(3);
            }
            if let Some(s) = scores {
                s.truncate(3);
            }
            if let Some(t) = tehais {
                t.truncate(3);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(line: &str) -> MjaiEvent {
        serde_json::from_str(line).expect("valid mjai event")
    }

    /// 13 tiles, as a JSON array literal.
    fn hand13() -> String {
        r#"["1p","2p","3p","4p","5p","6p","7p","8p","9p","1s","2s","3s","4s"]"#.to_string()
    }

    #[test]
    fn start_kyoku_truncated_to_three_seats() {
        let h = hand13();
        let mut e = ev(&format!(
            r#"{{"type":"start_kyoku","bakaze":"E","dora_marker":"1s","kyoku":1,"honba":0,
                 "kyotaku":0,"oya":0,"scores":[35000,35000,35000,0],
                 "tehais":[{h},{h},{h},{h}]}}"#
        ));
        sanitize_3p(&mut e);
        match e {
            MjaiEvent::StartKyoku { scores, tehais, .. } => {
                assert_eq!(scores.len(), 3);
                assert_eq!(tehais.len(), 3);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn already_three_seats_is_untouched() {
        let h = hand13();
        let mut e = ev(&format!(
            r#"{{"type":"start_kyoku","bakaze":"E","dora_marker":"1s","kyoku":1,"honba":0,
                 "kyotaku":0,"oya":0,"scores":[35000,35000,35000],"tehais":[{h},{h},{h}]}}"#
        ));
        sanitize_3p(&mut e);
        match e {
            MjaiEvent::StartKyoku { scores, tehais, .. } => {
                assert_eq!(scores.len(), 3);
                assert_eq!(tehais.len(), 3);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn hora_and_ryukyoku_arrays_truncated() {
        let mut e = ev(r#"{"type":"hora","actor":0,"target":1,"pai":"1m",
                "scores":[35000,35000,35000,0],"deltas":[1000,-1000,0,0]}"#);
        sanitize_3p(&mut e);
        match e {
            MjaiEvent::Hora { scores, delta, .. } => {
                assert_eq!(scores.unwrap().len(), 3);
                assert_eq!(delta.unwrap().len(), 3);
            }
            other => panic!("unexpected: {other:?}"),
        }

        let h = hand13();
        let mut e = ev(&format!(
            r#"{{"type":"ryukyoku","reason":"fanpai","tehais":[{h},{h},{h},{h}],
                 "scores":[35000,35000,35000,0],"deltas":[0,0,0,0]}}"#
        ));
        sanitize_3p(&mut e);
        match e {
            MjaiEvent::Ryukyoku {
                scores,
                delta,
                tehais,
                ..
            } => {
                assert_eq!(scores.unwrap().len(), 3);
                assert_eq!(delta.unwrap().len(), 3);
                assert_eq!(tehais.unwrap().len(), 3);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn other_events_pass_through() {
        let mut e = ev(r#"{"type":"dahai","actor":1,"pai":"1m","tsumogiri":false}"#);
        sanitize_3p(&mut e);
        assert!(matches!(e, MjaiEvent::Dahai { actor: 1, .. }));
    }
}
