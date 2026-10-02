# Vue arborescente des médias

Dans **Médias (5)**, **v** bascule entre la liste habituelle et la vue dossiers.
Cette vue représente les chemins de la bibliothèque indexée sous
media.library_path. Scanner les médias pour y faire apparaître les nouveaux
fichiers. Les dossiers sans média indexé ne sont pas affichés.

- **↑ / ↓** : choisir un dossier lorsque l’arborescence a le focus.
- **→ / ←** : déplier / replier ; ← sur un dossier déjà replié remonte au parent.
- **Espace** : déplier / replier un dossier.
- **Tab / Maj+Tab** : passer des dossiers aux fichiers et inversement.
- **Entrée** sur un dossier : passer à ses fichiers ; sur un fichier : ouvrir sa fiche.
- **v** : revenir à la liste habituelle.
- **r** : actualiser les dossiers et les fichiers.
- **a** : inclure les fichiers indisponibles et leurs dossiers.

Le panneau de droite affiche uniquement les fichiers directement dans le
dossier sélectionné, sans mélanger ceux de ses sous-dossiers. Il montre les
noms de fichiers, titres/artistes, genres et durées ; le chemin et les genres
du fichier sélectionné apparaissent aussi en dessous, même sur un terminal
de 80 colonnes. La fiche contient les autres informations.

Les recherches, genres, âges, filtres de métadonnées manquantes et tris
restent utilisables. Le préfixe dossier: reste un filtre récursif et se
combine avec le dossier sélectionné. Les fichiers sont paginés comme dans
la liste. Les marques et les actions existantes sur les fichiers sont
conservées : tags, Type, playlist, file d’attente, override.

Le daemon et la TUI doivent être reconstruits ensemble. La vue utilise
LibraryService.ListFolders (inventaire paginé des dossiers et de leurs
effectifs, sous-dossiers compris) et le filtre facultatif directory de
SearchMedia (chemin relatif exact, chaîne vide pour la racine). Les anciens
clients gardent la recherche récursive existante.

L’inventaire lit les chemins indexés et inclut leurs ancêtres. Il ne parcourt
pas un chemin fourni par le client sur le disque. Les chemins comportant des
espaces, des accents et des différences de casse sont conservés.
