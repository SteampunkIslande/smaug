# Suivi d’exécution : logs, rapport Snakemake et squeue

Ce document décrit le flux de données qui relie un processus d’analyse lancé par
`src/routine.rs` à l’interface de suivi, puis les défauts constatés et les
corrections apportées.

## 1. Flux actuel

```
routine::run_script
        │
        ├── stdout ──► tee_stream_to_file ──► logs/<run>-tentative-<n>.stdout
        │
        └── stderr ──► tee_stream_to_file ──► logs/<run>-tentative-<n>.stderr
                              │
                              ├── LogProgressParser ──► RunProgressCache
                              │                         (clé run_id + attempt_number)
                              │
                              └── RunReport ──────────► smaug-run-report-attempt-<n>.db
                                                        (une ligne par job Snakemake)
```

L’interface interroge ensuite :

- `GET /smaug/api/watch/<run_id>/<attempt_number>` pour la progression ;
- `GET /smaug/api/squeue?...` pour l’état Slurm.

Le rapport SQLite (`smaug-run-report-attempt-<n>.db`) n’est pas exposé par
l’application. Il est destiné au traitement post-exécution défini par
l’utilisateur, qui lit la base et produit sa propre page HTML.

## 2. Ce que Snakemake écrit sur stderr

Le plugin Slurm de Snakemake utilise le même nom de job pour tous les jobs d’un
même workflow : l’UUID affiché par la ligne `SLURM run ID: <uuid>`. C’est
pourquoi `squeue -n <uuid>` permet de suivre tous les jobs d’une invocation.

Les marqueurs exploités sont :

| Marqueur                                                          | Information                                                            |
| ----------------------------------------------------------------- | ---------------------------------------------------------------------- |
| `SLURM run ID: <uuid>`                                            | nom Slurm commun aux jobs du workflow                                  |
| `rule <nom>:` / `localrule <nom>:`                                | début du bloc d’un job                                                 |
| `    input: ...`                                                  | entrées du job                                                         |
| `    log: ...`                                                    | logs déclarés par la règle                                             |
| `    wildcards: ...`                                              | valeurs de wildcards                                                   |
| `    jobid: <n>`                                                  | identifiant Snakemake du job                                           |
| `Job <n> has been submitted with SLURM jobid <id> (log: <path>).` | identifiant Slurm et log Slurm                                         |
| `Finished jobid: <n> (Rule: <nom>)`                               | job terminé avec succès (Snakemake ≥ 8)                                |
| `Finished job <n>.`                                               | variante locale du marqueur de succès                                  |
| `Error in rule <nom>:`                                            | job en échec ; le bloc qui suit contient `jobid`, `input`, `log`, etc. |
| `<n> of <m> steps (<p>%) done`                                    | progression                                                            |

Snakemake préfixe toujours ses blocs par un horodatage (`[Wed Sep 10 ...]`).
Les mises à jour de progression sont des lignes terminées par `\n`.

## 3. Défauts constatés

### 3.1 Le puits de logs n’était pas fiable

`tee_stream_to_file` utilisait `BufReader::read_line`. Cette méthode
exige de l’UTF-8 valide : un seul octet invalide dans la sortie d’un outil
provoquait une erreur, arrêtait la lecture du tube et donc l’analyse, alors que
le log est précisément ce qui permet de diagnostiquer l’échec.

De plus, les lignes stderr étaient envoyées au rapport via un canal borné avec
`try_send`. Quand le canal était plein, les lignes étaient silencieusement
perdues : un en-tête de règle ou un `jobid` manquant suffisait à rendre le
rapport incohérent.

Enfin, `tokio::try_join!` abandonnait l’attente de l’autre flux dès qu’un flux
échouait : le fichier de l’autre flux pouvait ne pas être vidé et
`RunReport::finish()` ne s’exécutait pas.

### 3.2 Le parseur de progression était trop rigide

- Chaque ligne recopiait l’intégralité de `RunProgress` dans le cache.

### 3.3 Le rapport ne contenait pas le statut

La table `snakemake` ne stockait ni le nom de la règle ni le statut. Les lignes
`Finished ...` et `Error in rule ...` étaient ignorées. Il était donc impossible
de savoir, depuis le rapport, quel job avait réussi ou échoué, ni de pointer
directement vers son log.

Les propriétés du bloc `Error in rule ...` étaient volontairement ignorées. Si
le job n’avait pas déjà été enregistré (job local, erreur avant soumission), il
disparaissait complètement du rapport.

### 3.4 Le cache squeue mélangeait les responsabilités et ne cachait pas les échecs

`SqueueCache` était indexé uniquement par nom de job. Il n’était pas rattaché à
`(run_id, attempt_number)`, alors que c’est la clé de suivi de l’interface.

Surtout, seuls les appels réussis étaient mis en cache. Un `squeue`
indisponible, un timeout ou un code de sortie non nul n’était jamais mémorisé :
chaque poll relançait la commande et le cache restait indéfiniment « manqué ».
Le `None` de l’API confondait par ailleurs « pas de nom de job », « squeue en
échec » et « aucun job dans la file ».

Enfin, le navigateur enchaînait lui-même `watch` puis `squeue`, ce qui recouplait
les deux sources et faisait disparaître le suivi dès que le parseur effaçait le
nom Slurm.

## 4. Corrections apportées

### 4.1 Puits de logs sans perte

`tee_stream_to_file` lit désormais des octets avec `read_until(b'\n')`
et écrit ces octets tels quels dans le fichier. L’UTF-8 n’est plus une condition
de succès : le parseur reçoit une conversion `from_utf8_lossy`, tandis que le
fichier reste la source de vérité octet pour octet.

Les erreurs d’écriture sont signalées, mais la lecture du tube continue pour ne
jamais bloquer le processus fils. Le rapport n’utilise plus de canal : chaque
ligne est observée directement après l’écriture du log. Il n’y a donc plus de
lignes perdues ni de copie de ligne.

Les deux tâches de multiplexage sont toujours attendues avec `tokio::join!`,
même si l’une échoue, afin de vider les fichiers et de terminer le rapport.

### 4.2 Cache de progression

`RunProgressCache` possède maintenant le parseur de chaque tentative et applique
les lignes directement dans son état sous verrou. Plus de clone complet de
`RunProgress` à chaque ligne. Les lignes sont traitées telles que Snakemake les
écrit (terminées par `\n`), sans découpage sur `\r`. Un marqueur `## STEP ...`
réinitialise explicitement le nom Slurm : chaque étape correspond à un nouveau
job Slurm et doit publier son propre `SLURM run ID`. Le nom n’est plus supprimé à
100 % ; il n’est remplacé qu’au début d’une nouvelle étape ou d’un nouveau
workflow.

### 4.3 Rapport enrichi

La table `snakemake` contient désormais :

| Colonne                       | Rôle                                                       |
| ----------------------------- | ---------------------------------------------------------- |
| `rule_name`                   | nom de la règle Snakemake                                  |
| `status`                      | `unknown`, `running`, `submitted`, `completed` ou `failed` |
| `slurm_job_name`              | UUID du workflow                                           |
| `slurm_job_id`                | identifiant Slurm du job                                   |
| `snakemake_job_id`            | identifiant Snakemake du job                               |
| `log_paths`                   | logs de la règle + log Slurm                               |
| `input_paths` / `input_sizes` | entrées et tailles en octets                               |
| `wildcards`                   | valeurs des wildcards                                      |

Le rapport est mis à jour de façon incrémentale :

1. un bloc de règle ouvre un job `running` ;
2. une ligne de soumission le passe à `submitted` et ajoute le log Slurm ;
3. `Finished ...` le passe à `completed` ;
4. `Error in rule ...` le passe à `failed` et conserve les propriétés du bloc
   d’erreur.

Les mises à jour sont fusionnées avec la ligne existante : les logs sont unis,
les champs vides ne remplacent jamais une valeur connue et un statut ne régresse
pas. La clé logique est `(slurm_job_name, snakemake_job_id)`.

Un marqueur `## STEP ...` clôt le bloc courant et remet `slurm_job_name` à zéro
dans le rapport aussi. Les jobs de l’étape suivante ne peuvent donc pas hériter
de l’UUID Slurm de l’étape précédente.

Le schéma est migré automatiquement pour les rapports déjà présents.

Le rapport n’est volontairement pas exposé dans l’interface : il est trop coûteux
de lister et de servir tous les logs de toutes les règles à chaque consultation.
La base SQLite est la seule interface ; l’utilisateur la lit après l’exécution
pour produire son propre rapport HTML.

### 4.4 Cache squeue par tentative

`SqueueCache` est maintenant indexé par `(run_id, attempt_number)` et mémorise le
nom de job associé. Il distingue trois résultats :

- `Available { jobs }` : `squeue` a répondu, éventuellement avec une liste vide ;
- `Unavailable { message }` : la commande a échoué, avec un TTL court ;
- `NoJobName` : aucun nom Slurm n’est encore connu.

Les échecs sont donc cachés eux aussi : un poll suivant ne relance pas la
commande immédiatement, mais le cache se répare automatiquement après quelques
secondes. Un changement de nom de job invalide l’entrée précédente.

La route `/smaug/api/squeue` résout le nom de job côté serveur à partir de
`RunProgressCache` ; le navigateur n’a plus à recopier cette information.
