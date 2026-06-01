# cc-sessions — CLI Rust de gestion des sessions Claude Code

> Design validé le 2026-06-01. Spec destinée à un repo Rust dédié `cc-sessions`
> (binaire `cs`), consommé en input par le flake `infra`.

## Problème

L'utilisateur jongle entre 8+ projets, chacun avec une ou plusieurs sessions
Claude Code en cours. Aujourd'hui le suivi se fait à la main dans
`~/Desktop/Reste à faire.md` (liste de `cd <dir>` + `claude --resume <uuid>` +
note de contexte). Limites : pas de vue d'ensemble navigable, reprise en 2
étapes manuelles, notes déconnectées des sessions réelles.

Des prototypes jetables (`~/.local/bin/cs` python+fzf, `~/.local/bin/note` bash)
ont validé le concept. Cette spec décrit une construction **greenfield** : pas de
migration ni de rétrocompatibilité de format avec ces prototypes.

## Objectif

Un binaire unique `cs` qui :
1. liste **toutes** les sessions Claude Code, cross-projet, dans un picker fuzzy ;
2. reprend une session sélectionnée d'un seul geste (`cd <cwd> && claude --resume <uuid>`) ;
3. attache à chaque session une **note d'état** et un **statut** de cycle de vie,
   stockés **localement dans le projet** ;
4. offre des **stats** et un **archivage non destructif** pour le ménage.

## Décisions de design (validées)

| Axe | Choix |
| --- | --- |
| Moteur TUI | `skim` natif (crate Rust) — zéro dépendance externe |
| Packaging | Repo Rust dédié `cc-sessions` + `flake.nix`, consommé en input par `infra` |
| Périmètre | Binaire unique : picker + `note` + statuts + stats + archivage |
| Stockage métadonnées | **JSONL local par projet** (cf. ci-dessous) |
| Statuts | 3 états manuels : `active` / `hold` / `done` |
| Ménage | Archivage non destructif + `purge-archive` séparée (double confirmation) |
| Onboarding | Zéro-friction par défaut ; pas d'importeur automatique (fait à la main) |
| Construction | **Greenfield** — pas de migration ni rétrocompat avec les prototypes |
| Perf | Index cache `~/.cache/cc-sessions/` invalidé par `(mtime,size)` |

## Modèle de données

Fichier **local par projet** : `<git-root>/.claude/session-notes.jsonl`
(fallback : `<cwd>/.claude/...` si hors d'un repo git). Exclu de git via
`.git/info/exclude` (jamais committé — peut contenir du contexte sensible).

Une ligne JSON par session (réécriture *last-wins* par `sessionId`, fichier
compact) :

```json
{"sessionId":"11575aee-c202-…","updatedAt":"2026-06-01T13:45","status":"active","note":"Système cs posé. Reste : commit + rebuild macbook."}
```

- `status` ∈ `active` (défaut implicite) | `hold` | `done`.
- Champs absents tolérés (rétrocompatibilité ascendante : ajout futur de `tags`,
  `waitingFor`… sans casse).
- Une entrée n'est créée que lorsque l'utilisateur pose une note ou un statut.
  Les sessions sans entrée sont `active` implicite et affichent leur titre auto.

## Source des sessions

Scan de `~/.claude/projects/*/*.jsonl`. Pour chaque fichier (lecture partielle
**head 32 Kio + tail 64 Kio** pour rester < 1 s même sur des `.jsonl` de
dizaines de Mo) on extrait :

- `sessionId` (= nom du fichier sans extension) ;
- `cwd` (dernier rencontré) ;
- `aiTitle` (lignes `type:"ai-title"`) — titre auto-généré ;
- `lastPrompt` (lignes `type:"last-prompt"`) ;
- premier message utilisateur (fallback de résumé ; ignoré s'il commence par
  `<` — caveats/bang-commands).

Exclusions : répertoire `subagents`, fichiers < 2 Kio (quasi vides). Tri par
mtime décroissant, plafonné aux 300 plus récentes pour le picker.

La colonne « projet » du picker = **nom du repo git (basename du git-root)**, et
`repo/sous-dossier` si la session est dans un sous-dossier (ex.
`/Users/lambda/Documents/temp/A2A-COMM/cli` → `A2A-COMM/cli`). Hors git :
les deux derniers segments du chemin. Le chemin complet reste fuzzy-cherchable et
visible en preview.

## Cache d'accélération

Reparser tous les `.jsonl` à chaque appel est gaspillé : presque rien ne change
entre deux lancements. On maintient un **index cache** régénérable.

- Emplacement : `~/.cache/cc-sessions/index.json` (XDG). **Jamais source de
  vérité** — purgeable à tout moment, reconstruit au besoin.
- Clé d'invalidation : **`(mtime, size)`** obtenue par un seul `stat()` — *aucune
  lecture de contenu*. (Un sha256 imposerait de lire le fichier, soit exactement
  ce qu'on veut éviter ; sur APFS `mtime` ns + `size` suffisent.)
- Entrée par fichier : `{path, mtime, size, sessionId, cwd, aiTitle, lastPrompt,
  firstUser}`.
- Flux : `stat` tous les `.jsonl` → réutiliser les entrées inchangées → head/tail
  + parse uniquement les fichiers nouveaux ou modifiés → réécrire l'index.
- Les entrées dont le fichier a disparu sont éliminées de l'index.

Attendu : 1ᵉʳ appel ≈ plein scan ; appels suivants ≈ `stat`-only + parse des
quelques fichiers modifiés → quasi instantané.

Les **notes/statuts** locaux (`session-notes.jsonl`, petits et peu nombreux) sont
relus à chaque appel — pas mis en cache, pour rester toujours frais.

## Sous-commandes (clap)

| Commande | Effet |
| --- | --- |
| `cs` | Picker skim (défaut). Enter → `cd <cwd>` puis `execvp claude --resume <uuid>` dans le terminal courant. |
| `cs note "<texte>"` | Écrit/remplace la note de la session courante (`$CLAUDE_CODE_SESSION_ID`). `-a` pour append. |
| `cs hold` / `cs done` / `cs active` | Change le statut de la session courante. |
| `cs stats` | Vue d'ensemble : nb sessions & taille `.jsonl` par projet, âge de la dernière activité, candidats au ménage. |
| `cs done --older-than <Nd>` | Marque `done` en masse les sessions inactives depuis N jours. |
| `cs archive [--older-than <Nd> \| <uuid>…]` | Déplace les `.jsonl` ciblés vers `~/.claude/projects-archive/<même arborescence>/`. Non destructif. |
| `cs purge-archive` | Vide définitivement l'archive. **Double confirmation** + récap de ce qui part. |

`note`, `hold`, `done`, `active` requièrent `$CLAUDE_CODE_SESSION_ID` (présent
dans l'env des bang-commands et des sessions). Erreur claire sinon.

## Picker (skim)

Colonnes : `statut │ date │ projet │ note-ou-titre │ uuid`.

- Tri : `hold` en tête, puis `active` par récence ; `done` **masquées** par
  défaut, révélées par `Ctrl-D` (toggle).
- Indicateurs : `⏳` hold, `✓` done, `📝` note présente.
- Preview (droite) : titre auto, note, statut, dernier prompt, premier message.
- Résumé affiché : note locale > `aiTitle` > `lastPrompt`/premier message > `(vide)`.

## Intégration `/note` (slash command Claude Code)

`~/.claude/commands/note.md` (déjà créé) : Claude rédige une note d'état courte
(≤ 200 car., « où on en est + next step ») et la persiste en appelant
`cs note "<texte>"`.

## Onboarding

1. **Défaut zéro-friction** : toute session existante est navigable
   immédiatement (titre auto + `active` implicite). Aucune migration requise.
2. **Manuel** : l'utilisateur posera notes/statuts sur ses sessions clés à la
   main. Pas d'importeur automatique de `Reste à faire.md`.
3. **Ménage optionnel** : `cs done --older-than 30d` pour dégraisser le picker.

## Architecture du code (unités isolées)

- `scan` — découverte + lecture partielle des `.jsonl`, extraction des champs.
  In : `~/.claude/projects`. Out : `Vec<SessionInfo>`.
- `cache` — index `~/.cache/cc-sessions/index.json` ; invalidation `(mtime,size)`
  via `stat`, réutilisation des entrées inchangées, éviction des fichiers
  disparus. Enveloppe `scan` (ne parse que le delta).
- `meta` — lecture/écriture des `session-notes.jsonl` locaux, résolution
  git-root (avec cache), merge note+statut sur les sessions.
- `picker` — rendu skim, tri/filtre par statut, preview, sélection.
- `archive` — déplacement non destructif, purge avec confirmation.
- `cli` — clap, dispatch des sous-commandes, `execvp` de reprise.

Chaque unité testable indépendamment via fixtures.

## Tests

Unitaires Rust :
- parsing/écriture JSONL (last-wins, champs absents, rétrocompat) ;
- extraction head/tail sur `.jsonl` de fixtures (avec/sans `aiTitle`, caveats) ;
- résolution git-root et fallback cwd ; libellé projet `repo/sous-dossier` ;
- tri par statut + masquage `done` ;
- logique `--older-than` (sélection par âge) ;
- cache : hit sur `(mtime,size)` inchangés, miss/reparse sur modif, éviction des
  fichiers disparus.

Manuel : picker skim interactif, `execvp` de reprise, archivage/purge.

## Packaging Nix

Repo `cc-sessions` : `flake.nix` exposant `packages.<system>.default`
(`buildRustPackage` via crane ou naersk). Le flake `infra` l'ajoute en input et
l'inscrit dans `environment.systemPackages` du host `macbook`. Déploiement via
`safe-rebuild.sh macbook switch`. Dépendance externe `fzf` retirée (skim natif).

## Hors scope (YAGNI, plus tard)

Tags, `waitingFor`/rappels datés, statuts auto-calculés, import automatique du
`.md`. (Le cache `(mtime,size)` est, lui, dans le scope — cf. § Cache.)
