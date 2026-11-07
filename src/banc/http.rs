//! Adaptateurs joignables par une simple adresse : LM Studio et API SystemOne.
//! Un modèle peut donc changer de machine sans changer de code. La place du contenu se
//! déduit de l'adresse, jamais d'une déclaration.
use crate::adapter::{
    host_of, instructions, parse_text_answer, is_loopback, probabilities_from, response_schema, validate, Classifier,
    Prediction,
};
use crate::categories::CATEGORIES;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::cell::OnceCell;
use std::io::Read;
use std::time::{Duration, Instant};

const MAX_RESPONSE_BYTES: u64 = 8 * 1024 * 1024;
const SECURITY_BIN: &str = "/usr/bin/security";

fn post(url: &str, api_key: Option<&str>, body: &Value, timeout: Duration) -> Result<Value, String> {
    // Une clé ne part jamais en clair, sauf vers la machine elle-même.
    if api_key.is_some() && !url.starts_with("https://") && !is_loopback(url) {
        return Err("clé refusée sur une adresse non chiffrée".into());
    }
    // Sans redirection : une réponse venue d'ailleurs ne vaut pas prédiction.
    let agent = ureq::AgentBuilder::new().timeout(timeout).redirects(0).build();
    let mut request = agent.post(url);
    if let Some(key) = api_key {
        request = request.set("Authorization", &format!("Bearer {key}"));
    }
    let response = request
        .send_json(body)
        // L'erreur ne reprend ni le corps envoyé ni la réponse : l'instantané reste privé.
        .map_err(|e| match e {
            ureq::Error::Status(code, _) => format!("réponse HTTP {code}"),
            ureq::Error::Transport(t) => format!("transport : {}", t.kind()),
        })?;
    if response.status() != 200 {
        return Err(format!("réponse HTTP {}", response.status()));
    }
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(MAX_RESPONSE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "réponse interrompue".to_string())?;
    if bytes.len() as u64 > MAX_RESPONSE_BYTES {
        return Err("réponse trop longue".into());
    }
    serde_json::from_slice(&bytes).map_err(|_| "réponse illisible".to_string())
}

const MAX_VERSION_CHARS: usize = 64;

/// La version annoncée vient du serveur : elle part dans les fichiers de résultat,
/// donc seulement sous une forme courte et sans texte libre.
fn version_from(answer: &Value) -> Option<String> {
    let version = answer["model"].as_str()?;
    let valid = |c: char| c.is_ascii_alphanumeric() || "._-/:".contains(c);
    (version.len() <= MAX_VERSION_CHARS && version.chars().all(valid)).then(|| version.to_string())
}

fn fingerprint(parts: &[&str]) -> String {
    let digest = Sha256::digest(parts.join("\n").as_bytes());
    format!("{digest:x}")
}

fn catalog_text() -> String {
    CATEGORIES.iter().map(|(k, c)| format!("{k}={c}")).collect::<Vec<_>>().join(";")
}

/// Un modèle de chat servi par LM Studio : Qwen, sur `localhost:1234`.
pub struct LmStudio {
    pub label: String,
    pub base_url: String,
    pub model: String,
    pub timeout: Duration,
}

impl LmStudio {
    pub fn qwen(base_url: &str) -> LmStudio {
        LmStudio {
            label: "qwen".into(),
            base_url: base_url.trim_end_matches('/').into(),
            model: "qwen/qwen3.5-9b".into(),
            timeout: Duration::from_secs(600),
        }
    }
}

impl Classifier for LmStudio {
    fn name(&self) -> &str {
        &self.label
    }

    fn target(&self) -> String {
        host_of(&self.base_url)
    }

    fn preamble(&self) -> String {
        format!("modèle {}, consignes :\n{}", self.model, instructions())
    }

    fn signature(&self) -> String {
        fingerprint(&[&self.label, &self.base_url, &self.model, &instructions()])
    }

    fn predict(&self, text: &str) -> Result<Prediction, String> {
        let start = Instant::now();
        let body = json!({
            "model": self.model,
            "temperature": 0,
            "messages": [
                {"role": "system", "content": instructions()},
                {"role": "user", "content": text},
            ],
            "response_format": {
                "type": "json_schema",
                "json_schema": {"name": "categories", "strict": true, "schema": response_schema()},
            },
        });
        let url = format!("{}/v1/chat/completions", self.base_url);
        let answer = post(&url, None, &body, self.timeout)?;
        let content = answer["choices"][0]["message"]["content"]
            .as_str()
            .ok_or("réponse sans contenu")?;
        let mut prediction = parse_text_answer(content, start.elapsed().as_millis() as u64)?;
        prediction.model_version = version_from(&answer);
        Ok(prediction)
    }
}

/// D'où vient la clé : elle n'est lue qu'au premier envoi, jamais par `--dry-run`.
pub enum ApiKey {
    None,
    /// Service du trousseau, lu par son nom exact.
    Keychain(String),
    #[cfg(test)]
    Fixed(String),
}

/// `POST /v1/systemone` : Jev chez TypeSafe, ou un serveur Clef sur une machine de
/// l'utilisateur (même interface d'après la fiche du modèle). Seule l'adresse les distingue.
pub struct SystemOne {
    pub label: String,
    pub base_url: String,
    pub model: String,
    pub key: ApiKey,
    pub timeout: Duration,
    resolved: OnceCell<Option<String>>,
}

pub const QUESTION_ID: &str = "category";

impl SystemOne {
    pub fn new(label: &str, base_url: &str, model: &str, key: ApiKey) -> SystemOne {
        SystemOne {
            label: label.into(),
            base_url: base_url.trim_end_matches('/').into(),
            model: model.into(),
            key,
            timeout: Duration::from_secs(600),
            resolved: OnceCell::new(),
        }
    }

    fn api_key(&self) -> Result<Option<&str>, String> {
        if self.resolved.get().is_none() {
            let value = match &self.key {
                ApiKey::None => None,
                #[cfg(test)]
                ApiKey::Fixed(key) => Some(key.clone()),
                ApiKey::Keychain(service) => {
                    let out = std::process::Command::new(SECURITY_BIN)
                        .args(["find-generic-password", "-s", service, "-w"])
                        .output()
                        .map_err(|_| "trousseau indisponible".to_string())?;
                    if !out.status.success() {
                        return Err(format!("clé absente du trousseau ({service})"));
                    }
                    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
                }
            };
            let _ = self.resolved.set(value);
        }
        Ok(self.resolved.get().and_then(|k| k.as_deref()))
    }
}

impl Classifier for SystemOne {
    fn name(&self) -> &str {
        &self.label
    }

    fn target(&self) -> String {
        host_of(&self.base_url)
    }

    fn preamble(&self) -> String {
        format!("modèle {}, question : {QUESTION_TEXT}\nCritères : {}", self.model, catalog_text())
    }

    fn signature(&self) -> String {
        fingerprint(&[&self.label, &self.base_url, &self.model, QUESTION_TEXT, &catalog_text()])
    }

    fn predict(&self, text: &str) -> Result<Prediction, String> {
        let start = Instant::now();
        let criteria: serde_json::Map<String, Value> = CATEGORIES
            .iter()
            .map(|(k, c)| (k.to_string(), json!(c)))
            .collect();
        let body = json!({
            "model": self.model,
            "state": text,
            "questions": {QUESTION_ID: {
                "type": "choice",
                "instructions": QUESTION_TEXT,
                "criteria": criteria,
            }},
        });
        let url = format!("{}/v1/systemone", self.base_url);
        let answer = post(&url, self.api_key()?, &body, self.timeout)?;
        let probabilities = probabilities_from(&answer["answers"][QUESTION_ID]["probabilities"])?;
        let mut prediction = validate(probabilities, start.elapsed().as_millis() as u64)?;
        prediction.model_version = version_from(&answer);
        Ok(prediction)
    }
}

const QUESTION_TEXT: &str = "Comment l'utilisateur classe-t-il cette session de travail ?";

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::adapter::tests::uniform;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;

    /// Faux serveur local, une seule requête : renvoie le corps et le chemin reçus.
    pub fn fake_server(response: Value) -> (String, mpsc::Receiver<(String, String, Value)>) {
        raw_server("200 OK", "", response.to_string())
    }

    fn raw_server(
        status: &'static str,
        extra_header: &'static str,
        text: String,
    ) -> (String, mpsc::Receiver<(String, String, Value)>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut first = String::new();
            reader.read_line(&mut first).unwrap();
            let path = first.split_whitespace().nth(1).unwrap_or_default().to_string();
            let mut length = 0usize;
            let mut auth = String::new();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let lower = line.to_lowercase();
                if let Some(v) = lower.strip_prefix("content-length:") {
                    length = v.trim().parse().unwrap();
                }
                if lower.starts_with("authorization:") {
                    auth = line.trim().to_string();
                }
                if line.trim().is_empty() {
                    break;
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let mut out = stream;
            let _ = write!(
                out,
                "HTTP/1.1 {status}\r\n{extra_header}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
                text.len()
            );
            let _ = tx.send((path, auth, serde_json::from_slice(&body).unwrap()));
        });
        (url, rx)
    }

    #[test]
    fn qwen_request_and_response_roundtrip() {
        let content = json!({"probabilities": uniform()}).to_string();
        let (url, rx) = fake_server(json!({"model": "qwen-x", "choices": [{"message": {"content": content}}]}));
        let prediction = LmStudio::qwen(&url).predict("instantané").unwrap();
        assert_eq!(prediction.probabilities.len(), 5);
        assert_eq!(prediction.model_version.as_deref(), Some("qwen-x"));
        let (path, _, body) = rx.recv().unwrap();
        assert_eq!(path, "/v1/chat/completions");
        assert_eq!(body["model"], "qwen/qwen3.5-9b");
        assert_eq!(body["messages"][1]["content"], "instantané");
        assert_eq!(body["response_format"]["type"], "json_schema");
    }

    #[test]
    fn systemone_sends_state_and_choice_question() {
        let (url, rx) = fake_server(json!({"model": "jev-1.14.0", "answers": {QUESTION_ID: {
            "type": "choice", "choice": "hold", "confidence": 0.2, "probabilities": uniform(),
        }}}));
        let jev = SystemOne::new("jev", &url, "jev-latest", ApiKey::Fixed("cle-de-test".into()));
        let prediction = jev.predict("instantané").unwrap();
        assert_eq!(prediction.probabilities.len(), 5);
        assert_eq!(prediction.model_version.as_deref(), Some("jev-1.14.0"));
        let (path, auth, body) = rx.recv().unwrap();
        assert_eq!(path, "/v1/systemone");
        assert!(auth.ends_with("Bearer cle-de-test"), "{auth}");
        assert_eq!(body["state"], "instantané");
        assert_eq!(body["questions"][QUESTION_ID]["type"], "choice");
        assert_eq!(body["questions"][QUESTION_ID]["criteria"].as_object().unwrap().len(), 5);
    }

    #[test]
    fn target_is_the_host_of_the_address() {
        let at = |url: &str| SystemOne::new("clef", url, "clef", ApiKey::None);
        assert_eq!(at("https://api.typesafe.ai").target(), "api.typesafe.ai");
        assert_eq!(LmStudio::qwen("https://x.example").target(), "x.example");
    }

    #[test]
    fn long_or_odd_version_is_dropped() {
        assert_eq!(version_from(&json!({"model": "jev-1.14.0"})).as_deref(), Some("jev-1.14.0"));
        assert_eq!(version_from(&json!({"model": "x".repeat(65)})), None);
        assert_eq!(version_from(&json!({"model": "texte libre"})), None);
    }

    #[test]
    fn signature_follows_the_address() {
        let at = |url: &str| SystemOne::new("clef", url, "clef", ApiKey::None).signature();
        assert_ne!(at("http://a:1"), at("http://b:1"));
        assert_eq!(at("http://a:1"), at("http://a:1/"));
    }

    #[test]
    fn key_is_refused_on_plain_http_except_loopback() {
        let error = post("http://machine:1/x", Some("k"), &json!({}), Duration::from_secs(1)).unwrap_err();
        assert!(error.contains("clé refusée"), "{error}");
        let error = post("http://8.8.8.8:1/x", Some("k"), &json!({}), Duration::from_millis(50)).unwrap_err();
        assert!(error.contains("clé refusée"), "{error}");
    }

    #[test]
    fn redirect_is_a_failure_not_a_prediction() {
        let (url, _rx) = raw_server("302 Found", "Location: http://127.0.0.1:9/\r\n", String::new());
        assert!(LmStudio::qwen(&url).predict("x").is_err());
    }

    #[test]
    fn oversized_answer_is_a_failure() {
        let (url, _rx) = raw_server("200 OK", "", "x".repeat(MAX_RESPONSE_BYTES as usize + 10));
        assert_eq!(LmStudio::qwen(&url).predict("x").unwrap_err(), "réponse trop longue");
    }

    #[test]
    fn unparsable_answer_is_a_failure_not_a_crash() {
        let (url, _rx) = fake_server(json!({"choices": [{"message": {"content": "je ne sais pas"}}]}));
        assert!(LmStudio::qwen(&url).predict("x").is_err());
    }

    #[test]
    fn invented_category_from_server_is_a_failure_without_quoting_it() {
        let (url, _rx) = fake_server(json!({"answers": {QUESTION_ID: {"probabilities": {"TEXTE-PRIVE": 1.0}}}}));
        let clef = SystemOne::new("clef", &url, "clef", ApiKey::None);
        assert!(!clef.predict("x").unwrap_err().contains("TEXTE-PRIVE"));
    }

    #[test]
    fn unreachable_server_is_a_failure_and_quotes_nothing() {
        let mut qwen = LmStudio::qwen("http://127.0.0.1:9");
        qwen.timeout = Duration::from_secs(2);
        let error = qwen.predict("CONTENU-PRIVE").unwrap_err();
        assert!(!error.contains("CONTENU-PRIVE"), "{error}");
    }

    #[test]
    fn timeout_is_a_failure() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let mut qwen = LmStudio::qwen(&url);
        qwen.timeout = Duration::from_millis(200);
        assert!(qwen.predict("x").is_err());
        drop(listener);
    }
}
