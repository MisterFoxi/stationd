//! Agenda (dossier §5.4) : la projection de la grille (`ScheduleService.Preview`)
//! mise en forme pour l'affichage — bandes de fond, repères, fenêtres live,
//! créneaux d'affichage. Rien n'est calculé ici sur la grille elle-même :
//! stationd projette (priorités, validités, fenêtres ouvertes, marques
//! consommées), la TUI découpe ce qu'il rend selon l'horloge civile de la
//! station.
//!
//! Journées et semaines vont d'un minuit civil au suivant dans le fuseau de
//! la station : une journée fait 23, 24 ou 25 h. Les créneaux d'affichage sont
//! des instants réels (une heure répétée donne deux lignes, une heure sautée
//! aucune).

use jiff::civil::{Date, Time};
use jiff::tz::{AmbiguousOffset, TimeZone};
use jiff::{Timestamp, ToSpan};
use stationd_proto::schedule::decision::Origin;
use stationd_proto::schedule::{Occurrence, PreviewResponse};

/// La projection commence une heure avant la période demandée : les
/// rendez-vous « en retard » qu'une projection neuve rejoue à sa première
/// minute sont consommés avant, et la base active au début de la période
/// est connue. Ce qui tombe dans cette heure n'est pas affiché.
pub const WARMUP: i64 = 3600;

/// Pas d'affichage proposés (minutes), `+` / `-`.
pub const STEPS: [i64; 3] = [15, 30, 60];

/// Une période affichée : `[from, to)` en secondes epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Period {
    pub from: i64,
    pub to: i64,
}

impl Period {
    /// La fenêtre à demander à `Preview` (échauffement compris).
    pub fn request(&self) -> (i64, i64) {
        (self.from - WARMUP, self.to - self.from + WARMUP)
    }
}

/// Minuit civil de `date` dans `tz` (le premier instant du jour, même un
/// jour où minuit n'existe pas).
pub fn midnight(date: Date, tz: &TimeZone) -> Option<i64> {
    date.to_zoned(tz.clone()).ok().map(|z| z.timestamp().as_second())
}

/// La journée `date` : de son minuit au minuit suivant.
pub fn day(date: Date, tz: &TimeZone) -> Option<Period> {
    Some(Period { from: midnight(date, tz)?, to: midnight(date.tomorrow().ok()?, tz)? })
}

/// Le lundi de la semaine de `date`.
pub fn monday(date: Date) -> Date {
    let back = i64::from(date.weekday().to_monday_zero_offset());
    date.checked_sub(back.days()).unwrap_or(date)
}

/// La semaine (lundi → lundi suivant) qui contient `date`.
pub fn week(date: Date, tz: &TimeZone) -> Option<Period> {
    let m = monday(date);
    Some(Period { from: midnight(m, tz)?, to: midnight(m.checked_add(7.days()).ok()?, tz)? })
}

/// Le jour civil de l'instant `t` dans `tz`.
pub fn date_of(t: i64, tz: &TimeZone) -> Option<Date> {
    Some(Timestamp::from_second(t).ok()?.to_zoned(tz.clone()).date())
}

/// `n` jours après `date` (avant si négatif).
pub fn shift(date: Date, n: i64) -> Date {
    date.checked_add(n.days()).unwrap_or(date)
}

/// Décalage UTC en secondes à l'instant `t`.
pub fn offset_at(t: i64, tz: &TimeZone) -> i32 {
    Timestamp::from_second(t).map(|ts| tz.to_offset(ts).seconds()).unwrap_or(0)
}

/// La période traverse-t-elle un changement d'heure ?
pub fn has_transition(p: Period, tz: &TimeZone) -> bool {
    offset_at(p.from, tz) != offset_at(p.to - 1, tz)
}

/// `HH:MM` local, suivi du décalage (`+02`) si `with_offset`.
pub fn hm(t: i64, tz: &TimeZone, with_offset: bool) -> String {
    let Ok(ts) = Timestamp::from_second(t) else { return "??:??".into() };
    let z = ts.to_zoned(tz.clone());
    let base = z.strftime("%H:%M").to_string();
    if with_offset { format!("{base}{}", fmt_offset(z.offset().seconds())) } else { base }
}

/// `+02`, `-05`, `+05:30`.
pub fn fmt_offset(secs: i32) -> String {
    let sign = if secs < 0 { '-' } else { '+' };
    let a = secs.unsigned_abs();
    if a.is_multiple_of(3600) { format!("{sign}{:02}", a / 3600) } else { format!("{sign}{:02}:{:02}", a / 3600, (a % 3600) / 60) }
}

/// Un repère fixe dans le temps : rendez-vous, ouverture live. Les `every`
/// n'en sont pas (leur cadence suit le dernier passage ou le compteur de
/// pistes) : ils vont dans [`Projection::floating`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkKind {
    Hard,
    Soft,
    /// Ouverture d'une fenêtre de connexion live.
    Live,
}

impl MarkKind {
    pub fn glyph(self) -> &'static str {
        match self {
            MarkKind::Hard => "!",
            MarkKind::Soft => "*",
            MarkKind::Live => "♪",
        }
    }
}

/// Une bande de fond : la base active (`day_part`, `base_rotation`, ou le
/// filet de sécurité faute de règle).
#[derive(Debug, Clone, PartialEq)]
pub struct Band {
    pub start: i64,
    pub end: i64,
    pub origin: Origin,
    pub rule_id: String,
    pub playlist_ref: String,
    /// L'occurrence de `Preview` qui l'a ouverte (pool, groupe).
    pub occ: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Mark {
    pub at: i64,
    pub kind: MarkKind,
    pub rule_id: String,
    /// Playlist (ou DJ pour un live).
    pub target: String,
    /// L'occurrence de `Preview` (absent pour un live).
    pub occ: Option<usize>,
}

/// Une fenêtre de connexion live, ramenée à la période.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveSpan {
    pub rule_id: String,
    pub dj: String,
    pub opens: i64,
    /// `None` = encore ouverte à la fin de la période.
    pub closes: Option<i64>,
    /// Ouverte avant le début de la période.
    pub open_before: bool,
}

/// Une projection prête à afficher.
#[derive(Debug, Clone, Default)]
pub struct Projection {
    pub bands: Vec<Band>,
    pub marks: Vec<Mark>,
    pub live: Vec<LiveSpan>,
    pub occurrences: Vec<Occurrence>,
    /// Les `every` qui passent dans la période, hors horloge : ceux à
    /// l'intervalle (projetés par le serveur, mais leur heure réelle suit le
    /// dernier passage) puis ceux au compteur de pistes. Ids de règle, sans
    /// doublon, dans l'ordre d'apparition.
    pub floating: Vec<String>,
}

/// Un instant (pas une bande) ; `None` dans l'option = `every`, sans place
/// sur la ligne du temps.
fn is_mark(o: Origin) -> Option<Option<MarkKind>> {
    match o {
        Origin::AtClockHard => Some(Some(MarkKind::Hard)),
        Origin::AtClockSoft => Some(Some(MarkKind::Soft)),
        Origin::Every => Some(None),
        _ => None,
    }
}

/// Met en forme la réponse de `Preview` demandée par `period.request()`.
pub fn project(resp: PreviewResponse, period: Period) -> Projection {
    let PreviewResponse { occurrences, indicative, live } = resp;
    let at = |o: &Occurrence| o.at_utc.as_ref().map(|t| t.seconds).unwrap_or(i64::MIN);

    // Bandes : une par changement de base ; un repère ne coupe pas la bande
    // (après un rendez-vous, la même base reprend : même bande).
    let mut bands: Vec<Band> = Vec::new();
    let mut marks: Vec<Mark> = Vec::new();
    let mut floating: Vec<String> = Vec::new();
    // Début des repères qui précèdent directement la base suivante : une
    // base qui change sous un rendez-vous (22:00 soft, puis la tranche de
    // 22:00 à la minute d'après) commence avec le premier de ces repères.
    let mut marks_since: Option<i64> = None;
    for (i, o) in occurrences.iter().enumerate() {
        let origin = Origin::try_from(o.origin).unwrap_or(Origin::Unspecified);
        let t = at(o);
        if let Some(kind) = is_mark(origin) {
            if t >= period.from && t < period.to {
                match kind {
                    Some(kind) => marks.push(Mark {
                        at: t,
                        kind,
                        rule_id: o.rule_id.clone(),
                        target: o.playlist_ref.clone(),
                        occ: Some(i),
                    }),
                    None if !floating.contains(&o.rule_id) => floating.push(o.rule_id.clone()),
                    None => {}
                }
            }
            marks_since.get_or_insert(t);
            continue;
        }
        let started = marks_since.take().unwrap_or(t);
        let same = bands
            .last()
            .is_some_and(|b| b.origin == origin && b.rule_id == o.rule_id && b.playlist_ref == o.playlist_ref);
        if same {
            continue;
        }
        let t = started;
        if let Some(prev) = bands.last_mut() {
            prev.end = t;
        }
        bands.push(Band {
            start: t,
            end: period.to,
            origin,
            rule_id: o.rule_id.clone(),
            playlist_ref: o.playlist_ref.clone(),
            occ: i,
        });
    }
    // Ramenées à la période.
    bands.retain(|b| b.end > period.from && b.start < period.to);
    for b in &mut bands {
        b.start = b.start.max(period.from);
        b.end = b.end.min(period.to);
    }

    let mut spans = Vec::new();
    for w in live {
        let opens = w.opens_at.as_ref().map(|t| t.seconds).unwrap_or(period.from);
        let closes = w.closes_at.as_ref().map(|t| t.seconds);
        if closes.is_some_and(|c| c <= period.from) || opens >= period.to {
            continue;
        }
        let open_before = w.open_before || opens < period.from;
        let opens = opens.max(period.from);
        let closes = closes.filter(|c| *c < period.to);
        if !open_before {
            marks.push(Mark { at: opens, kind: MarkKind::Live, rule_id: w.rule_id.clone(), target: w.dj.clone(), occ: None });
        }
        spans.push(LiveSpan { rule_id: w.rule_id, dj: w.dj, opens, closes, open_before });
    }
    marks.sort_by_key(|m| m.at);
    for r in &indicative {
        if !floating.contains(&r.rule_id) {
            floating.push(r.rule_id.clone());
        }
    }

    Projection { bands, marks, live: spans, occurrences, floating }
}

impl Projection {
    /// La bande active à l'instant `t`.
    pub fn band_at(&self, t: i64) -> Option<&Band> {
        self.bands.iter().find(|b| b.start <= t && t < b.end)
    }

    /// Bandes qui commencent dans `[a, b)`.
    pub fn bands_starting(&self, a: i64, b: i64) -> impl Iterator<Item = &Band> {
        self.bands.iter().filter(move |x| x.start >= a && x.start < b)
    }

    /// Repères dans `[a, b)`, dans l'ordre.
    pub fn marks_in(&self, a: i64, b: i64) -> Vec<&Mark> {
        self.marks.iter().filter(|m| m.at >= a && m.at < b).collect()
    }

    /// Fenêtres live ouvertes à un moment de `[a, b)`.
    pub fn live_in(&self, a: i64, b: i64) -> Vec<&LiveSpan> {
        self.live.iter().filter(|l| l.opens < b && l.closes.is_none_or(|c| c > a)).collect()
    }
}

/// Un créneau d'affichage `[start, end)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slot {
    pub start: i64,
    pub end: i64,
}

/// Créneaux de `step` minutes d'une période, en temps réel : une journée
/// de 25 h en a plus, une de 23 h moins.
pub fn slots(p: Period, step_min: i64) -> Vec<Slot> {
    let step = step_min.max(1) * 60;
    let mut out = Vec::new();
    let mut t = p.from;
    while t < p.to {
        let end = (t + step).min(p.to);
        out.push(Slot { start: t, end });
        t = end;
    }
    out
}

/// Heures civiles des lignes de la semaine (`00:00`, `00:30`…).
pub fn week_rows(step_min: i64) -> Vec<Time> {
    let step = step_min.clamp(1, 1440);
    (0..1440 / step)
        .filter_map(|i| Time::new(((i * step) / 60) as i8, ((i * step) % 60) as i8, 0, 0).ok())
        .collect()
}

/// Une case de la semaine : l'heure civile `time` du jour `date`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cell {
    /// Cette heure n'existe pas ce jour-là (passage à l'heure d'été).
    Gap,
    /// `[start, end)` : jusqu'à la ligne suivante (une heure répétée est
    /// comprise dans la case, qui dure alors plus longtemps).
    At(Slot),
}

pub fn cell(date: Date, time: Time, step_min: i64, tz: &TimeZone) -> Cell {
    let amb = tz.to_ambiguous_zoned(date.to_datetime(time));
    if matches!(amb.offset(), AmbiguousOffset::Gap { .. }) {
        return Cell::Gap;
    }
    let Ok(z) = amb.compatible() else { return Cell::Gap };
    let start = z.timestamp().as_second();
    // Fin : la ligne suivante le même jour, ou minuit suivant.
    let next_min = i64::from(time.hour()) * 60 + i64::from(time.minute()) + step_min;
    let end = if next_min >= 1440 {
        date.tomorrow().ok().and_then(|d| midnight(d, tz))
    } else {
        Time::new((next_min / 60) as i8, (next_min % 60) as i8, 0, 0)
            .ok()
            .and_then(|t| tz.to_ambiguous_zoned(date.to_datetime(t)).compatible().ok())
            .map(|z| z.timestamp().as_second())
    };
    let end = end.unwrap_or(start + step_min * 60).max(start + 60);
    Cell::At(Slot { start, end })
}

/// L'instant `t` tombe-t-il dans le créneau ?
pub fn contains(s: Slot, t: i64) -> bool {
    s.start <= t && t < s.end
}

#[cfg(test)]
mod tests {
    use super::*;
    use stationd_proto::prost_types;
    use stationd_proto::schedule::LiveWindow;

    fn paris() -> TimeZone {
        TimeZone::get("Europe/Paris").unwrap()
    }

    fn ts(s: i64) -> Option<prost_types::Timestamp> {
        Some(prost_types::Timestamp { seconds: s, nanos: 0 })
    }

    fn occ(t: i64, origin: Origin, rule: &str, pl: &str) -> Occurrence {
        Occurrence {
            at_utc: ts(t),
            rule_id: rule.into(),
            playlist_ref: pl.into(),
            origin: origin as i32,
            ..Default::default()
        }
    }

    #[test]
    fn days_run_from_civil_midnight_to_civil_midnight() {
        let tz = paris();
        let normal = day(Date::new(2026, 9, 29).unwrap(), &tz).unwrap();
        assert_eq!(normal.to - normal.from, 24 * 3600);
        // Passage à l'heure d'hiver : 25 h ; à l'heure d'été : 23 h.
        let autumn = day(Date::new(2026, 10, 25).unwrap(), &tz).unwrap();
        assert_eq!(autumn.to - autumn.from, 25 * 3600);
        assert!(has_transition(autumn, &tz));
        let spring = day(Date::new(2026, 3, 29).unwrap(), &tz).unwrap();
        assert_eq!(spring.to - spring.from, 23 * 3600);
        assert!(!has_transition(normal, &tz));
        // Créneaux en temps réel : 100 quarts d'heure un jour de 25 h.
        assert_eq!(slots(autumn, 15).len(), 100);
        assert_eq!(slots(spring, 60).len(), 23);
    }

    #[test]
    fn a_repeated_hour_gives_two_lines_with_their_offset() {
        let tz = paris();
        let autumn = day(Date::new(2026, 10, 25).unwrap(), &tz).unwrap();
        let labels: Vec<String> = slots(autumn, 60).iter().map(|s| hm(s.start, &tz, true)).collect();
        assert_eq!(labels[2], "02:00+02");
        assert_eq!(labels[3], "02:00+01");
        let spring = day(Date::new(2026, 3, 29).unwrap(), &tz).unwrap();
        let labels: Vec<String> = slots(spring, 60).iter().map(|s| hm(s.start, &tz, false)).collect();
        assert!(!labels.contains(&"02:00".to_string()), "heure sautée absente : {labels:?}");
    }

    #[test]
    fn weeks_start_on_monday() {
        let tz = paris();
        assert_eq!(monday(Date::new(2026, 10, 4).unwrap()), Date::new(2026, 9, 28).unwrap());
        assert_eq!(monday(Date::new(2026, 9, 28).unwrap()), Date::new(2026, 9, 28).unwrap());
        // La semaine du passage à l'heure d'hiver fait 169 h.
        let w = week(Date::new(2026, 10, 22).unwrap(), &tz).unwrap();
        assert_eq!(w.to - w.from, 169 * 3600);
    }

    #[test]
    fn week_cells_know_the_skipped_and_the_repeated_hour() {
        let tz = paris();
        let two = Time::new(2, 0, 0, 0).unwrap();
        assert_eq!(cell(Date::new(2026, 3, 29).unwrap(), two, 60, &tz), Cell::Gap);
        let Cell::At(s) = cell(Date::new(2026, 10, 25).unwrap(), two, 60, &tz) else { panic!() };
        assert_eq!(s.end - s.start, 2 * 3600, "l'heure répétée tient dans la case de 02:00");
        // …et seulement dans celle-là : la case de 01:00 s'arrête où elle commence.
        let Cell::At(one) = cell(Date::new(2026, 10, 25).unwrap(), Time::new(1, 0, 0, 0).unwrap(), 60, &tz) else {
            panic!()
        };
        assert_eq!((one.end - one.start, one.end), (3600, s.start));
        // Au printemps, la case d'avant l'heure sautée va jusqu'à 03:00 (1 h réelle).
        let Cell::At(one) = cell(Date::new(2026, 3, 29).unwrap(), Time::new(1, 0, 0, 0).unwrap(), 60, &tz) else {
            panic!()
        };
        assert_eq!(one.end - one.start, 3600);
        let Cell::At(s) = cell(Date::new(2026, 9, 29).unwrap(), Time::new(23, 30, 0, 0).unwrap(), 30, &tz) else {
            panic!()
        };
        assert_eq!(s.end - s.start, 1800);
        assert_eq!(week_rows(15).len(), 96);
    }

    #[test]
    fn projection_keeps_the_base_across_marks_and_drops_the_warmup() {
        let p = Period { from: 10_000, to: 20_000 };
        let resp = PreviewResponse {
            occurrences: vec![
                occ(6_400, Origin::BaseRotation, "floor", "music"),
                // Rendez-vous de l'échauffement : pas affiché.
                occ(6_400 + 60, Origin::AtClockSoft, "top", "jingle"),
                occ(6_400 + 120, Origin::BaseRotation, "floor", "music"),
                occ(12_000, Origin::AtClockHard, "news", "flash"),
                occ(12_060, Origin::BaseRotation, "floor", "music"),
                occ(15_000, Origin::DayPart, "night", "nuit"),
                occ(18_000, Origin::Every, "cool", "pub"),
                occ(18_060, Origin::DayPart, "night", "nuit"),
            ],
            ..Default::default()
        };
        let pr = project(resp, p);
        assert_eq!(pr.bands.len(), 2, "{:?}", pr.bands);
        assert_eq!((pr.bands[0].start, pr.bands[0].end), (10_000, 15_000));
        // Une tranche qui commence sous des rendez-vous commence avec eux.
        let under = PreviewResponse {
            occurrences: vec![
                occ(6_400, Origin::BaseRotation, "floor", "music"),
                occ(15_000, Origin::AtClockSoft, "top", "jingle"),
                occ(15_060, Origin::Every, "cool", "pub"),
                occ(15_120, Origin::DayPart, "night", "nuit"),
            ],
            ..Default::default()
        };
        let u = project(under, p);
        assert_eq!(u.bands[1].start, 15_000, "{:?}", u.bands);
        assert_eq!(u.bands[0].end, 15_000);
        assert_eq!(pr.bands[0].playlist_ref, "music");
        assert_eq!((pr.bands[1].start, pr.bands[1].end), (15_000, 20_000));
        let kinds: Vec<_> = pr.marks.iter().map(|m| (m.at, m.kind)).collect();
        // Un `every` n'est pas sur la ligne du temps : hors horloge.
        assert_eq!(kinds, vec![(12_000, MarkKind::Hard)]);
        assert_eq!(pr.floating, vec!["cool".to_string()]);
        assert_eq!(pr.band_at(12_030).unwrap().rule_id, "floor");
        assert_eq!(pr.marks_in(11_000, 13_000).len(), 1);
    }

    #[test]
    fn live_windows_are_clipped_and_open_with_a_mark() {
        let p = Period { from: 10_000, to: 20_000 };
        let resp = PreviewResponse {
            live: vec![
                LiveWindow { rule_id: "a".into(), dj: "alex".into(), opens_at: ts(6_400), open_before: true, closes_at: ts(11_000), ..Default::default() },
                LiveWindow { rule_id: "b".into(), dj: "bea".into(), opens_at: ts(15_000), ..Default::default() },
                LiveWindow { rule_id: "c".into(), dj: "cyd".into(), opens_at: ts(7_000), closes_at: ts(9_000), ..Default::default() },
            ],
            ..Default::default()
        };
        let pr = project(resp, p);
        assert_eq!(pr.live.len(), 2);
        assert!(pr.live[0].open_before && pr.live[0].opens == 10_000 && pr.live[0].closes == Some(11_000));
        assert_eq!(pr.live[1].closes, None);
        assert_eq!(pr.marks.len(), 1, "seule une ouverture dans la période fait un repère");
        assert_eq!((pr.marks[0].at, pr.marks[0].kind), (15_000, MarkKind::Live));
        assert_eq!(pr.live_in(10_500, 10_600).len(), 1);
        assert_eq!(pr.live_in(12_000, 13_000).len(), 0);
    }

    #[test]
    fn offsets_are_short() {
        assert_eq!(fmt_offset(7200), "+02");
        assert_eq!(fmt_offset(-18_000), "-05");
        assert_eq!(fmt_offset(19_800), "+05:30");
    }
}
