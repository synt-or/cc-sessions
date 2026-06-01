use crate::model::{SessionRow, Status};
use anyhow::Result;
use skim::prelude::*;
use std::borrow::Cow;
use std::os::unix::process::CommandExt;
use std::sync::Arc;

struct Item {
    display: String,
    sid: String,
    cwd: String,
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
        Cow::Owned(format!("{}\t{}", self.sid, self.cwd))
    }
}

fn icon(status: Status, has_note: bool) -> &'static str {
    match (status, has_note) {
        (Status::Hold, _) => "⏳",
        (Status::Done, _) => "✓ ",
        (_, true) => "📝",
        _ => "  ",
    }
}

/// Lance le picker ; à la sélection, cd + execvp claude --resume.
pub fn run(rows: &[SessionRow]) -> Result<()> {
    let show_done = false;
    let options = SkimOptionsBuilder::default()
        .height(Some("90%"))
        .reverse(true)
        .prompt(Some("claude session ❯ "))
        .preview(Some(""))
        .preview_window(Some("right:55%:wrap"))
        .build()
        .unwrap();

    let (tx, rx): (SkimItemSender, SkimItemReceiver) = unbounded();
    for r in rows {
        if !show_done && r.status() == Status::Done {
            continue;
        }
        let display = format!(
            "{}\t{}\t{}\t{}",
            icon(r.status(), r.meta.as_ref().and_then(|m| m.note.as_ref()).is_some()),
            r.project_label,
            r.summary(),
            r.info.session_id
        );
        let preview = format!(
            "{}\nprojet : {}\nuuid : {}\nstatut : {:?}\n\nnote : {}\n\ndernier prompt :\n{}",
            r.info.ai_title.clone().unwrap_or_else(|| "(sans titre)".into()),
            r.info.cwd.clone().unwrap_or_default(),
            r.info.session_id,
            r.status(),
            r.meta.as_ref().and_then(|m| m.note.clone()).unwrap_or_else(|| "—".into()),
            r.info.last_prompt.clone().unwrap_or_default(),
        );
        let _ = tx.send(Arc::new(Item {
            display,
            sid: r.info.session_id.clone(),
            cwd: r.info.cwd.clone().unwrap_or_default(),
            preview,
        }));
    }
    drop(tx);

    let selected = Skim::run_with(&options, Some(rx))
        .filter(|o| !o.is_abort)
        .map(|o| o.selected_items)
        .unwrap_or_default();

    if let Some(item) = selected.first() {
        let out = item.output();
        let mut parts = out.splitn(2, '\t');
        let sid = parts.next().unwrap_or("").to_string();
        let cwd = parts.next().unwrap_or("").to_string();
        let target = if std::path::Path::new(&cwd).is_dir() { cwd } else { dirs::home_dir().unwrap_or_default().to_string_lossy().into_owned() };
        std::env::set_current_dir(&target)?;
        let err = std::process::Command::new("claude").args(["--resume", &sid]).exec();
        return Err(err.into());
    }
    Ok(())
}
