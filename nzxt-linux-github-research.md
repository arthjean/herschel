# Recherche initiale : GitHub NZXT et écosystème Linux

> État de la recherche : 30 juillet 2026. Sources limitées à l'organisation GitHub officielle `NZXTCorp`, ses dépôts, son code, ses tags et ses releases.
>
> Note de décision : ce document conserve l'exploration et sa recommandation initiale comme historique. La direction actuelle est une application GPUI hardware-only, définie dans le [PRD](./tasks/prd-native-nzxt-hardware-control.md). Les Web Integrations sont hors périmètre.

## Verdict

L'organisation publique de NZXT ne contient ni le code de NZXT CAM, ni driver de périphérique NZXT, ni spécification USB/HID, ni tables VID/PID, ni API publique pour piloter ventilateurs, pompes, RGB, écrans LCD ou firmware. Elle ne publie pas non plus de contrat d'authentification ou d'API cloud NZXT utilisable pour un clone.

La découverte réellement intéressante est le trio **Web Integrations**. Il documente une interface JavaScript injectée par CAM dans un navigateur Chromium et le pipeline qui affiche une page web sur l'écran d'un Kraken. Ce n'est pas une API de contrôle matériel, mais c'est un bon contrat de compatibilité : un CAM Linux pourrait réimplémenter `window.nzxt.v1`, puis faire tourner les intégrations Kraken existantes sans les modifier.

Les quelques forks HID, USB et monitoring présents dans l'organisation sont des bibliothèques génériques anciennes. Ils donnent des indices sur les briques historiques de CAM, pas sur le protocole des appareils NZXT.

## Périmètre vérifié

L'[API GitHub de l'organisation](https://api.github.com/orgs/NZXTCorp) annonce 59 dépôts publics. L'[inventaire officiel](https://api.github.com/orgs/NZXTCorp/repos?per_page=100&type=public) contient seulement 7 dépôts non-forks et 52 forks. Une recherche de dépôts sur `cam`, `kraken` et `hardware` ne remonte, côté CAM, que `web-integrations-docs`, `web-integrations-types` et `web-integrations-examples`.

Les 7 dépôts originaux ont été inspectés, ainsi que les forks dont le nom ou le contenu pouvait toucher HID, USB, GPU ou écrans. Aucun code NZXT spécifique de contrôle matériel n'a émergé. C'est une conclusion sur ce qui est publié dans cette organisation, pas la preuve que les protocoles n'existent pas dans des dépôts privés ou dans le binaire CAM.

## Ce qui est exploitable

### 1. `web-integrations-types` : le contrat de monitoring à reproduire

Le fichier [`v1/index.d.ts`](https://github.com/NZXTCorp/web-integrations-types/blob/dc41ac2fc12e2c47320d253f1130478d184f162c/v1/index.d.ts) définit :

- `window.nzxt.v1.onMonitoringDataUpdate(data)`, appelé une fois par seconde par CAM ;
- les attributs d'affichage `width`, `height`, `shape` (`circle` ou `square`) et `targetFps` ;
- un objet `MonitoringData` composé de `cpus`, `gpus`, `ram` et `kraken` ;
- pour CPU et GPU : charge, températures, fréquences, vitesse de ventilateur et puissance ;
- pour la RAM : taille, usage, modules et fréquences ;
- pour le Kraken : uniquement `liquidTemperature`.

Les unités et conventions sont suffisamment précises pour servir de schéma public : charge entre 0 et 1, températures en Celsius, fréquences en MHz, ventilateurs en RPM, puissance en watts et mémoire en MiB. Les valeurs numériques peuvent être `undefined`.

**Intérêt pour le projet : élevé, mais seulement au niveau compatibilité UI.** Une implémentation Linux peut collecter ses propres métriques, les normaliser selon ces types et injecter le même objet dans un renderer web. Rien dans ce dépôt ne permet d'ouvrir un Kraken, de lire ses rapports HID, de régler une courbe ou d'envoyer une frame au LCD.

Le dépôt est petit, en TypeScript, et sous [licence MIT](https://github.com/NZXTCorp/web-integrations-types/blob/dc41ac2fc12e2c47320d253f1130478d184f162c/LICENSE). Son dernier commit sur `main` date du [17 septembre 2024](https://github.com/NZXTCorp/web-integrations-types/commit/dc41ac2fc12e2c47320d253f1130478d184f162c). Le tag `v0.4.1` pointe sur ce commit, mais la dernière release GitHub publiée est [`v0.4.0`](https://github.com/NZXTCorp/web-integrations-types/releases/tag/v0.4.0), datée du 31 août 2023.

Il existe une incohérence à ne pas propager : le [README](https://github.com/NZXTCorp/web-integrations-types/blob/dc41ac2fc12e2c47320d253f1130478d184f162c/README.md) montre `npm install @nzxt/web-integrations`, tandis que le [`package.json`](https://github.com/NZXTCorp/web-integrations-types/blob/dc41ac2fc12e2c47320d253f1130478d184f162c/package.json) et la documentation utilisent `@nzxt/web-integrations-types`.

### 2. `web-integrations-docs` : l'architecture du mode LCD

La [documentation de développement](https://github.com/NZXTCorp/web-integrations-docs/blob/1f769ba5a75c65c656aeb0946ab6ca8f509075ba/pages/docs/development.md) révèle le comportement de CAM :

1. CAM ouvre deux navigateurs Chromium : un navigateur de configuration visible et un « Kraken Browser » caché.
2. Le Kraken Browser charge la même URL avec `?kraken=1`.
3. Les deux contextes partagent l'état de session du même origin, notamment `localStorage` et les cookies.
4. CAM injecte dans le Kraken Browser la géométrie de l'écran, le FPS cible et le callback de monitoring.
5. CAM rend le contenu du Kraken Browser, puis l'envoie à l'écran du Kraken.

La documentation indique que les données de monitoring sont disponibles à partir de CAM 4.50.0. Elle documente aussi deux URI schemes, `nzxt-cam://` et `nzxt-cam-beta://`, dont l'action `action/load-web-integration?url=...`. Ce sont des deep links locaux vers CAM, pas une API cloud ni un canal matériel.

La [FAQ](https://github.com/NZXTCorp/web-integrations-docs/blob/1f769ba5a75c65c656aeb0946ab6ca8f509075ba/pages/docs/faq.md) liste les familles prises en charge : Kraken Elite, Kraken Z et Kraken. La [page de soumission](https://github.com/NZXTCorp/web-integrations-docs/blob/1f769ba5a75c65c656aeb0946ab6ca8f509075ba/pages/docs/submissions.md) fournit les profils connus :

| Famille documentée | Résolution | Forme |
|---|---:|---|
| Kraken Z | 320 x 320 | circulaire |
| Kraken | 240 x 240 | carrée |
| Kraken Elite | 640 x 640 | circulaire |

**Intérêt pour le projet : élevé comme spécification fonctionnelle du sous-système LCD.** Le driver USB et le format de transfert des frames restent totalement absents.

Le dépôt est en TypeScript/Next.js. Son dernier commit date du [25 janvier 2024](https://github.com/NZXTCorp/web-integrations-docs/commit/1f769ba5a75c65c656aeb0946ab6ca8f509075ba). GitHub ne détecte aucune licence et la racine du dépôt ne contient pas de fichier `LICENSE`. Il faut donc traiter son code comme une référence de comportement, pas comme du code réutilisable, tant que NZXT n'a pas clarifié la licence.

### 3. `web-integrations-examples` : fixtures de compatibilité LCD

Le [README](https://github.com/NZXTCorp/web-integrations-examples/blob/0c3888a99005e0e2d1195aeed97a64e44124ec12/README.md) fournit quatre exemples : Google Photos, Spotify, Unsplash et YouTube. Ils montrent comment distinguer le renderer Kraken, partager l'état entre les deux navigateurs et adapter l'affichage.

**Intérêt pour le projet : bon jeu de fixtures end-to-end.** Un test de compatibilité pourrait charger ces pages dans le renderer Linux et vérifier la géométrie, le query parameter, les sessions partagées et l'injection `window.nzxt.v1`.

Le dépôt est sous [licence MIT](https://github.com/NZXTCorp/web-integrations-examples/blob/0c3888a99005e0e2d1195aeed97a64e44124ec12/LICENSE). Il paraît actif au niveau de l'organisation, avec des merges jusqu'au [23 juillet 2026](https://github.com/NZXTCorp/web-integrations-examples/commit/0c3888a99005e0e2d1195aeed97a64e44124ec12), mais les changements récents concernent la liste [`community.md`](https://github.com/NZXTCorp/web-integrations-examples/blob/0c3888a99005e0e2d1195aeed97a64e44124ec12/community.md). Les fichiers des quatre exemples officiels n'ont pas changé depuis leur commit initial du [12 avril 2023](https://github.com/NZXTCorp/web-integrations-examples/commit/fcdf05085c2155ca22fd2341faee1cd3acb7d501).

Les flux OAuth présents sont exclusivement ceux de Google et Spotify. Par exemple, le code Spotify appelle `accounts.spotify.com` et `api.spotify.com`, et le code Google appelle `oauth2.googleapis.com`. Ils ne documentent aucun login NZXT, token CAM ou endpoint cloud NZXT. Ce code d'exemple ancien doit rester une fixture, pas une dépendance de production.

## Briques périphériques intéressantes, mais non spécifiques à NZXT

### `hidapi-rs`

Le fork [`NZXTCorp/hidapi-rs`](https://github.com/NZXTCorp/hidapi-rs) est un wrapper Rust générique de HIDAPI. Son [`Cargo.toml`](https://github.com/NZXTCorp/hidapi-rs/blob/7cdbb94cd8f14ab1240ba392318c02cfd7d9b250/Cargo.toml) expose des backends Linux `libusb` et `hidraw`, et son [README](https://github.com/NZXTCorp/hidapi-rs/blob/7cdbb94cd8f14ab1240ba392318c02cfd7d9b250/README.md) montre seulement comment ouvrir un VID/PID arbitraire et lire ou écrire des octets.

Il ne contient ni VID/PID NZXT ni rapports HID ou commandes propres aux Kraken, Hue, Grid ou Smart Device. Le dernier commit du fork sur sa branche par défaut date du 11 mai 2019, il annonce la version 0.5.2 et il pointe explicitement vers le projet amont. Licence MIT.

**Décision :** indice historique confirmant que HID est une voie plausible, mais mauvais socle à reprendre tel quel. Choisir une bibliothèque Linux maintenue une fois le protocole réel connu.

### `periscope-usbid`

Le fork [`periscope-usbid`](https://github.com/NZXTCorp/periscope-usbid) est une API Python pour parcourir la topologie USB Linux dans `/sys/bus/usb/devices`. Son [README](https://github.com/NZXTCorp/periscope-usbid/blob/2a54e0e41024e068fb9d5b1553b2ab19d8d7a039/README.rst) couvre bus, ports, interfaces et TTY, pas les transferts HID. Le dernier commit date du 2 février 2016. Sa [`setup.py`](https://github.com/NZXTCorp/periscope-usbid/blob/2a54e0e41024e068fb9d5b1553b2ab19d8d7a039/setup.py) annonce une licence BSD simplifiée.

**Décision :** éventuellement utile comme référence d'énumération sysfs, sans valeur pour le protocole appareil.

### `nvapi-rs` et `rust-edid`

[`nvapi-rs`](https://github.com/NZXTCorp/nvapi-rs/blob/c8db27108f97cac5d662e0935a9346759279a819/README.md) fournit du monitoring NVIDIA via NVAPI, explicitement sous Windows. C'est incompatible avec la cible Linux et son dernier commit remonte à mars 2018.

[`rust-edid`](https://github.com/NZXTCorp/rust-edid/blob/d044e9a14d07b51bb0d7d9f52070a07df697f208/README.md) est un parseur EDID générique sous MIT. EDID décrit les écrans vidéo conventionnels, pas le transport USB d'un LCD Kraken. Aucun de ces forks n'est une brique prioritaire.

### `enunion` et `km-wrappers`

[`enunion`](https://github.com/NZXTCorp/enunion/blob/14affdad80483bb4eae6d37bffd4173bec35f6ff/README.md) convertit des enums Rust en discriminated unions TypeScript via N-API. Il est relativement récent, sous MIT ou Apache 2.0, et suggère que NZXT utilise une frontière Rust/Node. Cela peut inspirer une architecture, mais ne justifie pas d'introduire Node ou N-API dans un MVP.

[`km-wrappers`](https://github.com/NZXTCorp/km-wrappers/blob/e2081135047cf847fb8f4df4b4a43708489a2f8e/README.md) ne contient que des wrappers Rust de kernel-mode Windows. Il est hors cible Linux et ne révèle aucun driver NZXT.

Les deux autres dépôts originaux, [`crucible`](https://github.com/NZXTCorp/crucible/blob/74a0287fe63add7ce23dec51f2a7e9d28ec301e0/README.md) et `obs-studio-non-fork`, concernent l'ancien produit de capture Forge et OBS. Ils sont sans rapport avec CAM ou les périphériques NZXT.

## Couverture des fonctions d'un CAM Linux

| Fonction visée | Ce que fournit l'organisation NZXT | Couverture |
|---|---|---|
| Monitoring CPU/GPU/RAM | Schéma public et fréquence d'injection, aucune collecte Linux | Partielle |
| Température liquide Kraken | Champ `kraken.liquidTemperature`, aucune commande de lecture | Partielle |
| Contrôle ventilateurs | Aucun protocole, aucune courbe, aucun mapping de canal | Nulle |
| Contrôle pompe | Aucun protocole ni consigne | Nulle |
| RGB | Aucun protocole, effet ou topologie LED | Nulle |
| LCD Kraken | Contrat navigateur, résolutions, forme et FPS, aucun transport de frame | Partielle |
| Détection USB | Deux bibliothèques génériques anciennes, aucun identifiant NZXT | Faible |
| Firmware | Aucun format, endpoint ou mécanisme de mise à jour | Nulle |
| Compte et cloud NZXT | Aucun OAuth, endpoint, schéma ou client NZXT | Nulle |
| Compatibilité Web Integrations | Types, comportement et exemples officiels | Bonne |

## Conséquences pratiques

Le GitHub officiel fournit une **spécification de compatibilité en haut de pile**, pas le bas de pile matériel. Le meilleur usage est :

1. adopter `window.nzxt.v1` comme interface publique du renderer LCD Linux ;
2. convertir les métriques Linux vers le schéma officiel ;
3. utiliser les exemples MIT comme tests de compatibilité ;
4. maintenir une couche driver séparée par famille et firmware, car rien dans l'organisation ne garantit que Kraken, RGB et contrôleurs de ventilateurs partagent un protocole ;
5. chercher les protocoles, identifiants USB et séquences de commandes hors de l'organisation NZXT, ou les établir par reverse engineering propre avec des appareils réels.

Ne pas appeler le projet ou son API « NZXT CAM » comme si c'était un produit officiel. Les licences MIT des deux dépôts autorisent la réutilisation de leur code sous leurs conditions, mais elles ne constituent pas une autorisation d'utiliser la marque NZXT. Pour les dépôts forkés, la présence dans l'organisation NZXT ne signifie ni maintenance actuelle ni support produit.

## Les dépôts hors NZXTCorp qui changent la stratégie

### `liquidctl/liquidctl` : le socle matériel le plus complet

[`liquidctl`](https://github.com/liquidctl/liquidctl) est le point de départ prioritaire. Le projet fournit des drivers Python et une CLI JSON pour de nombreuses générations NZXT : Kraken X, Z, 2023, 2024 Elite RGB et Plus, Grid+ V3, Smart Device, HUE 2, plusieurs RGB & Fan Controllers, H1 V2, alimentations E-series et, sur la branche de développement, Control Hub.

Les drivers [`kraken3.py`](https://github.com/liquidctl/liquidctl/blob/main/liquidctl/driver/kraken3.py) et [`smart_device.py`](https://github.com/liquidctl/liquidctl/blob/main/liquidctl/driver/smart_device.py) contiennent les VID/PID, formats de rapports HID, lectures de températures et RPM, commandes de courbes, effets RGB et transferts LCD issus du reverse engineering. Le [guide Kraken X3/Z3](https://github.com/liquidctl/liquidctl/blob/main/docs/kraken-x3-z3-guide.md) documente aussi les limites par modèle et firmware.

Le support reste partiel sur du matériel récent : anneaux lumineux non pilotés sur certains Kraken, modèle Elite 2023 marqué cassé dans une table de détection, GIF indisponible avec certains firmwares 2.x. Ce sont des frontières de MVP à tester sur le matériel exact, pas des détails à masquer derrière une abstraction générique.

Le code est sous GPL-3.0. Pour un projet personnel GPL, le chemin court consiste à contribuer à `liquidctl` ou à l'utiliser comme backend. Sa CLI JSON peut aussi servir de prototype avant toute réécriture.

### Kernel `hwmon` et `liquidtux` : préférer les interfaces Linux standard

[`liquidctl/liquidtux`](https://github.com/liquidctl/liquidtux) développe les drivers `hwmon` dont plusieurs sont déjà dans Linux : `nzxt-kraken2` depuis Linux 5.13 et `nzxt-kraken3` depuis Linux 6.9, avec les Kraken 2023 ajoutés en 6.10. Le driver mainline [`nzxt-smart2`](https://github.com/torvalds/linux/blob/master/drivers/hwmon/nzxt-smart2.c) couvre plusieurs Smart Device V2 et RGB & Fan Controllers.

Cela permet de lire et régler températures, RPM et PWM par `/sys/class/hwmon`, sans réimplémenter le protocole dans l'application. Le matériel 2024 le plus récent n'est pas encore entièrement couvert par les tables du kernel mainline au 30 juillet 2026, donc un fallback direct `liquidctl` reste nécessaire.

### CoolerControl : le « CAM Linux » existe déjà en grande partie

[`coolercontrol/coolercontrol`](https://gitlab.com/coolercontrol/coolercontrol) fournit déjà un daemon systemd, une Web UI, une application desktop, l'auto-détection `hwmon`/`liquidctl`/GPU, des profils de ventilateurs, des modes, alertes, RGB et LCD. Il expose une [REST API complète](https://docs.coolercontrol.org/automation/scripting.html) sur le port local 11987 et une API gRPC principalement destinée aux plugins.

La conséquence stratégique est nette : refaire monitoring, profils, persistance, reprise après veille, permissions et contrôle GPU serait une duplication coûteuse. Le projet différenciant serait plutôt un renderer Kraken compatible Web Integrations, branché sur CoolerControl par REST ou livré comme contribution/plugin. CoolerControl est sous GPLv3+.

### OpenRGB : utile si le périmètre RGB dépasse NZXT

[`OpenRGB`](https://gitlab.com/CalcProgrammer1/OpenRGB) prend en charge de nombreux périphériques NZXT et publie la [matrice actuelle des VID/PID](https://openrgb.org/devices.html?search=nzxt), dont HUE 2, Kraken X3, plusieurs RGB & Fan Controllers et le Kraken 2024 Elite RGB. C'est une bonne référence C++ pour les effets et la topologie LED, mais il ne remplace ni le contrôle thermique ni le LCD. Licence GPL-2.0.

### `AIOLCDUnchained` : la piste permissive pour le streaming de frames

[`brokenmass/AIOLCDUnchained`](https://github.com/brokenmass/AIOLCDUnchained) est un prototype MIT qui cible Kraken Z3 (`1e71:3008`), Elite 2023 (`1e71:300c`) et Elite 2024 (`1e71:3012`). Son [`driver.py`](https://github.com/brokenmass/AIOLCDUnchained/blob/main/driver.py) documente les transferts HID et bulk USB, les buckets RGBA, ainsi qu'un mode Q565 vers une mémoire rapide permettant d'envoyer des frames générées en temps réel.

Le transport actuel utilise WinUSB et les binaires documentés sont Windows. L'intérêt n'est donc pas l'application telle quelle, mais le protocole et l'encodeur Rust Q565 à porter vers libusb sur Linux. Comme le dernier push date de novembre 2024 et que le projet reste expérimental, chaque commande doit être validée sur le modèle et le firmware ciblés.

### `KrakenZPlayground` : précédent Linux, mais ancien et étroit

[`ProtozeFOSS/KrakenZPlayground`](https://github.com/ProtozeFOSS/KrakenZPlayground) communique directement en USB avec les Z53/Z63/Z73 et sait afficher en temps réel animations, images et vues QML sous Linux. Il fournit une preuve que le pipeline dynamique fonctionne, mais sa dernière release date d'avril 2022, son périmètre est limité au PID `1e71:3008`, et sa dépendance à Qt5/QML n'est pas un choix à reprendre par défaut. Licence GPL-3.0.

## Direction recommandée

Ne pas démarrer par un clone généraliste de CAM. Démarrer par un **runtime Web Integrations pour Kraken sous Linux** :

1. supporter un seul modèle réellement possédé et identifié par son VID/PID et son firmware ;
2. laisser `hwmon` et CoolerControl gérer capteurs, pompes, ventilateurs et sécurité thermique ;
3. réserver l'accès HID/libusb direct au LCD, dans un daemon unique afin d'éviter les accès concurrents ;
4. charger une Web Integration dans un renderer Chromium isolé avec `?kraken=1` ;
5. injecter `window.nzxt.v1` et les métriques normalisées depuis l'API CoolerControl ;
6. capturer le framebuffer à la résolution du device, l'encoder, puis le transmettre au Kraken ;
7. utiliser les exemples MIT officiels comme tests de compatibilité.

La première expérience à faire avant de choisir le stack UI est un spike sans interface : page locale animée, 640 x 640 ou 320 x 320 selon le matériel, capture de frames et envoi stable pendant trente minutes. Le débit réel, les erreurs USB, le coût CPU et le comportement après veille décideront si la compatibilité Web Integrations est viable.

Si le renderer accepte des URL arbitraires, il doit rester sans bridge privilégié vers le daemon ou le système de fichiers. La page distante est du code non fiable : processus isolé, permissions minimales, stockage par origin et liste explicite des seules données injectées.

Enfin, éviter le copier-coller entre projets GPL-2.0, GPL-3.0 et MIT. Pour un projet publié, choisir la licence avant d'importer du code. Pour un MVP personnel, utiliser les programmes existants comme processus séparés réduit immédiatement la surface à écrire.
