//! Écran pas encore réalisé : dit ce qu'il contiendra et dans quel lot, au
//! lieu d'un onglet vide ou absent.

use anyhow::Error;
use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Rect;
use ratatui_core::text::{Line, Span};
use ratatui_core::widgets::Widget;
use ratatui_widgets::block::Block;
use ratatui_widgets::borders::BorderType;
use ratatui_widgets::paragraph::{Paragraph, Wrap};

use crate::app::Global;
use crate::i18n;
use crate::screen::{Availability, Screen};
use crate::store::Store;
use crate::style::Styles;
use crate::tr;

pub struct Planned {
    /// Clé de traduction du titre.
    pub title: &'static str,
    /// Lot de réalisation (dossier §7).
    pub lot: u32,
    /// Clés de traduction du contenu prévu.
    pub summary: &'static [&'static str],
    /// Plugin dont l'écran dépend (chargé = disponible).
    pub plugin: Option<&'static str>,
}

impl Screen for Planned {
    fn title(&self) -> String {
        i18n::text(self.title, &[])
    }

    fn availability(&self, store: &Store) -> Availability {
        match self.plugin {
            Some(p) if !store.plugin_loaded(p) => {
                Availability::Unavailable(tr!("planned-plugin-missing", plugin = p))
            }
            _ => Availability::Available,
        }
    }

    fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &mut Global) -> Result<(), Error> {
        let s = Styles(&ctx.theme);
        let mut lines = vec![
            Line::from(vec![
                Span::styled(format!("{} ", tr!("planned-coming")), s.warn()),
                Span::styled(format!("({})", tr!("planned-lot", lot = self.lot)), s.muted()),
            ]),
            Line::default(),
        ];
        lines.extend(self.summary.iter().map(|key| Line::from(format!("  · {}", i18n::text(key, &[])))));
        if let Availability::Unavailable(why) = self.availability(&ctx.store) {
            lines.push(Line::default());
            lines.push(Line::styled(tr!("planned-unavailable", reason = why), s.error()));
        }
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .style(s.base())
            .block(
                Block::bordered()
                    .border_type(BorderType::Rounded)
                    .border_style(s.border())
                    .title(Span::styled(format!(" {} ", self.title()), s.title())),
            )
            .render(area, buf);
        Ok(())
    }
}
