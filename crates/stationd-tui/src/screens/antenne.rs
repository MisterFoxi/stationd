//! Antenne (`1`) — vue principale.
//!
//! Lot 0 : vue PROVISOIRE, alimentée par `Liquidsoap.GetStatus` (déjà lu pour
//! le bandeau) : ce qui passe, depuis quand, ce qui est préparé. La vue
//! complète (morceau enrichi, 10 à suivre, 20 joués, playlists en cours et à
//! suivre) attend `OnAirService.Watch` (dossier §3.2, lots 1–2).

use std::time::Instant;

use anyhow::Error;
use ratatui_core::buffer::Buffer;
use ratatui_core::layout::{Constraint, Layout, Rect};
use ratatui_core::text::{Line, Span};
use ratatui_core::widgets::Widget;
use ratatui_widgets::block::Block;
use ratatui_widgets::borders::BorderType;
use ratatui_widgets::paragraph::{Paragraph, Wrap};

use crate::app::Global;
use crate::screen::Screen;
use crate::store::{human_duration, local_hms};
use crate::style::Styles;

#[derive(Default)]
pub struct Antenne;

/// Libellé humain de `on_air_kind` (track | fallback | halted | unknown | vide).
fn kind_label(kind: &str) -> &'static str {
    match kind {
        "track" => "piste de la grille",
        "fallback" => "FALLBACK (filet de sécurité)",
        "halted" => "à l'arrêt (bruit de fond)",
        "unknown" => "inconnu",
        "" => "rien reçu de Liquidsoap",
        _ => "état non reconnu",
    }
}

impl Screen for Antenne {
    fn title(&self) -> &'static str {
        "Antenne"
    }

    fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &mut Global) -> Result<(), Error> {
        let s = Styles(&ctx.theme);
        let store = &ctx.store;
        let now = Instant::now();
        let [main, note] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(2)]).areas(area);

        let mut lines: Vec<Line> = Vec::new();
        let label = |t: &'static str| Span::styled(format!("{t:<14}"), s.label());
        match store.liquidsoap.value.as_ref() {
            None => {
                let why = store
                    .liquidsoap
                    .error
                    .clone()
                    .unwrap_or_else(|| "lecture en cours…".into());
                lines.push(Line::styled(format!("Antenne inconnue : {why}"), s.muted()));
            }
            Some(ls) if !ls.enabled => {
                lines.push(Line::styled(
                    "Liquidsoap non configuré (pas de section [liquidsoap])",
                    s.warn(),
                ));
            }
            Some(ls) => {
                let kind_style = match ls.on_air_kind.as_str() {
                    "track" => s.ok(),
                    "fallback" => s.error(),
                    "halted" => s.calm(),
                    _ => s.muted(),
                };
                lines.push(Line::from(vec![
                    label("À l'antenne"),
                    Span::styled(kind_label(&ls.on_air_kind), kind_style),
                ]));
                if !ls.on_air_media.is_empty() {
                    lines.push(Line::from(vec![
                        label("Fichier"),
                        Span::styled(ls.on_air_media.clone(), s.accent()),
                    ]));
                }
                lines.push(Line::from(vec![
                    label("Playlist"),
                    if ls.on_air_playlist.is_empty() {
                        Span::styled("—", s.muted())
                    } else {
                        Span::raw(ls.on_air_playlist.clone())
                    },
                ]));
                if ls.on_air_since > 0 {
                    let at = local_hms(store.tz.as_ref(), ls.on_air_since).unwrap_or_default();
                    let wall = jiff::Timestamp::now().as_second() - ls.on_air_since;
                    let ago = if wall >= 0 {
                        format!("  (il y a {})", human_duration(std::time::Duration::from_secs(wall as u64)))
                    } else {
                        String::new()
                    };
                    lines.push(Line::from(vec![label("Depuis"), Span::raw(at), Span::styled(ago, s.muted())]));
                }
                lines.push(Line::from(vec![
                    label("Préparé"),
                    if ls.next_media.is_empty() {
                        Span::styled("rien en attente", s.muted())
                    } else {
                        Span::raw(ls.next_media.clone())
                    },
                ]));
                if store.liquidsoap.is_stale(now) {
                    let age = store
                        .liquidsoap
                        .age(now)
                        .map(|a| format!(" (reçu il y a {})", human_duration(a)))
                        .unwrap_or_default();
                    lines.push(Line::default());
                    lines.push(Line::styled(format!("~ données anciennes{age}"), s.warn()));
                }
            }
        }

        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .style(s.base())
            .block(
                Block::bordered()
                    .border_type(BorderType::Rounded)
                    .border_style(s.border())
                    .title(Span::styled(" À l'antenne ", s.title())),
            )
            .render(main, buf);

        Paragraph::new(vec![Line::styled(
            "Vue provisoire (Liquidsoap, lue toutes les 2 s). À suivre, joués et playlists : \
             avec OnAirService (lots 1–2).",
            s.muted(),
        )])
        .wrap(Wrap { trim: true })
        .style(s.base())
        .render(note, buf);
        Ok(())
    }
}
