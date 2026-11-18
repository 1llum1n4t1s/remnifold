[English](README.md) | [日本語](README.ja.md) | [简体中文](README.zh-CN.md) | [繁體中文](README.zh-TW.md) | [한국어](README.ko.md) | [Español](README.es.md) | [Français](README.fr.md) | [Deutsch](README.de.md) | [Português (Brasil)](README.pt-BR.md)

# Remnifold

Un serveur de mémoire Cloudflare auto-hébergé, avec accès MCP distant, et une application locale autonome pour retrouver l'historique d'implémentation utile à travers les dossiers de projet dans Claude Code et Codex.

L'application enregistre le travail terminé dans une collection partagée et conserve son dossier de projet comme provenance. Parcourez les enregistrements par thèmes de contenu, ou interrogez un sujet comme « extension Chrome » : le rappel recherche dans plusieurs espaces mémoire en une seule requête serveur, injecte des index concis et permet à l'assistant d'ouvrir via MCP un enregistrement source pertinent avant de réutiliser l'implémentation.

## Prérequis

Les clients MCP distants se connectent au serveur déployé sans rien installer en local. L'application locale est un exécutable natif unique pour votre système d'exploitation et votre processeur ; son serveur MCP, ses hooks, la configuration de la connexion, l'import de l'historique et le programme d'installation n'exigent ni Node.js, ni Codex, ni système de plugins de Claude Code. Les utilisateurs ordinaires l'installent avec le programme d'installation Setup, qui ne demande aucune ligne de commande et se met à jour tout seul. La voie stricte `remnifold install`, destinée aux utilisateurs avancés, exige un GitHub CLI natif authentifié. L'enregistrement auprès de Claude Code utilise le CLI de Claude Code lorsqu'il est disponible ; sinon, la commande exacte à exécuter est affichée.

L'installation et la maintenance du serveur utilisent le même exécutable natif et votre propre compte Cloudflare avec Workers, D1, Vectorize et Workers AI activés. Ni Node.js, ni gestionnaire de paquets, ni Wrangler, ni Rust ne sont nécessaires pour utiliser le paquet de la plateforme.

## Connecter un client MCP distant

Une fois le serveur mis à jour et vérifié comme décrit plus bas, utilisez `https://your-worker.example.workers.dev/mcp` comme point de terminaison MCP Streamable HTTP. Un client compatible peut s'authentifier avec la découverte OAuth 2.1, l'enregistrement dynamique public de client et PKCE S256. Sur la page d'autorisation du serveur, saisissez la clé d'API mémoire existante et approuvez le client nommé. Si la page de fin ne renvoie pas automatiquement vers le client, suivez son lien pour continuer. Les jetons OAuth ne sont valables que pour MCP ; les clients REST existants continuent d'utiliser la clé d'API. Les clients qui acceptent un identifiant Bearer configuré peuvent aussi utiliser directement la clé d'API mémoire. Gardez les clés et les jetons confidentiels.

[Connect apps de Grok Bot](https://docs.x.ai/grok-bot/overview) et [les connecteurs Grok](https://docs.x.ai/grok/connectors) offrent des voies de connexion pour les applications et les serveurs MCP personnalisés. Choisissez OAuth lorsque le client le prend en charge ; la disponibilité de la saisie manuelle d'une clé Bearer dans Grok Bot n'a pas été vérifiée.

Pour ChatGPT, suivez la [configuration de plugin privé](https://developers.openai.com/plugins/build/plugins) : choisissez **+ Add custom MCP server**, saisissez l'URL `/mcp` et l'authentification, puis choisissez **Create as a plugin**. Cette connexion privée n'exige pas de soumission à l'annuaire public. [ChatGPT dots](https://learn.chatgpt.com/docs/dots/computers-and-apps) utilise les plugins pris en charge, installés et activés ; vérifiez dans votre compte que ce plugin apparaît et fonctionne dans dots. La saisie directe d'une URL MCP arbitraire dans dots n'a pas été vérifiée.

Le serveur demande à l'IA de rechercher le contexte pertinent, d'ouvrir les enregistrements complets si nécessaire et de sauvegarder les résultats durables. L'utilisation des outils est décidée par le client et le modèle : les recherches ou sauvegardes automatiques ne sont donc pas garanties. Demandez explicitement à l'IA de rechercher ou de sauvegarder quand c'est nécessaire. Cette intégration distante ne remplace ni le serveur stdio local ni les hooks ; évitez d'enregistrer un serveur distant en double dans le même client.

## Installer l'application locale

### Programme d'installation Setup (recommandé)

Sous Windows x64, téléchargez [`RemnifoldApp-win-Setup.exe`](https://remnifold-updates.kagayoi.com/velopack/RemnifoldApp-win-Setup.exe) (signé par l'éditeur, sans droits d'administrateur) et exécutez-le. Les paquets Setup pour macOS et Linux décrits plus bas sont publiés avec celui de Windows à partir de la première version postérieure à 1.0.32. Setup installe l'application pour votre utilisateur sous `%LOCALAPPDATA%\RemnifoldApp`, l'enregistre auprès de Claude Code et de Codex (uniquement ceux dont l'outil en ligne de commande est trouvé) et ouvre dans le terminal un guide de premier lancement qui explique l'étape suivante, la connexion à votre serveur (`remnifold setup`). Ses vérifications n'exigent aucun compte GitHub. Sous macOS (Apple Silicon), ouvrez `RemnifoldApp-osx-Setup.pkg` (signé et notarisé avec le Developer ID de l'éditeur). Il installe `Remnifold.app` (si vous choisissez « Installer pour moi seulement », il est placé dans `~/Applications` ; une installation pour tous les utilisateurs va dans `/Applications`, où l'application d'une mise à jour peut demander un mot de passe administrateur) et lance l'application une fois, ce qui l'enregistre auprès de Claude Code et de Codex. Si l'outil en ligne de commande d'un client est installé plus tard, le prochain démarrage de Remnifold l'enregistre. Exécutez `"/Applications/Remnifold.app/Contents/MacOS/remnifold" setup --base-url <URL>` (adaptez le chemin pour `~/Applications`) pour connecter votre serveur. Sous Linux (x64 ou ARM64), Setup est un unique AppImage (`RemnifoldApp-linux-x64.AppImage` ou `RemnifoldApp-linux-arm64.AppImage`) : placez-le là où vous voulez le conserver (par exemple `~/Applications`), rendez-le exécutable et lancez-le une fois sans argument, ce qui enregistre cet AppImage auprès de Claude Code et de Codex ; si vous déplacez le fichier plus tard, le nouvel emplacement est enregistré à son prochain démarrage. Exécutez `<path-to-AppImage> setup --base-url <URL>` pour connecter votre serveur. Les AppImage ne portent aucune signature de code ; sous Linux, l'application se fie donc au HTTPS du site de mises à jour et aux sommes de contrôle du flux de mises à jour.

Les mises à jour sont automatiques. Pendant qu'une session MCP est active, l'application consulte le flux de mises à jour au plus une fois par jour (une vérification échouée attend aussi un jour) et télécharge en arrière-plan une version plus récente sans rien interrompre. La nouvelle version est appliquée à votre prochaine ouverture de session (Windows : une entrée `Run` ; macOS : le LaunchAgent `~/Library/LaunchAgents/com.kagayoi.RemnifoldApp.update.plist` ; Linux : l'entrée de démarrage automatique `~/.config/autostart/com.kagayoi.RemnifoldApp.update.desktop`), et seulement si aucun autre processus Remnifold n'est en cours, de sorte qu'une session ouverte n'est jamais fermée. Avant l'application, l'application vérifie que chaque exécutable du paquet téléchargé est signé par l'éditeur (macOS : le Developer ID de l'éditeur ; Linux : pas de signature, mais le dossier de téléchargement `/var/tmp/velopack/RemnifoldApp` doit vous appartenir et ne pas être modifiable par d'autres utilisateurs). Pour désactiver ce comportement, définissez `"auto_update": false` dans `app-update.json` du dossier d'état de l'application (`%APPDATA%\Remnifold`, `~/Library/Application Support/Remnifold`, `$XDG_STATE_HOME/remnifold` (par défaut `~/.local/state/remnifold`) sous Linux, ou `<REMNIFOLD_HOME>/remnifold`) ; les diagnostics sont écrits dans `app-update.log` à côté. Si vous utilisez déjà l'installation stricte ci-dessous, Setup bascule les enregistrements Claude Code et Codex sur lui-même et utilise la même connexion et le même état de mémoire ; les fichiers de l'installation stricte sont conservés, supprimez donc vous-même `%LOCALAPPDATA%\Programs\Remnifold` lorsque vous n'en avez plus besoin. Si un enregistrement n'a pas pu se terminer pendant Setup (par exemple à cause d'un CLI client lent), il est retenté automatiquement à votre prochaine ouverture de session. Désinstallez depuis les Paramètres de Windows ; les enregistrements Claude Code et Codex et l'entrée d'ouverture de session sont supprimés avec lui, tandis que votre connexion et votre état de mémoire sont conservés. Pour revenir ensuite à l'installation stricte, enregistrez-la de nouveau avec ses propres `connect claude-code --apply` et `connect codex --apply`. Pour supprimer les enregistrements sans désinstaller, exécutez `remnifold connect claude-code --remove --apply` et `remnifold connect codex --remove --apply`.

macOS n'a pas de désinstalleur. Avant de mettre `Remnifold.app` à la corbeille, supprimez ses enregistrements et son entrée d'ouverture de session (sans `--apply`, la commande se contente d'afficher ce qu'elle supprimerait) :

```sh
"/Applications/Remnifold.app/Contents/MacOS/remnifold" uninstall-setup --apply
```

Votre connexion, votre configuration et votre état de mémoire dans `~/Library/Application Support/Remnifold` sont conservés ; le cache de mise à jour téléchargé se trouve dans `~/Library/Caches/velopack/RemnifoldApp`.

Linux n'a pas non plus de désinstalleur. Avant de supprimer l'AppImage, exécutez-le de la même manière (adaptez le chemin à l'endroit où vous le conservez) :

```sh
~/Applications/RemnifoldApp-linux-x64.AppImage uninstall-setup --apply
```

Votre connexion, votre configuration et votre état de mémoire dans `~/.config/remnifold` et `~/.local/state/remnifold` sont conservés ; le cache de mise à jour téléchargé `/var/tmp/velopack/RemnifoldApp` peut être supprimé.

### Installation stricte (avancée)

L'installation stricte est figée à sa dernière version, 1.0.32. Les nouvelles versions ne sont publiées que par le programme d'installation Setup ci-dessus (Windows, macOS, Linux x64 et ARM64) ; `remnifold install` ne peut donc installer que les versions jusqu'à 1.0.32, tandis que les versions déjà installées et `--verify-only` continuent de fonctionner. Pour passer aux nouvelles versions, exécutez le programme d'installation Setup, qui reprend les enregistrements auprès de Claude Code et de Codex.

Les paquets natifs publiés sont servis depuis [Remnifold updates](https://remnifold-updates.kagayoi.com). L'index des versions est `releases/<version>/index.json` ; son manifeste sélectionne une archive nommée d'après son empreinte pour chaque cible ci-dessous. Authentifiez la publication via GitHub avant d'exécuter un exécutable téléchargé ; une URL R2 ou un lien de statut de commit ne suffit pas, à lui seul, à établir son origine.

| Plateforme | Cible Rust |
| --- | --- |
| Windows x64 | `x86_64-pc-windows-msvc` |
| Mac Apple Silicon | `aarch64-apple-darwin` |
| Linux x64 | `x86_64-unknown-linux-gnu` |
| Linux ARM64 | `aarch64-unknown-linux-gnu` |

Mac Intel n'est pas pris en charge.

L'application s'installe par utilisateur, sans droits d'administrateur :

| Plateforme | Dossier de l'application | Exécutable fixe |
| --- | --- | --- |
| Windows | `%LOCALAPPDATA%\Programs\Remnifold` | `bin\remnifold.exe` |
| macOS | `~/Library/Application Support/Remnifold/app` | `bin/remnifold` |
| Linux | `$XDG_DATA_HOME/remnifold` (par défaut `~/.local/share/remnifold`) | `bin/remnifold` |

`remnifold install` vérifie le commit de la version, le journal CI réussi de l'éditeur, le manifeste R2, la taille et l'empreinte de l'archive ainsi que la source du paquet avant d'écrire quoi que ce soit. Il décompresse chaque version une seule fois dans `versions/<version>` (une version installée n'est jamais écrasée par des octets différents), conserve un reçu dans `receipts/` pour une vérification hors ligne ultérieure et remplace l'exécutable fixe. Les enregistrements pointent toujours vers l'exécutable fixe, si bien que les mises à jour n'exigent aucun nouvel enregistrement. Sous Windows, un serveur MCP en cours d'exécution conserve l'ancien exécutable jusqu'à son redémarrage ; le résultat indique alors `"restartRequired": true`.

### Première installation

Sous Windows, le workflow de publication dépose l'exécutable bootstrap signé sous une clé immuable et versionnée dans R2. Il existe à partir de la version 1.0.30 ; pour les versions antérieures, compilez le premier exécutable à partir de sources de confiance. Comme `install` n'accepte que les versions de l'installation stricte, indiquez une version comprise entre 1.0.30 et 1.0.32. Choisissez un répertoire privé de confiance existant, où un autre utilisateur ne peut pas remplacer le fichier téléchargé entre la vérification et l'exécution. Remplacez la version et le chemin entre guillemets ci-dessous, puis exécutez depuis PowerShell :

```powershell
$version = '<published-version>'
$bootstrap = 'C:\absolute\trusted\private\path\remnifold-bootstrap.exe'
Invoke-WebRequest -MaximumRedirection 0 -Uri "https://remnifold-updates.kagayoi.com/velopack/remnifold-bootstrap-$version-x86_64-pc-windows-msvc.exe" -OutFile $bootstrap
$signature = Get-AuthenticodeSignature -LiteralPath $bootstrap
if ($signature.Status -eq 'Valid' -and $signature.SignerCertificate.Subject -like 'CN=Open Source Developer Yuichiro Shinozaki*' -and $signature.TimeStamperCertificate) { & $bootstrap install --version $version --apply } else { Write-Error 'Signature verification failed. Do not run this file.' }
```

Le téléchargement ne doit pas être redirigé, et le fichier doit réussir la vérification Authenticode avant d'être exécuté (état `Valid`, signataire `CN=Open Source Developer Yuichiro Shinozaki`, horodatage RFC 3161 présent) ; en cas d'échec, ne l'exécutez pas. Cette signature prouve seulement qui a signé ; elle ne lie ni un produit, ni une version, ni l'acceptation d'une publication. `install` exige toujours un GitHub CLI natif authentifié et vérifie lui-même la publication (voir [DESIGN.md](DESIGN.md#release-installation)). Ne substituez pas un assistant R2 non vérifié et ne contournez pas une vérification échouée.

Sous macOS et Linux, obtenez et examinez le [bootstrap Bash](scripts/bootstrap-plugin.sh) de source de confiance, en l'enregistrant hors du répertoire de travail. Il exige un GitHub CLI natif, curl, tar, et `sha256sum` ou `shasum` :

```sh
bash ./bootstrap-plugin.sh --version '<published-version>' --apply
```

Sans `--apply`, il ne fait que vérifier la publication ; `--download-only` renvoie l'archive vérifiée sans l'exécuter.

### Mettre à jour, vérifier et enregistrer

```sh
remnifold install --version <published-version>                 # vérifier la publication uniquement
remnifold install --version <published-version> --apply         # installer ou mettre à jour
remnifold install --version <published-version> --verify-only   # revérifier hors ligne la version installée
remnifold connect claude-code                                    # afficher l'enregistrement prévu
remnifold connect claude-code --apply                            # enregistrer le serveur MCP et les hooks
remnifold connect claude-code --remove --apply                   # supprimer uniquement l'entrée MCP et les hooks de Remnifold
```

Utilisez l'exécutable fixe du dossier de l'application (ou ajoutez son dossier `bin` au `PATH`). `install` utilise un dossier de travail sous `%LOCALAPPDATA%\remnifold` sous Windows et `~/.cache/remnifold` ailleurs ; `--work-root <absolute-path>` sélectionne un autre dossier situé en dehors des dossiers de l'application, de configuration et du dépôt. Lorsque les journaux de témoin expirent, les nouvelles installations échouent de façon fermée ; le reçu conservé permet toujours `--verify-only`. L'accès GitHub aux métadonnées source de la release reste nécessaire.

`connect claude-code --apply` enregistre le serveur MCP avec `claude mcp add --scope user remnifold -- <fixed executable> mcp` (ou affiche cette commande lorsque le CLI de Claude Code n'est pas dans le `PATH` ; passez `--cli <path>` pour le choisir) et fusionne les hooks `SessionStart`, `UserPromptSubmit`, `Stop` (asynchrone) et `PreToolUse` dans le `settings.json` de Claude Code (`$CLAUDE_CONFIG_DIR` ou `~/.claude`). Seules les entrées de hook Remnifold existantes sont remplacées ; la première modification conserve `settings.json.remnifold-backup`. Un fichier de paramètres mal formé est laissé inchangé. `connect <tool> --remove` (aperçu d'abord ; `--apply` écrit) supprime uniquement l'entrée MCP et les hooks propres à Remnifold, conserve tous les autres paramètres et, avec `--executable <path>`, uniquement les entrées pointant vers cet exécutable. `connect codex --apply` enregistre le serveur MCP avec `codex mcp add` ; Codex n'offre aucun moyen pris en charge d'enregistrer des hooks en dehors des plugins, de sorte que le rappel et la capture automatiques passent par Claude Code. Démarrez une nouvelle session après l'enregistrement.

La capture `Stop` lit les transcriptions de Claude Code ainsi que les rollouts de Codex. Elle ne stocke que chaque demande humaine et le texte final de l'assistant, en ignorant la réflexion, les appels d'outils et leurs résultats, les copies de requête et les lignes des autres sessions. Une réponse faisant suite à une notification en arrière-plan est stockée comme un tour de continuation distinct, afin que les tours déjà sauvegardés conservent leurs clés de capture. Les enregistrements de Claude Code portent `sm_source: "claude-code"`. Pour éviter deux systèmes de mémoire, désactivez la mémoire automatique intégrée de Claude Code avec `"autoMemoryEnabled": false` ; Remnifold ne lit ni n'importe ces fichiers. Dans ce dépôt, le `.mcp.json` source (`cargo run`) prévaut sur l'enregistrement utilisateur ; ajoutez `"disabledMcpjsonServers": ["remnifold"]` au fichier non suivi `.claude/settings.local.json` pour utiliser l'application installée à la place.

Les anciennes commandes de plugin Codex `plugin-update` et `plugin-bootstrap` ont été retirées ; elles renvoient `USE_REMNIFOLD_INSTALL`. Installez plutôt l'application autonome ; l'ancien plugin peut ensuite être supprimé de Codex.

### Connexion à votre serveur

```sh
remnifold setup --base-url https://your-worker.example.workers.dev
```

Saisissez la clé d'API à l'invite masquée si elle n'est pas déjà fournie. Setup vérifie qu'une session authentifiée fonctionne avant d'écrire la connexion. Pour mettre à jour une connexion existante, utilisez `--force` ; les paramètres sans rapport sont préservés. `REMNIFOLD_API_URL` et `REMNIFOLD_API_KEY` peuvent fournir des remplacements au niveau du processus. Pour une configuration non interactive, `--api-key-stdin` lit la clé depuis l'entrée standard.

La connexion et l'état local résident dans le dossier de configuration propre à l'application, indépendant de Codex et de Claude Code :

| Plateforme | Connexion (`remnifold.json`) et état du serveur | État de capture et de rappel |
| --- | --- | --- |
| Windows | `%APPDATA%\Remnifold` | même dossier |
| macOS | `~/Library/Application Support/Remnifold` | même dossier |
| Linux | `$XDG_CONFIG_HOME/remnifold` (par défaut `~/.config/remnifold`) | `$XDG_STATE_HOME/remnifold` (par défaut `~/.local/state/remnifold`) |

`REMNIFOLD_HOME` remplace les deux (connexion dans `<REMNIFOLD_HOME>/remnifold.json`, état sous `<REMNIFOLD_HOME>/remnifold/`, application sous `<REMNIFOLD_HOME>/app`) ; `setup --config-dir <directory>` écrit la connexion ailleurs. Les versions antérieures conservaient ces fichiers dans le dossier personnel de Codex (`CODEX_HOME`, par défaut `~/.codex`). Lorsque le nouveau dossier ne contient aucune connexion, le premier lancement y copie une connexion héritée valide et lit l'état de capture et de rappel antérieur, en n'écrivant le nouvel état que dans le nouveau dossier. Les fichiers hérités ne sont jamais supprimés ; `server-setup` ne déplace son état hérité qu'avec `--apply`.

Exécutez l'outil MCP `whoAmI` pour vérifier la connectivité et la découverte des espaces.

Pour importer des sessions Codex existantes, prévisualisez un lot limité avec `remnifold import --limit 10`. Ajoutez `--apply` lorsque vous voulez téléverser les enregistrements assainis. L'aperçu n'écrit aucune mémoire distante ; les imports répétés réutilisent des identités de capture déterministes. `--codex-home` indique où lire les sessions Codex.

## Créer ou mettre à jour un serveur

Le paquet inclut le bundle du Worker et les migrations. Fournissez un jeton d'API Cloudflare via la variable d'environnement du processus `CLOUDFLARE_API_TOKEN` ou `--token-stdin`, puis prévisualisez les ressources existantes :

```sh
remnifold server-setup --account-id <Cloudflare-account-id>
```

Par défaut, la découverte est authentifiée et en lecture seule. Le jeton exige les autorisations de lecture du compte, Workers scripts, D1 et Vectorize ; les modifications de domaine ou de routes exigent aussi les autorisations Zone, Routes ou Custom Domain correspondantes. Setup n'importe pas les identifiants OAuth de Wrangler. Gardez le jeton et la clé d'API mémoire confidentiels.

Pour une installation existante, passez `--config <absolute-existing-jsonc-path>` afin de lire sa configuration sans l'écraser. Setup réutilise les noms de ressources dans l'ordre `remnifold`, `kagayoi-memory`, puis `cloudflare-supermemory` ; les options explicites `--worker-name`, `--database-name` et `--vector-index-name` sont prioritaires. Vérifiez que le plan sélectionne le Worker, la base D1 et l'index Vectorize existants. Les ID configurés ainsi que les dimensions et la métrique des vecteurs doivent correspondre. Les liaisons, planifications cron et routes existantes sont préservées ; les conflits arrêtent Setup. Ne créez pas de serveur de remplacement et ne copiez pas de mémoires pour cette mise à niveau.

Lorsque vous comptez provisionner ou déployer, fournissez la clé mémoire choisie via `REMNIFOLD_API_KEY` et appliquez le plan :

```sh
remnifold server-setup --account-id <Cloudflare-account-id> --apply --location apac
```

Utilisez la même configuration et les mêmes noms que pour l'aperçu. `--apply` crée les ressources manquantes, installe la clé fournie comme secret du Worker, applique les migrations D1 additives, déploie le Worker embarqué et vérifie le point de terminaison. Pour un serveur existant, fournissez sa clé existante afin de ne pas la remplacer par inadvertance. Sans clé fournie, Setup conserve le secret et signale que la vérification authentifiée est ignorée ; l'état de santé seul ne vérifie ni les migrations ni l'accès MCP. Une clé fournie active la vérification de session, l'initialisation MCP distante et la découverte de `search_memory`, `getDocument` et `add_memory`.

Appliquez la mise à jour du serveur et les migrations avant de connecter ou de mettre à jour les clients. Setup stocke la configuration reprenable hors du dépôt, dans le dossier de configuration de l'application, sous le nom `server/config.json` (voir [Connexion à votre serveur](#connexion-à-votre-serveur)) ; `--state <absolute-json-path>` choisit un autre emplacement externe. Conservez cet état pour les mises à jour, ne commitez jamais les ID générés ni les identifiants, et relancez la même commande après avoir corrigé une étape échouée. Utilisez `--base-url` pour un point de terminaison personnalisé existant et `--help` pour toutes les options. L'utilisation de Cloudflare est facturée au compte qui héberge le serveur.

Pour 1.0.22, mettez à jour le serveur avec le `server-setup --apply` de cette version avant de mettre à jour les clients. La migration `0013_memory_created_index.sql` ajoute un index pour l'organisation par plage de jours ; elle ne modifie ni les enregistrements stockés ni les ID.

Pour 1.0.21, mettez à jour le serveur avec le `server-setup --apply` de cette version avant de mettre à jour les clients. Les migrations `0010_capture_variants.sql`, `0011_consolidation_recall_opt_out.sql` et `0012_organization_retry_fairness.sql` ajoutent des variantes de capture vérifiées, invalident les points de contrôle historiques contenant des sources exclues du rappel et ramènent à une heure les futurs reports hérités. Les enregistrements et ID d'origine sont conservés ; les points de contrôle concernés peuvent être régénérés à partir des sources éligibles.

## Fonctionnement de la mémoire

- Les demandes d'utilisateur terminées et les réponses finales sont sauvegardées dans la collection partagée `memories` avec la provenance du dossier source. Les définitions de skills jointes et les enveloppes d'état de navigateur/page sont omises du texte de requête sauvegardé ; les réponses finales sont préservées. Une identité de capture déterministe fait que les nouvelles tentatives réutilisent le même enregistrement stocké au lieu de créer des doublons.
- Utilisez `listTopics` pour trouver les catégories de contenu, puis passez un `topic` à `listDocuments` ou `listMemories`. Les listes incluent les enregistrements des anciens espaces par dossier comme de la collection partagée. Utilisez `topic: "__unclassified__"` pour voir les enregistrements sans étiquette de thème actuelle, y compris ceux dont la classification a échoué, est désactivée ou a été volontairement ignorée.
- `add_memory` manuel sauvegarde dans la collection partagée. Pour oublier un enregistrement après l'avoir lu, appelez `add_memory` avec `action: "forget"`, son `documentId` et le `containerTag` source ; le contenu stocké exact reste pris en charge pour la compatibilité. Avec le plugin Codex, un `sourceFolder` absolu ou une racine d'espace de travail unique fournie par le client MCP ajoute la provenance ; l'outil MCP distant accepte à la place un `projectId` explicite. Un `containerTag` explicite remplace le stockage. Le plugin n'utilise jamais son dossier d'installation comme projet de l'utilisateur. `listSpaces` expose les espaces physiques pour la compatibilité et le diagnostic.
- Le rappel d'invite recherche dans plusieurs espaces par thème. Les relances courtes peuvent retrouver un thème à partir de messages d'utilisateur antérieurs de la même session, même avant de volumineuses sorties d'outils. La récupération de l'historique est bornée ; sans thème concret, aucune recherche par thème n'est lancée. Le rappel automatique sélectionne la dernière révision disponible de chaque tour et de chaque partie capturés, tandis que la recherche manuelle conserve les révisions historiques. La recherche globale inclut les espaces au-delà de l'ancienne limite de découverte ; les serveurs plus anciens utilisent un chemin de compatibilité plafonné à 100 espaces et signalent lorsque la découverte est incomplète. Les recherches partielles signalent les espaces et index de documents indisponibles tout en conservant les résultats obtenus. L'événement de session initial ne fait que réinitialiser l'état du rappel. Chaque invite ordinaire non vide affiche aussi jusqu'à deux tâches terminées récentes, tous projets confondus, indépendamment de la correspondance de thème, avec le projet source, la date et l'ID du document. La tâche en cours et les enregistrements de tâche en double sont exclus. Ces résumés sont étiquetés séparément afin de suggérer un travail antérieur sans laisser entendre qu'il est pertinent. Définissez `maxRecentTasks` dans `remnifold.json` sur un entier de 0 à 3 ; 0 désactive ce contexte de travail récent. Les serveurs plus anciens sans ce mode de listage continuent de fournir le rappel par thème.
- Les recherches manuelles correspondent aussi au nom de projet stocké. Les recherches portant uniquement sur le nom du projet privilégient les projets sources correspondants et les enregistrements sources plus récents ; les recherches combinant projet et thème continuent de donner la priorité à la couverture du thème.
- Le contexte automatique contient un titre, une description, des noms de sections, des horodatages et des ID de documents. Un court passage d'origine correspondant peut révéler des détails absents d'un index ; il reste un texte source historique à vérifier avant réutilisation. Codex ouvre les enregistrements complets avec `getDocument` si nécessaire.
- La formulation fixe du bloc `<remnifold-index>` injecté, des résultats de recherche et des avis de rappel suit la langue de l'environnement qui les écrit : `REMNIFOLD_LANGUAGE`, puis `LC_ALL`, `LC_MESSAGES` et `LANG`, puis la langue d'affichage du système (langue d'interface de l'utilisateur Windows, `AppleLanguages` de macOS). Les environnements japonais reçoivent une formulation japonaise et tous les autres une formulation anglaise. Les titres, descriptions et extraits stockés sont affichés tels qu'enregistrés, sans traduction.
- Le rappel par thème est limité par défaut à trois enregistrements. Définissez `maxMemories` dans `remnifold.json` sur un entier de 1 à 5 pour changer la limite du rappel automatique et de la recherche manuelle ; Setup préserve une valeur existante lors de la mise à jour de la connexion. Le contexte automatique est également limité à 3 000 unités de code UTF-16, de sorte que moins d'enregistrements peuvent y tenir.
- Le contexte automatique suit les tâches récentes séparément des index de thèmes et omet les entrées inchangées aux invites suivantes lorsque les informations de session et de transcription sont disponibles. Les enregistrements mis à jour peuvent réapparaître, et la compaction ou l'effacement du contexte réinitialise la suppression des doublons. L'état local du rappel reste sous `remnifold/recall-state/` dans le dossier Codex sélectionné, sans copier le texte des conversations ; un état indisponible laisse le rappel normal activé. La correspondance des requêtes et les détails de l'état sont décrits dans [DESIGN.md](https://github.com/1llum1n4t1s/remnifold/blob/main/DESIGN.md#recall).
- Le serveur examine les projets chaque jour à 03:00 JST. Il demande à Workers AI de créer un point de contrôle consolidé lorsqu'un projet compte au moins 20 enregistrements non consolidés, ou lorsqu'il a des enregistrements non consolidés et que trois jours se sont écoulés depuis le dernier point de contrôle réussi (avant le premier point de contrôle, depuis l'enregistrement en attente le plus ancien). Lors de la première exécution due d'un projet, le serveur résorbe automatiquement tout l'arriéré existant en lots d'IA successifs et bornés, de sorte que les anciens enregistrements n'exigent aucune commande manuelle de rattrapage. Les enregistrements d'origine sont conservés et liés depuis le point de contrôle. Les points de contrôle plus anciens sont remplacés, et la modification ou l'oubli d'une source invalide le point de contrôle qui en dépendait.
- Utilisez l'outil MCP `consolidate_memory` pour faire avancer immédiatement d'un lot le point de contrôle d'un projet, sans attendre la vérification quotidienne. Le MCP distant exige `projectId`, l'ID de provenance stocké. Le plugin Codex peut déduire le projet courant à partir d'une seule racine d'espace de travail, ou accepter explicitement `sourceFolder` ou `projectId`. Chaque appel traite au plus 40 nouveaux enregistrements ; répétez-le si un arriéré plus important subsiste. L'exécution manuelle exige tout de même au moins un enregistrement source non consolidé.
- Le rappel automatique préfère un point de contrôle à jour à pertinence de thème égale et évite de répéter les originaux couverts par un point de contrôle sélectionné. Les détails correspondants précis absents du point de contrôle exposent tout de même leurs originaux. La recherche manuelle continue d'exposer les enregistrements d'origine.
- Le contenu et les métadonnées d'un point de contrôle ne peuvent pas être modifiés directement : les mises à jour renvoient HTTP 409. Modifiez plutôt les enregistrements d'origine et régénérez le point de contrôle. Le ré-enrichissement et la suppression restent disponibles.
- La capture masque la clé d'API mémoire configurée et les motifs de secrets courants, et retente les enregistrements non acquittés. L'état de capture local reste sous `capture-state/` dans le dossier d'état de l'application ; les reçus des versions antérieures dans le dossier personnel de Codex restent lisibles.

Les échecs de capture incluent des codes fixes `stage`, `cause` et `action`, sans afficher le texte des exceptions ni les identifiants. Un résultat `action=repair-required`, tel que `stage=receipt; cause=invalid`, exige de réparer l'état de capture local correspondant ; les nouvelles tentatives seules continueront d'échouer. `action=retry-or-repair` conserve le comportement normal de nouvelle tentative tout en permettant d'étudier un problème persistant de connexion ou de réponse. Pour vérifier séparément l'accessibilité de l'API, exécutez ceci depuis le dossier du plugin extrait :

```sh
remnifold status
```

Les thèmes sont dérivés du contenu des enregistrements lors de l'étape d'enrichissement Workers AI existante. La classification est asynchrone. Les simples accusés de réception sans résultat substantiel, et les enregistrements pauvres en information similaires, restent disponibles pour la recherche manuelle mais sont exclus du rappel automatique et de l'enrichissement ; les étiquettes de thème explicites sont tout de même conservées. Les noms de dossiers sont conservés comme métadonnées de source. Les enregistrements existants gardent leurs ID de document et leurs emplacements de stockage d'origine et apparaissent dans le même navigateur de thèmes.

Mettez à jour le serveur et appliquez ses migrations avant d'installer un client qui utilise la navigation par thèmes ou la consolidation. Pour classer d'anciens enregistrements, prévisualisez un lot borné avec l'exécutable de la plateforme, en utilisant la connexion mémoire existante :

```sh
remnifold topics-setup --limit 10
```

Le lot par défaut est de 10 ; `--limit` accepte de 1 à 50. Ajoutez `--apply` pour mettre en file ce lot fini lorsque l'enrichissement distant est voulu. Cela utilise la facturation Workers AI existante, actualise l'enrichissement dérivé IA/vecteurs/faits et invalide les points de contrôle de projet concernés tout en préservant les exclusions et les protections de révision. Le résultat indique les tâches acceptées, pas la classification terminée. Attendez la fin du traitement avant de sélectionner le lot suivant ; inspectez `getDocument` et `listTopics` pour vérifier le résultat.

La clé d'API donne accès aux enregistrements d'une installation de serveur. Utilisez des installations distinctes pour des frontières de confiance distinctes. Le masquage réduit la capture accidentelle de secrets ; passez en revue ce que vous choisissez de stocker dans votre propre backend.

## Organisation de la mémoire

Après avoir installé l'application et appliqué les [migrations actuelles du serveur](#créer-ou-mettre-à-jour-un-serveur), y compris le schéma d'organisation de `0009_memory_organization.sql` et l'index d'organisation par plage de `0013_memory_created_index.sql`, organisez la mémoire à la demande avec le [skill organize-memory](skills/organize-memory/SKILL.md), par exemple `$organize-memory 7` ou « organise les 7 derniers jours de mémoire ». Le nombre de jours (1–3650) sélectionne les mémoires créées au cours des N derniers jours selon l'heure du serveur ; l'exécution répète des lots jusqu'à ce que cette plage n'ait plus de travail dû ou qu'un plafond de lots facultatif (3 par défaut, 20 au plus) soit atteint. Sans nombre de jours, un lot est pris dans la file globale. L'organisation n'est plus enregistrée comme tâche planifiée Codex ; [setup-remnifold](skills/setup-remnifold/SKILL.md) se contente de mettre en pause les tâches laissées par les versions antérieures.

Chaque exécution utilise l'exécutable actuellement installé. Un lot traite au plus vingt enregistrements cibles et 48 000 unités source UTF-16, y compris le travail partiel ou différé. Ce ne sont ni des nombres d'enregistrements terminés ni des garanties de lecture ou de jetons au total : les résumés de candidats et les lectures de preuves ciblées s'y ajoutent. La recherche est limitée à deux appels par lot. Le travail non résolu est différé plutôt que de s'étendre en exploration massive ; une exécution par plage arrêtée par son plafond peut simplement être relancée.

Pour une utilisation directe en CLI, exécutez l'exécutable installé :

```text
remnifold memory-organize prepare [--days N]
remnifold memory-organize lookup --line
remnifold memory-organize commit --line
remnifold memory-organize sources <summary-document-id> [cursor]
```

Ces commandes utilisent la connexion existante par clé d'API du propriétaire ; les identifiants OAuth du MCP distant ne peuvent pas autoriser l'organisation. Elles ne planifient pas Codex et ne déploient ni ne migrent un serveur. `prepare` crée un bail de trente minutes ; `--days N` le limite aux mémoires créées au cours des N derniers jours sans déplacer la file de découverte globale ; chaque réponse par plage renvoie `range.cursor`/`range.exhausted`, et l'appel suivant passe un curseur non nul avec `--range-cursor <cursor>` ; un serveur plus ancien le rejette au lieu de se rabattre sur autre chose. Il renvoie les ID/révisions/fragments des sources, des notes de continuation et des résumés de candidats ; ce n'est pas une vérification de connectivité en lecture seule. `lookup` lit `{"query":"a concise topic"}` depuis l'entrée standard. `commit` lit un objet JSON contenant `batchId`, `summaries` et `coverage`. Suivez les limites de champs exactes, les règles de partition et la procédure `groupProgress` multipartite du skill d'exécution. Ne déclarez `complete` que lorsque chaque fragment et chaque sujet important sont représentés, en préservant les conclusions, les raisons, les preuves et l'incertitude ; utilisez une continuation partielle ou un report lorsque ce n'est pas possible. Codex porte ce jugement. Vérifiez le reçu du commit et distinguez les résumés publiés de la progression inachevée.

Avant `prepare`, établissez un chemin d'entrée standard sûr et un moyen de conserver et de paginer sa réponse complète ; sous Windows, suivez la sonde PTY synthétique locale du skill d'exécution avant d'acquérir un bail. Lisez chaque fragment source, chaque mémo de continuation et chaque résumé candidat depuis la même réponse conservée. Un affichage tronqué se récupère à partir de cet appel, et non en préparant un autre lot. Avant le commit, validez le JSON exact en mémoire : chaque entrée de couverture exige son ID/sa révision préparés et un tableau `summaries` explicite, y compris `summaries:[]` pour les entrées partielles ou différées. Suivez le [skill d'exécution](skills/organize-memory/SKILL.md#one-batch) pour la pagination, le contrôle préalable et la reprise après une exécution interrompue.

L'entrée standard JSON a un délai de trente secondes et une limite de 128 000 octets UTF-8, échappements compris. `--line` exige une seule ligne JSON compacte et son saut de ligne final ; sans lui, fermez l'entrée standard après l'objet. Utilisez un canal d'entrée sans écho et gardez le texte généré hors des arguments du shell et des journaux de commandes. Les serveurs non pris en charge s'arrêtent explicitement, sans repli sur sauvegarde/oubli.

Pour l'entrée PTY sous Windows, lancez directement l'exécutable natif dans un PTY dédié avec `memory-organize lookup --line --private-stdin` ou `memory-organize commit --line --private-stdin`. Attendez `REMNIFOLD_STDIN_READY` sur stderr avant d'envoyer exactement une ligne JSON compacte et son saut de ligne final, sans aucun octet ensuite. Gardez la charge utile plus le saut de ligne à 127 000 octets UTF-8 au plus, afin de laisser de la marge aux transformations du terminal. Le mode privé désactive l'écho et l'édition de ligne, et restaure le mode d'origine de la console après le terminateur. Si le signal de disponibilité est absent ou si une étape échoue, cessez d'envoyer de l'entrée et fermez ce PTY ; ne le réutilisez pas et ne rétablissez pas l'écho tant que de l'entrée peut subsister. N'utilisez ni console partagée ni enveloppe Git Bash. Une entrée standard redirigée et les hôtes non Windows rejettent `--private-stdin` ; sous macOS/Linux, utilisez un tube sans écho avec une entrée par ligne ou jusqu'à EOF. Consultez le [skill d'exécution](skills/organize-memory/SKILL.md#safe-stdin-and-failures) pour la procédure d'entrée complète.

Aucun original n'est supprimé. Les originaux entièrement représentés quittent la recherche normale et le rappel par thème seulement tant que leurs résumés restent valides ; les ID et corps d'origine restent directement récupérables, et la connaissance des tâches récentes conserve son éligibilité existante. Les captures en conflit restent visibles et différées, sauf si le serveur vérifie des clés de capture v2 monopartites distinctes et des empreintes de corps exactes comme preuves indépendantes. Cette vérification n'établit pas quelle version est la plus récente ; les contradictions non résolues doivent subsister dans le résumé. Les reports ordinaires deviennent éligibles après une heure, tandis que les conflits de capture non résolus continuent de bloquer les nouvelles tentatives automatiques. Les files bornées partagent la progression entre le travail dû plus ancien, les continuations et les enregistrements intacts ; une exécution n'établit pas l'achèvement sur toute l'archive. Les modifications ou l'oubli invalident les résumés dépendants et exposent de nouveau les originaux survivants. Les anciens points de contrôle de projet peuvent encore exposer des informations tant qu'ils ne sont pas traités comme des enregistrements entiers. Avec l'IA activée, les originaux qui alimentent déjà la recherche sémantique attendent que les embeddings/vecteurs de résumé vérifiés soient prêts avant de disparaître. Avec l'IA désactivée, la couverture lexicale suffit ; activer l'IA peut faire réapparaître des originaux jusqu'à ce que les remplaçants sémantiques soient prêts. La reprise des vecteurs est asynchrone, et le cron de quinze minutes n'est pas une échéance d'achèvement. Les embeddings de résumé utilisent la facturation Workers AI existante ; Codex génère la prose.

`sources` est en lecture seule et pagine jusqu'à cinquante ID de sources enregistrés, avec leurs espaces et dates. Récupérez les corps avec getDocument ; suivez l'aperçu des sources/le curseur propre à un point de contrôle jusqu'à ses membres enregistrés. Des chemins de provenance distincts peuvent répéter un ID. Les réponses getDocument actuelles du MCP natif et distant incluent aussi un aperçu des sources et une pagination `sourceCursor` facultative ; les anciens plugins natifs peuvent utiliser le CLI lorsqu'ils omettent ces champs. Les curseurs s'arrêtent sur un conflit si leur parent change. La [conception de l'organisation](DESIGN.md#memory-organization) décrit en détail la couverture, l'invalidation et la reprise des vecteurs.

## Migration depuis les noms 1.x

Supprimez un plugin Codex `kagayoi-memory`, `cloudflare-supermemory` 1.x ou `remnifold` installé, puis installez l'application autonome. Le client lit d'abord `remnifold.json`, puis `kagayoi-memory.json` et `supermemory.json`, dans son dossier de configuration puis dans l'ancien dossier personnel de Codex. `REMNIFOLD_API_URL` et `REMNIFOLD_API_KEY` sont les variables d'environnement normales ; les anciens lecteurs `KAGAYOI_MEMORY_*`, `SUPERMEMORY_API_URL`, `SUPERMEMORY_CODEX_API_KEY` et `CLOUDFLARE_MEMORY_API_KEY` subsistent pour la compatibilité. Exécutez `remnifold setup` pour écrire le `remnifold.json` normal tout en conservant les fichiers précédents. Les reçus de capture et l'état de rappel existants restent lisibles depuis les anciens dossiers de produit. La configuration du serveur réutilise les ressources nommées `remnifold`, puis `kagayoi-memory`, puis `cloudflare-supermemory` ; les noms de ressources explicites l'emportent toujours. Modifier l'application locale ne déploie aucun Worker et ne déplace aucune base distante. Les ID de document, ID de capture, provenances de projet et métadonnées `sm_*` persistées existants restent inchangés. Les détails de compatibilité du stockage figurent dans [DESIGN.md](https://github.com/1llum1n4t1s/remnifold/blob/main/DESIGN.md#storage-and-provenance).

Remnifold implémente sa propre API auto-hébergée. Il est indépendant du service Supermemory hébergé et ne lit ni ne modifie les fichiers de mémoire locale intégrés de Codex.

Pour l'architecture et l'organisation du dépôt, consultez le [DESIGN.md](https://github.com/1llum1n4t1s/remnifold/blob/main/DESIGN.md) du dépôt. Les contraintes pour les contributeurs, la validation et les commandes de packaging figurent dans [AGENTS.md](https://github.com/1llum1n4t1s/remnifold/blob/main/AGENTS.md).

Sous [licence MIT](LICENSE).
