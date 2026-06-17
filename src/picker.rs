use crate::model::Status;
use crate::{build, meta};
use anyhow::Result;
use skim::prelude::*;
use std::borrow::Cow;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::sync::Arc;

struct Item {
    display: String,
    sid: String,
    cwd: String,
    label: String,
    preview: String,
}

impl SkimItem for Item {
    fn text(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.display)
    }
    fn preview(&self, _ctx: PreviewContext) -> ItemPreview {
        ItemPreview::Text(self.preview.clone())
    }
    fn output(&self) -> Cow<'_, str> {
        // sid \t cwd \t label — reparsé après sélection
        Cow::Owned(format!("{}\t{}\t{}", self.sid, self.cwd, self.label))
    }
}

/// Un choix du sous-menu d'action.
struct Choice {
    label: String,
    key: String,
}

impl SkimItem for Choice {
    fn text(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.label)
    }
    fn output(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.key)
    }
}

fn icon(status: Status, has_note: bool) -> &'static str {
    match (status, has_note) {
        (Status::NeedsManualWork, _) => "🔍",
        (Status::ReadyToBurn, _) => "🔥",
        (Status::Hold, _) => "⏳",
        (Status::Done, _) => "✓ ",
        (_, true) => "📝",
        _ => "  ",
    }
}

fn now_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

/// Offset TZ local en secondes, lu une seule fois via `date +%z` (±HHMM → ±s).
fn local_offset_secs() -> i64 {
    let s = std::process::Command::new("date")
        .arg("+%z")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();
    let s = s.trim();
    if s.len() >= 5 {
        let sign = if s.starts_with('-') { -1 } else { 1 };
        let h: i64 = s[1..3].parse().unwrap_or(0);
        let m: i64 = s[3..5].parse().unwrap_or(0);
        sign * (h * 3600 + m * 60)
    } else {
        0
    }
}

/// Date civile (année, mois, jour) depuis un compte de jours epoch — algo de H. Hinnant.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `epoch_secs` (UTC) + offset local → "YYYY-MM-DD HH:MM".
fn fmt_datetime(epoch_secs: i64, offset: i64) -> String {
    let local = epoch_secs + offset;
    let days = local.div_euclid(86_400);
    let tod = local.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}", tod / 3600, (tod % 3600) / 60)
}

/// Âge relatif lisible depuis le mtime de la session.
fn human_age(mtime_ns: u64, now: u64) -> String {
    if now <= mtime_ns {
        return "à l'instant".into();
    }
    let secs = (now - mtime_ns) / 1_000_000_000;
    let (mins, hours, days) = (secs / 60, secs / 3600, secs / 86_400);
    if days >= 1 {
        let rem_h = hours % 24;
        if rem_h > 0 {
            format!("il y a {days} j {rem_h} h")
        } else {
            format!("il y a {days} j")
        }
    } else if hours >= 1 {
        format!("il y a {hours} h")
    } else if mins >= 1 {
        format!("il y a {mins} min")
    } else {
        "il y a < 1 min".into()
    }
}

/// Lance le picker principal en boucle ; Enter sur une session ouvre un sous-menu
/// d'action (reprendre / changer statut / éditer note). Seul « reprendre » quitte
/// le programme (execvp claude --resume) ; les autres actions rafraîchissent la liste.
pub fn run(projects_dir: &Path, reverse: bool) -> Result<()> {
    let tz_offset = local_offset_secs();
    loop {
        let mut rows = build::rows(projects_dir);
        if reverse {
            rows.reverse();
        }
        let now = now_ns();

        let options = SkimOptionsBuilder::default()
            .height("90%")
            .reverse(true)
            .cycle(true)
            .prompt("claude session ❯ ")
            .preview("")
            .preview_window("right:55%:wrap")
            .build()
            .unwrap();

        let (tx, rx): (SkimItemSender, SkimItemReceiver) = unbounded();
        let mut items: Vec<Arc<dyn SkimItem>> = Vec::new();
        for r in &rows {
            if r.status() == Status::Done {
                continue;
            }
            let display = format!(
                "{}\t{}\t{}\t{}",
                icon(r.status(), r.meta.as_ref().and_then(|m| m.note.as_ref()).is_some()),
                r.project_label,
                r.summary(),
                r.info.session_id
            );
            let age = human_age(r.info.mtime_ns, now);
            let when = fmt_datetime((r.info.mtime_ns / 1_000_000_000) as i64, tz_offset);
            let preview = format!(
                "{}\nprojet : {}\nuuid : {}\nstatut : {:?}\nâge : {} ({})\n\nnote : {}\n\ndernier prompt :\n{}",
                r.info.ai_title.clone().unwrap_or_else(|| "(sans titre)".into()),
                r.info.cwd.clone().unwrap_or_default(),
                r.info.session_id,
                r.status(),
                age,
                when,
                r.meta.as_ref().and_then(|m| m.note.clone()).unwrap_or_else(|| "—".into()),
                r.info.last_prompt.clone().unwrap_or_default(),
            );
            items.push(Arc::new(Item {
                display,
                sid: r.info.session_id.clone(),
                cwd: r.info.cwd.clone().unwrap_or_default(),
                label: r.project_label.clone(),
                preview,
            }));
        }
        let _ = tx.send(items);
        drop(tx);

        let selected = Skim::run_with(options, Some(rx))
            .ok()
            .filter(|o| !o.is_abort)
            .map(|o| o.selected_items)
            .unwrap_or_default();

        // ESC / liste vide → on quitte.
        let Some(item) = selected.first() else {
            return Ok(());
        };
        let out = item.output();
        let mut parts = out.splitn(3, '\t');
        let sid = parts.next().unwrap_or("").to_string();
        let cwd = parts.next().unwrap_or("").to_string();
        let label = parts.next().unwrap_or("").to_string();

        match action_menu(&label) {
            // « Reprendre » : ne revient jamais (remplace le process).
            Some(Action::Resume) => {
                // `claude --resume <id>` retrouve la session via le projet encodé
                // depuis le cwd courant. Il faut donc se placer dans le cwd d'origine.
                // S'il a été supprimé/déplacé, on le recrée (dossier vide) — sinon un
                // fallback HOME pointerait vers le mauvais projet → « No conversation found ».
                if !cwd.is_empty() && !Path::new(&cwd).is_dir() {
                    eprintln!("cs: dossier d'origine absent, recréé pour le resume : {cwd}");
                    let _ = std::fs::create_dir_all(&cwd);
                }
                let target = if Path::new(&cwd).is_dir() {
                    cwd
                } else {
                    dirs::home_dir().unwrap_or_default().to_string_lossy().into_owned()
                };
                std::env::set_current_dir(&target)?;
                let err = std::process::Command::new("claude").args(["--resume", &sid]).exec();
                return Err(err.into());
            }
            Some(Action::SetStatus(status)) => {
                apply_status(&sid, &cwd, status)?;
            }
            Some(Action::EditNote) => {
                let current = meta::load(&meta::notes_file(&cwd)).get(&sid).and_then(|m| m.note.clone());
                if let Some(note) = prompt_note(current.as_deref()) {
                    apply_note(&sid, &cwd, note)?;
                }
            }
            // ESC dans le sous-menu → retour à la liste.
            None => {}
        }
        // boucle : la liste se reconstruit avec les méta à jour.
    }
}

enum Action {
    Resume,
    SetStatus(Status),
    EditNote,
}

/// Sous-menu skim des actions possibles sur la session sélectionnée.
fn action_menu(label: &str) -> Option<Action> {
    let prompt = format!("{label} ❯ ");
    let options = SkimOptionsBuilder::default()
        .height("50%")
        .reverse(true)
        .cycle(true)
        .prompt(prompt)
        .build()
        .unwrap();

    let choices = [
        ("resume", "Reprendre (claude --resume)"),
        ("active", "Marquer active"),
        ("hold", "Marquer hold ⏳"),
        ("burn", "Marquer ready to burn 🔥"),
        ("manual", "Marquer needs manual work 🔍"),
        ("done", "Marquer done ✓"),
        ("note", "Éditer la note 📝"),
    ];

    let (tx, rx): (SkimItemSender, SkimItemReceiver) = unbounded();
    let items: Vec<Arc<dyn SkimItem>> = choices
        .into_iter()
        .map(|(key, text)| Arc::new(Choice { label: text.into(), key: key.into() }) as Arc<dyn SkimItem>)
        .collect();
    let _ = tx.send(items);
    drop(tx);

    let selected = Skim::run_with(options, Some(rx))
        .ok()
        .filter(|o| !o.is_abort)
        .map(|o| o.selected_items)
        .unwrap_or_default();

    match selected.first()?.output().as_ref() {
        "resume" => Some(Action::Resume),
        "active" => Some(Action::SetStatus(Status::Active)),
        "hold" => Some(Action::SetStatus(Status::Hold)),
        "burn" => Some(Action::SetStatus(Status::ReadyToBurn)),
        "manual" => Some(Action::SetStatus(Status::NeedsManualWork)),
        "done" => Some(Action::SetStatus(Status::Done)),
        "note" => Some(Action::EditNote),
        _ => None,
    }
}

/// Change le statut en préservant la note existante.
fn apply_status(sid: &str, cwd: &str, status: Status) -> Result<()> {
    let existing = meta::load(&meta::notes_file(cwd)).get(sid).and_then(|m| m.note.clone());
    meta::upsert(cwd, sid, &crate::now_stamp_pub(), status, existing)?;
    Ok(())
}

/// Écrit/remplace la note en préservant le statut existant.
fn apply_note(sid: &str, cwd: &str, note: String) -> Result<()> {
    let status = meta::load(&meta::notes_file(cwd)).get(sid).map(|m| m.status).unwrap_or_default();
    meta::upsert(cwd, sid, &crate::now_stamp_pub(), status, Some(note))?;
    Ok(())
}

/// Saisie d'une note sur `/dev/tty` (skim a consommé stdin). Ligne vide → annule.
fn prompt_note(current: Option<&str>) -> Option<String> {
    let mut tty = std::fs::OpenOptions::new().read(true).write(true).open("/dev/tty").ok()?;
    if let Some(c) = current {
        let _ = writeln!(tty, "note actuelle : {c}");
    }
    let _ = write!(tty, "nouvelle note (vide = annuler) ❯ ");
    let _ = tty.flush();

    let mut reader = std::io::BufReader::new(tty);
    let mut line = String::new();
    std::io::BufRead::read_line(&mut reader, &mut line).ok()?;
    let line = line.trim().to_string();
    if line.is_empty() {
        None
    } else {
        Some(line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fmt_datetime_known_epoch() {
        // 1 700 000 000 = 2023-11-14 22:13:20 UTC
        assert_eq!(fmt_datetime(1_700_000_000, 0), "2023-11-14 22:13");
        // epoch 0 = 1970-01-01 00:00 UTC
        assert_eq!(fmt_datetime(0, 0), "1970-01-01 00:00");
        // offset +2h sur l'epoch 0 → 02:00 le même jour
        assert_eq!(fmt_datetime(0, 7200), "1970-01-01 02:00");
    }

    #[test]
    fn human_age_buckets() {
        let day_ns = 86_400u64 * 1_000_000_000;
        assert_eq!(human_age(0, day_ns * 3), "il y a 3 j");
        assert_eq!(human_age(0, 30 * 60 * 1_000_000_000), "il y a 30 min");
        assert_eq!(human_age(day_ns, 0), "à l'instant"); // mtime futur
    }
}
