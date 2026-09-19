#!/usr/bin/env bash
# seed_media.sh — remplit `media` (+ media_genre) avec des albums cohérents,
# pour tester filtres / sélection / preview SANS vrais médias.
#
#   ./seed_media.sh                        # 5 albums x 8 pistes → ./data/stationd.db
#   DB=./data/stationd.db ALBUMS=8 TRACKS=12 ./seed_media.sh
#   TOUCH_FILES=1 ./seed_media.sh          # crée aussi des fichiers vides sous ./media
set -euo pipefail

DB="${DB:-./data/stationd.db}"
MEDIA_ROOT="${MEDIA_ROOT:-./media}"
ALBUMS="${ALBUMS:-5}"
TRACKS="${TRACKS:-8}"
TOUCH_FILES="${TOUCH_FILES:-0}"

now_s=$(date +%s); now_ns="${now_s}000000000"

names=(  "Neon Tigers" "Blue Horizon" "Midnight Mode" "Paper Streets" "Deep Field" "Golden Hour" "Static Bloom" "Riverbend" )
artist=( "The Volts"   "Claire Nova"  "Dusklight"     "Foxglove"      "Orbital Kite" "Marlowe"    "Kite String"  "Ash Ember" )
year=(   2019          2021           2016            2023            2018          2022          2020           2015 )
genre=(  "rock"        "pop"          "electronic"    "indie"         "jazz"        "pop"         "electronic"   "folk" )

esc() { printf "%s" "$1" | sed "s/'/''/g"; }

sql="PRAGMA foreign_keys=ON;
BEGIN;"
for ((a=0; a<ALBUMS; a++)); do
  i=$(( a % ${#names[@]} ))
  al="${names[$i]}"; (( a >= ${#names[@]} )) && al="$al vol$(( a/${#names[@]} + 1 ))"
  ar="${artist[$i]}"; yr="${year[$i]}"; ge="${genre[$i]}"
  base=$(( 150 + RANDOM % 120 ))                 # durée moyenne de l'album
  for ((t=1; t<=TRACKS; t++)); do
    tt=$(printf "%02d" "$t"); ti="Track $tt"
    rel="$ar/$al/$tt - $ti.mp3"
    d=$(( base - 40 + RANDOM % 80 )); dms=$(( d*1000 )); sz=$(( d*16000 ))
    r=$(esc "$rel"); T=$(esc "$ti"); A=$(esc "$ar"); L=$(esc "$al")
    sql+="
INSERT OR REPLACE INTO media (rel_path,title,artist,album,year,duration_ms,size_bytes,mtime_ns,available,scanned_at)
 VALUES ('$r','$T','$A','$L',$yr,$dms,$sz,$now_ns,1,$now_s);
DELETE FROM media_genre WHERE rel_path='$r';
INSERT INTO media_genre (rel_path,genre) VALUES ('$r','$ge');"
    if [[ "$TOUCH_FILES" == "1" ]]; then mkdir -p "$MEDIA_ROOT/$(dirname "$rel")"; : > "$MEDIA_ROOT/$rel"; fi
  done
done
sql+="
COMMIT;"

printf '%s\n' "$sql" | sqlite3 "$DB"
echo "seed: $((ALBUMS*TRACKS)) médias (albums=$ALBUMS, pistes=$TRACKS, touch_files=$TOUCH_FILES) → $DB"