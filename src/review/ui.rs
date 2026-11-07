//! L'écran de revue. Aucune logique : tout vient de `Review`.
use super::state::{Mode, Review, Zone};
use crate::journal::Kind;
use crate::snapshot::status_label;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::Frame;

const BAR_WIDTH: u32 = 15;
const KEYS: &str =
    "[d]one [h]old [b]urn [m]anual [a]ctive [n]ote [r]eprendre [s]auter [z]annuler [q]uitter  ↑↓ défiler ⇥ zone ←→ naviguer";

fn bar(done: u32, total: u32) -> String {
    let filled = (done * BAR_WIDTH).checked_div(total).unwrap_or(0).min(BAR_WIDTH);
    format!("{}{}", "█".repeat(filled as usize), "░".repeat((BAR_WIDTH - filled) as usize))
}

fn section(title: &str) -> Line<'static> {
    Line::from(Span::styled(
        format!("── {title} ──"),
        Style::default().add_modifier(Modifier::BOLD),
    ))
}

fn prompt_line(mode: &Mode) -> Option<String> {
    match mode {
        Mode::Confirm(status) => Some(format!(
            "Marquer {} ? o ou Entrée = oui, autre touche = non",
            status_label(*status)
        )),
        Mode::EditNote(text) => Some(format!("note (Entrée = enregistrer, Échap = annuler) ❯ {text}")),
        // Seul le champ annulé : l'autre reste tel qu'il est.
        Mode::ConfirmUndo { kind, theirs, ours } => {
            let (what, now, back) = match kind {
                Kind::Status => (
                    "Statut modifié",
                    status_label(theirs.status).to_string(),
                    status_label(ours.status).to_string(),
                ),
                Kind::Note => {
                    let note = |n: &Option<String>| n.clone().unwrap_or_else(|| "sans note".into());
                    ("Note modifiée", note(&theirs.note), note(&ours.note))
                }
            };
            Some(format!(
                "{what} depuis : {now}. Revenir à : {back} ? o = oui, autre touche = non"
            ))
        }
        Mode::ConfirmResume => Some(
            "Session déjà ouverte : deux Claude Code écriraient le même transcript. Reprendre quand même ? o = oui"
                .into(),
        ),
        _ => None,
    }
}

pub fn draw(frame: &mut Frame, review: &Review) {
    let (done, total) = review.progress();
    let mut top = vec![Line::from(format!("Revue  {}  {done}/{total}", bar(done, total)))];
    let mut recaps: Vec<Line> = Vec::new();
    let mut exchanges: Vec<Line> = Vec::new();
    let focus = |zone: Zone, title: &str| {
        if review.zone == zone {
            section(&format!("▶ {title}"))
        } else {
            section(title)
        }
    };

    // Tout vient de l'instantané : l'écran ne relit jamais le transcript.
    match review.frozen() {
        Some(frozen) => {
            top.extend(frozen.header.iter().map(|l| Line::from(l.clone())));
            // La note peut faire des centaines de caractères : elle vit dans la
            // zone qui défile, pour ne jamais être coupée.
            recaps.push(focus(Zone::Recaps, "Note"));
            recaps.extend(frozen.note.iter().map(|l| Line::from(l.clone())));
            recaps.push(Line::from(""));
            recaps.push(section("Recaps (du plus récent au plus ancien)"));
            recaps.extend(frozen.recaps.iter().map(|l| Line::from(l.clone())));
            exchanges.push(focus(Zone::Exchanges, "Derniers échanges"));
            for line in &frozen.exchanges {
                let style = if line.is_label {
                    Style::default().add_modifier(Modifier::DIM)
                } else {
                    Style::default()
                };
                exchanges.push(Line::from(Span::styled(line.text.clone(), style)));
            }
        }
        None => {
            recaps.push(Line::from("Rien à revoir."));
            recaps.push(Line::from("z annule la dernière décision, toute autre touche quitte."));
        }
    }

    let area = frame.area();
    let recaps = Paragraph::new(recaps).wrap(Wrap { trim: false });
    let exchanges = Paragraph::new(exchanges).wrap(Wrap { trim: false });
    let mut top_lines = top;
    if let Some(alert) = review.alert() {
        top_lines.push(Line::from(alert));
    }
    let top = Paragraph::new(top_lines).wrap(Wrap { trim: false });
    let bottom = prompt_line(&review.mode)
        .or_else(|| review.message.clone())
        .unwrap_or_else(|| KEYS.to_string());
    let bottom = Paragraph::new(bottom).wrap(Wrap { trim: true });

    // Les invites longues restent entières : conflit d'annulation, note éditée.
    let bottom_lines = bottom.line_count(area.width) as u16;
    let bottom_height = bottom_lines.min(area.height / 3).max(1);
    // Sans recap, la zone se réduit et laisse la place aux échanges.
    let recaps_height = (recaps.line_count(area.width) as u16).min(area.height * 2 / 5);
    let [header, recaps_area, exchanges_area, footer] = Layout::vertical([
        Constraint::Length((top.line_count(area.width) as u16).min(area.height / 3).max(1)),
        Constraint::Length(recaps_height),
        Constraint::Min(0),
        Constraint::Length(bottom_height),
    ])
    .areas(area);

    frame.render_widget(top, header);
    let overflow = |p: &Paragraph, area: ratatui::layout::Rect| {
        (p.line_count(area.width) as u16).saturating_sub(area.height)
    };
    let recaps_max = overflow(&recaps, recaps_area);
    let exchanges_max = overflow(&exchanges, exchanges_area);
    review.scroll_limits.set((recaps_max, exchanges_max));
    let recaps_top = review.recaps_scroll.min(recaps_max);
    // Le dernier message en bas de la zone, visible sans rien faire.
    let top_line = exchanges_max.saturating_sub(review.exchanges_up);
    frame.render_widget(recaps.scroll((recaps_top, 0)), recaps_area);
    frame.render_widget(exchanges.scroll((top_line, 0)), exchanges_area);
    // La fin du texte d'abord : c'est là qu'on tape, et là qu'est la question.
    let bottom_scroll = bottom_lines.saturating_sub(bottom_height);
    frame.render_widget(bottom.scroll((bottom_scroll, 0)), footer);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{SessionMeta, SessionRow, Status};
    use crate::paths::Paths;
    use crate::queue::tests::row;
    use crate::review::state::tests::sandbox;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::collections::HashSet;

    fn screen(review: &Review, w: u16, h: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| draw(f, review)).unwrap();
        let buffer = term.backend().buffer().clone();
        buffer
            .content
            .chunks(w as usize)
            .map(|l| l.iter().map(|c| c.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Une session avec un vrai transcript sur disque.
    fn on_disk(root: &std::path::Path, sid: &str, body: &str, meta: Option<SessionMeta>) -> SessionRow {
        let path = root.join(format!("{sid}.jsonl"));
        std::fs::write(&path, body).unwrap();
        let mut r = row(sid, Some(4_000_000_000_000), meta);
        r.info.path = path;
        r
    }

    const WITH_RECAP: &str = concat!(
        "{\"type\":\"system\",\"subtype\":\"away_summary\",\"timestamp\":\"2026-10-15T14:55:35.100Z\",\"content\":\"Travail en pause.\"}\n",
        "{\"type\":\"user\",\"timestamp\":\"2026-10-15T15:00:00.000Z\",\"origin\":{\"kind\":\"human\"},\"message\":{\"content\":\"on reprend ?\"}}\n",
    );

    fn review(paths: &Paths, rows: Vec<SessionRow>, open: &[&str]) -> Review {
        Review::new(paths.clone(), rows, open.iter().map(|s| s.to_string()).collect::<HashSet<_>>())
    }

    #[test]
    fn screen_shows_progress_note_recaps_and_keys() {
        let (p, root) = sandbox("ui_main");
        let meta = SessionMeta {
            session_id: "s".into(),
            updated_at: "t".into(),
            status: Status::Active,
            note: Some("note de test".into()),
            decided_at: None,
        };
        let r = review(&p, vec![on_disk(&root, "s", WITH_RECAP, Some(meta))], &[]);
        let s = screen(&r, 120, 30);
        for expected in ["0/1", "note de test", "Travail en pause.", "on reprend ?", "[d]one", "repo"] {
            assert!(s.contains(expected), "{expected} absent de :\n{s}");
        }
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn banner_is_visible_for_resumed_session() {
        let (p, root) = sandbox("ui_banner");
        let meta = SessionMeta {
            session_id: "s".into(),
            updated_at: "t".into(),
            status: Status::Hold,
            note: None,
            decided_at: Some("2026-10-12T14:32:00+02:00".into()),
        };
        let r = review(&p, vec![on_disk(&root, "s", WITH_RECAP, Some(meta))], &[]);
        let s = screen(&r, 120, 30);
        assert!(s.contains("hold posé le 12/10"), "{s}");
        assert!(s.contains("Depuis : 1 tours"), "{s}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn last_exchange_is_visible_despite_many_recaps_and_messages() {
        let (p, root) = sandbox("ui_last");
        let mut body = String::new();
        for i in 0..40 {
            body.push_str(&format!("{{\"type\":\"system\",\"subtype\":\"away_summary\",\"timestamp\":\"2026-10-15T14:55:35.100Z\",\"content\":\"recap {i}\"}}\n"));
        }
        for i in 0..10 {
            body.push_str(&format!("{{\"type\":\"user\",\"timestamp\":\"2026-10-15T15:00:00.000Z\",\"origin\":{{\"kind\":\"human\"}},\"message\":{{\"content\":\"message {i}\\nsuite\\nsuite\\nsuite\"}}}}\n"));
        }
        body.push_str("{\"type\":\"assistant\",\"timestamp\":\"2026-10-15T15:00:01.000Z\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"dernière réponse\"}]}}\n");
        let mut r = review(&p, vec![on_disk(&root, "s", &body, None)], &[]);
        let s = screen(&r, 100, 30);
        assert!(s.contains("dernière réponse"), "{s}");
        assert!(s.contains("recap 39"), "le recap le plus récent d'abord : {s}");
        // ↑ remonte vers les anciens messages, sans dépasser le début
        for _ in 0..200 {
            r.on_key(crate::review::state::Key::Up);
        }
        let top = screen(&r, 100, 30);
        assert!(top.contains("Derniers échanges"), "{top}");
        assert!(!top.contains("dernière réponse"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn titles_prefer_the_one_given_by_hand() {
        let (p, root) = sandbox("ui_title");
        let body = concat!(
            "{\"type\":\"ai-title\",\"aiTitle\":\"Auto\"}\n",
            "{\"type\":\"custom-title\",\"customTitle\":\"Mon titre\"}\n",
        );
        let mut session = on_disk(&root, "s", body, None);
        session.info.digest = crate::transcript::read_digest(&session.info.path, None).unwrap();
        session.info.digest.last_real_at_ms = Some(1);
        let r = review(&p, vec![session], &[]);
        assert!(screen(&r, 100, 30).contains("Mon titre"));
        std::fs::remove_dir_all(&root).ok();
    }

    fn with_note(text: &str) -> SessionMeta {
        SessionMeta {
            session_id: "s".into(),
            updated_at: "t".into(),
            status: Status::Active,
            note: Some(text.into()),
            decided_at: None,
        }
    }

    #[test]
    fn long_note_is_not_cut() {
        let (p, root) = sandbox("ui_longnote");
        let note = format!("{} FIN-DE-NOTE", "mot ".repeat(150));
        let r = review(&p, vec![on_disk(&root, "s", WITH_RECAP, Some(with_note(&note)))], &[]);
        let s = screen(&r, 80, 30);
        assert!(s.contains("FIN-DE-NOTE"), "{s}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn typing_at_end_of_long_note_is_visible() {
        let (p, root) = sandbox("ui_edit");
        let note = "mot ".repeat(300);
        let mut r = review(&p, vec![on_disk(&root, "s", WITH_RECAP, Some(with_note(&note)))], &[]);
        r.on_key(crate::review::state::Key::Char('n'));
        r.on_paste("TAPÉ");
        assert!(screen(&r, 80, 24).contains("TAPÉ"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn empty_recaps_leave_room_for_exchanges() {
        let (p, root) = sandbox("ui_room");
        let body: String = (0..30)
            .map(|i| format!("{{\"type\":\"user\",\"timestamp\":\"2026-10-15T15:00:00.000Z\",\"origin\":{{\"kind\":\"human\"}},\"message\":{{\"content\":\"ligne {i}\"}}}}\n"))
            .collect();
        let r = review(&p, vec![on_disk(&root, "s", &body, None)], &[]);
        let s = screen(&r, 100, 40);
        // 10 derniers messages, 2 lignes chacun : tout tient sous 7 lignes de recaps.
        assert!(s.contains("ligne 20") && s.contains("ligne 29"), "{s}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn long_title_is_visible() {
        let (p, root) = sandbox("ui_title_long");
        let mut session = on_disk(&root, "s", WITH_RECAP, None);
        session.info.ai_title = Some(format!("{} FIN-TITRE", "mot ".repeat(25)));
        let r = review(&p, vec![session], &[]);
        assert!(screen(&r, 80, 24).contains("FIN-TITRE"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn conflict_prompt_names_only_the_undone_field() {
        let (p, root) = sandbox("ui_conflict");
        use crate::review::state::Key;
        let mut r = review(&p, vec![on_disk(&root, "s", WITH_RECAP, None)], &[]);
        r.on_key(Key::Char('n'));
        r.on_paste("revue");
        r.on_key(Key::Enter);
        crate::meta::set_note(&p, "/Users/x/repo", "s", Some("externe".into())).unwrap();
        r.on_key(Key::Char('z'));
        let s = screen(&r, 100, 30);
        assert!(s.contains("Note modifiée depuis : externe"), "{s}");
        assert!(s.contains("sans note"), "{s}");
        assert!(!s.contains("active /"), "{s}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn no_recap_message_is_shown() {
        let (p, root) = sandbox("ui_norecap");
        let r = review(&p, vec![on_disk(&root, "s", "{\"type\":\"x\"}\n", None)], &[]);
        assert!(screen(&r, 100, 30).contains("aucun recap"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn file_date_is_labelled_without_real_message() {
        let (p, root) = sandbox("ui_filedate");
        let mut session = on_disk(&root, "s", "{\"type\":\"x\"}\n", None);
        session.info.digest.last_real_at_ms = None;
        let r = review(&p, vec![session], &[]);
        assert!(screen(&r, 100, 30).contains("date du fichier"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn renders_at_small_sizes_without_panic() {
        let (p, root) = sandbox("ui_small");
        let r = review(&p, vec![on_disk(&root, "s", WITH_RECAP, None)], &["s"]);
        for (w, h) in [(80, 24), (20, 5), (1, 1)] {
            screen(&r, w, h);
        }
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn open_indicator_is_shown() {
        let (p, root) = sandbox("ui_open");
        let r = review(&p, vec![on_disk(&root, "s", WITH_RECAP, None)], &["s"]);
        assert!(screen(&r, 100, 30).contains("ouverte"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn confirmation_replaces_keys() {
        let (p, root) = sandbox("ui_confirm");
        let mut r = review(&p, vec![on_disk(&root, "s", WITH_RECAP, None)], &[]);
        r.on_key(crate::review::state::Key::Char('h'));
        let s = screen(&r, 100, 30);
        assert!(s.contains("Marquer") && s.contains("hold ?"), "{s}");
        assert!(!s.contains("[d]one"));
        std::fs::remove_dir_all(&root).ok();
    }
}
