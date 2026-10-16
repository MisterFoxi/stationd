# breath-detect

Détecteur de respirations Suno sur **stem vocal**, CLI autonome (pur Rust,
Linux/Windows, aucune dépendance système). Implémente la spec v1 (mode
assistance) — voir `Doc/breath-detection-spec.md`.

## Build

```
cargo build --release
```
Binaire : `target/release/breath-detect` (`.exe` sous Windows). Rien d'autre à
installer (hound, rustfft, serde, clap — tout pur Rust).

## Entrée

Un **stem vocal** WAV **48 kHz** (mono ou stéréo, 16/24/32-bit ou float). Pas le
mix : sur le mix le souffle est masqué. Un SR ≠ 48 kHz émet un avertissement
(ré-échantillonner avant).

## Usage

Construire la bibliothèque depuis des souffles confirmés (centres en secondes) :
```
breath-detect build-lib --stem voix_metal.wav --centers "30.10,32.60,40.00,42.66" --lib breath_lib.json --note metal
```
L'enrichir (nouvelle voix, ou un souffle raté — « trou dans la raquette ») :
```
breath-detect build-lib --stem autre_voix.wav --centers "106.6" --lib breath_lib.json --note autre --append
```
Détecter :
```
breath-detect detect --stem voix.wav --lib breath_lib.json --thr 0.45
```
Sortie : `voix_breath.json` au format du contrat `{s,e,lo,hi,c}` (secondes, Hz,
confiance), + la liste à l'écran. `--out chemin.json` pour choisir le fichier.

Écrire un WAV marqué pour RX (depuis le JSON de régions) :
```
breath-detect cue --stem voix.wav --regions voix_breath.json --out voix_cue.wav
```
Copie l'audio tel quel et ajoute, par souffle, un **cue point** (RX affiche le
marqueur) + une **region** `ltxt` bornée ("breath N"). Tu ouvres `voix_cue.wav`
dans RX : les zones sont déjà posées, tu nettoies au lasso spectral.

Debug (caler les seuils) :
```
breath-detect probe --stem voix.wav --times "30.1,32.6,55"
```
→ apériodicité / flatness / rms aux instants donnés.

## Ce qu'il fait (spec v1)

STFT → log-mel (48k, n_fft 2048, hop 256, 64 mels) → match **max-cosine** sur la
biblio (bande ≥ 2 kHz) → **garde-fou** non-voisé (YIN, apériodicité > 0,50) +
bruité (flatness 1,5–9 kHz > 0,40) + non-silence → régions (fusion < 0,1 s,
durée ≥ 0,08 s) avec bande `[lo,hi]` et confiance.

## Limites (assumées)

- **Rappel élevé, précision limitée** : ramasse aussi sifflantes/consonnes
  soufflées (acoustiquement une bouffée HF comme le souffle). → outil
  d'**assistance** : on valide dans RX. Pas d'auto-correction.
- **Cas limite connu** : un souffle très court dont la portion au-dessus du
  plancher de niveau fait < ~80 ms peut passer sous la durée mini (ex. 32,60 s
  sur le stem metal de test). Baisser `--thr` augmente le rappel au prix de FP.
- Implémentation indépendante du proto Python : comptages voisins, pas
  identiques (YIN maison ≠ pYIN).

## Paramètres figés

Dans `src/detect.rs` (seuils) et `src/dsp.rs` (STFT/mel). Changer un paramètre de
features = reconstruire la biblio et bumper `av` (cf. spec §2/§4).

## Portage / intégration StationD

DSP pur → peut vivre en module core (pas de risque de crash natif). Sortie →
frame `TXXX:STATIOND_BREATH` + cache `media_breath` (migration 0037). Le seul
point délicat du port était le voisement : ici **YIN** (autocorrélation)
remplace pYIN ; un `voiced_prob` grossier suffit au garde-fou (validé : souffles
apér. 0,63–0,80 vs chant 0,04–0,06).
