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


# --- TENSORFLOW (modèles Discogs-EffNet, dans STATIOND_ESSENTIA_MODELS) ----
#
# Noms vérifiés sur le model zoo essentia.upf.edu (famille discogs-effnet) :
#   discogs-effnet-bs64-1.pb                         (embeddings)
#   genre_discogs400-discogs-effnet-1.pb / .json     (400 styles Discogs)
#   danceability-discogs-effnet-1.pb / .json
#   mood_{happy,aggressive,party,relaxed}-discogs-effnet-1.pb / .json
# Il n'existe PAS de tête mood/theme unique pour discogs-effnet : on combine
# les têtes binaires d'humeur et on garde la plus marquée. Les .json portent la
# clé "classes" (ordre des labels). Noeud de sortie des têtes : "model/Softmax".

def _head(embeddings, graph_pb: str, meta_json: str):
    """(classes, activations moyennes sur les frames) d'une tête TF 2D.

    Les noms de noeuds d'entrée/sortie sont lus dans le `.json` du modèle
    (clé `schema`), robustes aux variations d'export : graphe figé
    (`model/Placeholder` / `model/Softmax`) vs SavedModel
    (`serving_default_model_Placeholder` / `PartitionedCall:0`)."""
    import numpy as np
    import essentia.standard as es
    with open(meta_json, "r", encoding="utf-8") as f:
        meta = json.load(f)
    classes = meta["classes"]
    schema = meta.get("schema", {})
    inputs = schema.get("inputs") or [{}]
    outputs = schema.get("outputs") or []
    inp = inputs[0].get("name")
    out = next((o.get("name") for o in outputs
                if o.get("output_purpose") in ("predictions", "activations")), None)
    if out is None and outputs:
        out = outputs[0].get("name")
    kwargs = {"graphFilename": graph_pb}
    if inp:
        kwargs["input"] = inp
    if out:
        kwargs["output"] = out
    acts = es.TensorflowPredict2D(**kwargs)(embeddings)
    return classes, np.mean(acts, axis=0)


def _positive_index(classes: list) -> int:
    """Index de la classe « positive » d'une tête binaire (happy vs non_happy)."""
    for i, c in enumerate(classes):
        lc = str(c).lower()
        if not (lc.startswith("non") or lc.startswith("not")):
            return i
    return 0


def high_level(path: str) -> dict:
    """danceability, genre_top/prob, mood/prob via essentia-tensorflow."""
    models = os.environ.get("STATIOND_ESSENTIA_MODELS")
    if not models:
        die("STATIOND_ESSENTIA_MODELS non défini : modèles TensorFlow requis "
            "(voir le module). Aucun label émis pour ne pas polluer les tags.")

    import numpy as np
    import essentia.standard as es
    p = lambda name: os.path.join(models, name)  # noqa: E731

    # Embeddings EffNet-Discogs (entrée commune à toutes les têtes).
    embeddings = es.TensorflowPredictEffnetDiscogs(
        graphFilename=p("discogs-effnet-bs64-1.pb"),
        output="PartitionedCall:1",
    )(es.MonoLoader(filename=path, sampleRate=16000, resampleQuality=4)())

    # Genre : argmax sur les 400 styles Discogs.
    g_classes, g_mean = _head(
        embeddings, p("genre_discogs400-discogs-effnet-1.pb"),
        p("genre_discogs400-discogs-effnet-1.json"))
    g_idx = int(np.argmax(g_mean))
    genre_top, genre_prob = str(g_classes[g_idx]), float(g_mean[g_idx])

    # Danceability : probabilité de la classe « danceable ».
    d_classes, d_mean = _head(
        embeddings, p("danceability-discogs-effnet-1.pb"),
        p("danceability-discogs-effnet-1.json"))
    danceability = float(d_mean[_positive_index(d_classes)])

    # Mood : têtes binaires effnet ; on garde l'humeur positive la plus forte.
    mood, mood_prob = "neutral", 0.0
    for name in ("mood_happy", "mood_aggressive", "mood_party", "mood_relaxed"):
        pb, js = p(f"{name}-discogs-effnet-1.pb"), p(f"{name}-discogs-effnet-1.json")
        if not (os.path.isfile(pb) and os.path.isfile(js)):
            continue
        classes, mean = _head(embeddings, pb, js)
        i = _positive_index(classes)
        prob = float(mean[i])
        if prob > mood_prob:
            mood, mood_prob = str(classes[i]), prob

    return {
        "danceability": danceability,
        "genre_top": genre_top,
        "genre_prob": genre_prob,
        "mood": mood,
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
