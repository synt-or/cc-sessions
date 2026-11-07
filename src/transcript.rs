//! Lecture en flux des transcripts : vrais messages de l'utilisateur, recaps,
//! échanges et durées de tour. Les fichiers vont jusqu'à 350 Mo : rien n'est
//! chargé en entier, et un préfiltre sur les octets évite le parse JSON.
use chrono::DateTime;
use memchr::memmem;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Seek, SeekFrom};
use std::path::Path;

const RECAP_SUFFIX: &str = "(disable recaps in /config)";
const MAX_USER_EXCHANGES: usize = 10;
const MAX_EXCHANGE_CHARS: usize = 1500;

/// Ce que le cache retient d'un transcript pour trier sans le relire.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Digest {
    pub real_count: u32,
    pub last_real_at_ms: Option<i64>,
    /// Position juste après la dernière ligne complète lue.
    pub offset: u64,
    /// Titre donné à la main (`/rename`), s'il existe.
    #[serde(default)]
    pub custom_title: Option<String>,
    /// Titre généré par Claude Code. Lu ici car il peut se trouver loin de la
    /// fin dans un très gros transcript.
    #[serde(default)]
    pub ai_title: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Recap {
    pub at_ms: i64,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Exchange {
    pub at_ms: i64,
    pub from_user: bool,
    pub text: String,
}

/// Le contenu d'une session, calculé à son ouverture dans la revue.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionDetail {
    pub recaps: Vec<Recap>,
    pub exchanges: Vec<Exchange>,
    pub real_at_ms: Vec<i64>,
    /// (horodatage, durée) de chaque tour.
    pub turns: Vec<(i64, u64)>,
}

fn first_text(v: &Value) -> Option<&str> {
    match v.get("message")?.get("content")? {
        Value::String(s) => Some(s),
        Value::Array(parts) => parts.iter().find_map(|p| {
            (p.get("type")?.as_str()? == "text")
                .then(|| p.get("text")?.as_str())
                .flatten()
        }),
        _ => None,
    }
}

fn flag(v: &Value, key: &str) -> bool {
    v.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// Une ligne est-elle un message écrit par l'utilisateur ?
///
/// `origin`, quand il existe, fait foi : un message collé commence par
/// `<pasted_content` et reste bien de lui. Les lignes anciennes ou lancées par
/// le SDK n'ont pas d'`origin` : on écarte alors les balises et les commandes.
pub fn is_real_message(v: &Value) -> bool {
    if v.get("type").and_then(Value::as_str) != Some("user")
        || flag(v, "isMeta")
        || flag(v, "isSidechain")
        || flag(v, "isCompactSummary")
    {
        return false;
    }
    let Some(text) = first_text(v) else {
        return false;
    };
    let t = text.trim_start();
    // Une commande tapée (`/note`, `/compact`) ou un Échap ne sont pas une reprise.
    if t.starts_with("<command-") || t.starts_with("[Request interrupted") {
        return false;
    }
    match v.get("origin").filter(|o| !o.is_null()) {
        Some(origin) => origin.get("kind").and_then(Value::as_str) == Some("human"),
        None => !t.is_empty() && !t.starts_with('<') && !t.starts_with('/'),
    }
}

fn timestamp_ms(v: &Value) -> Option<i64> {
    let s = v.get("timestamp")?.as_str()?;
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.timestamp_millis())
}

/// Parcourt les lignes complètes depuis `start`, renvoie l'offset final.
/// Une dernière ligne sans `\n` est en cours d'écriture : elle sera relue.
fn for_each_line(path: &Path, start: u64, mut on_line: impl FnMut(&[u8])) -> io::Result<u64> {
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(start))?;
    let mut reader = BufReader::with_capacity(1 << 20, file);
    let mut buf = Vec::new();
    let mut offset = start;
    loop {
        buf.clear();
        let read = reader.read_until(b'\n', &mut buf)?;
        if read == 0 || buf.last() != Some(&b'\n') {
            return Ok(offset);
        }
        offset += read as u64;
        on_line(&buf);
    }
}

/// Met à jour un digest en lisant le fichier depuis son offset (ou le début).
pub fn read_digest(path: &Path, from: Option<Digest>) -> io::Result<Digest> {
    let mut digest = from.unwrap_or_default();
    let needle = memmem::Finder::new(br#""type":"user""#);
    let mut real_count = digest.real_count;
    let mut last = digest.last_real_at_ms;
    let custom = memmem::Finder::new(br#""type":"custom-title""#);
    let ai = memmem::Finder::new(br#""type":"ai-title""#);
    let mut custom_title = digest.custom_title.take();
    let mut ai_title = digest.ai_title.take();
    let title_of = |line: &[u8], key: &str| -> Option<String> {
        let v: Value = serde_json::from_slice(line).ok()?;
        v.get(key)?.as_str().map(String::from)
    };
    digest.offset = for_each_line(path, digest.offset, |line| {
        if custom.find(line).is_some() {
            custom_title = title_of(line, "customTitle").or(custom_title.take());
            return;
        }
        if ai.find(line).is_some() {
            ai_title = title_of(line, "aiTitle").or(ai_title.take());
            return;
        }
        if needle.find(line).is_none() {
            return;
        }
        if let Ok(v) = serde_json::from_slice::<Value>(line) {
            if is_real_message(&v) {
                real_count += 1;
                last = timestamp_ms(&v).or(last);
            }
        }
    })?;
    digest.real_count = real_count;
    digest.last_real_at_ms = last;
    digest.custom_title = custom_title;
    digest.ai_title = ai_title;
    Ok(digest)
}

fn truncated(text: &str) -> String {
    let mut chars = text.chars();
    let mut out: String = chars.by_ref().take(MAX_EXCHANGE_CHARS).collect();
    if chars.next().is_some() {
        out.push('…');
    }
    out
}

fn assistant_text(v: &Value) -> Option<String> {
    let parts = v.get("message")?.get("content")?;
    let text = match parts {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter(|p| p.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => return None,
    };
    (!text.trim().is_empty()).then_some(text)
}

/// Lit tout ce que l'écran de revue affiche d'une session.
pub fn read_detail(path: &Path) -> io::Result<SessionDetail> {
    let mut detail = SessionDetail::default();
    let mut exchanges: VecDeque<Exchange> = VecDeque::new();
    let user = memmem::Finder::new(br#""type":"user""#);
    let assistant = memmem::Finder::new(br#""type":"assistant""#);
    let recap = memmem::Finder::new(br#""away_summary""#);
    let turn = memmem::Finder::new(br#""turn_duration""#);
    for_each_line(path, 0, |line| {
        if user.find(line).is_none()
            && assistant.find(line).is_none()
            && recap.find(line).is_none()
            && turn.find(line).is_none()
        {
            return;
        }
        let Ok(v) = serde_json::from_slice::<Value>(line) else {
            return;
        };
        let at_ms = timestamp_ms(&v).unwrap_or(0);
        match v.get("type").and_then(Value::as_str) {
            Some("user") if is_real_message(&v) => {
                detail.real_at_ms.push(at_ms);
                exchanges.push_back(Exchange {
                    at_ms,
                    from_user: true,
                    text: truncated(first_text(&v).unwrap_or_default()),
                });
                while exchanges.iter().filter(|e| e.from_user).count() > MAX_USER_EXCHANGES {
                    exchanges.pop_front();
                }
            }
            Some("assistant") if !flag(&v, "isSidechain") => {
                if let Some(text) = assistant_text(&v) {
                    let exchange = Exchange {
                        at_ms,
                        from_user: false,
                        text: truncated(&text),
                    };
                    // Plusieurs messages dans un même tour : seul le dernier
                    // sert à décider, les autres annoncent des appels d'outils.
                    match exchanges.back_mut() {
                        Some(last) if !last.from_user => *last = exchange,
                        _ => exchanges.push_back(exchange),
                    }
                }
            }
            Some("system") => match v.get("subtype").and_then(Value::as_str) {
                Some("away_summary") => {
                    let text = v.get("content").and_then(Value::as_str).unwrap_or_default();
                    detail.recaps.push(Recap {
                        at_ms,
                        text: text.trim_end_matches(RECAP_SUFFIX).trim().to_string(),
                    });
                }
                Some("turn_duration") => {
                    let ms = v.get("durationMs").and_then(Value::as_u64).unwrap_or(0);
                    detail.turns.push((at_ms, ms));
                }
                _ => {}
            },
            _ => {}
        }
    })?;
    detail.exchanges = exchanges.into();
    Ok(detail)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;

    fn write(name: &str, body: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("cs_transcript_test");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, body).unwrap();
        p
    }

    fn user(ts: &str, text: &str) -> String {
        format!(
            "{}\n",
            json!({"type":"user","timestamp":ts,"origin":{"kind":"human"},"message":{"content":text}})
        )
    }

    #[test]
    fn human_origin_with_pasted_content_is_real() {
        let v = json!({"type":"user","origin":{"kind":"human"},
            "message":{"content":"  <pasted_content id=\"a\">x</pasted_content>"}});
        assert!(is_real_message(&v));
    }

    #[test]
    fn task_notification_origin_is_not_real() {
        let v = json!({"type":"user","origin":{"kind":"task-notification"},
            "message":{"content":"bonjour"}});
        assert!(!is_real_message(&v));
    }

    #[test]
    fn no_origin_uses_text_heuristic() {
        let msg = |t: &str| json!({"type":"user","message":{"content":t}});
        assert!(is_real_message(&msg("bonjour")));
        assert!(!is_real_message(&msg("<command-name>/clear</command-name>")));
        assert!(!is_real_message(&msg("/compact")));
        assert!(!is_real_message(&msg("  ")));
    }

    #[test]
    fn interruption_and_typed_command_are_not_real() {
        let none = json!({"type":"user","message":{"content":"[Request interrupted by user]"}});
        assert!(!is_real_message(&none));
        let cmd = json!({"type":"user","origin":{"kind":"human"},
            "message":{"content":"<command-message>note</command-message>"}});
        assert!(!is_real_message(&cmd));
    }

    #[test]
    fn tool_result_only_is_not_real() {
        let v = json!({"type":"user","message":{"content":[{"type":"tool_result","content":"x"}]}});
        assert!(!is_real_message(&v));
    }

    #[test]
    fn text_in_array_is_real() {
        let v = json!({"type":"user","message":{"content":[{"type":"text","text":"salut"}]}});
        assert!(is_real_message(&v));
    }

    #[test]
    fn meta_sidechain_compact_are_not_real() {
        for key in ["isMeta", "isSidechain", "isCompactSummary"] {
            let mut v = json!({"type":"user","message":{"content":"salut"}});
            v[key] = json!(true);
            assert!(!is_real_message(&v), "{key}");
        }
    }

    #[test]
    fn digest_counts_and_finds_last_real() {
        let body = format!(
            "{}{}{}",
            user("2026-09-11T17:11:03.682Z", "un"),
            user("2026-09-12T10:00:00.000Z", "deux"),
            // ligne système plus tardive : ne déplace pas l'activité
            "{\"type\":\"user\",\"timestamp\":\"2026-10-20T13:35:44.502Z\",\"origin\":{\"kind\":\"task-notification\"},\"message\":{\"content\":\"<task>\"}}\n"
        );
        let d = read_digest(&write("digest.jsonl", &body), None).unwrap();
        assert_eq!(d.real_count, 2);
        assert_eq!(
            d.last_real_at_ms,
            Some(DateTime::parse_from_rfc3339("2026-09-12T10:00:00Z").unwrap().timestamp_millis())
        );
        assert_eq!(d.offset, body.len() as u64);
    }

    #[test]
    fn digest_keeps_latest_titles() {
        let body = concat!(
            "{\"type\":\"ai-title\",\"aiTitle\":\"Vieux\"}\n",
            "{\"type\":\"ai-title\",\"aiTitle\":\"Récent\"}\n",
            "{\"type\":\"custom-title\",\"customTitle\":\"Le mien\"}\n",
        );
        let d = read_digest(&write("titles.jsonl", body), None).unwrap();
        assert_eq!(d.ai_title.as_deref(), Some("Récent"));
        assert_eq!(d.custom_title.as_deref(), Some("Le mien"));
    }

    #[test]
    fn digest_resumes_from_offset() {
        let first = user("2026-09-11T10:00:00.000Z", "un");
        let p = write("resume.jsonl", &first);
        let d1 = read_digest(&p, None).unwrap();
        // Le début est remplacé : s'il était relu, le compte serait différent.
        let garbage = "x".repeat(first.len() - 1) + "\n";
        std::fs::write(&p, format!("{garbage}{}", user("2026-09-12T10:00:00.000Z", "deux"))).unwrap();
        let d2 = read_digest(&p, Some(d1)).unwrap();
        assert_eq!(d2.real_count, 2);
    }

    #[test]
    fn digest_ignores_partial_last_line() {
        let done = user("2026-09-11T10:00:00.000Z", "un");
        let p = write("partial.jsonl", &format!("{done}{{\"type\":\"user\""));
        let d = read_digest(&p, None).unwrap();
        assert_eq!(d.real_count, 1);
        assert_eq!(d.offset, done.len() as u64);
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        writeln!(f, ",\"timestamp\":\"2026-09-12T10:00:00.000Z\",\"message\":{{\"content\":\"suite\"}}}}").unwrap();
        assert_eq!(read_digest(&p, Some(d)).unwrap().real_count, 2);
    }

    #[test]
    fn detail_strips_recap_suffix() {
        let body = "{\"type\":\"system\",\"subtype\":\"away_summary\",\"timestamp\":\"2026-10-15T14:55:35.100Z\",\"content\":\"Je charge. (disable recaps in /config)\"}\n";
        let d = read_detail(&write("recap.jsonl", body)).unwrap();
        assert_eq!(d.recaps.len(), 1);
        assert_eq!(d.recaps[0].text, "Je charge.");
    }

    #[test]
    fn detail_sums_turn_durations() {
        let body = "{\"type\":\"system\",\"subtype\":\"turn_duration\",\"durationMs\":55552,\"timestamp\":\"2026-08-13T13:16:44.063Z\"}\n";
        let d = read_detail(&write("turn.jsonl", body)).unwrap();
        assert_eq!(d.turns.len(), 1);
        assert_eq!(d.turns[0].1, 55552);
    }

    #[test]
    fn exchanges_exclude_tools_and_keep_order() {
        let body = format!(
            "{}{}{}{}",
            user("2026-09-11T10:00:00.000Z", "question"),
            "{\"type\":\"assistant\",\"timestamp\":\"2026-09-11T10:00:01.000Z\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"je regarde\"},{\"type\":\"tool_use\",\"name\":\"Bash\"}]}}\n",
            "{\"type\":\"assistant\",\"timestamp\":\"2026-09-11T10:00:02.000Z\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"name\":\"Read\"}]}}\n",
            "{\"type\":\"assistant\",\"timestamp\":\"2026-09-11T10:00:03.000Z\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"réponse finale\"}]}}\n",
        );
        let d = read_detail(&write("ex.jsonl", &body)).unwrap();
        let texts: Vec<_> = d.exchanges.iter().map(|e| (e.from_user, e.text.as_str())).collect();
        assert_eq!(texts, vec![(true, "question"), (false, "réponse finale")]);
    }

    #[test]
    fn exchanges_keep_only_last_ten_user_messages() {
        let body: String = (0..15).map(|i| user("2026-09-11T10:00:00.000Z", &format!("m{i}"))).collect();
        let d = read_detail(&write("ten.jsonl", &body)).unwrap();
        assert_eq!(d.exchanges.len(), 10);
        assert_eq!(d.exchanges[0].text, "m5");
        assert_eq!(d.real_at_ms.len(), 15);
    }

    #[test]
    fn long_exchange_is_shortened() {
        let d = read_detail(&write("long.jsonl", &user("2026-09-11T10:00:00.000Z", &"é".repeat(2000)))).unwrap();
        assert_eq!(d.exchanges[0].text.chars().count(), MAX_EXCHANGE_CHARS + 1);
    }
}
