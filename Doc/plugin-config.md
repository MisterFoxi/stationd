# Configuration des plugins depuis la TUI

Activer le plugin natif dans le fichier passé à `stationd --config`, puis redémarrer stationd :

```toml
[[plugin]]
name = "plugin-config"
enabled = true
```

L’onglet **Configuration** apparaît dans les vues des plugins. Dans le
catalogue Plugins (8), sélectionner plugin-config et appuyer sur Entrée.
Tab / n et Maj+Tab / p parcourent aussi ces vues.

- ← / → : choisir un plugin ; ↑ / ↓ : choisir un champ.
- Entrée : modifier (ou basculer un booléen), puis Entrée valider le champ.
  Esc annule la saisie. Les secrets sont masqués et ne sont jamais renvoyés
  par le daemon ; une nouvelle saisie remplace leur valeur.
- Suppr : revenir au défaut ou retirer un champ facultatif.
- Ctrl+S : valider et prévisualiser les changements.
- ↑ / ↓ fait défiler les changements dans la prévisualisation.
- La confirmation commence sur **Annuler**. Choisir **Enregistrer** ou
  **Enregistrer et recharger**, puis Entrée.
- Esc, hors saisie et confirmation, abandonne le brouillon. Un brouillon
  empêche le changement de plugin ; changer d’onglet le conserve.
- r actualise lorsque le brouillon est vide.

Enregistrer conserve la configuration en mémoire de l’instance actuelle.
Le prochain démarrage, redémarrage ou rechargement explicite du plugin relit
ses paramètres enregistrés. Enregistrer et recharger applique immédiatement
les paramètres si le plugin est chargé. Un plugin arrêté reste arrêté.
Si le chargement échoue, le fichier reste enregistré et l’échec est affiché ;
l’enregistrement n’est pas présenté comme une application réussie.

Premier formulaire : **stop-when-idle**, natif ou WASM reconstruit :

- min_zero_samples : entier entre 1 et 4294967295, défaut 1 ;
- max_connection_age : facultatif, durée positive (s, m, h, d).
  Le mode âge exige que la collecte des connexions soit déjà active.

Les plugins sans schéma restent sélectionnables avec une explication.
Un plugin WASM doit avoir été chargé au moins une fois pour découvrir son
schéma ; celui-ci reste disponible après son arrêt. Les paramètres imbriqués,
listes et tables ne sont pas éditables dans cette première version.

## Contrat pour les plugins

Natif : `Plugin::config_schema() -> Result<Vec<plugin_config::Field>, String>`.
WASM : export facultatif `config_schema`, entrée vide, sortie JSON
(maximum 64 KiB). Le schéma est découvert avant on_load, sans mutations
autorisées sur le host, et conservé pour l’éditeur.

```json
[
  {
    "key": "min_zero_samples",
    "label": "Échantillons consécutifs",
    "kind": "integer",
    "default": "1",
    "minimum": 1,
    "maximum": 4294967295
  },
  {
    "key": "max_connection_age",
    "label": "Âge maximal des connexions",
    "kind": "duration",
    "optional": true
  }
]
```

Au plus 32 champs : key ASCII alphanumérique ou underscore, unique, 64 octets ;
label non vide sans caractères de contrôle, 256 octets. Types : integer,
boolean, text, duration. Valeurs de saisie : 4096 octets maximum, sans
caractères de contrôle. default est une chaîne interprétée selon le type ;
optional autorise l’absence. minimum et maximum bornent les entiers.
secret masque toute valeur et interdit de publier un défaut.

La validation hôte des types est obligatoire. Le hook facultatif natif
`validate_config(config, host)`, ou l’export WASM `validate_config`
(entrée : objet JSON des paramètres candidats ; sortie : chaîne, erreur
Extism en cas de refus), complète cette validation. Le daemon crée une
instance candidate sans appeler on_load ; le host refuse les mutations.
Ce hook ne doit pas dépendre d’un état créé par on_load.

Un onglet `UiTab` de kind = "plugin_config" ouvre l’éditeur générique,
sans SQL ni capacité db. kind absent ou "table" conserve le contrat des
tableaux. Le plugin plugin-config fournit ce descripteur et n’a aucun accès
direct au fichier de configuration.

## Persistance et limites

GetConfig renvoie champs, valeurs non secrètes et deux révisions.
UpdateConfig prend le nom, les révisions, les modifications et le mode
PREVIEW, SAVE ou SAVE_RELOAD. Aucun chemin de fichier n’est fourni par le
client. Toute modification inconnue ou invalide est refusée avant écriture.

La révision couvre le fichier entier ; celle du schéma protège contre un
rechargement ayant changé les champs. Un conflit (gRPC ABORTED) conserve le
brouillon et nécessite une actualisation explicite, sans réessai automatique.
Les opérations du daemon sont sérialisées par l’acteur des plugins. La
révision est revérifiée juste avant le remplacement ; une écriture externe
dans cet intervalle très court ne peut pas être exclue sans verrou partagé.

Seul [plugin.config] du plugin ciblé est modifié, en préservant les paramètres
inconnus, les autres sections, les commentaires des champs modifiés et les
mode Unix et groupe du fichier. Le propriétaire est conservé lorsque les droits
Unix le permettent. Si le fichier appartient à un administrateur et que le daemon
ne peut pas lui rendre le fichier temporaire, le fichier remplacé appartient au
daemon ; son groupe et son mode restent identiques. Les administrateurs membres
du groupe conservent donc leur accès en écriture. Écriture dans un fichier
temporaire du même dossier, synchronisation et remplacement atomique.
Le dossier doit être accessible
en écriture au daemon. Après le remplacement, le daemon relit le fichier et
vérifie la révision et les valeurs avant de confirmer l'enregistrement ou de
recharger le plugin ; une modification concurrente est signalée comme conflit. Utiliser les tables [plugin.config] ; les tables
inline sont explicitement refusées. Le chemin de configuration est résolu
au démarrage. Ajouter/supprimer des déclarations, modifier leurs capacités
ou leur activation dans le fichier nécessite toujours un redémarrage du daemon.
