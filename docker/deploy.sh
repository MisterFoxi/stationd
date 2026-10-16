#!/usr/bin/env bash
# Déploie un paquet existant, sans le reconstruire :
# bash docker/deploy.sh dist/stationd-<tag>.tar utilisateur@hôte
set -euo pipefail

die() { echo "deploy.sh : $*" >&2; exit 1; }
[ $# = 2 ] || die "usage : $0 archive.tar cible-SSH"
archive="$1"
vm="$2"
[ -s "$archive" ] || die "archive absente ou vide : $archive"
[[ "$vm" =~ ^[a-zA-Z0-9][a-zA-Z0-9_.@:-]*$ ]] || die "cible SSH invalide : $vm"
name="$(basename "$archive")"
[[ "$name" =~ ^stationd-([a-zA-Z0-9][a-zA-Z0-9_.-]*)\.tar$ ]] || die "nom de paquet invalide : $name"
tag="${BASH_REMATCH[1]}"
for cmd in ssh scp; do
  command -v "$cmd" >/dev/null || die "commande requise absente : $cmd"
done

echo "== préparation du transfert vers $vm"
remote="$(ssh "$vm" 'mktemp -d /var/tmp/stationd-deploy.XXXXXXXXXX')"
# Le chemin reçu est utilisé dans des commandes distantes, notamment le nettoyage.
[[ "$remote" =~ ^/var/tmp/stationd-deploy\.[a-zA-Z0-9]{10}$ ]] || die "répertoire distant invalide : $remote"
failed() {
  result=$?
  if [ "$result" != 0 ]; then
    echo "deploy.sh : échec ; paquet local conservé : $archive" >&2
    echo "deploy.sh : fichiers distants conservés dans $vm:$remote" >&2
  fi
}
trap failed EXIT

echo "== transfert de $name vers $vm"
scp -- "$archive" "$vm:$remote/bundle.tar"
echo "== extraction et installation sur $vm (sudo peut demander un mot de passe)"
# Un terminal permet à sudo de demander le mot de passe de la session SSH.
ssh -t "$vm" "cd '$remote' && tar -xf bundle.tar && sudo './stationd-$tag/install.sh'"
echo "== installation réussie sur $vm"
ssh "$vm" "rm -rf -- '$remote'"
