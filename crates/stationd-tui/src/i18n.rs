//! Traductions de l'interface (Fluent).
//!
//! Aucune chaîne affichée n'est écrite en dur dans le code : tout passe par
//! une clé des catalogues `locales/<langue>/tui.ftl`, intégrés au binaire.
//! Langues : français (défaut et repli), anglais, allemand. Une clé absente
//! d'une langue retombe sur le français ; absente partout, elle s'affiche
//! `⟦clé⟧` (visible, jamais un vide) — et les tests l'interdisent.
//!
//! Ce que stationd envoie à afficher arrive en OPCODES (ex. les notes de
//! l'antenne) et se traduit ici ; les noms (playlists, titres, DJ) et les
//! messages d'erreur techniques relayés tels quels ne se traduisent pas.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::OnceLock;

use fluent_templates::fluent_bundle::FluentValue;
use fluent_templates::{static_loader, Loader};
use unic_langid::LanguageIdentifier;

static_loader! {
    static LOCALES = {
        locales: "./locales",
        fallback_language: "fr",
        // Pas de marques d'isolation Unicode autour des variables : elles
        // faussent les largeurs dans un terminal.
        customise: |bundle| bundle.set_use_isolating(false),
    };
}

/// Langues livrées (la première est le défaut).
pub const LANGUAGES: &[&str] = &["fr", "en", "de"];

static CURRENT: OnceLock<LanguageIdentifier> = OnceLock::new();

/// Choisit la langue : `--lang`, sinon `LC_ALL` / `LC_MESSAGES` / `LANG`
/// (« de_DE.UTF-8 » → « de »), sinon le français. Une langue non livrée
/// retombe sur le français. À appeler une fois, au démarrage.
pub fn init(explicit: Option<&str>) -> &'static str {
    let from_env = || {
        ["LC_ALL", "LC_MESSAGES", "LANG"]
            .iter()
            .filter_map(|v| std::env::var(v).ok())
            .find(|v| !v.is_empty() && v != "C" && v != "POSIX")
    };
    let wanted = explicit.map(str::to_string).or_else(from_env).unwrap_or_default();
    let code = supported(&wanted);
    let _ = CURRENT.set(code.parse().expect("langue livrée valide"));
    code
}

/// Code de langue livré correspondant à `raw` (« en_GB.UTF-8 » → « en »).
pub fn supported(raw: &str) -> &'static str {
    let base = raw.split(['_', '-', '.', '@']).next().unwrap_or("").to_ascii_lowercase();
    LANGUAGES.iter().copied().find(|l| *l == base).unwrap_or(LANGUAGES[0])
}

fn current() -> &'static LanguageIdentifier {
    CURRENT.get_or_init(|| LANGUAGES[0].parse().expect("langue par défaut valide"))
}

/// Une variable passée à une traduction.
pub type Arg = (&'static str, FluentValue<'static>);

/// Convertit une valeur en variable Fluent (texte ou nombre).
pub fn arg<V: Into<FluentValue<'static>>>(v: V) -> FluentValue<'static> {
    v.into()
}

/// Traduit `key` avec ses variables dans la langue courante.
pub fn text(key: &str, args: &[Arg]) -> String {
    text_in(current(), key, args)
}

fn text_in(lang: &LanguageIdentifier, key: &str, args: &[Arg]) -> String {
    let found = if args.is_empty() {
        LOCALES.try_lookup(lang, key)
    } else {
        let map: HashMap<Cow<'static, str>, FluentValue<'static>> =
            args.iter().map(|(k, v)| (Cow::Borrowed(*k), v.clone())).collect();
        LOCALES.try_lookup_with_args(lang, key, &map)
    };
    found.unwrap_or_else(|| format!("⟦{key}⟧"))
}

/// Traduit une clé : `tr!("cle")`, `tr!("cle", nom = valeur, …)`.
#[macro_export]
macro_rules! tr {
    ($key:literal) => {
        $crate::i18n::text($key, &[])
    };
    ($key:literal, $($name:ident = $val:expr),+ $(,)?) => {
        $crate::i18n::text($key, &[$((stringify!($name), $crate::i18n::arg($val))),+])
    };
}

/// Marque une clé rangée dans une table (traduite plus tard par `text`) :
/// les tests retrouvent ainsi toutes les clés utilisées.
#[macro_export]
macro_rules! k {
    ($key:literal) => {
        $key
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::path::Path;

    fn ids(lang: &str) -> BTreeSet<String> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("locales").join(lang);
        let re = regex::Regex::new(r"(?m)^([a-z][a-z0-9-]*)\s*=").unwrap();
        let mut out = BTreeSet::new();
        for e in std::fs::read_dir(&dir).unwrap() {
            let text = std::fs::read_to_string(e.unwrap().path()).unwrap();
            out.extend(re.captures_iter(&text).map(|c| c[1].to_string()));
        }
        out
    }

    #[test]
    fn every_language_has_exactly_the_same_keys() {
        let fr = ids("fr");
        assert!(!fr.is_empty());
        for lang in &LANGUAGES[1..] {
            let other = ids(lang);
            let missing: Vec<_> = fr.difference(&other).collect();
            let extra: Vec<_> = other.difference(&fr).collect();
            assert!(missing.is_empty() && extra.is_empty(), "{lang}: manque {missing:?}, en trop {extra:?}");
        }
    }

    #[test]
    fn every_key_used_in_the_code_exists() {
        let fr = ids("fr");
        let re = regex::Regex::new(r#"(?:tr|k)!\(\s*"([^"]+)""#).unwrap();
        let mut used = BTreeSet::new();
        fn walk(dir: &Path, f: &mut dyn FnMut(&str)) {
            for e in std::fs::read_dir(dir).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    walk(&p, f);
                } else if p.extension().is_some_and(|x| x == "rs") {
                    f(&std::fs::read_to_string(&p).unwrap());
                }
            }
        }
        walk(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut |src| {
            used.extend(re.captures_iter(src).map(|c| c[1].to_string()));
        });
        // Les clés de la doc de la macro elle-même.
        used.remove("cle");
        let missing: Vec<_> = used.difference(&fr).collect();
        assert!(missing.is_empty(), "clés absentes du catalogue fr : {missing:?}");
        let unused: Vec<_> = fr.difference(&used).collect();
        assert!(unused.is_empty(), "clés du catalogue jamais utilisées : {unused:?}");
    }

    #[test]
    fn every_catalog_translates_with_variables_and_plurals() {
        for lang in LANGUAGES {
            let l: LanguageIdentifier = lang.parse().unwrap();
            let one = text_in(&l, "banner-overrides", &[("n", arg(1))]);
            let many = text_in(&l, "banner-overrides", &[("n", arg(3))]);
            assert!(one.contains('1') && many.contains('3') && one != many.replace('3', "1"), "{lang}: {one} / {many}");
            assert!(!text_in(&l, "note-plugin-filter-failed", &[("plugin", arg("tags")), ("reason", arg("x"))]).contains('⟦'));
        }
    }

    /// Une entrée mal formée est écartée en silence par Fluent : chaque clé
    /// de chaque catalogue doit se résoudre.
    #[test]
    fn every_entry_of_every_catalog_parses() {
        let var = regex::Regex::new(r"\{\s*\$([a-z_]+)").unwrap();
        for lang in LANGUAGES {
            let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("locales").join(lang);
            for e in std::fs::read_dir(&dir).unwrap() {
                let text = std::fs::read_to_string(e.unwrap().path()).unwrap();
                if let Err((_, errors)) = fluent_templates::fluent_bundle::FluentResource::try_new(text) {
                    panic!("{lang}: catalogue mal formé : {errors:?}");
                }
            }
            // Toutes les variables des catalogues, avec une valeur numérique
            // (valable aussi pour les sélecteurs de pluriel).
            let mut names = BTreeSet::new();
            for e in std::fs::read_dir(&dir).unwrap() {
                let text = std::fs::read_to_string(e.unwrap().path()).unwrap();
                names.extend(var.captures_iter(&text).map(|c| c[1].to_string()));
            }
            let args: Vec<Arg> =
                names.into_iter().map(|n| (&*Box::leak(n.into_boxed_str()), arg(2))).collect();
            let l: LanguageIdentifier = lang.parse().unwrap();
            for id in ids(lang) {
                let t = text_in(&l, &id, &args);
                assert!(!t.contains('⟦'), "{lang}: « {id} » ne se résout pas");
            }
        }
    }

    #[test]
    fn language_is_picked_from_the_locale_name() {
        assert_eq!(supported("de_DE.UTF-8"), "de");
        assert_eq!(supported("en-GB"), "en");
        assert_eq!(supported("es_ES.UTF-8"), "fr", "non livrée → français");
        assert_eq!(supported(""), "fr");
    }

    #[test]
    fn a_missing_key_is_visible() {
        assert_eq!(text_in(&"fr".parse().unwrap(), "pas-une-cle", &[]), "⟦pas-une-cle⟧");
    }
}
