# Webmin — préparation et prise d’antenne DJ

La page `/station/<id>/dj`, accessible par « Passer en direct » depuis la page
de station ou la console d’administration, sépare la réception du flux de sa diffusion publique.

## Parcours

1. Se connecter au Webmin avec un compte autorisé sur la station.
2. Ouvrir « Passer en direct » et saisir les identifiants du fichier des DJ.
   Ce sont les mêmes identifiants que dans BUTT/Mixxx, distincts du compte Webmin.
3. Copier les paramètres Icecast dans le logiciel et lancer la diffusion.
4. StationD attend trois secondes de son décodable continu au-dessus du seuil
   `silence_threshold`. Les mesures viennent de Liquidsoap chaque seconde :
   disponibilité, présence de trames, niveau RMS et crête. Une mesure absente
   depuis plus de trois secondes, une indisponibilité ou du silence retire
   immédiatement le feu vert. Une crête à moins de 0,2 dBFS de zéro avertit le DJ.
5. « Écouter mon flux » ouvre le retour audio privé reçu par StationD. Le lecteur
   possède ses propres commandes de volume et d’arrêt. Le tampon serveur et le
   lecteur ajoutent du délai : utiliser un casque et ne pas réinjecter le retour.
6. « Prendre l’antenne » revérifie l’état sonore et le droit de connexion.
   « Prise d’antenne en cours » reste affiché jusqu’à la transition réelle de
   Liquidsoap, qui confirme « Vous êtes à l’antenne ».
7. « Déconnecter mon flux » ou la déconnexion du logiciel rend l’antenne à la
   programmation. Cette fin volontaire ne bannit pas le DJ. Une déconnexion
   avant la prise d’antenne ne saute pas le morceau ni la piste préchargée.

Un seul flux DJ peut se connecter à la fois, comme avant. La page signale un
DJ déjà connecté et ne permet pas de le remplacer. Les créneaux de grille,
ouvertures ponctuelles et droits urgents restent les règles d’accès. Le silence
pendant la préparation ne déconnecte pas le DJ ; pendant le direct, le délai
`silence_timeout` existant continue de s’appliquer.

La validation vérifie la disponibilité et la continuité du son, pas une qualité
artistique ni l’absence de toute distorsion. La préécoute concerne le flux DJ
avant les traitements de sortie ; ce n’est pas le retour de l’antenne publique.

## Configuration et activation

Sur **chaque station** qui reçoit des DJ :

```toml
[live]
djs_path = "./djs.toml"
harbor_port = 8005
mount = "/live"
manual_takeover = true
buffer = 5.0
silence_threshold = -40.0
silence_timeout = 30
```

`manual_takeover` vaut **true par défaut**. Pour conserver une prise d’antenne
immédiate à la connexion, fixer explicitement `manual_takeover = false`.
La page affiche alors un avertissement et ne propose pas de prise manuelle.

Dans le plugin Webmin, préciser le nom public utilisé par le logiciel DJ :

```toml
[[plugin.config.stations]]
id = "my-station"
label = "My station"
grpc_endpoint = "http://127.0.0.1:50051"
dj_source_host = "dj.example.org" # sans protocole ni port
```

Si `dj_source_host` est absent, la page demande l’adresse à l’administrateur ;
elle ne présente pas l’adresse gRPC interne comme une adresse de diffusion.
Le port à ouvrir pour les logiciels reste `harbor_port`.

Compiler et installer StationD, puis redémarrer StationD **et Liquidsoap** pour
activer le nouveau script. Mettre également à jour le daemon hébergeant le
Webmin si celui-ci est sur un nœud séparé. Un Webmin mis à jour ne peut pas
utiliser les nouveaux RPC contre un ancien daemon. Ne pas redémarrer une station
pendant un direct : le DJ devra se reconnecter après un redémarrage du daemon.

## Accès et transport

- Les identifiants DJ sont vérifiés dans StationD contre les empreintes Argon2.
- Les tickets opaques durent au maximum 12 heures, sont bornés à 256 par station
  et sont invalidés si le compte DJ est désactivé ou son mot de passe change.
- Le gateway lie chaque ticket à la session Webmin et à la station ; les tickets
  et les mots de passe ne sont ni exposés au JavaScript ni placés dans une URL.
- La lecture et l’écoute exigent toujours la permission de lecture de station.
  Les commandes exigent aussi origine et CSRF valides, puis le ticket du DJ.
  Même un viewer peut gérer **son propre** flux avec des identifiants DJ valides.
- Les demandes de prise/fin d’antenne sont auditées sans secrets. Les tentatives
  d’identification et les calculs Argon2 sont limités.
- Liquidsoap encode la préécoute en MP3 128 kbit/s sur le même port harbor,
  au mount réservé `/__stationd_preview`. Cette sortie exige une connexion
  loopback et le secret du pont StationD : elle n’est pas un mount Icecast public.
- StationD relaie cet audio via `LiveService.DjPreview` (gRPC), puis le gateway
  le sert en HTTPS sur la même origine que la page. Le navigateur n’accède jamais
  au harbor privé. Les limites sont huit écoutes côté station et seize côté
  gateway, trente minutes par lecture ; cliquer de nouveau pour reprendre.
- La révocation de session est contrôlée pendant l’écoute. Une déconnexion ou
  l’arrivée d’un autre DJ termine le retour pour éviter d’écouter un autre flux.

## Validation

```sh
node tests/webmin-dj.js
node tests/webmin-language.js
cargo test --locked --lib live::tests
cargo test --locked --lib webmin_dj
cargo test --locked --lib dj_preparation_script_in_liquidsoap -- --ignored
python3 tests/liquidsoap_dj_preview.py
cargo check --locked --workspace --bins
```

Le test audio utilise des ports et fichiers temporaires, un pont HTTP de test,
le générateur Rust de production et deux signaux distincts. Il vérifie la
préécoute privée, le refus d’écoute anonyme, l’absence de fuite vers l’antenne,
l’absence de saut en quittant la préparation, la bascule explicite et le retour
à la programmation. Il ne touche pas les services de la station en exploitation.
