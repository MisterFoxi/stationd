//! Brouillon d'une règle de grille : le TOML de la grille modifié par
//! `toml_edit` (commentaires et mise en forme gardés), une règle à la fois.
//! La TUI ne juge rien : les valeurs sont écrites telles que saisies (un
//! nombre mal tapé reste du texte) et stationd dit ce qui ne va pas
//! (`ValidateGrid`, diagnostics sur `rule[n].champ`).

use toml_edit::{Array, ArrayOfTables, DocumentMut, Item, Table, value};

/// Les natures de règle, dans l'ordre du sélecteur.
pub const KINDS: [&str; 5] = ["day_part", "at_clock", "every", "live", "base_rotation"];

/// Jours de la grammaire, lundi d'abord.
pub const DAYS: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];

/// Une règle telle que le formulaire la montre : chaque champ en texte
/// (vide = absent du TOML), les jours en cases.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuleFields {
    pub id: String,
    pub enabled: bool,
    pub kind: String,
    pub playlist_ref: String,
    pub dj: String,
    pub start: String,
    pub end: String,
    /// `at_clock` : `every_minutes`, `minute` ou `at`.
    pub anchor: String,
    pub anchor_value: String,
    pub mode: String,
    pub expiry: String,
    /// `every` : `min_tracks` ou `min_elapsed`.
    pub cadence: String,
    pub cadence_value: String,
    pub days: [bool; 7],
    pub date_start: String,
    pub date_end: String,
}

impl RuleFields {
    /// Une règle neuve de nature `kind`.
    pub fn new(kind: &str) -> Self {
        Self {
            enabled: true,
            kind: kind.to_string(),
            anchor: "at".into(),
            mode: "soft".into(),
            cadence: "min_elapsed".into(),
            ..Self::default()
        }
    }
}

fn text(item: Option<&Item>) -> String {
    match item {
        Some(Item::Value(v)) => match v.as_str() {
            Some(s) => s.to_string(),
            None => v.to_string().trim().to_string(),
        },
        _ => String::new(),
    }
}

/// Lit la table d'une règle.
pub fn read(t: &Table) -> RuleFields {
    let mut f = RuleFields::new(&text(t.get("kind")));
    f.id = text(t.get("id"));
    f.enabled = t.get("enabled").and_then(|i| i.as_bool()).unwrap_or(true);
    f.playlist_ref = text(t.get("playlist_ref"));
    f.dj = text(t.get("dj"));
    f.start = text(t.get("start"));
    f.end = text(t.get("end"));
    if t.contains_key("every_minutes") {
        f.anchor = "every_minutes".into();
        f.anchor_value = text(t.get("every_minutes"));
    } else if t.contains_key("minute") {
        f.anchor = "minute".into();
        f.anchor_value = text(t.get("minute"));
    } else {
        f.anchor = "at".into();
        f.anchor_value = text(t.get("at"));
    }
    let mode = text(t.get("mode"));
    f.mode = if mode.is_empty() { "soft".into() } else { mode };
    f.expiry = text(t.get("expiry"));
    if t.contains_key("min_tracks") {
        f.cadence = "min_tracks".into();
        f.cadence_value = text(t.get("min_tracks"));
    } else {
        f.cadence = "min_elapsed".into();
        f.cadence_value = text(t.get("min_elapsed"));
    }
    if let Some(a) = t.get("days").and_then(|i| i.as_array()) {
        for v in a.iter() {
            if let Some(i) = v.as_str().and_then(|d| DAYS.iter().position(|x| *x == d)) {
                f.days[i] = true;
            }
        }
    }
    f.date_start = text(t.get("date_start"));
    f.date_end = text(t.get("date_end"));
    f
}

/// Un nombre s'il en est un, sinon le texte tel quel (stationd dira).
fn number_or_text(s: &str) -> Item {
    match s.trim().parse::<i64>() {
        Ok(n) => value(n),
        Err(_) => value(s),
    }
}

/// Écrit `f` dans la table : seuls les champs de sa nature, un champ vide
/// est retiré ; les champs inconnus du formulaire ne sont pas touchés.
pub fn write(t: &mut Table, f: &RuleFields) {
    let mut set = |k: &str, v: Option<Item>| match v {
        Some(v) => {
            match t.get_mut(k) {
                // Garder la décoration (commentaire de fin de ligne) d'une valeur existante.
                Some(Item::Value(old)) if v.is_value() => {
                    let decor = old.decor().clone();
                    let mut nv = v.into_value().expect("value");
                    *nv.decor_mut() = decor;
                    *old = nv;
                }
                _ => {
                    t.insert(k, v);
                }
            }
        }
        None => {
            t.remove(k);
        }
    };
    let opt = |s: &str| (!s.trim().is_empty()).then(|| value(s.trim()));
    let kind = f.kind.as_str();
    set("id", Some(value(f.id.trim())));
    set("enabled", (!f.enabled).then(|| value(false)));
    set("kind", Some(value(kind)));
    set("playlist_ref", if kind == "live" { None } else { Some(value(f.playlist_ref.trim())) });
    set("dj", if kind == "live" { Some(value(f.dj.trim())) } else { None });
    set("start", if matches!(kind, "day_part" | "live") { opt(&f.start) } else { None });
    set("end", if kind == "day_part" { opt(&f.end) } else { None });
    let at_clock = kind == "at_clock";
    set(
        "every_minutes",
        (at_clock && f.anchor == "every_minutes" && !f.anchor_value.trim().is_empty())
            .then(|| number_or_text(&f.anchor_value)),
    );
    set(
        "minute",
        (at_clock && f.anchor == "minute" && !f.anchor_value.trim().is_empty())
            .then(|| number_or_text(&f.anchor_value)),
    );
    set("at", (at_clock && f.anchor == "at").then(|| opt(&f.anchor_value)).flatten());
    set("mode", (at_clock && f.mode == "hard").then(|| value("hard")));
    set("expiry", if at_clock { opt(&f.expiry) } else { None });
    let every = kind == "every";
    set(
        "min_tracks",
        (every && f.cadence == "min_tracks" && !f.cadence_value.trim().is_empty())
            .then(|| number_or_text(&f.cadence_value)),
    );
    set("min_elapsed", (every && f.cadence == "min_elapsed").then(|| opt(&f.cadence_value)).flatten());
    let days: Vec<&str> = DAYS.iter().zip(f.days).filter(|(_, on)| *on).map(|(d, _)| *d).collect();
    set("days", (!days.is_empty()).then(|| value(Array::from_iter(days))));
    set("date_start", opt(&f.date_start));
    set("date_end", opt(&f.date_end));
}

/// Le document d'une grille ; un texte vide donne une grille vide.
pub fn parse(text: &str) -> Result<DocumentMut, String> {
    let mut doc = if text.trim().is_empty() {
        DocumentMut::new()
    } else {
        text.parse::<DocumentMut>().map_err(|e| e.to_string())?
    };
    if !doc.contains_key("schema_version") {
        doc.insert("schema_version", value(1));
    }
    Ok(doc)
}

fn rules(doc: &DocumentMut) -> Option<&ArrayOfTables> {
    doc.get("rule").and_then(|i| i.as_array_of_tables())
}

/// Position (0…) de la règle `id` dans le fichier.
pub fn index_of(doc: &DocumentMut, id: &str) -> Option<usize> {
    rules(doc)?.iter().position(|t| t.get("id").and_then(|i| i.as_str()) == Some(id))
}

/// Nombre de règles du fichier.
pub fn count(doc: &DocumentMut) -> usize {
    rules(doc).map(|a| a.len()).unwrap_or(0)
}

/// Les champs de la règle n° `index`.
pub fn read_at(doc: &DocumentMut, index: usize) -> Option<RuleFields> {
    rules(doc)?.get(index).map(read)
}

/// Le texte de la grille avec la règle n° `index` remplacée par `f` —
/// ajoutée en fin de fichier si `index` vaut le nombre de règles.
pub fn with_rule(text: &str, index: usize, f: &RuleFields) -> Result<String, String> {
    let mut doc = parse(text)?;
    if !doc.contains_key("rule") {
        doc.insert("rule", Item::ArrayOfTables(ArrayOfTables::new()));
    }
    let arr = doc["rule"].as_array_of_tables_mut().ok_or("`rule` is not an array of tables")?;
    if index == arr.len() {
        arr.push(Table::new());
    }
    let t = arr.get_mut(index).ok_or("no such rule")?;
    write(t, f);
    Ok(doc.to_string())
}

/// Le texte de la grille sans la règle `id`.
pub fn without_rule(text: &str, id: &str) -> Result<String, String> {
    let mut doc = parse(text)?;
    let i = index_of(&doc, id).ok_or_else(|| format!("no rule `{id}`"))?;
    let arr = doc["rule"].as_array_of_tables_mut().ok_or("`rule` is not an array of tables")?;
    arr.remove(i);
    Ok(doc.to_string())
}

/// Un id libre : `base`, sinon `base-2`, `base-3`…
pub fn free_id(doc: &DocumentMut, base: &str) -> String {
    let taken = |id: &str| index_of(doc, id).is_some();
    if !taken(base) {
        return base.to_string();
    }
    (2..).map(|n| format!("{base}-{n}")).find(|c| !taken(c)).unwrap_or_else(|| base.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const GRID: &str = "schema_version = 1\n\n# Plancher\n[[rule]]\nid = \"plancher\"\nkind = \"base_rotation\"\nplaylist_ref = \"mix\"  # le mix\n\n[[rule]]\nid = \"top\"\nkind = \"at_clock\"\nplaylist_ref = \"jingles\"\nevery_minutes = 60\nexpiry = \"5m\"\ndays = [\"mon\", \"fri\"]\n";

    #[test]
    fn a_rule_reads_into_fields() {
        let doc = parse(GRID).unwrap();
        assert_eq!(count(&doc), 2);
        let f = read_at(&doc, index_of(&doc, "top").unwrap()).unwrap();
        assert_eq!((f.kind.as_str(), f.anchor.as_str(), f.anchor_value.as_str()), ("at_clock", "every_minutes", "60"));
        assert_eq!(f.expiry, "5m");
        assert_eq!(f.days, [true, false, false, false, true, false, false]);
        assert!(f.enabled);
    }

    #[test]
    fn editing_keeps_comments_and_writes_only_the_kind_fields() {
        let doc = parse(GRID).unwrap();
        let mut f = read_at(&doc, 0).unwrap();
        f.playlist_ref = "musique".into();
        f.start = "07:00".into(); // not a base_rotation field: not written
        let out = with_rule(GRID, 0, &f).unwrap();
        assert!(out.contains("# Plancher") && out.contains("# le mix"), "{out}");
        assert!(out.contains("playlist_ref = \"musique\""), "{out}");
        assert!(!out.contains("start"), "{out}");
        // A switch of anchor rewrites the right key only.
        let mut t = read_at(&doc, 1).unwrap();
        t.anchor = "at".into();
        t.anchor_value = "08:00".into();
        t.mode = "hard".into();
        t.days = [false; 7];
        let out = with_rule(GRID, 1, &t).unwrap();
        assert!(out.contains("at = \"08:00\"") && out.contains("mode = \"hard\""), "{out}");
        assert!(!out.contains("every_minutes") && !out.contains("days"), "{out}");
        // The top of the hour, offset: `minute` alone.
        t.anchor = "minute".into();
        t.anchor_value = "58".into();
        let out = with_rule(GRID, 1, &t).unwrap();
        assert!(out.contains("minute = 58") && !out.contains("at = ") && !out.contains("every_minutes"), "{out}");
        let back = read_at(&parse(&out).unwrap(), 1).unwrap();
        assert_eq!((back.anchor.as_str(), back.anchor_value.as_str()), ("minute", "58"));
    }

    #[test]
    fn a_new_rule_is_appended_and_a_typo_stays_text() {
        let mut f = RuleFields::new("day_part");
        f.id = "evt".into();
        f.playlist_ref = "nuit".into();
        f.start = "22:00".into();
        f.date_start = "2026-10-03".into();
        f.date_end = "2026-10-03".into();
        let out = with_rule(GRID, 2, &f).unwrap();
        let doc = parse(&out).unwrap();
        assert_eq!(count(&doc), 3);
        assert_eq!(read_at(&doc, 2).unwrap().date_start, "2026-10-03");
        assert!(out.ends_with("date_end = \"2026-10-03\"\n"), "{out}");
        // Empty file: schema_version added.
        let out = with_rule("", 0, &f).unwrap();
        assert!(out.starts_with("schema_version = 1"), "{out}");
        let mut e = RuleFields::new("every");
        e.id = "x".into();
        e.cadence = "min_tracks".into();
        e.cadence_value = "4a".into();
        assert!(with_rule("", 0, &e).unwrap().contains("min_tracks = \"4a\""));
    }

    #[test]
    fn a_rule_is_removed_and_ids_are_kept_free() {
        let out = without_rule(GRID, "top").unwrap();
        assert!(!out.contains("id = \"top\"") && out.contains("# Plancher"), "{out}");
        let doc = parse(GRID).unwrap();
        assert_eq!(free_id(&doc, "top"), "top-2");
        assert_eq!(free_id(&doc, "evt"), "evt");
        assert!(without_rule(GRID, "nope").is_err());
    }
}
