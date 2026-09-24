#!/usr/bin/env python3
"""Complete Title/Artist ID3v2 depuis ID3v1, champ par champ.

Ubuntu : sudo apt install python3-eyed3
Apercu : python3 id3v1-vers-id3v2.py /mnt/nfs/radio
Ecrire : python3 id3v1-vers-id3v2.py --apply /mnt/nfs/radio

Parcourt les MP3 (extension insensible a la casse) et les sous-dossiers.
Un champ absent, vide ou compose d'espaces est considere comme manquant.
Un champ ID3v2 renseigne n'est jamais remplace. Les ID3v1 sont conserves.
Sans tag ID3v2, cree un ID3v2.3 contenant uniquement les champs a copier.
Les versions ID3v2.3 et 2.4 existantes sont conservees ; 2.2 est signale
comme non pris en charge, sans modification du fichier.
Chaque fichier modifie est sauvegarde en .orig (ou .orig.N si necessaire).
Code retour : 0 = termine ; 1 = erreurs ; 2 = arguments invalides.
"""

import argparse
import os
from pathlib import Path
import sys

try:
    from eyed3.id3 import Tag, ID3_V1, ID3_V2, ID3_V2_3, ID3_V2_4
except ImportError:
    sys.exit("Module eyeD3 absent de ce Python. Ubuntu : sudo apt install python3-eyed3")


def empty(value):
    return value is None or not value.strip()


def complete(path, apply):
    source = Tag()
    if not source.parse(str(path), version=ID3_V1):
        return False

    target = Tag()
    has_v2 = target.parse(str(path), version=ID3_V2)
    changes = {}
    for field in ("title", "artist"):
        value = getattr(source, field)
        if empty(getattr(target, field)) and not empty(value):
            changes[field] = value
    if not changes:
        return False

    version = target.version if has_v2 else ID3_V2_3
    if version not in (ID3_V2_3, ID3_V2_4):
        raise ValueError(f"ID3v{'.'.join(map(str, version))} non pris en charge en ecriture")

    for field, value in changes.items():
        setattr(target, field, value)
    if apply:
        # Ecrit uniquement la version ID3v2 choisie ; ne sauvegarde pas ID3v1.
        target.save(version=version, backup=True)
    label = "MODIFIE" if apply else "APERCU"
    print(f"[{label}] {str(path)!r}")
    for field, value in changes.items():
        print(f"  {field}: {value!r}")
    return True


def main():
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("path", type=Path, help="Fichier MP3 ou dossier")
    parser.add_argument("--apply", action="store_true", help="Ecrire avec sauvegardes .orig")
    args = parser.parse_args()
    root = args.path.absolute()
    if not root.exists():
        parser.error(f"Chemin inexistant : {root}")
    if root.is_file() and root.suffix.lower() != ".mp3":
        parser.error("Le fichier doit avoir une extension .mp3")
    if not root.is_file() and not root.is_dir():
        parser.error("Le chemin doit etre un fichier ou un dossier")

    counts = {"read": 0, "changed": 0, "errors": 0}

    def report(exc):
        counts["errors"] += 1
        print(f"[ERREUR] {exc}", file=sys.stderr)

    def files():
        if root.is_file():
            yield root
        else:
            for directory, dirs, names in os.walk(root, onerror=report):
                dirs.sort()
                for name in sorted(names):
                    path = Path(directory) / name
                    if path.suffix.lower() == ".mp3":
                        yield path

    for path in files():
        counts["read"] += 1
        try:
            if path.is_symlink():
                raise ValueError("Lien symbolique ignore ; fournir le fichier cible directement")
            if not path.is_file():
                raise ValueError("Ce chemin n'est pas un fichier ordinaire")
            counts["changed"] += int(complete(path, args.apply))
        except Exception as exc:
            report(f"{str(path)!r} : {exc}")

    action = "modifies" if args.apply else "a modifier"
    print(f"\n{counts['read']} MP3 examines ; {counts['changed']} {action} ; "
          f"{counts['errors']} erreurs.")
    if not args.apply:
        print("Aucune ecriture. Ajouter --apply pour appliquer avec sauvegardes .orig.")
    return 1 if counts["errors"] else 0


if __name__ == "__main__":
    sys.exit(main())
