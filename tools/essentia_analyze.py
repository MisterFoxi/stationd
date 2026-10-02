#!/usr/bin/env python3
"""Extracteur d'analyse média pour stationd (option A : sous-process).

Émet sur stdout le CONTRAT JSON attendu par `src/media_analysis.rs::parse`,
un objet plat :

    {"bpm":128.0,"key":"A","scale":"minor","loudness_lufs":-9.3,
     "replaygain_db":-6.1,"danceability":0.82,"genre_top":"house",
     "genre_prob":0.74,"mood":"energetic","mood_prob":0.6}

Usage (c'est stationd qui l'appelle, un fichier à la fois) :
    essentia_analyze.py [--profile PROFIL] <fichier_audio>

Partie SIGNAL (bpm / key / loudness / replaygain) : complète, fiable, via
essentia.standard. Partie TENSORFLOW (danceability / genre / mood) : structurée
mais DÉPENDANTE de TES modèles — voir high_level(). Sans répertoire de modèles
configuré (STATIOND_ESSENTIA_MODELS), le script s'arrête en erreur plutôt que
d'émettre des labels bidon (qui seraient écrits dans les tags).

En cas d'échec : code de sortie non nul + message sur stderr (stationd le
remonte comme FailKind::Extractor et laisse le fichier jouable, retenté).
"""

import argparse
import json
import os
import sys


def die(msg: str) -> "NoReturn":  # type: ignore[name-defined]
    print(f"essentia_analyze: {msg}", file=sys.stderr)
    raise SystemExit(1)


# --- SIGNAL ---------------------------------------------------------------

def signal_descriptors(path: str) -> dict:
    """bpm, key, scale, loudness_lufs (EBU R128 intégré), replaygain_db."""
    try:
        import essentia.standard as es
    except Exception as e:  # pragma: no cover - dépend de l'install
        die(f"essentia indisponible: {e}")

    # Mono 44.1 kHz pour tempo / tonalité / replaygain.
    mono = es.MonoLoader(filename=path, sampleRate=44100)()
    if mono.size == 0:
        die("décodage vide")

    bpm, _beats, _conf, _est, _intervals = es.RhythmExtractor2013(method="multifeature")(mono)
    key, scale, _strength = es.KeyExtractor()(mono)
    replaygain_db = float(es.ReplayGain(sampleRate=44100)(mono))

    # Loudness EBU R128 intégré : nécessite le signal stéréo tel quel.
    audio_stereo, sr, _ch, _md5, _br, _codec = es.AudioLoader(filename=path)()
    _mom, _short, integrated, _range = es.LoudnessEBUR128(sampleRate=sr)(audio_stereo)

    return {
        "bpm": float(bpm),
        "key": str(key),
        "scale": str(scale),
        "loudness_lufs": float(integrated),
        "replaygain_db": replaygain_db,
    }


# --- TENSORFLOW (à compléter avec TES modèles) ----------------------------

def _predict_top(embeddings, graph_pb: str, meta_json: str) -> tuple[str, float]:
    """Classifieur TF 2D sur embeddings → (label dominant, probabilité).

    `meta_json` est le .json d'accompagnement du modèle essentia (clé
    "classes" = liste ordonnée des labels). À VÉRIFIER contre tes modèles :
    nom du noeud de sortie, agrégation temporelle (ici: moyenne des frames)."""
    import numpy as np
    import essentia.standard as es

    with open(meta_json, "r", encoding="utf-8") as f:
        classes = json.load(f)["classes"]
    activations = es.TensorflowPredict2D(graphFilename=graph_pb)(embeddings)
    mean = np.mean(activations, axis=0)
    idx = int(np.argmax(mean))
    return str(classes[idx]), float(mean[idx])


def high_level(path: str) -> dict:
    """danceability, genre_top/prob, mood/prob via essentia-tensorflow.

    Modèles attendus dans STATIOND_ESSENTIA_MODELS (à adapter aux fichiers que
    TU embarques) :
      - discogs-effnet-bs64-1.pb                 (embeddings)
      - genre_discogs400-discogs-effnet-1.pb/.json
      - mood_* (ou mtg_jamendo_moodtheme)   .pb/.json
      - danceability-discogs-effnet-1.pb/.json
    """
    models = os.environ.get("STATIOND_ESSENTIA_MODELS")
    if not models:
        die("STATIOND_ESSENTIA_MODELS non défini : modèles TensorFlow requis "
            "(voir high_level()). Aucun label émis pour ne pas polluer les tags.")

    import essentia.standard as es
    p = lambda name: os.path.join(models, name)  # noqa: E731

    # Embeddings EffNet-Discogs (entrée commune aux têtes de classif).
    embeddings = es.TensorflowPredictEffnetDiscogs(
        graphFilename=p("discogs-effnet-bs64-1.pb"),
        output="PartitionedCall:1",
    )(es.MonoLoader(filename=path, sampleRate=16000, resampleQuality=4)())

    genre_top, genre_prob = _predict_top(
        embeddings, p("genre_discogs400-discogs-effnet-1.pb"),
        p("genre_discogs400-discogs-effnet-1.json"))
    mood_top, mood_prob = _predict_top(
        embeddings, p("mtg_jamendo_moodtheme-discogs-effnet-1.pb"),
        p("mtg_jamendo_moodtheme-discogs-effnet-1.json"))

    # danceability : tête régressive (ou binaire) → probabilité « danceable ».
    import numpy as np
    dance = es.TensorflowPredict2D(
        graphFilename=p("danceability-discogs-effnet-1.pb"))(embeddings)
    danceability = float(np.mean(dance, axis=0)[0])

    return {
        "danceability": danceability,
        "genre_top": genre_top,
        "genre_prob": genre_prob,
        "mood": mood_top,
        "mood_prob": mood_prob,
    }


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--profile", default=None, help="réservé (modèles/fenêtre)")
    ap.add_argument("audio", help="fichier audio à analyser")
    args = ap.parse_args()

    if not os.path.isfile(args.audio):
        die(f"introuvable: {args.audio}")

    out = {}
    out.update(signal_descriptors(args.audio))
    out.update(high_level(args.audio))

    # Le contrat exige ces 10 clés ; parse() rejette tout manque / label vide.
    json.dump(out, sys.stdout, ensure_ascii=False)
    sys.stdout.write("\n")


if __name__ == "__main__":
    main()
