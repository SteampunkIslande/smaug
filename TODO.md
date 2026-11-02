# Remaniement de code

## Langue

- [X] S'assurer que tous les commentaires de code sont en anglais, mais que toutes les chaînes de caractères en lien avec les utilisateurs (affichage des erreurs notamment) soient en français. Pour les valeurs d'enum, celles-ci doivent être en anglais, mais traduites par une simple table de traduction en jinja dans le template.

## Rafraîchissements

- [X] Ajouter une route /smaug/data qui sert statiquement le sous-dossier `exports-{{ attempt_number }}` du dossier d'analyse (avec une profondeur jusqu'à 1, c'est-à-dire que l'exécution d'un script peut venir écrire des sous-dossiers, par exemple `exports-1/logs/fichier.log`, ou `exports-1/rapports/rapport.pdf`).

- [X] Faire en sorte de rendre plus cohérente la gestion des erreurs dans la routine: problème de la responsabilité de la mise à jour de la base de données. Actuellement, c'est `run_script` qui écrit dans la base de données, mais quand `run_script` renvoie une erreur, personne n'écrit dans la base (il y a juste un `error!(e)` qui est loggé...). Objectif: c'est l'appelant de `run_script` qui a la responsabilité de gérer les erreurs. `run_script` doit, pour cela, renvoyer une `anyhow::Error` qui doit alors être `downcast_ref`, pour traiter les différents cas (ex: en cas de `GitCheckError` ou `io::Result` ou toute autre erreur survenant avant même le lancement de l'analyse à proprement parler, appeler `failure_cannot_analyse_run`).

- [X] Ajouter un avertissement quand, au moment d'éditer un run, le formulaire n'a pas les mêmes variables qu'à l'exécution précédente (comparer la liste des clés dans la colonne `user_defined_vars` de la table `Run` avec la liste des noms de variables dans le formulaire **à jour** correspondant). Le message doit être explicite, un simple avertissement, qui dit: `Le formulaire 'pipeline_name/.forms/version_simple.yaml' sur la branche 'main' n'a pas les mêmes variables qu'à l'exécution précédente`.

- [X] Faire que l'erreur soit plus claire lorsque l'utilisateur veut éditer un run dont le formulaire `(nom de la branche, nom du formulaire)` n'existe plus.

- [X] Les erreurs Frontend devraient être plus claires: le `downcast_ref` devrait prendre en charge plus de cas.

## Nouvelles fonctionnalités

- [X] Créer un rapport d'exécution dans le dossier de run (dans `{analysis_dir}/run-{run_id}/smaug-run-report-attempt-{attempt_number}.db`): un fichier sqlite avec une table snakemake pour stocker, pour chaque étape de snakemake:
  - le slurm job name (le UUID tel que parsé dans `parse_log.rs`)
  - le slurm job id
  - le snakemake job id
  - les chemins complets de tous les logs (snakemake et slurm)
  - les valeurs de wildcards associées à ce job (séparés par des virgules, liste de chaînes de caractères fournies par snakemake)
  - les chemins complets de tous les inputs (séparés par des virgules)
  - les tailles en octets de tous les inputs (dans le même ordre que les chemins des inputs)
  Ce fichier sera généré au fur et à mesure de l'exécution de chaque run.

- [X] S'assurer que les routes suivantes sont soumises à authentification:
  - [X] `/smaug/logs`: seulement servir `/smaug/logs/run-{run_id}-tentative-{attempt_number}.{stdout,stderr}` aux utilisateurs qui peuvent voir ce run
    Update: `/smaug/logs/<file..>` devrait plutôt être `/smaug/logs/<run_id>/<attempt_number>/<out_type>` (où `out_type` est `stder` ou `stdout`)
  - [X] `/smaug/uploads`: seulement à un utilisateur authentifié (faute de mieux, sinon il faudrait trouver un moyen de savoir pour chaque fichier de upload, quel utilisateur l'a uplaodé...)

- [X] Ajouter, sur chaque page qui affiche une tentative, une liste de titres, correspondant chacun à un sous-dossier (s'il existe) de `{analysis_dir}/run-{run_id}/exports`. Sous chacune de ces sections,
un lien http (`<a href=...>`) par fichier. Ainsi, on aurait par exemple:
    | Rapports            |
    | ------------------- |
    | rapport_sampleA.pdf |
    | rapport_sampleB.pdf |

    | Logs                         |
    | ---------------------------- |
    | Tous les logs disponibles... |

- [X] Déplacer la route `/smaug/data` dans le module backend...

- [ ] En cas d'interruption de l'exécution d'un run, le code d'erreur n'est pas correctement récupéré (affiche exit code: ?)

## Intégration continue

- [X] Ajouter au fichier `.github/workflows/release.yml` de lire la version dans `Cargo.toml`. La version est lue dans `Cargo.toml` (étape `Read version from Cargo.toml`) et utilisée pour le tag par défaut (`v<version>`), sauf si un `tag_name` est fourni manuellement. Un script `scripts/release.sh` permet de bump la version (major/minor/patch), de maj `Cargo.toml` + `Cargo.lock`, de commit/tag/pusher, et de déclencher la release.

## Vérifications post-IA

- [ ] Dans `routes/front_end/admin.rs`, les fonctions `password_edit_get` et `register_get` passent des variables curieuses au contexte (pages `admin/passedit` et `admin/register`).