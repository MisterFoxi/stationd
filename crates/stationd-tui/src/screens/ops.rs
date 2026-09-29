//! Actions partagées entre écrans (Antenne, Contrôle) : chaque fonction
//! construit la modale qui nomme exactement ce qui va être fait, ou l'action
//! quand elle ne touche pas l'antenne. L'exécution reste dans `action`.

use stationd_proto::broadcast::State;

use crate::action::{Action, OverrideContent, PluginVerb, TagChanges};
use crate::dialog::{Confirm, Field, Form, Modal};
use crate::store::Store;
use crate::tr;

/// État de diffusion connu (bandeau), sinon celui de l'antenne.
pub fn state(store: &Store) -> State {
    if let Some(b) = &store.broadcast.value {
        return State::try_from(b.state).unwrap_or(State::Unspecified);
    }
    match store.onair.as_ref().map(|o| o.state.as_str()) {
        Some("running") => State::Running,
        Some("paused") => State::Paused,
        Some("draining") => State::Draining,
        Some("sleeping") => State::Sleeping,
        _ => State::Unspecified,
    }
}

/// Morceau à l'antenne, pour nommer ce qu'une action va toucher.
pub fn on_air_label(store: &Store) -> String {
    match store.onair.as_ref().and_then(|o| o.on_air.as_ref()) {
        Some(t) => super::antenne::label(t),
        None => match store.live.value.as_ref().and_then(|l| l.on_air.as_ref()) {
            Some(s) => tr!("onair-live", dj = s.dj.clone()),
            None => "—".into(),
        },
    }
}

/// Espace : pause (confirmée : l'antenne s'arrête), reprise, réveil.
/// `None` quand l'état est inconnu : on ne devine pas.
pub fn toggle_pause(store: &Store) -> Option<Outcome> {
    match state(store) {
        State::Running => Some(Outcome::Open(Modal::Confirm(Confirm::new(
            tr!("confirm-pause-title"),
            vec![tr!("confirm-pause-body", track = on_air_label(store))],
            tr!("confirm-pause-yes"),
            Action::Pause,
        )))),
        State::Paused | State::Draining => Some(Outcome::Run(Action::Resume)),
        State::Sleeping => Some(Outcome::Run(Action::Wake)),
        State::Unspecified => None,
    }
}

/// Ce qu'un écran demande à l'application.
pub enum Outcome {
    Open(Modal),
    Run(Action),
}

pub fn skip(store: &Store) -> Modal {
    let next = store
        .onair
        .as_ref()
        .and_then(|o| o.prefetched.as_ref().or(o.upcoming.first()))
        .map(super::antenne::label)
        .unwrap_or_else(|| "—".into());
    Modal::Confirm(Confirm::new(
        tr!("confirm-skip-title"),
        vec![tr!("confirm-skip-body", track = on_air_label(store)), tr!("confirm-skip-next", track = next)],
        tr!("confirm-skip-yes"),
        Action::Skip,
    ))
}

pub fn stop_when_idle() -> Modal {
    Modal::Confirm(Confirm::new(
        tr!("confirm-drain-title"),
        vec![tr!("confirm-drain-body")],
        tr!("confirm-drain-yes"),
        Action::StopWhenIdle,
    ))
}

fn content_text(c: &OverrideContent) -> String {
    match c {
        OverrideContent::Media(m) => tr!("override-media", path = m.clone()),
        OverrideContent::Playlist(p) => tr!("override-playlist", playlist = p.clone()),
    }
}

/// Formulaire d'override, puis confirmation qui dit quoi et comment.
pub fn push_override() -> Modal {
    push_override_with(None)
}

/// Idem, pré-rempli avec un média (écran Médias).
pub fn push_override_with(media: Option<&str>) -> Modal {
    let fields = vec![
        Field::choice(
            tr!("form-override-kind"),
            vec![(tr!("form-override-kind-media"), "media".into()), (tr!("form-override-kind-playlist"), "playlist".into())],
        ),
        Field::text(tr!("form-override-target"), media.unwrap_or("")),
        Field::choice(
            tr!("form-override-mode"),
            vec![(tr!("form-override-soft"), "soft".into()), (tr!("form-override-hard"), "hard".into())],
        ),
        Field::text(tr!("form-override-expiry"), ""),
        Field::text(tr!("form-override-tracks"), "1"),
    ];
    let form = Form::new(tr!("form-override-title"), fields, |f| {
        let target = f[1].value();
        if target.is_empty() {
            return Err(tr!("form-required", field = f[1].label.clone()));
        }
        let tracks: u32 = f[4]
            .value()
            .parse()
            .ok()
            .filter(|n| *n >= 1)
            .ok_or_else(|| tr!("form-positive-integer", field = f[4].label.clone()))?;
        let content =
            if f[0].value() == "playlist" { OverrideContent::Playlist(target) } else { OverrideContent::Media(target) };
        Ok(Action::PushOverride { content, hard: f[2].value() == "hard", expiry: f[3].value(), tracks })
    })
    .confirm_with(|a| {
        let Action::PushOverride { content, hard, expiry, tracks } = a else { return None };
        let mut lines = vec![content_text(content)];
        lines.push(if *hard { tr!("confirm-override-hard") } else { tr!("confirm-override-soft") });
        if matches!(content, OverrideContent::Playlist(_)) {
            lines.push(tr!("confirm-override-tracks", n = *tracks));
        }
        lines.push(if expiry.is_empty() {
            tr!("confirm-override-no-expiry")
        } else {
            tr!("confirm-override-expiry", expiry = expiry.clone())
        });
        let c = Confirm::new(tr!("confirm-override-title"), lines, tr!("confirm-override-yes"), a.clone());
        Some(if *hard { c.danger() } else { c })
    });
    Modal::Form(form)
}

pub fn clear_override(id: u64, what: String) -> Modal {
    Modal::Confirm(Confirm::new(
        tr!("confirm-clear-one-title"),
        vec![tr!("confirm-clear-one-body", id = id, what = what)],
        tr!("confirm-clear-yes"),
        Action::ClearOverrides(Some(id)),
    ))
}

pub fn clear_all_overrides(n: usize) -> Modal {
    Modal::Confirm(
        Confirm::new(
            tr!("confirm-clear-all-title"),
            vec![tr!("confirm-clear-all-body", n = n)],
            tr!("confirm-clear-yes"),
            Action::ClearOverrides(None),
        )
        .danger(),
    )
}

pub fn kick(dj: &str) -> Modal {
    Modal::Confirm(
        Confirm::new(
            tr!("confirm-kick-title"),
            vec![tr!("confirm-kick-body", dj = dj.to_string())],
            tr!("confirm-kick-yes"),
            Action::LiveKick,
        )
        .danger(),
    )
}

pub fn live_open() -> Modal {
    let fields = vec![Field::text(tr!("form-live-dj"), ""), Field::text(tr!("form-live-duration"), "2h")];
    Modal::Form(Form::new(tr!("form-live-open-title"), fields, |f| {
        let dj = f[0].value();
        if dj.is_empty() {
            return Err(tr!("form-required", field = f[0].label.clone()));
        }
        let duration = f[1].value();
        if duration.is_empty() {
            return Err(tr!("form-required", field = f[1].label.clone()));
        }
        Ok(Action::LiveOpen { dj, duration })
    }))
}

pub fn live_close(dj: &str) -> Modal {
    Modal::Confirm(Confirm::new(
        tr!("confirm-live-close-title"),
        vec![tr!("confirm-live-close-body", dj = dj.to_string())],
        tr!("confirm-live-close-yes"),
        Action::LiveClose { dj: dj.to_string() },
    ))
}

pub fn enqueue() -> Modal {
    let fields = vec![Field::text(tr!("form-enqueue-playlist"), ""), Field::text(tr!("form-enqueue-media"), "")];
    Modal::Form(Form::new(tr!("form-enqueue-title"), fields, |f| {
        if let Some(empty) = f.iter().find(|x| x.value().is_empty()) {
            return Err(tr!("form-required", field = empty.label.clone()));
        }
        Ok(Action::Enqueue { playlist: f[0].value(), media: f[1].value() })
    }))
}

pub fn scan() -> Modal {
    Modal::Confirm(Confirm::new(tr!("confirm-scan-title"), vec![tr!("confirm-scan-body")], tr!("confirm-scan-yes"), Action::Scan))
}

pub fn plugin(name: &str, verb: PluginVerb) -> Modal {
    let (title, yes) = match verb {
        PluginVerb::Start => (tr!("confirm-plugin-start", name = name.to_string()), tr!("plugin-verb-start")),
        PluginVerb::Stop => (tr!("confirm-plugin-stop", name = name.to_string()), tr!("plugin-verb-stop")),
        PluginVerb::Restart => (tr!("confirm-plugin-restart", name = name.to_string()), tr!("plugin-verb-restart")),
        PluginVerb::Reload => (tr!("confirm-plugin-reload", name = name.to_string()), tr!("plugin-verb-reload")),
    };
    let c = Confirm::new(title, vec![tr!("confirm-plugin-body")], yes, Action::Plugin { name: name.to_string(), verb });
    Modal::Confirm(if verb == PluginVerb::Stop { c.danger() } else { c })
}

/// Arrêt opérateur : deux confirmations, la seconde redit l'effet.
pub fn shutdown(force: bool, dj_on_air: Option<&str>) -> Modal {
    let mut lines = vec![tr!("confirm-shutdown-body")];
    match (force, dj_on_air) {
        (true, Some(dj)) => lines.push(tr!("confirm-shutdown-kicks", dj = dj.to_string())),
        (false, Some(dj)) => lines.push(tr!("confirm-shutdown-refused-live", dj = dj.to_string())),
        _ => {}
    }
    let action = Action::Shutdown { force };
    let second = Confirm::new(
        tr!("confirm-shutdown-again-title"),
        vec![tr!("confirm-shutdown-again-body")],
        tr!("confirm-shutdown-yes"),
        action.clone(),
    )
    .danger();
    Modal::Confirm(
        Confirm::new(tr!("confirm-shutdown-title"), lines, tr!("confirm-shutdown-continue"), action)
            .danger()
            .then(second),
    )
}

/// Valeurs saisies → changements à écrire. Un seul fichier (`original`
/// connu) : seuls les champs modifiés partent, vide = champ retiré. Un lot
/// (`None`) : vide = inchangé. L'année doit être un nombre de 1 à 9999.
pub fn tag_changes(original: Option<&stationd_proto::library::MediaTags>, v: [&str; 5], year_label: &str) -> Result<TagChanges, String> {
    let [title, artist, album, year, genre] = v.map(str::trim);
    let year_n: Option<u32> = match year {
        "" => None,
        y => Some(
            y.parse::<u32>()
                .ok()
                .filter(|n| (1..=9999).contains(n))
                .ok_or_else(|| tr!("form-year-invalid", field = year_label.to_string()))?,
        ),
    };
    let out = match original {
        Some(o) => {
            let text = |new: &str, old: &str| (new != old).then(|| new.to_string());
            TagChanges {
                title: text(title, &o.title),
                artist: text(artist, &o.artist),
                album: text(album, &o.album),
                year: (year_n.unwrap_or(0) != o.year).then(|| year_n.unwrap_or(0)),
                genre: text(genre, &o.genre),
            }
        }
        None => {
            let text = |new: &str| (!new.is_empty()).then(|| new.to_string());
            TagChanges { title: text(title), artist: text(artist), album: text(album), year: year_n, genre: text(genre) }
        }
    };
    if out.is_empty() {
        return Err(tr!("form-tags-nothing"));
    }
    Ok(out)
}

/// Lignes de la confirmation : ce qui va changer, champ par champ.
fn tag_lines(c: &TagChanges) -> Vec<String> {
    let mut out = Vec::new();
    let mut line = |label: String, v: &Option<String>| {
        if let Some(v) = v {
            out.push(if v.is_empty() { tr!("confirm-tags-remove", field = label) } else { tr!("confirm-tags-set", field = label, value = v.clone()) });
        }
    };
    line(tr!("media-field-title"), &c.title);
    line(tr!("media-field-artist"), &c.artist);
    line(tr!("media-field-album"), &c.album);
    line(tr!("form-tags-genre"), &c.genre);
    if let Some(y) = c.year {
        out.push(if y == 0 { tr!("confirm-tags-remove", field = tr!("media-field-year")) } else { tr!("confirm-tags-set", field = tr!("media-field-year"), value = y.to_string()) });
    }
    out
}

fn tag_fields(t: Option<&stationd_proto::library::MediaTags>) -> Vec<Field> {
    let g = |f: fn(&stationd_proto::library::MediaTags) -> String| t.map(f).unwrap_or_default();
    vec![
        Field::text(tr!("media-field-title"), &g(|t| t.title.clone())),
        Field::text(tr!("media-field-artist"), &g(|t| t.artist.clone())),
        Field::text(tr!("media-field-album"), &g(|t| t.album.clone())),
        Field::text(tr!("media-field-year"), &g(|t| if t.year == 0 { String::new() } else { t.year.to_string() })),
        Field::text(tr!("form-tags-genre"), &g(|t| t.genre.clone())),
    ]
}

fn values(f: &[Field]) -> [String; 5] {
    [f[0].value(), f[1].value(), f[2].value(), f[3].value(), f[4].value()]
}

/// Modifier les tags d'UN fichier (valeurs lues dans le fichier), puis
/// confirmation qui dit ce qui sera écrit.
pub fn edit_tags(t: stationd_proto::library::MediaTags) -> Modal {
    let path = t.rel_path.clone();
    let orig = t.clone();
    let form = Form::new(tr!("form-tags-title", path = path.clone()), tag_fields(Some(&t)), move |f| {
        let v = values(f);
        let edit = tag_changes(Some(&orig), [&v[0], &v[1], &v[2], &v[3], &v[4]], &f[3].label)?;
        Ok(Action::SetTags { targets: vec![(orig.rel_path.clone(), orig.revision.clone())], edit })
    })
    .confirm_with(move |a| {
        let Action::SetTags { edit, .. } = a else { return None };
        let mut lines = vec![tr!("confirm-tags-body", path = path.clone())];
        lines.extend(tag_lines(edit));
        Some(Confirm::new(tr!("confirm-tags-title"), lines, tr!("confirm-tags-yes"), a.clone()))
    });
    Modal::Form(form)
}

/// Modifier les tags de plusieurs fichiers : champ vide = inchangé.
pub fn edit_tags_many(paths: Vec<String>) -> Modal {
    let n = paths.len();
    let form = Form::new(tr!("form-tags-many-title", n = n), tag_fields(None), move |f| {
        let v = values(f);
        let edit = tag_changes(None, [&v[0], &v[1], &v[2], &v[3], &v[4]], &f[3].label)?;
        Ok(Action::SetTags { targets: paths.iter().map(|p| (p.clone(), String::new())).collect(), edit })
    })
    .confirm_with(move |a| {
        let Action::SetTags { edit, .. } = a else { return None };
        let mut lines = vec![tr!("confirm-tags-many-body", n = n)];
        lines.extend(tag_lines(edit));
        Some(Confirm::new(tr!("confirm-tags-title"), lines, tr!("confirm-tags-yes"), a.clone()).danger())
    });
    Modal::Form(form)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags() -> stationd_proto::library::MediaTags {
        stationd_proto::library::MediaTags {
            rel_path: "a.mp3".into(),
            title: "T".into(),
            artist: "A".into(),
            year: 2001,
            revision: "tags:1".into(),
            ..Default::default()
        }
    }

    #[test]
    fn one_file_sends_only_what_changed_and_empty_removes() {
        let c = tag_changes(Some(&tags()), ["T", "", "Alb", "2001", ""], "année").unwrap();
        assert_eq!(c, TagChanges { artist: Some(String::new()), album: Some("Alb".into()), ..Default::default() });
        let c = tag_changes(Some(&tags()), ["T", "A", "", "", ""], "année").unwrap();
        assert_eq!(c.year, Some(0), "année effacée");
        assert!(tag_changes(Some(&tags()), ["T", "A", "", "2001", ""], "année").is_err(), "rien n'a changé");
    }

    #[test]
    fn a_batch_leaves_empty_fields_unchanged_and_checks_the_year() {
        let c = tag_changes(None, ["", "", "", "", "talks"], "année").unwrap();
        assert_eq!(c, TagChanges { genre: Some("talks".into()), ..Default::default() });
        assert!(tag_changes(None, ["", "", "", "", ""], "année").is_err());
        assert!(tag_changes(None, ["", "", "", "20 01", ""], "année").is_err());
        assert!(tag_changes(None, ["", "", "", "0", ""], "année").is_err());
    }
}
