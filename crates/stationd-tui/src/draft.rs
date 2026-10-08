//! Brouillon de playlist : le TOML lui-même, lu et modifié avec `toml_edit`
//! pour garder les commentaires et la mise en forme de l'utilisateur.
//!
//! Aucune règle métier ici (D2) : ce module ne VALIDE rien, il range des
//! valeurs aux bons endroits de la grammaire (dossier §3.5 : le format
//! d'échange est le TOML, stationd seul juge). Une valeur mal typée est
//! écrite telle quelle — stationd la refusera sur son champ (`BAD_VALUE`) et
//! le formulaire l'affichera là.
//!
//! Seule commodité : changer de mode met de côté les champs de l'ancien
//! mode (et les rend si on y revient), pour que le formulaire n'envoie pas
//! des champs que le nouveau mode interdit.

use std::collections::HashMap;

use toml_edit::{Array, ArrayOfTables, DocumentMut, InlineTable, Item, Table, Value};

/// Les 5 modes de la grammaire, dans l'ordre du sélecteur.
pub const MODES: [&str; 5] = ["dynamic", "static", "group", "queue", "remote"];

/// Champs de `[selection]` propres à chaque mode (hors `mode`).
fn mode_keys(mode: &str) -> &'static [&'static str] {
    match mode {
        "static" => &["order", "files"],
        "dynamic" => &["order", "match", "filter", "order_by", "unplayed_only"],
        "remote" => &["url"],
        "queue" => &["order", "max_len"],
        "group" => &["strategy", "members", "on_member_unavailable"],
        _ => &[],
    }
}

/// Valeurs de `order` proposées par mode (le sélecteur, pas un contrôle).
pub fn orders(mode: &str) -> &'static [&'static str] {
    match mode {
        "static" => &["shuffle", "sequential"],
        "dynamic" => &["shuffle", "sequential", "newest", "oldest"],
        "queue" => &["fifo", "lifo"],
        _ => &[],
    }
}

/// Champs de filtre du catalogue, et opérateurs proposés pour chacun.
pub const FILTER_FIELDS: [&str; 14] = [
    "path", "genre", "genre_ai", "mood", "artist", "title", "album", "year", "duration", "age",
    "creation", "tempo", "play_count", "last_played",
];

pub fn filter_ops(field: &str) -> &'static [&'static str] {
    match field {
        "path" => &["prefix", "eq", "ne"],
        "title" | "artist" | "album" => &["contains", "eq", "ne", "prefix"],
        "genre_ai" | "mood" => &["contains", "eq", "ne"],
        "year" | "duration" => &[">=", "<=", "=", "!=", ">", "<"],
        "genre" => &["has_any", "has", "has_all", "has_none"],
        // Âge de la date de création, durée (`10d`) : `<` = plus récent que.
        "age" => &["<", "<=", ">", ">="],
        // Date RFC 3339 avec fuseau (`2026-09-01T00:00:00+02:00`).
        "creation" => &[">=", "<=", ">", "<", "eq", "ne"],
        "tempo" => &["eq", "ne"],
        // Historique : nombre de passages (entier) dans la fenêtre `within`.
        "play_count" => &[">=", "<=", "=", "!=", ">", "<"],
        // Âge du dernier passage, durée (`7d`) : `>=` = pas passé depuis.
        "last_played" => &["<", "<=", ">", ">="],
        _ => &[],
    }
}

/// Un opérateur qui attend une liste (valeurs séparées par des virgules).
fn list_op(op: &str) -> bool {
    matches!(op, "has_any" | "has_all" | "has_none")
}

fn numeric_field(field: &str) -> bool {
    matches!(field, "year" | "duration" | "play_count")
}

/// Un champ simple du brouillon (hors listes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    Name,
    Enabled,
    Mode,
    Order,
    Match,
    OrderBy,
    UnplayedOnly,
    Url,
    MaxLen,
    Strategy,
    OnMemberUnavailable,
    Limit,
    Repeat,
    OnExhausted,
    NoSameArtist,
    NoSameTrack,
    NoSameTitle,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Str,
    Int,
    Bool,
}

impl Key {
    /// Chemin de table + nom de clé, dans la grammaire.
    fn place(self) -> (&'static [&'static str], &'static str, Kind) {
        const SEL: &[&str] = &["selection"];
        const BC: &[&str] = &["broadcast"];
        const CT: &[&str] = &["broadcast", "constraints"];
        match self {
            Key::Name => (&[], "name", Kind::Str),
            Key::Enabled => (&[], "enabled", Kind::Bool),
            Key::Mode => (SEL, "mode", Kind::Str),
            Key::Order => (SEL, "order", Kind::Str),
            Key::Match => (SEL, "match", Kind::Str),
            Key::OrderBy => (SEL, "order_by", Kind::Str),
            Key::UnplayedOnly => (SEL, "unplayed_only", Kind::Bool),
            Key::Url => (SEL, "url", Kind::Str),
            Key::MaxLen => (SEL, "max_len", Kind::Int),
            Key::Strategy => (SEL, "strategy", Kind::Str),
            Key::OnMemberUnavailable => (SEL, "on_member_unavailable", Kind::Str),
            Key::Limit => (BC, "limit", Kind::Int),
            Key::Repeat => (BC, "repeat", Kind::Bool),
            Key::OnExhausted => (BC, "on_exhausted", Kind::Str),
            Key::NoSameArtist => (CT, "no_same_artist_within", Kind::Str),
            Key::NoSameTrack => (CT, "no_same_track_within", Kind::Str),
            Key::NoSameTitle => (CT, "no_same_title_within", Kind::Str),
        }
    }

    /// `field_path` des diagnostics de stationd (`selection.order`…).
    pub fn path(self) -> String {
        let (tables, key, _) = self.place();
        let mut p: Vec<&str> = tables.to_vec();
        p.push(key);
        p.join(".")
    }
}

/// Un filtre dynamique, tel qu'affiché. `within` n'est rempli que pour le
/// filtre d'historique `play_count` (sa fenêtre glissante) ; vide ailleurs.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FilterView {
    pub field: String,
    pub op: String,
    pub value: String,
    pub within: String,
}

/// Un membre de groupe, tel qu'affiché (vide = absent).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MemberView {
    pub r#ref: String,
    pub weight: String,
    pub take: String,
    pub take_random_min: String,
    pub take_random_max: String,
    pub runtime: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterPart {
    Field,
    Op,
    Value,
    Within,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberPart {
    Ref,
    Weight,
    Take,
    TakeRandomMin,
    TakeRandomMax,
    Runtime,
}

impl MemberPart {
    fn key(self) -> &'static str {
        match self {
            MemberPart::Ref => "ref",
            MemberPart::Weight => "weight",
            MemberPart::Take => "take",
            MemberPart::TakeRandomMin => "take_random_min",
            MemberPart::TakeRandomMax => "take_random_max",
            MemberPart::Runtime => "runtime",
        }
    }
}

/// Le brouillon : le texte (ce qui part vers stationd) et son document s'il
/// se lit. Un texte illisible (édition brute en cours) n'a pas de document :
/// le formulaire attend qu'il le redevienne.
#[derive(Debug, Clone)]
pub struct Draft {
    text: String,
    doc: Option<DocumentMut>,
    /// Champs d'un autre mode, mis de côté au changement de mode.
    stash: HashMap<String, Item>,
}

/// Rend une valeur lisible : texte brut, liste « a, b », nombre, booléen.
fn show(v: &Value) -> String {
    match v {
        Value::String(s) => s.value().clone(),
        Value::Integer(i) => i.value().to_string(),
        Value::Float(f) => f.value().to_string(),
        Value::Boolean(b) => b.value().to_string(),
        Value::Array(a) => a.iter().map(show).collect::<Vec<_>>().join(", "),
        other => other.to_string().trim().to_string(),
    }
}

fn show_item(i: &Item) -> Option<String> {
    i.as_value().map(show)
}

/// Écrit `raw` selon son type attendu : un nombre qui n'en est pas un est
/// gardé en texte (stationd dira `BAD_VALUE` sur ce champ).
fn typed(raw: &str, kind: Kind) -> Value {
    let t = raw.trim();
    match kind {
        Kind::Int => t.parse::<i64>().map(Value::from).unwrap_or_else(|_| Value::from(t)),
        Kind::Bool => match t {
            "true" => Value::from(true),
            "false" => Value::from(false),
            _ => Value::from(t),
        },
        Kind::Str => Value::from(t),
    }
}

/// Valeur d'un filtre selon son champ et son opérateur.
fn filter_value(field: &str, op: &str, raw: &str) -> Value {
    if list_op(op) {
        let mut a = Array::new();
        for part in raw.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            a.push(part);
        }
        Value::Array(a)
    } else if numeric_field(field) {
        typed(raw, Kind::Int)
    } else {
        Value::from(raw.trim())
    }
}

/// Écrit (ou retire) la fenêtre `within` d'un filtre : elle n'existe que pour
/// `play_count` et non vide ; partout ailleurs elle est retirée (stationd
/// refuse un `within` hors `play_count`).
fn write_within(t: &mut dyn toml_edit::TableLike, f: &FilterView) {
    if f.field == "play_count" && !f.within.trim().is_empty() {
        let slot = t.entry("within").or_insert(Item::None);
        replace_value(slot, Value::from(f.within.trim()));
    } else {
        t.remove("within");
    }
}

/// Garde la décoration d'une valeur remplacée (commentaire de fin de ligne,
/// alignement) : seule la valeur change.
fn replace_value(slot: &mut Item, mut v: Value) {
    if let Some(old) = slot.as_value() {
        *v.decor_mut() = old.decor().clone();
    } else {
        v.decor_mut().set_prefix(" ");
    }
    *slot = Item::Value(v);
}

/// Élément de liste présenté un par ligne (nouvel élément seulement : un
/// élément existant garde sa mise en forme et ses commentaires).
fn push_line(a: &mut Array, v: Value) {
    let multiline = a.is_empty() || a.iter().any(|x| x.decor().prefix().and_then(|p| p.as_str()).is_some_and(|p| p.contains('\n')));
    a.push(v);
    // Une liste sur une ligne qui devient trop longue passe à un élément par
    // ligne — sauf si elle porte des commentaires (on ne les déplace pas).
    let commented = a.iter().any(|x| {
        [x.decor().prefix(), x.decor().suffix()].into_iter().flatten().any(|d| d.as_str().is_some_and(|d| d.contains('#')))
    });
    if !multiline && !commented && a.to_string().len() > 60 {
        for x in a.iter_mut() {
            x.decor_mut().set_prefix("\n  ");
            x.decor_mut().set_suffix("");
        }
        a.set_trailing_comma(true);
        a.set_trailing("\n");
        return;
    }
    if multiline {
        let n = a.len();
        if let Some(last) = a.get_mut(n - 1) {
            last.decor_mut().set_prefix("\n  ");
            last.decor_mut().set_suffix("");
        }
        a.set_trailing_comma(true);
        a.set_trailing("\n");
    }
}

impl Draft {
    pub fn parse(text: &str) -> Self {
        Self { text: text.to_string(), doc: text.parse::<DocumentMut>().ok(), stash: HashMap::new() }
    }

    /// Brouillon neuf d'un mode, avec le nom donné.
    pub fn template(mode: &str, name: &str) -> Self {
        let mut d = Self::parse(&format!("name = \"\"\n\n[selection]\nmode = \"{mode}\"\n"));
        d.set(Key::Name, name);
        match mode {
            "dynamic" | "static" => d.set(Key::Order, "shuffle"),
            "queue" => d.set(Key::Order, "fifo"),
            "group" => d.set(Key::Strategy, "weighted"),
            "remote" => d.set(Key::Url, ""),
            _ => {}
        }
        d
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// Le texte se lit comme du TOML (le formulaire est utilisable).
    pub fn readable(&self) -> bool {
        self.doc.is_some()
    }

    /// Édition brute : le texte est pris tel quel ; le formulaire suit s'il
    /// se lit.
    pub fn set_text(&mut self, text: &str) {
        self.text = text.to_string();
        self.doc = text.parse::<DocumentMut>().ok();
    }

    fn sync_text(&mut self) {
        if let Some(d) = &self.doc {
            self.text = d.to_string();
        }
    }

    fn table(&self, path: &[&str]) -> Option<&dyn toml_edit::TableLike> {
        let mut t: &dyn toml_edit::TableLike = self.doc.as_ref()?.as_table();
        for p in path {
            t = t.get(p)?.as_table_like()?;
        }
        Some(t)
    }

    /// Table `path`, créée si besoin (une table intermédiaire créée ici est
    /// implicite : pas d'en-tête `[broadcast]` vide au-dessus de
    /// `[broadcast.constraints]`).
    fn table_mut(&mut self, path: &[&str]) -> Option<&mut dyn toml_edit::TableLike> {
        let mut t: &mut dyn toml_edit::TableLike = self.doc.as_mut()?.as_table_mut();
        for (i, p) in path.iter().enumerate() {
            let leaf = i + 1 == path.len();
            let item = t.entry(p).or_insert_with(|| {
                let mut nt = Table::new();
                nt.set_implicit(!leaf);
                Item::Table(nt)
            });
            if let Item::Table(tb) = item
                && leaf
            {
                tb.set_implicit(false);
            }
            t = item.as_table_like_mut()?;
        }
        Some(t)
    }

    /// Retire les tables devenues vides sous `path` (en remontant) ; une
    /// table qui ne contient plus que des sous-tables devient implicite
    /// (pas d'en-tête vide).
    fn prune(&mut self, path: &[&str]) {
        self.prune_empty(path);
        for depth in 1..=path.len() {
            // `Table::get_mut` et non `Item::get_mut` : ce dernier créerait
            // la clé absente (table en ligne vide).
            let Some(doc) = self.doc.as_mut() else { return };
            let mut cur: &mut Table = doc.as_table_mut();
            for p in &path[..depth - 1] {
                match cur.get_mut(p).and_then(Item::as_table_mut) {
                    Some(next) => cur = next,
                    None => return,
                }
            }
            if let Some(Item::Table(tb)) = cur.get_mut(path[depth - 1])
                && !tb.is_empty()
                && tb.iter().all(|(_, i)| i.is_table() || i.is_array_of_tables())
            {
                tb.set_implicit(true);
            }
        }
    }

    fn prune_empty(&mut self, path: &[&str]) {
        for depth in (1..=path.len()).rev() {
            let (parent, leaf) = (&path[..depth - 1], path[depth - 1]);
            let empty = self.table(&path[..depth]).is_some_and(|t| t.is_empty());
            if !empty {
                break;
            }
            if let Some(p) = self.table_mut_existing(parent) {
                p.remove(leaf);
            }
        }
    }

    fn table_mut_existing(&mut self, path: &[&str]) -> Option<&mut dyn toml_edit::TableLike> {
        let mut t: &mut dyn toml_edit::TableLike = self.doc.as_mut()?.as_table_mut();
        for p in path {
            t = t.get_mut(p)?.as_table_like_mut()?;
        }
        Some(t)
    }

    pub fn get(&self, key: Key) -> Option<String> {
        let (tables, k, _) = key.place();
        self.table(tables)?.get(k).and_then(show_item)
    }

    /// Écrit un champ ; vide = retiré (la grammaire le rend facultatif, ou
    /// stationd dira qu'il manque).
    pub fn set(&mut self, key: Key, raw: &str) {
        if self.doc.is_none() {
            return;
        }
        if key == Key::Mode {
            self.set_mode(raw);
            return;
        }
        let (tables, k, kind) = key.place();
        if raw.trim().is_empty() && key != Key::Name && key != Key::Url {
            if let Some(t) = self.table_mut_existing(tables) {
                t.remove(k);
            }
            self.prune(tables);
        } else if let Some(t) = self.table_mut(tables) {
            let slot = t.entry(k).or_insert(Item::None);
            replace_value(slot, typed(raw, kind));
        }
        self.sync_text();
    }

    pub fn mode(&self) -> String {
        self.get(Key::Mode).unwrap_or_default()
    }

    /// Change de mode : les champs de l'ancien mode que le nouveau n'a pas
    /// sont mis de côté, ceux du nouveau qui l'avaient été reviennent.
    pub fn set_mode(&mut self, mode: &str) {
        let keep = mode_keys(mode);
        let valid_orders = orders(mode);
        let Some(sel) = self.table_mut(&["selection"]) else { return };
        let mut out = Vec::new();
        for (k, _) in sel.iter() {
            let stays = match k {
                "mode" => true,
                "order" => keep.contains(&"order"),
                _ => keep.contains(&k),
            };
            if !stays {
                out.push(k.to_string());
            }
        }
        let mut moved = Vec::new();
        for k in out {
            if let Some(item) = sel.remove(&k) {
                moved.push((k, item));
            }
        }
        // `order` d'un autre mode (fifo pour un dynamique…) : de côté aussi.
        let bad_order = sel
            .get("order")
            .and_then(|o| o.as_str())
            .is_some_and(|o| !valid_orders.contains(&o));
        if bad_order && let Some(item) = sel.remove("order") {
            moved.push(("order".into(), item));
        }
        let slot = sel.entry("mode").or_insert(Item::None);
        replace_value(slot, Value::from(mode));
        let absent: Vec<&str> = keep.iter().copied().filter(|k| sel.get(k).is_none()).collect();
        let back: Vec<String> =
            absent.into_iter().filter(|k| self.stash.contains_key(*k)).map(str::to_string).collect();
        let mut restored = Vec::new();
        for k in back {
            let ok = k != "order"
                || self.stash.get("order").and_then(|o| o.as_str()).is_some_and(|o| valid_orders.contains(&o));
            if ok && let Some(item) = self.stash.remove(&k) {
                restored.push((k, item));
            }
        }
        for (k, item) in moved {
            self.stash.insert(k, item);
        }
        if let Some(sel) = self.table_mut(&["selection"]) {
            for (k, item) in restored {
                sel.insert(&k, item);
            }
        }
        self.sync_text();
    }

    // --- filtres ----------------------------------------------------------------

    fn filter_tables(&self) -> Vec<&dyn toml_edit::TableLike> {
        let Some(item) = self.table(&["selection"]).and_then(|s| s.get("filter")) else { return vec![] };
        match item {
            Item::ArrayOfTables(a) => a.iter().map(|t| t as &dyn toml_edit::TableLike).collect(),
            Item::Value(Value::Array(a)) => {
                a.iter().filter_map(|v| v.as_inline_table().map(|t| t as &dyn toml_edit::TableLike)).collect()
            }
            _ => vec![],
        }
    }

    pub fn filters(&self) -> Vec<FilterView> {
        self.filter_tables()
            .into_iter()
            .map(|t| FilterView {
                field: t.get("field").and_then(show_item).unwrap_or_default(),
                op: t.get("op").and_then(show_item).unwrap_or_default(),
                value: t.get("value").and_then(show_item).unwrap_or_default(),
                within: t.get("within").and_then(show_item).unwrap_or_default(),
            })
            .collect()
    }

    fn filter_mut(&mut self, i: usize) -> Option<&mut dyn toml_edit::TableLike> {
        let item = self.table_mut_existing(&["selection"])?.get_mut("filter")?;
        match item {
            Item::ArrayOfTables(a) => a.get_mut(i).map(|t| t as &mut dyn toml_edit::TableLike),
            Item::Value(Value::Array(a)) => {
                a.get_mut(i)?.as_inline_table_mut().map(|t| t as &mut dyn toml_edit::TableLike)
            }
            _ => None,
        }
    }

    /// Modifie une partie d'un filtre. Un champ changé prend le premier
    /// opérateur proposé si l'ancien ne lui est pas proposé ; la valeur est
    /// retypée (liste, nombre, texte) selon le champ et l'opérateur.
    pub fn set_filter(&mut self, i: usize, part: FilterPart, raw: &str) {
        let Some(mut f) = self.filters().get(i).cloned() else { return };
        match part {
            FilterPart::Field => {
                f.field = raw.to_string();
                let ops = filter_ops(raw);
                if !ops.is_empty() && !ops.contains(&f.op.as_str()) {
                    f.op = ops[0].to_string();
                }
            }
            FilterPart::Op => f.op = raw.to_string(),
            FilterPart::Value => f.value = raw.to_string(),
            FilterPart::Within => f.within = raw.to_string(),
        }
        let value = filter_value(&f.field, &f.op, &f.value);
        let Some(t) = self.filter_mut(i) else { return };
        for (k, v) in [("field", Value::from(f.field.as_str())), ("op", Value::from(f.op.as_str())), ("value", value)] {
            let slot = t.entry(k).or_insert(Item::None);
            replace_value(slot, v);
        }
        write_within(t, &f);
        self.sync_text();
    }

    pub fn add_filter(&mut self) {
        let Some(sel) = self.table_mut(&["selection"]) else { return };
        let mut t = Table::new();
        t.insert("field", toml_edit::value("path"));
        t.insert("op", toml_edit::value("prefix"));
        t.insert("value", toml_edit::value(""));
        match sel.entry("filter").or_insert_with(|| Item::ArrayOfTables(ArrayOfTables::new())) {
            Item::ArrayOfTables(a) => a.push(t),
            Item::Value(Value::Array(a)) => a.push(t.into_inline_table()),
            _ => {}
        }
        self.sync_text();
    }

    pub fn remove_filter(&mut self, i: usize) {
        let Some(sel) = self.table_mut_existing(&["selection"]) else { return };
        let empty = match sel.get_mut("filter") {
            Some(Item::ArrayOfTables(a)) if i < a.len() => {
                a.remove(i);
                a.is_empty()
            }
            Some(Item::Value(Value::Array(a))) if i < a.len() => {
                a.remove(i);
                a.is_empty()
            }
            _ => false,
        };
        if empty {
            sel.remove("filter");
        }
        self.sync_text();
    }

    /// Échange le filtre `i` avec son voisin (`up` : celui d'avant).
    pub fn move_filter(&mut self, i: usize, up: bool) -> Option<usize> {
        let j = if up { i.checked_sub(1)? } else { i + 1 };
        let n = self.filters().len();
        if j >= n || i >= n {
            return None;
        }
        let (a, b) = (self.filters()[i].clone(), self.filters()[j].clone());
        for (at, f) in [(i, &b), (j, &a)] {
            let value = filter_value(&f.field, &f.op, &f.value);
            let t = self.filter_mut(at)?;
            for (k, v) in [("field", Value::from(f.field.as_str())), ("op", Value::from(f.op.as_str())), ("value", value)] {
                let slot = t.entry(k).or_insert(Item::None);
                replace_value(slot, v);
            }
            write_within(t, f);
        }
        self.sync_text();
        Some(j)
    }

    // --- membres ----------------------------------------------------------------

    fn member_tables(&self) -> Vec<&dyn toml_edit::TableLike> {
        let Some(item) = self.table(&["selection"]).and_then(|s| s.get("members")) else { return vec![] };
        match item {
            Item::ArrayOfTables(a) => a.iter().map(|t| t as &dyn toml_edit::TableLike).collect(),
            Item::Value(Value::Array(a)) => {
                a.iter().filter_map(|v| v.as_inline_table().map(|t| t as &dyn toml_edit::TableLike)).collect()
            }
            _ => vec![],
        }
    }

    pub fn members(&self) -> Vec<MemberView> {
        self.member_tables()
            .into_iter()
            .map(|t| {
                let g = |k: &str| t.get(k).and_then(show_item).unwrap_or_default();
                MemberView {
                    r#ref: g("ref"), weight: g("weight"), take: g("take"),
                    take_random_min: g("take_random_min"), take_random_max: g("take_random_max"),
                    runtime: g("runtime"),
                }
            })
            .collect()
    }

    fn member_mut(&mut self, i: usize) -> Option<&mut dyn toml_edit::TableLike> {
        let item = self.table_mut_existing(&["selection"])?.get_mut("members")?;
        match item {
            Item::ArrayOfTables(a) => a.get_mut(i).map(|t| t as &mut dyn toml_edit::TableLike),
            Item::Value(Value::Array(a)) => {
                a.get_mut(i)?.as_inline_table_mut().map(|t| t as &mut dyn toml_edit::TableLike)
            }
            _ => None,
        }
    }

    /// Modifie une partie d'un membre ; vide = retirée (sauf `ref`).
    pub fn set_member(&mut self, i: usize, part: MemberPart, raw: &str) {
        let Some(t) = self.member_mut(i) else { return };
        let k = part.key();
        if raw.trim().is_empty() && part != MemberPart::Ref {
            t.remove(k);
        } else {
            let kind = match part {
                MemberPart::Weight | MemberPart::Take | MemberPart::TakeRandomMin | MemberPart::TakeRandomMax => Kind::Int,
                MemberPart::Ref | MemberPart::Runtime => Kind::Str,
            };
            let slot = t.entry(k).or_insert(Item::None);
            replace_value(slot, typed(raw, kind));
        }
        self.sync_text();
    }

    /// Ajoute un membre ; `weight = 1` dans un groupe pondéré.
    pub fn add_member(&mut self, r#ref: &str) {
        let weighted = self.get(Key::Strategy).as_deref() == Some("weighted");
        let Some(sel) = self.table_mut(&["selection"]) else { return };
        let mut t = InlineTable::new();
        t.insert("ref", Value::from(r#ref));
        if weighted {
            t.insert("weight", Value::from(1));
        }
        match sel.entry("members").or_insert_with(|| Item::Value(Value::Array(Array::new()))) {
            Item::ArrayOfTables(a) => a.push(t.into_table()),
            Item::Value(Value::Array(a)) => push_line(a, Value::InlineTable(t)),
            _ => {}
        }
        self.sync_text();
    }

    pub fn remove_member(&mut self, i: usize) {
        let Some(sel) = self.table_mut_existing(&["selection"]) else { return };
        let empty = match sel.get_mut("members") {
            Some(Item::ArrayOfTables(a)) if i < a.len() => {
                a.remove(i);
                a.is_empty()
            }
            Some(Item::Value(Value::Array(a))) if i < a.len() => {
                a.remove(i);
                a.is_empty()
            }
            _ => false,
        };
        if empty {
            sel.remove("members");
        }
        self.sync_text();
    }

    pub fn move_member(&mut self, i: usize, up: bool) -> Option<usize> {
        let j = if up { i.checked_sub(1)? } else { i + 1 };
        let sel = self.table_mut_existing(&["selection"])?;
        match sel.get_mut("members")? {
            Item::Value(Value::Array(a)) if i < a.len() && j < a.len() => {
                // Échange des valeurs, chaque place garde sa décoration.
                let (vi, vj) = (a.get(i)?.clone(), a.get(j)?.clone());
                let (di, dj) = (vi.decor().clone(), vj.decor().clone());
                let mut ni = vj;
                *ni.decor_mut() = di;
                let mut nj = vi;
                *nj.decor_mut() = dj;
                a.replace(i, ni);
                a.replace(j, nj);
            }
            Item::ArrayOfTables(a) if i < a.len() && j < a.len() => {
                let (ti, tj) = (a.get(i)?.clone(), a.get(j)?.clone());
                *a.get_mut(i)? = tj;
                *a.get_mut(j)? = ti;
            }
            _ => return None,
        }
        self.sync_text();
        Some(j)
    }

    // --- fichiers (static) ------------------------------------------------------

    pub fn files(&self) -> Vec<String> {
        match self.table(&["selection"]).and_then(|s| s.get("files")).and_then(Item::as_array) {
            Some(a) => a.iter().map(show).collect(),
            None => vec![],
        }
    }

    fn files_mut(&mut self, create: bool) -> Option<&mut Array> {
        let sel = if create { self.table_mut(&["selection"])? } else { self.table_mut_existing(&["selection"])? };
        if create && sel.get("files").is_none() {
            sel.insert("files", Item::Value(Value::Array(Array::new())));
        }
        sel.get_mut("files")?.as_array_mut()
    }

    /// Ajoute des médias à la liste, sans doublon. Rend le nombre ajouté.
    pub fn add_files(&mut self, paths: &[String]) -> usize {
        let have = self.files();
        let Some(a) = self.files_mut(true) else { return 0 };
        let mut added = 0;
        for p in paths {
            if !have.contains(p) && !a.iter().any(|v| v.as_str() == Some(p)) {
                push_line(a, Value::from(p.as_str()));
                added += 1;
            }
        }
        self.sync_text();
        added
    }

    pub fn set_file(&mut self, i: usize, raw: &str) {
        let Some(a) = self.files_mut(false) else { return };
        if let Some(old) = a.get(i) {
            let mut v = Value::from(raw.trim());
            *v.decor_mut() = old.decor().clone();
            a.replace(i, v);
        }
        self.sync_text();
    }

    pub fn remove_file(&mut self, i: usize) {
        let Some(a) = self.files_mut(false) else { return };
        if i < a.len() {
            a.remove(i);
        }
        self.sync_text();
    }

    pub fn move_file(&mut self, i: usize, up: bool) -> Option<usize> {
        let j = if up { i.checked_sub(1)? } else { i + 1 };
        let a = self.files_mut(false)?;
        if i >= a.len() || j >= a.len() {
            return None;
        }
        let (vi, vj) = (a.get(i)?.clone(), a.get(j)?.clone());
        let mut ni = Value::from(vj.as_str().unwrap_or_default());
        *ni.decor_mut() = vi.decor().clone();
        let mut nj = Value::from(vi.as_str().unwrap_or_default());
        *nj.decor_mut() = vj.decor().clone();
        a.replace(i, ni);
        a.replace(j, nj);
        self.sync_text();
        Some(j)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MUSIQUE: &str = r#"# Rotation musicale
name = "Musique"

[selection]
mode  = "dynamic"
order = "shuffle"   # au hasard
match = "all"

[[selection.filter]]
field = "path"
op    = "prefix"
value = "Musique/"

[[selection.filter]]
field = "genre"
op    = "has_none"
value = ["talks", "jingle"]

[broadcast.constraints]
no_same_artist_within = "1h"
"#;

    #[test]
    fn reads_the_fields_of_the_grammar() {
        let d = Draft::parse(MUSIQUE);
        assert!(d.readable());
        assert_eq!(d.get(Key::Name).as_deref(), Some("Musique"));
        assert_eq!(d.mode(), "dynamic");
        assert_eq!(d.get(Key::Order).as_deref(), Some("shuffle"));
        assert_eq!(d.get(Key::NoSameArtist).as_deref(), Some("1h"));
        assert_eq!(d.get(Key::Limit), None);
        assert_eq!(
            d.filters(),
            vec![
                FilterView { field: "path".into(), op: "prefix".into(), value: "Musique/".into(), ..Default::default() },
                FilterView { field: "genre".into(), op: "has_none".into(), value: "talks, jingle".into(), ..Default::default() },
            ]
        );
    }

    #[test]
    fn an_edit_keeps_comments_and_the_rest_of_the_file() {
        let mut d = Draft::parse(MUSIQUE);
        d.set(Key::Order, "newest");
        assert!(d.text().starts_with("# Rotation musicale\n"));
        assert!(d.text().contains("order = \"newest\"   # au hasard"), "{}", d.text());
        assert_eq!(d.text().replace("\"newest\"", "\"shuffle\""), MUSIQUE);
    }

    #[test]
    fn values_are_typed_by_field_and_empty_removes() {
        let mut d = Draft::parse(MUSIQUE);
        d.set(Key::Limit, "5");
        assert!(d.text().contains("[broadcast]\nlimit = 5\n"), "{}", d.text());
        d.set(Key::Limit, "cinq");
        assert!(d.text().contains("limit = \"cinq\""), "une valeur mal typée part telle quelle");
        d.set(Key::Limit, "");
        assert!(!d.text().contains("limit"));
        assert!(!d.text().contains("[broadcast]\n\n"), "pas de table vide laissée : {}", d.text());
        d.set(Key::NoSameArtist, "");
        assert!(!d.text().contains("broadcast"), "tables vides retirées : {}", d.text());
        d.set(Key::NoSameTrack, "6h");
        assert!(d.text().contains("[broadcast.constraints]\nno_same_track_within = \"6h\""), "{}", d.text());
        assert!(!d.text().contains("[broadcast]\n"), "table intermédiaire implicite : {}", d.text());
        d.set(Key::Enabled, "false");
        assert!(d.text().contains("enabled = false"));
    }

    #[test]
    fn filters_are_retyped_by_field_and_op() {
        let mut d = Draft::parse(MUSIQUE);
        d.set_filter(1, FilterPart::Value, "talks, jingle, pub");
        assert!(d.text().contains(r#"value = ["talks", "jingle", "pub"]"#), "{}", d.text());
        d.set_filter(1, FilterPart::Op, "has");
        assert!(d.text().contains("value = \"talks, jingle, pub\""), "{}", d.text());
        d.set_filter(0, FilterPart::Field, "duration");
        let f = &d.filters()[0];
        assert_eq!((f.op.as_str(), f.value.as_str()), (">=", "Musique/"), "op proposé par défaut pour ce champ");
        d.set_filter(0, FilterPart::Value, "180");
        assert!(d.text().contains("value = 180"), "{}", d.text());
        d.add_filter();
        assert_eq!(d.filters().len(), 3);
        assert_eq!(d.move_filter(2, true), Some(1));
        assert_eq!(d.filters()[1].field, "path");
        d.remove_filter(1);
        d.remove_filter(1);
        d.remove_filter(0);
        assert!(d.filters().is_empty());
        assert!(!d.text().contains("filter"), "{}", d.text());
        d.add_filter();
        assert!(d.text().contains("[[selection.filter]]\nfield = \"path\""), "{}", d.text());
    }

    #[test]
    fn age_creation_and_tempo_filters_are_written_as_text() {
        let mut d = Draft::parse(MUSIQUE);
        d.set_filter(0, FilterPart::Field, "age");
        assert_eq!(d.filters()[0].op, "<", "op proposé par défaut pour l'âge");
        d.set_filter(0, FilterPart::Value, "10d");
        assert!(d.text().contains("field = \"age\"\nop    = \"<\"\nvalue = \"10d\""), "{}", d.text());
        d.set_filter(0, FilterPart::Field, "creation");
        d.set_filter(0, FilterPart::Value, "2026-09-01T00:00:00+02:00");
        assert!(d.text().contains("value = \"2026-09-01T00:00:00+02:00\""), "{}", d.text());
        d.set_filter(0, FilterPart::Field, "tempo");
        assert_eq!(d.filters()[0].op, "eq");
        d.set_filter(0, FilterPart::Value, "fast");
        assert!(d.text().contains("value = \"fast\""), "{}", d.text());
    }

    #[test]
    fn play_count_carries_a_within_window_only_for_play_count() {
        let mut d = Draft::parse(MUSIQUE);
        // path/prefix → play_count : op numérique par défaut, valeur entière,
        // fenêtre `within` écrite.
        d.set_filter(0, FilterPart::Field, "play_count");
        assert_eq!(d.filters()[0].op, ">=", "op numérique par défaut");
        d.set_filter(0, FilterPart::Value, "3");
        d.set_filter(0, FilterPart::Within, "30d");
        assert!(d.text().contains("field = \"play_count\""), "{}", d.text());
        assert!(d.text().contains("value = 3"), "entier : {}", d.text());
        assert!(d.text().contains("within = \"30d\""), "{}", d.text());
        let f = Draft::parse(d.text()).filters()[0].clone();
        assert_eq!((f.value.as_str(), f.within.as_str()), ("3", "30d"), "round-trip");

        // Changer de champ retire le `within` (interdit hors play_count).
        d.set_filter(0, FilterPart::Field, "last_played");
        assert!(!d.text().contains("within"), "within retiré hors play_count : {}", d.text());
        // last_played : valeur durée écrite en texte.
        d.set_filter(0, FilterPart::Value, "7d");
        assert!(
            d.text().contains("field = \"last_played\"") && d.text().contains("value = \"7d\""),
            "{}",
            d.text()
        );
    }

    #[test]
    fn analysis_filters_replace_genre_lists_with_text_and_round_trip() {
        let mut d = Draft::parse(MUSIQUE);
        for (field, value) in [("genre_ai", "Electronic---Italo-Disco"), ("mood", "party")] {
            d.set_filter(1, FilterPart::Field, "genre");
            d.set_filter(1, FilterPart::Op, "has_any");
            d.set_filter(1, FilterPart::Value, "talks, jingle");
            d.set_filter(1, FilterPart::Field, field);
            assert_eq!(d.filters()[1].op, "contains", "le filtre de liste devient un filtre texte");
            d.set_filter(1, FilterPart::Value, value);
            for op in ["contains", "eq", "ne"] {
                d.set_filter(1, FilterPart::Op, op);
                let doc: DocumentMut = d.text().parse().unwrap();
                assert_eq!(doc["selection"]["filter"][1]["value"].as_str(), Some(value));
                assert_eq!(
                    Draft::parse(d.text()).filters()[1],
                    FilterView { field: field.into(), op: op.into(), value: value.into(), ..Default::default() }
                );
            }
        }
    }

    #[test]
    fn switching_mode_sets_aside_the_other_mode_fields_and_brings_them_back() {
        let mut d = Draft::parse(MUSIQUE);
        d.set_mode("queue");
        assert_eq!(d.mode(), "queue");
        assert!(d.filters().is_empty());
        assert_eq!(d.get(Key::Order), None, "shuffle n'est pas un ordre de file");
        assert_eq!(d.get(Key::Match), None);
        d.set(Key::Order, "lifo");
        d.set_mode("dynamic");
        assert_eq!(d.filters().len(), 2, "les filtres reviennent");
        assert_eq!(d.get(Key::Match).as_deref(), Some("all"));
        assert_eq!(d.get(Key::Order).as_deref(), Some("shuffle"), "lifo mis de côté, shuffle revenu");
        assert_eq!(d.get(Key::NoSameArtist).as_deref(), Some("1h"), "diffusion inchangée");
        assert!(Draft::parse(d.text()).readable());
    }

    #[test]
    fn random_member_quotas_round_trip_as_integers_and_can_be_cleared() {
        let mut d = Draft::parse("name = \"G\"\n[selection]\nmode = \"group\"\nstrategy = \"sequence\"\nmembers = [{ ref = \"a\" }]");
        d.set_member(0, MemberPart::TakeRandomMin, "2");
        d.set_member(0, MemberPart::TakeRandomMax, "4");
        let doc: DocumentMut = d.text().parse().unwrap();
        assert_eq!(doc["selection"]["members"][0]["take_random_min"].as_integer(), Some(2));
        assert_eq!(doc["selection"]["members"][0]["take_random_max"].as_integer(), Some(4));
        let parsed = Draft::parse(d.text());
        assert_eq!(parsed.members()[0].take_random_min, "2");
        assert_eq!(parsed.members()[0].take_random_max, "4");
        d.set_member(0, MemberPart::TakeRandomMin, "");
        d.set_member(0, MemberPart::TakeRandomMax, "");
        assert!(!d.text().contains("take_random"));
    }

    #[test]
    fn members_in_an_inline_array_keep_their_style() {
        let src = "name = \"Mix\"\n\n[selection]\nmode     = \"group\"\nstrategy = \"weighted\"\nmembers  = [\n  { ref = \"musique\", weight = 3 },\n  { ref = \"nuit\",    weight = 1 },\n]\n";
        let mut d = Draft::parse(src);
        assert_eq!(d.members()[1], MemberView { r#ref: "nuit".into(), weight: "1".into(), ..Default::default() });
        d.set_member(1, MemberPart::Weight, "2");
        assert!(d.text().contains("{ ref = \"nuit\",    weight = 2 }"), "{}", d.text());
        d.add_member("jingles");
        assert!(d.text().contains("  { ref = \"jingles\", weight = 1 },\n]"), "{}", d.text());
        assert_eq!(d.move_member(2, true), Some(1));
        assert_eq!(d.members().iter().map(|m| m.r#ref.as_str()).collect::<Vec<_>>(), ["musique", "jingles", "nuit"]);
        d.set_member(0, MemberPart::Weight, "");
        assert_eq!(d.members()[0].weight, "");
        d.remove_member(0);
        d.remove_member(0);
        d.remove_member(0);
        assert!(!d.text().contains("members"));
        assert!(Draft::parse(d.text()).readable());
    }

    #[test]
    fn files_are_added_once_one_per_line() {
        let mut d = Draft::template("static", "Jingles");
        assert_eq!(d.add_files(&["J/a.mp3".into(), "J/b.mp3".into(), "J/a.mp3".into()]), 2);
        assert_eq!(d.add_files(&["J/b.mp3".into()]), 0);
        assert!(d.text().contains("files = [\n  \"J/a.mp3\",\n  \"J/b.mp3\",\n]"), "{}", d.text());
        assert_eq!(d.move_file(0, false), Some(1));
        assert_eq!(d.files(), ["J/b.mp3", "J/a.mp3"]);
        d.set_file(1, "J/c.mp3");
        d.remove_file(0);
        assert_eq!(d.files(), ["J/c.mp3"]);
        assert!(Draft::parse(d.text()).readable());
    }

    #[test]
    fn a_one_line_list_that_grows_goes_one_per_line() {
        let mut d = Draft::parse("name = \"I\"\n[selection]\nmode = \"static\"\nfiles = [\"Emission/generique-debut.mp3\"]\n");
        d.add_files(&["Jingles/id-01.mp3".into()]);
        assert!(d.text().contains("files = [\"Emission/generique-debut.mp3\", \"Jingles/id-01.mp3\"]"), "court : sur une ligne\n{}", d.text());
        d.add_files(&["Jingles/id-02.mp3".into()]);
        assert!(d.text().contains("files = [\n  \"Emission/generique-debut.mp3\",\n  \"Jingles/id-01.mp3\",\n  \"Jingles/id-02.mp3\",\n]"), "{}", d.text());
        assert_eq!(d.files().len(), 3);
    }

    #[test]
    fn a_template_is_a_readable_draft_of_its_mode() {
        for m in MODES {
            let d = Draft::template(m, "Nouvelle");
            assert!(d.readable(), "{m}: {}", d.text());
            assert_eq!(d.mode(), m);
            assert_eq!(d.get(Key::Name).as_deref(), Some("Nouvelle"));
        }
        let mut raw = Draft::parse("name = ");
        assert!(!raw.readable());
        raw.set(Key::Name, "x");
        assert_eq!(raw.text(), "name = ", "illisible : le formulaire n'écrit rien");
        raw.set_text("name = \"ok\"\n[selection]\nmode = \"queue\"\n");
        assert!(raw.readable());
    }

    #[test]
    fn diagnostic_paths_follow_the_grammar() {
        assert_eq!(Key::Order.path(), "selection.order");
        assert_eq!(Key::NoSameTitle.path(), "broadcast.constraints.no_same_title_within");
        assert_eq!(Key::Name.path(), "name");
    }
}
