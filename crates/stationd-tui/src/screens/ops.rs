//! Actions partagées entre écrans (Antenne, Contrôle) : chaque fonction
//! construit la modale qui nomme exactement ce qui va être fait, ou l'action
//! quand elle ne touche pas l'antenne. L'exécution reste dans `action`.

use stationd_proto::broadcast::State;

use crate::action::{Action, OverrideContent, PluginVerb};
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
    let fields = vec![
        Field::choice(
            tr!("form-override-kind"),
            vec![(tr!("form-override-kind-media"), "media".into()), (tr!("form-override-kind-playlist"), "playlist".into())],
        ),
        Field::text(tr!("form-override-target"), ""),
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
