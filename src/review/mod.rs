//! `cs review` : passer les sessions en revue une à une.
pub mod state;
pub mod ui;

use crate::paths::Paths;
use crate::{build, live, picker, project, queue};
use anyhow::Result;
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEventKind, KeyModifiers,
};
use ratatui::crossterm::execute;
use std::io::IsTerminal;
use state::{Key, Outcome, Review};

fn to_key(code: KeyCode) -> Option<Key> {
    Some(match code {
        KeyCode::Char(c) => Key::Char(c),
        KeyCode::Enter => Key::Enter,
        KeyCode::Esc => Key::Esc,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::Tab => Key::Tab,
        _ => return None,
    })
}

fn leave() {
    let _ = execute!(std::io::stdout(), DisableBracketedPaste);
    ratatui::restore();
}

pub fn run(paths: &Paths, scope: Option<&str>, all: bool, undo: bool) -> Result<()> {
    // Avant toute écriture : `cs -z` lancé par une session annulerait à l'aveugle.
    if !std::io::stdout().is_terminal() || !std::io::stdin().is_terminal() {
        anyhow::bail!("cs review a besoin d'un terminal");
    }
    let mut unreachable = 0;
    let rows: Vec<_> = build::all_rows(paths, scope)
        .into_iter()
        .filter(|r| all || queue::in_scope(&r.info))
        .filter(|r| {
            let missing = r.info.cwd.as_deref().is_some_and(project::volume_missing);
            unreachable += usize::from(missing);
            !missing
        })
        .collect();
    let mut review = Review::new(paths.clone(), rows, live::open_sessions(paths));
    if unreachable > 0 {
        let note = format!("{unreachable} sessions écartées : leur volume n'est pas monté.");
        review.message = Some(match review.message.take() {
            Some(previous) => format!("{previous} {note}"),
            None => note,
        });
    }
    if undo {
        review.undo();
    }

    // `ratatui::init` installe aussi le rétablissement du terminal sur panique.
    let mut terminal = ratatui::init();
    // Sans collage encadré, un retour à la ligne collé vaudrait Entrée et les
    // lettres suivantes, des décisions.
    let _ = execute!(std::io::stdout(), EnableBracketedPaste);
    let outcome = loop {
        if let Err(e) = terminal.draw(|f| ui::draw(f, &review)) {
            leave();
            return Err(e.into());
        }
        let Ok(event) = event::read() else {
            break Outcome::Exit;
        };
        match event {
            Event::Paste(text) => review.on_paste(&text),
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                    break Outcome::Exit;
                }
                if let Some(k) = to_key(key.code) {
                    match review.on_key(k) {
                        Outcome::Continue => {}
                        other => break other,
                    }
                }
            }
            _ => {}
        }
    };
    leave();

    if let Outcome::Resume { sid, cwd } = outcome {
        picker::resume(paths, &sid, &cwd)?;
    }
    Ok(())
}
