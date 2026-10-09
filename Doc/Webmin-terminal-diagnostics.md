# Retour visuel du terminal Webmin

Le bandeau distingue trois dimensions indépendantes :

- Connexion : CONNECTING, CONNECTED, RECONNECTING (nouvelle ouverture), CONNECTION UNRESPONSIVE, DISCONNECTED. BUSY indique aussi un quota refusé à l'ouverture.
- Focus : FOCUSED ou TERMINAL NOT FOCUSED, avec invitation à cliquer dans le terminal. Le focus est suivi par les événements du champ clavier xterm et les touches réellement reçues (onKey), sans veto de document.hasFocus(). Un clic dans le terminal redonne explicitement le focus au champ : les gestionnaires s'exécutent en capture et vérifient de nouveau le focus après les actions par défaut du clic, sans le reprendre à un autre contrôle. L'invitation conserve sa place dans la page pour que le terminal ne bouge pas sous la souris pendant le clic. Une perte de focus de la fenêtre, du champ ou un onglet masqué retire FOCUSED ; les réponses automatiques de xterm (onData) ne prouvent pas le focus du clavier.
- Application : READY si le signal de vie TUI et le transport sont récents ; BUSY si un signal précédemment reçu est retardé ; UNKNOWN si le diagnostic n'est pas disponible.

Le bandeau affiche INPUT DELAYED si la transmission au PTY tarde. L’envoi normal d’une entrée n’affiche aucune indication. Les lignes de debug (transmission PTY et compteur d'événements clavier) sont retirées de l'interface ; les signaux internes sont conservés pour alimenter les états du bandeau. Un accusé input_ack portant le numéro de l'entrée n'est émis qu'après l'écriture complète dans le PTY. Il ne prouve pas que StationD a exécuté une commande.

La TUI lancée par Webmin reçoit STATIOND_WEBMIN_DIAGNOSTICS=1. Sa boucle d'événements émet une séquence OSC 777 (stationd;compteur) environ chaque seconde et après le retour du gestionnaire d'un événement clavier. xterm consomme cette séquence sans modifier l'écran. Le compteur ne contient aucune touche ni donnée saisie. Il compte les événements clavier reçus même lorsqu'aucune action n'est associée à la touche ; ce compteur n'est pas une correspondance un-à-un avec les trames d'entrée (collage, séquences terminal, etc.), ni un accusé de réussite gRPC.

Les sondes WebSocket sont émises chaque seconde, sans prolonger le délai d'inactivité utilisateur. Après 3 secondes, une entrée sans accusé PTY est signalée. Après 5 secondes sans signal TUI, BUSY indique un retard, sans conclure à un blocage : un ralentissement du chemin de sortie peut aussi le provoquer. Une ancienne TUI sans télémétrie reste UNKNOWN. Le transport devient non réactif après 6 secondes sans réception et la connexion est fermée après 10 secondes. Les sorties PTY continuent de respecter le contrôle de flux existant.

Une nouvelle ouverture crée une nouvelle session ; aucune entrée ni commande en attente n'est rejouée. Les deux binaires stationd et stationd-tui doivent être mis à jour ensemble. La télémétrie est désactivée dans une TUI ordinaire.

Validation : node tests/webmin-console.js ; python3 tests/webmin-tui-diagnostics.py ; cargo test -p stationd webmin_console --lib. Le test PTY réel vérifie aussi l'arrêt et la reprise de la boucle via SIGSTOP/SIGCONT.
