# NZXT Control Linux

Application Linux native et open source pour monitorer et contrôler du matériel NZXT.

> État : fondation implémentée (EP-001), en revue. Le daemon détecte les deux appareils, expose un socket Unix typé et l'interface GPUI affiche l'état réel. Aucune écriture matérielle n'est encore implémentée. Nom technique provisoire. Ce projet n'est ni affilié à NZXT, ni approuvé par NZXT.

## Empreinte mesurée

| Mesure | Constaté | Budget PRD v1.2 |
|---|---|---|
| Démarrage à froid, médiane sur 5 lancements | 327 ms | ≤ 700 ms |
| `RssAnon` au repos, mémoire allouée par le processus | 81,3 MiB | ≤ 110 MiB |
| `VmRSS` total au repos | 253,2 MiB | ≤ 320 MiB |
| CPU au repos, moyenne sur 5 min | 1,10 % | ≤ 1,5 % |

Le `VmRSS` total est dominé par les mappings du pilote graphique et du compilateur de shaders liés par GPUI, partagés avec les autres clients GPU de la machine : une fenêtre GPUI vide en occupe 288,1 MiB. C'est un plafond de non-régression, pas une cible d'optimisation. La métrique que le projet pilote est `RssAnon`. Décomposition complète dans [`docs/ep-001-evidence.md`](./docs/ep-001-evidence.md).

## Intention

Construire une application desktop légère avec Rust et GPUI, centrée uniquement sur les tâches matérielles utiles :

- monitoring CPU, GPU, RAM et Kraken ;
- contrôle de la pompe, du ventilateur et des courbes thermiques ;
- contrôle du RGB par canal ;
- configuration et rendu du LCD Kraken.

Le produit reprend la densité opérationnelle et la sobriété visuelle de NZXT CAM, avec une identité, des composants et des assets originaux.

## Principes

- Interface GPUI native, sans HTML, JavaScript, WebView ou moteur de navigateur.
- Fonctionnement local, sans compte, cloud, télémétrie ou service réseau.
- Linux `hwmon` prioritaire pour le chemin thermique.
- Accès HID/USB direct limité aux capacités RGB et LCD validées.
- Un daemon utilisateur unique possède les écritures matérielles.
- Aucun accès spéculatif à un modèle ou firmware non validé.
- Le GUI et le daemon ne tournent pas en root.

## Cible initiale

Environnement de développement vérifié :

| Élément | Valeur |
|---|---|
| Distribution | Fedora 44 |
| Kernel | `7.1.6-201.fc44.x86_64` |
| Toolchain de build | Rust 1.97.1, édition 2024 (`rust-toolchain.toml`) |
| Rust minimum supporté | 1.90, vérifié par compilation |
| Kraken | `1e71:300e` NZXT Kraken Base, `bcdDevice` 0200 |
| RGB | `1e71:2021` NZXT RGB Controller, `bcdDevice` 0105 |
| Driver thermique | `kraken2023` sur l'interface HID 1 |

Le driver expose la température liquide, deux canaux RPM/PWM et 40 points de courbe par canal. Le Kraken expose en plus une interface 0 de classe `0xff` sans driver noyau : c'est le candidat pour le transport LCD, à valider par US-016. Les capacités RGB et LCD restent bloquées jusqu'à validation de leur protocole sur le matériel réel.

Les capacités observées sont enregistrées dans [`docs/capability-record.json`](./docs/capability-record.json), numéros de série expurgés.

## Architecture

```text
crates/
├── app             GPUI, écrans et contrôles natifs
├── daemon          propriété des appareils, commandes et IPC Unix
├── core            capacités, profils, protocole IPC et diagnostics
└── hardware-linux  découverte sysfs, hwmon et permissions (lecture seule)
```

Le crate `lcd-renderer` (`DisplayPreset` et framebuffer exact) arrivera avec EP-004, quand le transport LCD sera prouvé sur `1e71:300e`. Le module de télémétrie de `core` arrivera avec EP-002. Aucun des deux n'est créé à vide : un module que rien n'appelle n'est pas une fondation.

Le daemon reste indépendant de la fenêtre afin de sérialiser les commandes, détecter les writers concurrents et restaurer un profil compatible après reconnexion ou reprise de veille.

## Utilisation

```bash
cargo build --release

# Enregistrer les capacités réelles de la machine (lecture seule, sans socket).
./target/release/nzxt-controld --capabilities > docs/capability-record.json

# Démarrer le service, puis l'interface.
./target/release/nzxt-controld &
./target/release/nzxt-control
```

Le daemon refuse de démarrer en root. Sans règle udev, les attributs `hwmon` restent en lecture seule et l'application le signale explicitement au lieu d'échouer silencieusement.

Variables d'environnement lues :

| Variable | Rôle |
|---|---|
| `NZXT_CONTROL_SOCKET` | Chemin du socket Unix |
| `NZXT_CONTROL_CONFIG_DIR` | Répertoire de configuration |
| `NZXT_CONTROL_RUNTIME_DIR` | Répertoire des verrous et du socket |
| `NZXT_SYSFS_ROOT` | Racine sysfs, pour les tests sur arborescence factice |
| `NZXT_STARTUP_TRACE` | Affiche le délai jusqu'à la première frame |
| `NZXT_EXIT_AFTER_FIRST_FRAME` | Quitte après la première frame, pour mesurer le démarrage |

## Validation

```bash
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## Périmètre v1

L'application contient quatre destinations principales :

1. Monitoring
2. Cooling
3. Lighting
4. LCD

Sont explicitement hors périmètre : Web Integrations, cloud, comptes, mises à jour firmware, API distante, périphériques NZXT non validés et contrôle de matériel non-NZXT.

## Plan produit

- [PRD complet](./tasks/prd-native-nzxt-hardware-control.md)
- [Suivi des epics et stories](./tasks/prd-native-nzxt-hardware-control-status.json)

La première story valide GPUI sous Wayland et X11 avec un écran LCD représentatif, puis mesure le démarrage, la mémoire, le CPU, le focus clavier et le scaling avant d'étendre l'interface.

## Recherche

L'[exploration initiale de GitHub NZXT et de l'écosystème Linux](./nzxt-linux-github-research.md) est conservée comme historique de décision. Sa recommandation initiale de runtime Web Integrations a été remplacée par le PRD hardware-only.

## Licence

[GPL-3.0-or-later](./LICENSE). Aucun code externe n'est importé avant vérification de sa licence et de sa compatibilité.

L'inventaire des dépendances et l'audit de compatibilité restent dus avant toute distribution de paquet (US-020).
