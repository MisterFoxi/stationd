# Détection de respirations Suno — spécification v1 (mode assistance)

_Spec figée pour portage Rust. Prototype de référence : `breath_detect.py` + biblio `breath_lib.{npz,json}`. 2026-10-10._

## 1. But et position dans le pipeline

Repérer les respirations (« souffles ») générées par Suno pour les corriger
avant diffusion / mise sous StationD. Deux cibles partagent le même détecteur :

- **Post-prod (mode retenu v1)** : le détecteur **pose des marqueurs** aux
  positions suspectes. L'humain nettoie dans iZotope RX (sélection spectrale)
  et re-mixe depuis les stems. Pas de correction automatique.
- **StationD** : flag d'information sur le média (pas de correction auto en v1 —
  précision insuffisante, cf. §7).

**Entrée obligatoire : le STEM VOCAL isolé, jamais le mix.** Sur le mix final,
le souffle est masqué par l'instrumentation (metal symphonique) : détection non
fiable (ratés + faux positifs sur transitoires d'instruments). Sur le stem, les
features redeviennent exploitables. Le pipeline corrige donc au stade stem puis
re-mixe. Les stems extraits sont toujours un peu contaminés par la séparation :
c'est assumé (cf. choix biblio, §4).

Exécution : au **scan**, une seule fois par fichier, piloté par le marqueur de
version `av` (cf. §5). DSP pur → peut vivre en **module core Rust** (pas de
risque de crash C++ comme Essentia, donc pas d'obligation de sous-processus).

## 2. Chaîne de traitement (paramètres FIGÉS)

Changer un paramètre de cette liste = rebuild de la biblio + bump de `av`.

| Paramètre            | Valeur   |
|----------------------|----------|
| Fréquence éch. (SR)  | 48000 Hz |
| FFT (n_fft)          | 2048     |
| Hop                  | 256 (≈5,33 ms/trame, FPS≈187,5) |
| Bancs mel (n_mels)   | 64       |
| fmin / fmax mel      | 200 / 12000 Hz |
| Fenêtre patch (WIN)  | 0,20 s (≈37 trames) |
| Bande de matching    | ≥ 2000 Hz (bins mel) |

Étapes, par trame glissante :

1. **Log-mel** du signal (params ci-dessus).
2. **Match biblio** : pour la fenêtre de 200 ms centrée sur la trame, on garde
   les bins mel ≥ 2 kHz, on aplatit, on centre (−moyenne), on normalise (L2),
   puis **cosine** contre chaque exemplaire de la biblio. `score = max` sur la
   biblio. (Bande ≥2 kHz = là où le souffle domine et où le bleed de séparation
   fuit le moins.)
3. **Garde-fou** (un souffle est NON-VOISÉ + BRUITÉ ; sinon c'est du chant) :
   - voisement (`voiced_prob`) < 0,25 — voir note Rust §8 ;
   - flatness bande 1,5–9 kHz > 0,40 (moyenne géométrique / moyenne arithmétique
     des puissances mel de la bande) ;
   - `rms` > plancher, plancher = p90(rms) × 0,12 (rejette le silence).
4. **Régions** : trames où `score ≥ seuil` **ET** garde-fou vrai ; fusion des
   trous < 0,10 s ; durée minimale 0,08 s.
5. Par région : `c` = pic de score ; `[lo,hi]` = bande où la région dépasse la
   ligne de base voisine (±0,4 s) de ≥ 2 dB (STFT locale), repli [1500, 11000] Hz.

Seuil de match par défaut : **0,45** (ajustable ; le garde-fou fait le gros du
tri — passage de ~150 à ~60 régions sur un morceau type).

## 3. Caractérisation du souffle (mesurée)

- Bouffée de **bruit haute fréquence** : centroïde ~7 kHz, ~55–60 % de l'énergie
  au-dessus de 5 kHz. **Spectralement quasi identique à une sifflante** (s/ch/f) —
  d'où la limite de précision (§7).
- Non-harmonique (pas de hauteur) → `voiced_prob` effondré.
- Durée **variable** (~95–700 ms selon le contexte musical ; pas une constante).
- Le « grain » perçu est constant d'une voix à l'autre, mais la forme fine varie
  (et la contamination de séparation ajoute du bruit) → d'où l'approche biblio
  plutôt qu'un gabarit canonique unique.

## 4. Bibliothèque de gabarits

- **Rôle** : jeu d'exemplaires de souffles (patchs log-mel bande ≥2 kHz, centrés,
  normalisés). Score d'une position = max cosine sur la biblio. Beaucoup
  d'exemplaires contaminés → le souffle commun se renforce, les contaminations
  (aléatoires) ne coïncident pas. Plus la biblio grandit, plus c'est robuste.
- **Enrichissement sur « trous dans la raquette »** : un souffle raté → on
  l'ajoute à la biblio (`enrich <stem> <centre_s>`), la version monte.
- **Format** : vecteurs normalisés (`breath_lib.npz`) + manifeste de provenance
  (`breath_lib.json` : source, centre, note). En Rust : embarquer les vecteurs
  comme donnée chargée au démarrage.
- **Versionnement ↔ `av`** : la version de la biblio alimente le marqueur `av`
  de `TXXX:STATIOND_BREATH`. Enrichir = bumper `av` → pilote si on re-scanne la
  librairie pour rattraper les trous (run-once par fichier via ce marqueur).
- Biblio initiale : 4 souffles confirmés voix « metal » (30,10 / 32,60 / 40,00 /
  42,66 s) + Little_Shadow. À étendre.

## 5. Contrat de données

Frame unique dans le tag du fichier (cf. migration `0037_media_breath.sql`) :

```
TXXX:STATIOND_BREATH = {"v":1,"av":"1","regions":[{"s":..,"e":..,"lo":..,"hi":..,"c":..}]}
```
- `v` = version de schéma JSON ; `av` = version d'analyseur/biblio (marqueur
  run-once) ; `regions` = liste (vide si fichier analysé et propre).
- `s`,`e` en **secondes** (converties en samples au moment d'écrire les cue du
  WAV, avec le SR réel) ; `lo`,`hi` en Hz ; `c` confiance 0→1.

Cache SQLite `media_breath` (table 0037, clé `media_uuid`) : miroir
reconstructible depuis le tag en un scan ; `breath_count`, `max_confidence`,
`regions_json`, `analyzer_version`, `schema_version`.

## 6. Sortie vers RX (post-prod)

Les positions vivant dans le tag, le canal est interchangeable. Vérifié : **RX
lit les cue markers `cue `/`LIST/adtl` et les regions `ltxt` d'un WAV**. Un
utilitaire écrit, pour un stem WAV, une region `ltxt` par souffle (label
`breath N`, bornée en temps ; la borne fréquence `[lo,hi]` sert au lasso
spectral). On n'écrit QUE dans des WAV (les stems le sont). Pas de ré-encodage
retour : le WAV nettoyé repart au mixage.

## 7. Limite connue (assumée en v1)

- **Rappel élevé** : tous les souffles confirmés ressortent.
- **Précision limitée** : le détecteur ramasse aussi sifflantes et consonnes
  soufflées, acoustiquement confondues avec le souffle (même bouffée HF). Trois
  discriminateurs testés (forme spectrale, template, contexte voisé) ne séparent
  PAS proprement souffle / sifflante.
- Conséquence : **mode assistance** — l'humain valide les marques dans RX (un
  faux marqueur ne coûte rien). **Pas d'auto-correction** StationD en v1.
- **Voie précision (option 2, à part)** : petit classifieur entraîné sur un jeu
  étiqueté **2 classes** (souffles confirmés + non-souffles confirmés, ~10–15 de
  chaque par voix). C'est l'enrichissement biblio transformé en jeu positif +
  négatif. Hors périmètre v1.

## 8. Notes d'implémentation Rust

- **STFT / mel** : `rustfft` + banc de filtres mel maison (ou crate mel). Params §2.
- **Voisement** : point non trivial du port (le proto utilise pYIN, lourd à
  réimplémenter). Pragmatique : **YIN** (autocorrélation, portable) ou une
  harmonicité simple (rapport énergie harmonique/bruit). Le garde-fou n'a besoin
  que d'un `voiced_prob` grossier avec seuil 0,25 — pas de la précision pYIN.
- **Flatness** et **RMS** : triviaux (moyennes sur la bande / le signal).
- **Biblio** : vecteurs normalisés embarqués ; matmul `(exemplaires × dim)` par
  fenêtre, max. Chunker pour la mémoire sur longues pistes.
- **Core, pas sous-processus** : DSP pur Rust, pas de risque de crash natif →
  module core au scan (contrairement à Essentia, sorti hors-process pour l'isolation).
- **Sortie** : régions `{s,e,lo,hi,c}` → frame `TXXX:STATIOND_BREATH` (lofty) +
  upsert `media_breath`.
