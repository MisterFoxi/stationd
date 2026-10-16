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

## Préécoute dans Webmin

Dans **Médias → Modification**, ouvrir la fiche d’un seul média puis presser
**Ctrl+P** pour l’écouter dans le navigateur. Un lecteur « Préécoute privée — hors
antenne » apparaît au-dessus du terminal : lecture/pause, déplacement dans le
morceau et arrêt. Si le navigateur bloque le démarrage automatique, cliquer sur
Lecture. **Ctrl+P** pendant la lecture ou **Arrêter la préécoute** coupe le lecteur.
Fermer ou valider la fiche, ou déconnecter la console, arrête aussi la préécoute.

La lecture concerne le MP3 sélectionné et ne modifie ni ses tags, ni les compteurs,
ni la file ou l’antenne. Elle fonctionne aussi sur une station en lecture seule
avec les médias partagés. Elle est disponible dans la console Webmin pour une
fiche individuelle ; aucune sortie audio n’est ouverte sur le serveur.
Le daemon et la TUI doivent être reconstruits ensemble, puis la console rouverte.

Le raccourci est affiché dans la fiche individuelle et dans l’aide de l’écran,
même dans un terminal sans lecteur Webmin. Dans ce cas, Ctrl+P affiche
« Préécoute : aucun lecteur Webmin connecté à ce terminal. » plutôt que de
rester silencieux. Une sélection multiple ne propose pas de préécoute.
