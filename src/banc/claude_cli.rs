//! Sonnet et Opus via `claude -p`, sans outil ni réglage, et sans créer de session
//! qui apparaîtrait ensuite dans `cs`.
use crate::adapter::{instructions, parse_text_answer, Classifier, Prediction};
use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

pub struct ClaudeCli {
    pub label: String,
    pub model: String,
    pub binary: PathBuf,
    pub timeout: Duration,
}

impl ClaudeCli {
    fn new(label: &str, model: &str) -> ClaudeCli {
        ClaudeCli {
            label: label.into(),
            model: model.into(),
            binary: "claude".into(),
            timeout: Duration::from_secs(600),
        }
    }

    pub fn sonnet() -> ClaudeCli {
        ClaudeCli::new("sonnet", "claude-sonnet-5-5")
    }

    pub fn opus() -> ClaudeCli {
        ClaudeCli::new("opus", "claude-opus-5-5")
    }
}

const MAX_OUTPUT_BYTES: u64 = 8 * 1024 * 1024;

impl Classifier for ClaudeCli {
    fn name(&self) -> &str {
        &self.label
    }

    fn target(&self) -> String {
        "claude-cli".into()
    }

    fn preamble(&self) -> String {
        format!("modèle {}, consignes :\n{}", self.model, instructions())
    }

    fn signature(&self) -> String {
        format!("{} {} {}", self.label, self.model, instructions())
    }

    fn predict(&self, text: &str) -> Result<Prediction, String> {
        let start = Instant::now();
        let mut child = Command::new(&self.binary)
            .args(["-p", "--model", &self.model, "--no-session-persistence"])
            .args(["--tools", "", "--strict-mcp-config", "--setting-sources", ""])
            .args(["--system-prompt", &instructions()])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            // Son propre groupe : à l'échéance, ses sous-processus partent avec lui.
            .process_group(0)
            .spawn()
            .map_err(|e| format!("lancement impossible : {}", e.kind()))?;
        let pgid = child.id() as i32;
        let kill_group = || unsafe {
            libc::killpg(pgid, libc::SIGKILL);
        };
        // Le texte passe par stdin : il n'apparaît pas dans la liste des processus.
        let mut stdin = child.stdin.take().ok_or("stdin indisponible")?;
        let payload = text.to_string();
        std::thread::spawn(move || {
            let _ = stdin.write_all(payload.as_bytes());
        });
        // Lu en continu : une sortie abondante bloquerait sinon le processus.
        let stdout = child.stdout.take().ok_or("stdout indisponible")?;
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let result = stdout.take(MAX_OUTPUT_BYTES + 1).read_to_end(&mut bytes);
            let _ = sender.send(result.map(|_| bytes));
        });
        let status = loop {
            match child.try_wait().map_err(|e| format!("attente : {}", e.kind()))? {
                Some(status) => break status,
                None if start.elapsed() > self.timeout => {
                    kill_group();
                    let _ = child.wait();
                    return Err("délai dépassé".into());
                }
                None => std::thread::sleep(Duration::from_millis(20)),
            }
        };
        if !status.success() {
            kill_group();
            return Err(format!("code de sortie {:?}", status.code()));
        }
        // Un petit-fils peut garder stdout ouvert après la fin du processus.
        // Le tuer ferme le tube : ce qui a été écrit reste lisible, sans attendre.
        kill_group();
        let remaining = self.timeout.saturating_sub(start.elapsed()).max(Duration::from_millis(100));
        let received = receiver.recv_timeout(remaining);
        let bytes = match received {
            Ok(Ok(bytes)) => bytes,
            Ok(Err(e)) => return Err(format!("sortie : {}", e.kind())),
            Err(_) => return Err("délai dépassé".into()),
        };
        if bytes.len() as u64 > MAX_OUTPUT_BYTES {
            return Err("réponse trop longue".into());
        }
        parse_text_answer(&String::from_utf8_lossy(&bytes), start.elapsed().as_millis() as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::tests::uniform;
    use std::os::unix::fs::PermissionsExt;

    /// Faux `claude` : note ses arguments et stdin, puis répond.
    fn fake_binary(name: &str, script_body: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("cs_banc_cli_{name}"));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("claude");
        let log = dir.join("log");
        std::fs::write(
            &bin,
            format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > {log}.args\ncat > {log}.stdin\n{script_body}\n", log = log.display()),
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        (bin, log)
    }

    fn with_binary(bin: PathBuf) -> ClaudeCli {
        ClaudeCli { binary: bin, ..ClaudeCli::sonnet() }
    }

    #[test]
    fn claude_cli_uses_no_tools_and_no_persistence() {
        let answer = serde_json::json!({"probabilities": uniform()}).to_string();
        let (bin, log) = fake_binary("args", &format!("echo '{answer}'"));
        let prediction = with_binary(bin).predict("CONTENU-XYZ").unwrap();
        assert_eq!(prediction.probabilities.len(), 5);
        let args = std::fs::read_to_string(format!("{}.args", log.display())).unwrap();
        for expected in ["--no-session-persistence", "--tools", "--strict-mcp-config", "claude-sonnet-5-5"] {
            assert!(args.contains(expected), "{expected} : {args}");
        }
        assert!(!args.contains("CONTENU-XYZ"), "le contenu ne passe pas par les arguments");
        assert_eq!(std::fs::read_to_string(format!("{}.stdin", log.display())).unwrap(), "CONTENU-XYZ");
    }

    #[test]
    fn opus_uses_the_exact_model_id() {
        assert_eq!(ClaudeCli::opus().model, "claude-opus-5-5");
    }

    #[test]
    fn non_zero_exit_is_a_failure() {
        let (bin, _) = fake_binary("fail", "exit 3");
        assert!(with_binary(bin).predict("x").unwrap_err().contains("3"));
    }

    #[test]
    fn timeout_is_a_failure() {
        let (bin, _) = fake_binary("slow", "sleep 5");
        let mut cli = with_binary(bin);
        cli.timeout = Duration::from_millis(200);
        assert_eq!(cli.predict("x").unwrap_err(), "délai dépassé");
    }

    #[test]
    fn large_output_does_not_block() {
        let answer = serde_json::json!({"probabilities": uniform()}).to_string();
        let (bin, _) = fake_binary("big", &format!("head -c 200000 /dev/zero | tr '\\0' ' '\necho '{answer}'"));
        let mut cli = with_binary(bin);
        cli.timeout = Duration::from_secs(10);
        let start = Instant::now();
        assert!(cli.predict("x").is_ok());
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn grandchild_holding_stdout_does_not_exceed_timeout() {
        let (bin, _) = fake_binary("grandchild", "sleep 30 &\nexit 0");
        let mut cli = with_binary(bin);
        cli.timeout = Duration::from_millis(500);
        let start = Instant::now();
        assert!(cli.predict("x").is_err());
        assert!(start.elapsed() < Duration::from_secs(3), "{:?}", start.elapsed());
    }

    #[test]
    fn oversized_output_is_a_failure() {
        let (bin, _) = fake_binary("huge", "head -c 9000000 /dev/zero");
        let mut cli = with_binary(bin);
        cli.timeout = Duration::from_secs(15);
        // Le lecteur s'arrête à la limite : le processus s'interrompt, ou sa sortie est refusée.
        assert!(cli.predict("x").is_err());
    }

    #[test]
    fn missing_binary_is_a_failure() {
        assert!(with_binary("/n/existe/pas".into()).predict("x").is_err());
    }
}
