//! Mise en ligne dans une largeur donnée, sans couper un élément en plein
//! milieu : on garde les segments dans l'ordre de priorité tant qu'ils
//! tiennent ; un texte libre trop long est abrégé avec « … ».

use ratatui_core::text::{Line, Span};

/// Largeur affichée (cellules terminal, pas octets).
pub fn width(spans: &[Span]) -> usize {
    spans.iter().map(Span::width).sum()
}

/// Concatène `segments` (déjà triés par priorité décroissante) séparés par
/// `sep`, en s'arrêtant au premier qui ne tient plus. Le premier segment est
/// toujours gardé (abrégé si besoin).
pub fn segments<'a>(segments: Vec<Vec<Span<'a>>>, sep: Span<'a>, max: usize) -> Line<'a> {
    let mut out: Vec<Span<'a>> = Vec::new();
    for (i, seg) in segments.into_iter().enumerate() {
        let extra = if i == 0 { 0 } else { sep.width() };
        if i > 0 && width(&out) + extra + width(&seg) > max {
            break;
        }
        if i > 0 {
            out.push(sep.clone());
        }
        out.extend(seg);
    }
    Line::from(out)
}

/// Abrège `text` pour qu'il tienne dans `max` cellules (« … » final).
pub fn ellipsize(text: &str, max: usize) -> String {
    if Span::raw(text).width() <= max {
        return text.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut out = String::new();
    for c in text.chars() {
        let mut probe = out.clone();
        probe.push(c);
        if Span::raw(probe.as_str()).width() + 1 > max {
            break;
        }
        out.push(c);
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_whole_segments_only() {
        let segs = vec![vec![Span::raw("abc")], vec![Span::raw("defgh")], vec![Span::raw("ij")]];
        let l = segments(segs, Span::raw(" | "), 12);
        assert_eq!(l.to_string(), "abc | defgh");
    }

    #[test]
    fn stops_at_first_misfit_to_keep_priority() {
        let segs = vec![vec![Span::raw("abc")], vec![Span::raw("toolongtoolong")], vec![Span::raw("ij")]];
        assert_eq!(segments(segs, Span::raw(" "), 10).to_string(), "abc");
    }

    #[test]
    fn ellipsize_counts_cells() {
        assert_eq!(ellipsize("Veridis Quo", 20), "Veridis Quo");
        assert_eq!(ellipsize("Veridis Quo", 8), "Veridis…");
        assert_eq!(ellipsize("Électro", 4), "Éle…");
    }
}
