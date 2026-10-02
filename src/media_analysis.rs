//! Analyse média offline, hors-process, via un extracteur Essentia.
//!
//! Descripteurs signal (bpm / key / loudness / replaygain) + labels TensorFlow
//! (danceability / genre / mood). Les résultats sont écrits dans les tags du
//! fichier (la SOURCE DE VÉRITÉ) puis reflétés dans la table `media_analysis`
//! (migration 0030), un simple cache reconstructible. Aucun I/O réseau, aucun
//! playback : un fichier n'est jamais envoyé à Liquidsoap ni à l'antenne.
//!
//! Politique : l'analyse (chère) ne tourne QUE sur les fichiers sans marqueur
//! `TXXX:STATIOND_ANALYSIS` à jour. Un re-scan sur VM neuve relit les tags et
//! reconstitue la table sans relancer Essentia.
//!
//! SQUELETTE : les corps `todo!()` sont à implémenter. Rien ici n'est encore
//! branché dans le pipeline de scan (voir `library_actor` / `scan_writeback`).

use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::media::{CustomTag, ScanReport};

/// Version du CONTRAT d'analyse. À bumper quand la liste de descripteurs ou le
/// profil extracteur change : les fichiers marqués d'une version antérieure
/// sont ré-analysés. Stockée dans le frame marqueur et la colonne
/// `analyzer_version`.
pub const ANALYSIS_VERSION: &str = "essentia-tf/1";

/// Nom du frame TXXX marqueur « analysé avec succès » dans les tags.
pub const MARKER_TAG: &str = "STATIOND_ANALYSIS";

/// Clés de `report.metadata` écrites en frames TXXX par `scan_writeback`, en
/// plus de `bpm` (écrit en TBPM standard, géré à part). Le marqueur en fait
/// partie, écrit en dernier conceptuellement : il n'est dans le fichier
/// qu'après une écriture vérifiée. Source unique du contrat de tags.
pub const ANALYSIS_META_KEYS: &[&str] = &[
    "key",
    "scale",
    "loudness_lufs",
    "replaygain_db",
    "danceability",
    "genre_top",
    "genre_prob",
    "mood",
    "mood_prob",
    MARKER_TAG,
];

/// Le profil figé. Chaque champ ⇔ une colonne `media_analysis` ⇔ un tag.
#[derive(Debug, Clone, PartialEq)]
pub struct Analysis {
    // — signal (fiable, pas de ML) —
    pub bpm: f64,
    pub key: String,   // "A", "C#", …
    pub scale: String, // "major" | "minor"
    pub loudness_lufs: f64,
    pub replaygain_db: f64,
    // — TensorFlow (labels sémantiques) —
    pub danceability: f64, // 0..1
    pub genre_top: String,
    pub genre_prob: f64,
    pub mood: String,
    pub mood_prob: f64,
}

/// Valeur courante du marqueur d'analyse dans les tags relus, s'il est présent.
pub fn current_marker(tags: &[CustomTag]) -> Option<&str> {
    tags.iter()
        .find(|t| t.name.trim().eq_ignore_ascii_case(MARKER_TAG))
        .map(|t| t.value.trim())
}

impl Analysis {
    /// Les paires `(clé, valeur)` à déposer dans `report.metadata` pour que
    /// `scan_writeback` les écrive dans les tags. `bpm` part en `TBPM` entier
    /// (contrat : arrondi) ; le reste en `TXXX` (cf. `ANALYSIS_META_KEYS`). Le
    /// marqueur est ajouté à part par `analyze_pending`, après succès.
    pub fn to_metadata(&self) -> Vec<(String, String)> {
        vec![
            ("bpm".into(), (self.bpm.round() as i64).to_string()),
            ("key".into(), self.key.clone()),
            ("scale".into(), self.scale.clone()),
            ("loudness_lufs".into(), self.loudness_lufs.to_string()),
            ("replaygain_db".into(), self.replaygain_db.to_string()),
            ("danceability".into(), self.danceability.to_string()),
            ("genre_top".into(), self.genre_top.clone()),
            ("genre_prob".into(), self.genre_prob.to_string()),
            ("mood".into(), self.mood.clone()),
            ("mood_prob".into(), self.mood_prob.to_string()),
        ]
    }

    /// Reconstruit l'analyse depuis les tags relus d'un fichier (`custom_tags`),
    /// avec la version du marqueur. Les tags sont la source de vérité : c'est ce
    /// que lit un scan sur une VM neuve, sans relancer Essentia.
    ///
    /// Renvoie `None` si le marqueur `MARKER_TAG` est absent (jamais analysé) ou
    /// si un champ du contrat manque. Invariant tenu par `scan_writeback::write`:
    /// le marqueur n'est posé qu'après une écriture vérifiée de TOUS les champs,
    /// donc « marqueur présent ⇒ contrat complet ». `bpm` vient du `TBPM`
    /// standard (remonté sous le nom `BPM`), le reste des frames `TXXX`.
    pub fn from_tags(tags: &[CustomTag]) -> Option<(Analysis, String)> {
        let version = current_marker(tags)?.to_string();
        let get = |name: &str| {
            tags.iter()
                .find(|t| t.name.trim().eq_ignore_ascii_case(name))
                .map(|t| t.value.trim())
        };
        let num = |name: &str| get(name).and_then(|v| v.parse::<f64>().ok());
        Some((
            Analysis {
                bpm: num("BPM")?,
                key: get("key")?.to_string(),
                scale: get("scale")?.to_string(),
                loudness_lufs: num("loudness_lufs")?,
                replaygain_db: num("replaygain_db")?,
                danceability: num("danceability")?,
                genre_top: get("genre_top")?.to_string(),
                genre_prob: num("genre_prob")?,
                mood: get("mood")?.to_string(),
                mood_prob: num("mood_prob")?,
            },
            version,
        ))
    }
}

/// Pourquoi un fichier n'a pas été analysé (compté dans le `Tally`, détaillé
/// au log). Pas de playback : l'échec laisse le fichier jouable, retenté plus tard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FailKind {
    /// Le chemin ne résout pas / sort de la racine média.
    Resolve,
    /// L'extracteur n'a pas pu démarrer ou a renvoyé un code non nul.
    Extractor,
    /// Dépassement du délai : process tué.
    Timeout,
    /// Sortie illisible / JSON inattendu.
    Parse,
}

impl FailKind {
    pub fn as_str(self) -> &'static str {
        match self {
            FailKind::Resolve => "resolve",
            FailKind::Extractor => "extractor",
            FailKind::Timeout => "timeout",
            FailKind::Parse => "parse",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub kind: FailKind,
    pub detail: String,
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.detail)
    }
}

/// Abstraction : l'extracteur pourra devenir un sidecar gRPC (option B) plus
/// tard sans toucher au pipeline de scan.
pub trait MediaAnalyzer: Send + Sync {
    fn analyze(&self, file: &Path) -> Result<Analysis, Failure>;
}

/// Extracteur Essentia lancé en sous-process (jamais via un shell).
pub struct EssentiaExtractor {
    /// Exécutable : `STATIOND_ESSENTIA`, sinon le défaut sur le PATH.
    pub exe: OsString,
    /// Profil extracteur (quels modèles TF, fenêtre d'analyse). `None` = défaut.
    pub profile: Option<PathBuf>,
    /// Délai dur par fichier ; au-delà le process est tué. Ex. 90 s.
    pub timeout: Duration,
}

impl EssentiaExtractor {
    /// Exécutable par défaut : `STATIOND_ESSENTIA`, sinon `essentia_streaming_extractor_music`.
    pub fn default_exe() -> OsString {
        std::env::var_os("STATIOND_ESSENTIA")
            .unwrap_or_else(|| "essentia_streaming_extractor_music".into())
    }

    /// Lance l'extracteur sur `file`, renvoie le JSON brut.
    ///
    /// TODO: reprendre tel quel le pattern de `bpm_analysis::decode` :
    ///   - `Command::new(&self.exe)` avec arguments explicites (profil, in=file,
    ///     out=stdout), `Stdio::piped`, `creation_flags(0x08000000)` sous Windows ;
    ///   - lecture stdout/stderr sur threads (sorties bornées) ;
    ///   - `try_wait` + `self.timeout` → kill → `FailKind::Timeout`.
    fn run(&self, file: &Path) -> Result<Vec<u8>, Failure> {
        const MAX_OUT: u64 = 4 * 1024 * 1024; // le JSON est petit ; borne de sûreté
        let extractor = |detail: String| Failure { kind: FailKind::Extractor, detail };

        let mut cmd = Command::new(&self.exe);
        if let Some(p) = &self.profile {
            cmd.arg("--profile").arg(p);
        }
        cmd.arg(file).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| extractor(format!("cannot start extractor {:?}: {e}", self.exe)))?;
        let mut out = child.stdout.take().ok_or_else(|| extractor("missing extractor stdout".into()))?;
        let mut err = child.stderr.take().ok_or_else(|| extractor("missing extractor stderr".into()))?;
        // Les deux pipes sont lus jusqu'au bout (au-delà de la borne, jeté) : un
        // pipe plein bloquerait l'extracteur jusqu'au timeout.
        let reader = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = (&mut out).take(MAX_OUT + 1).read_to_end(&mut bytes);
            let _ = std::io::copy(&mut out, &mut std::io::sink());
            bytes
        });
        let errors = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = (&mut err).take(8192).read_to_end(&mut bytes);
            let _ = std::io::copy(&mut err, &mut std::io::sink());
            String::from_utf8_lossy(&bytes).to_string()
        });
        let started = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(s)) => break Ok(s),
                Ok(None) if started.elapsed() < self.timeout => {
                    std::thread::sleep(Duration::from_millis(25))
                }
                Ok(None) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break Err(Failure {
                        kind: FailKind::Timeout,
                        detail: format!("extractor timed out after {:?}", self.timeout),
                    });
                }
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break Err(extractor(format!("extractor wait: {e}")));
                }
            }
        };
        let bytes = reader.join().map_err(|_| extractor("extractor reader panicked".into()))?;
        let stderr = errors.join().unwrap_or_default();
        let status = status?;
        if !status.success() {
            return Err(extractor(format!("extractor failed ({status}): {}", stderr.trim())));
        }
        if bytes.len() as u64 > MAX_OUT {
            return Err(extractor("extractor output too large".into()));
        }
        Ok(bytes)
    }
}

impl MediaAnalyzer for EssentiaExtractor {
    fn analyze(&self, file: &Path) -> Result<Analysis, Failure> {
        let raw = self.run(file)?;
        parse(&raw)
    }
}

/// Parse le JSON Essentia → le sous-ensemble du contrat (`Analysis`).
///
/// TODO: `serde_json` vers une struct intermédiaire `EssentiaJson`, puis map.
/// NB — les CHEMINS exacts des champs (rhythm.bpm, tonal.key_*, highlevel.*)
/// dépendent du profil / des modèles TF choisis : À CONFIRMER sur une sortie
/// réelle avant de figer (une passe de l'extracteur sur un fichier témoin suffit).
fn parse(json: &[u8]) -> Result<Analysis, Failure> {
    #[derive(serde::Deserialize)]
    struct Raw {
        bpm: f64,
        key: String,
        scale: String,
        loudness_lufs: f64,
        replaygain_db: f64,
        danceability: f64,
        genre_top: String,
        genre_prob: f64,
        mood: String,
        mood_prob: f64,
    }
    let r: Raw = serde_json::from_slice(json)
        .map_err(|e| Failure { kind: FailKind::Parse, detail: format!("extractor JSON: {e}") })?;
    // `scan_writeback` refuse les chaînes vides ; on rejette tôt, plus lisible.
    for (name, v) in [("key", &r.key), ("scale", &r.scale), ("genre_top", &r.genre_top), ("mood", &r.mood)] {
        if v.trim().is_empty() {
            return Err(Failure { kind: FailKind::Parse, detail: format!("extractor returned empty {name}") });
        }
    }
    Ok(Analysis {
        bpm: r.bpm,
        key: r.key,
        scale: r.scale,
        loudness_lufs: r.loudness_lufs,
        replaygain_db: r.replaygain_db,
        danceability: r.danceability,
        genre_top: r.genre_top,
        genre_prob: r.genre_prob,
        mood: r.mood,
        mood_prob: r.mood_prob,
    })
}

/// Résout `rel_path` sous `root` en refusant toute sortie hors racine (mêmes
/// garde-fous que l'ancien `bpm_analysis`).
fn resolve_within_root(root: &Path, rel_path: &str) -> Result<PathBuf, Failure> {
    let resolve_err = |e: String| Failure { kind: FailKind::Resolve, detail: e };
    let full = crate::media_tags::resolve(root, rel_path).map_err(|e| resolve_err(e.to_string()))?;
    let full = full.canonicalize().map_err(|e| resolve_err(e.to_string()))?;
    let base = root.canonicalize().map_err(|e| resolve_err(e.to_string()))?;
    if !full.starts_with(&base) {
        return Err(resolve_err("audio resolves outside media root".into()));
    }
    Ok(full)
}

/// Ce qu'une passe d'analyse a fait : analysés, et laissés de côté (avec la raison).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tally {
    pub analyzed: usize,
    pub failed: Vec<(String, Failure)>,
}

impl Tally {
    /// Échecs par nature, dans un ordre stable.
    pub fn by_kind(&self) -> Vec<(FailKind, usize)> {
        let mut out: std::collections::BTreeMap<FailKind, usize> = Default::default();
        for (_, f) in &self.failed {
            *out.entry(f.kind).or_default() += 1;
        }
        out.into_iter().collect()
    }
}

/// Fichiers à analyser : marqueur `MARKER_TAG` absent ou de version != courante
/// (ou tous si `force`). Les fichiers déjà à jour sont sautés (pas de calcul).
///
/// TODO: lire le frame `MARKER_TAG` dans `report.custom_tags` et comparer à
/// `ANALYSIS_VERSION`.
pub fn pending(report: &ScanReport, force: bool) -> Vec<String> {
    report
        .media
        .iter()
        .filter(|m| {
            if force {
                return true;
            }
            match report.custom_tags.get(&m.rel_path) {
                Some(tags) => current_marker(tags) != Some(ANALYSIS_VERSION),
                None => true,
            }
        })
        .map(|m| m.rel_path.clone())
        .collect()
}

/// Analyse les fichiers en attente, un par un hors runtime async (le pool de
/// workers borné est géré par l'appelant). Pour chaque succès : dépose les
/// descripteurs dans `report` (→ tags via `scan_writeback`, → ligne typée
/// `media_analysis`) et pose le marqueur `MARKER_TAG = ANALYSIS_VERSION`.
/// Échec → fichier laissé jouable, loggé, poussé dans le `Tally`, retenté.
/// `progress(done, total)` après chaque fichier.
///
/// TODO: boucle calquée sur `bpm_analysis::analyze_missing_in` :
///   resolve(root, rel) → canonicalize → starts_with(base) → `analyzer.analyze`.
pub fn analyze_pending(
    root: &Path,
    report: &mut ScanReport,
    analyzer: &dyn MediaAnalyzer,
    force: bool,
    limit: usize,
    progress: &mut dyn FnMut(usize, usize),
) -> Tally {
    let mut todo = pending(report, force);
    // `limit` > 0 borne le nombre de fichiers analysés ce scan (les N premiers
    // non marqués) ; `0` = illimité. Le reste sera repris aux scans suivants.
    if limit > 0 && todo.len() > limit {
        todo.truncate(limit);
    }
    let total = todo.len();
    let mut tally = Tally::default();
    progress(0, total);
    for (i, rel_path) in todo.into_iter().enumerate() {
        let result = resolve_within_root(root, &rel_path).and_then(|full| analyzer.analyze(&full));
        match result {
            Ok(a) => {
                // Déposé dans metadata → écrit dans les tags par scan_writeback.
                // La ligne typée media_analysis sera reconstruite depuis les tags
                // (reconstitution), donc au plus tard au scan suivant.
                let meta = report.metadata.entry(rel_path.clone()).or_default();
                for (k, v) in a.to_metadata() {
                    meta.insert(k, v);
                }
                meta.insert(MARKER_TAG.to_string(), ANALYSIS_VERSION.to_string());
                tracing::info!(media = %rel_path, bpm = a.bpm, "media analysed");
                tally.analyzed += 1;
            }
            Err(f) => {
                tracing::warn!(event = "analysis_failed", media = %rel_path, reason = %f, "media not analysed; left playable");
                tally.failed.push((rel_path, f));
            }
        }
        progress(i + 1, total);
    }
    tally
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::ScannedMedia;

    fn tag(n: &str, v: &str) -> CustomTag {
        CustomTag { name: n.into(), value: v.into() }
    }

    fn full_tags() -> Vec<CustomTag> {
        vec![
            tag(MARKER_TAG, ANALYSIS_VERSION),
            tag("BPM", "128"),
            tag("key", "A"),
            tag("scale", "minor"),
            tag("loudness_lufs", "-9.3"),
            tag("replaygain_db", "-6.1"),
            tag("danceability", "0.82"),
            tag("genre_top", "house"),
            tag("genre_prob", "0.74"),
            tag("mood", "energetic"),
            tag("mood_prob", "0.6"),
        ]
    }

    #[test]
    fn from_tags_roundtrips_a_full_marked_file() {
        let (a, version) = Analysis::from_tags(&full_tags()).unwrap();
        assert_eq!(version, ANALYSIS_VERSION);
        assert_eq!(a.bpm, 128.0);
        assert_eq!((a.key.as_str(), a.scale.as_str()), ("A", "minor"));
        assert_eq!(a.loudness_lufs, -9.3);
        assert_eq!(a.genre_top, "house");
        assert_eq!(a.mood_prob, 0.6);
    }

    #[test]
    fn no_marker_or_incomplete_contract_yields_none() {
        // Pas de marqueur : jamais analysé.
        let mut tags = full_tags();
        tags.retain(|t| !t.name.eq_ignore_ascii_case(MARKER_TAG));
        assert!(Analysis::from_tags(&tags).is_none());
        // Marqueur présent mais un champ du contrat manquant.
        let mut tags = full_tags();
        tags.retain(|t| !t.name.eq_ignore_ascii_case("key"));
        assert!(Analysis::from_tags(&tags).is_none());
    }

    fn media(rel: &str) -> ScannedMedia {
        ScannedMedia {
            rel_path: rel.into(),
            title: None,
            artist: None,
            album: None,
            year: None,
            genres: Vec::new(),
            duration_ms: 1000,
            size_bytes: 1,
            mtime_ns: 0,
        }
    }

    #[test]
    fn pending_skips_files_marked_with_the_current_version() {
        let mut report = ScanReport::default();
        report.media.push(media("done.mp3"));
        report.media.push(media("stale.mp3"));
        report.media.push(media("fresh.mp3"));
        report.custom_tags.insert("done.mp3".into(), vec![tag(MARKER_TAG, ANALYSIS_VERSION)]);
        report.custom_tags.insert("stale.mp3".into(), vec![tag(MARKER_TAG, "essentia-tf/0")]);
        // fresh.mp3 : aucun tag.
        assert_eq!(pending(&report, false), vec!["stale.mp3".to_string(), "fresh.mp3".to_string()]);
        // force : tout est à refaire, dans l'ordre de report.media.
        assert_eq!(
            pending(&report, true),
            vec!["done.mp3".to_string(), "stale.mp3".to_string(), "fresh.mp3".to_string()]
        );
    }

    #[test]
    fn parse_reads_the_contract_and_rejects_bad_payloads() {
        let json = br#"{"bpm":128.0,"key":"A","scale":"minor","loudness_lufs":-9.3,"replaygain_db":-6.1,"danceability":0.82,"genre_top":"house","genre_prob":0.74,"mood":"energetic","mood_prob":0.6}"#;
        let a = parse(json).unwrap();
        assert_eq!(a.bpm, 128.0);
        assert_eq!(a.genre_top, "house");
        // JSON invalide, champ manquant, label vide : tous FailKind::Parse.
        assert!(matches!(parse(b"not json").unwrap_err().kind, FailKind::Parse));
        assert!(matches!(parse(br#"{"bpm":120.0}"#).unwrap_err().kind, FailKind::Parse));
        let empty_mood = br#"{"bpm":128.0,"key":"A","scale":"minor","loudness_lufs":-9.3,"replaygain_db":-6.1,"danceability":0.82,"genre_top":"house","genre_prob":0.74,"mood":"  ","mood_prob":0.6}"#;
        assert!(matches!(parse(empty_mood).unwrap_err().kind, FailKind::Parse));
    }

    #[test]
    fn to_metadata_and_from_tags_are_inverse() {
        let a = Analysis {
            bpm: 128.0,
            key: "A".into(),
            scale: "minor".into(),
            loudness_lufs: -9.3,
            replaygain_db: -6.1,
            danceability: 0.82,
            genre_top: "house".into(),
            genre_prob: 0.74,
            mood: "energetic".into(),
            mood_prob: 0.6,
        };
        let mut tags: Vec<CustomTag> =
            a.to_metadata().into_iter().map(|(name, value)| CustomTag { name, value }).collect();
        tags.push(CustomTag { name: MARKER_TAG.into(), value: ANALYSIS_VERSION.into() });
        let (b, version) = Analysis::from_tags(&tags).unwrap();
        assert_eq!(version, ANALYSIS_VERSION);
        assert_eq!(b, a);
    }

    #[test]
    fn a_missing_extractor_is_reported_not_panicked() {
        let x = EssentiaExtractor {
            exe: "/nonexistent/stationd-essentia-for-test".into(),
            profile: None,
            timeout: std::time::Duration::from_secs(5),
        };
        let err = x.analyze(std::path::Path::new("whatever.mp3")).unwrap_err();
        assert_eq!(err.kind, FailKind::Extractor);
    }
}
