use anyhow::{Context, Result, bail};
use log::{error, info, warn};
use smaug::config::get_smaug_config;
use smaug::models::{
    AttemptError, FormDefinitionError, RunDefinitionError, SigintBehavior, analysis, appstate,
};
use smaug_lib::pipeline_exec::{LogProgressParser, RunProgressCache, RunReport, versionning};
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::task::JoinSet;

use sqlx::SqlitePool;

use smaug_lib::models::Attempt;

use tokio::process::{Child, Command};
use tokio::sync::broadcast;
use tokio::time::{Duration, interval};

pub async fn run_routine_loop(
    pool: &SqlitePool,
    progress_cache: RunProgressCache,
    mut routine_shutdown_rx: broadcast::Receiver<()>,
) -> Result<()> {
    // Channel used to notify the child processes started by the routine
    let (child_shutdown_tx, _) = broadcast::channel(1);
    let mut join_set = JoinSet::new();

    let mut interval = interval(Duration::from_secs(10));

    loop {
        tokio::select! {

            _ = routine_shutdown_rx.recv() => {
                info!("Signal d'arrêt reçu par la routine...");
                break;
            }

            _ = interval.tick() => {
                if !matches!(appstate::AppState::get(pool).await.unwrap_or_default(), appstate::ActiveRoutine::RoutineOn) {
                    continue;
                }
                match find_run_to_launch(pool).await{
                    Ok(mut runs_to_launch) => {
                        while let Some(attempt) = runs_to_launch.pop() {
                            let child_rx = child_shutdown_tx.subscribe();

                            // 1. Clone the pool handle. This is cheap (Arc clone under the hood).
                            let pool_clone = pool.clone();
                            let progress_cache_clone = progress_cache.clone();

                            // 2. Move the cloned handle into the spawned task
                            join_set.spawn(async move {
                                if let Err(e) = analysis::start_run_analysis(attempt.run_id, &pool_clone).await {
                                    error!("Impossible de démarrer la tentative {} du run {}: {e:#}", attempt.attempt_number, attempt.run_id);
                                    if let Err(db_error) = analysis::fail_cannot_analyse_run(attempt.run_id, &format!("{e:#}"), &pool_clone).await {
                                        error!("Impossible d'enregistrer l'échec du run {}: {db_error:#}", attempt.run_id);
                                    }
                                    return;
                                }

                                info!("Lancement de la tentative {} du run {}", attempt.attempt_number, attempt.run_id);

                                let script_result = run_script(&attempt, &pool_clone, progress_cache_clone, child_rx).await;
                                if let Err(e) = finish_attempt(attempt.run_id, script_result, &pool_clone).await {
                                    error!(
                                        "Impossible de conclure la tentative {} du run {}: {e:#}",
                                        attempt.attempt_number, attempt.run_id
                                    );
                                }
                            });
                        }
                    },
                    Err(e) => {
                        error!("Something serious: cannot run 'find runs to launch' {}", e);
                    }
                }
            }
        }
    }

    match child_shutdown_tx.send(()) {
        Ok(n) => {
            info!(
                "{} jobs sont encore en cours, leur task tokio est informée.",
                n
            );
        }
        Err(_) => {
            info!("Aucun job n'est en train d'écouter");
        }
    }

    if join_set.is_empty() {
        info!("Aucun job n'était en train de tourner");
    } else {
        info!("Attente de l'arrêt propre des jobs encore en cours");
        while let Some(res) = join_set.join_next().await {
            if let Err(e) = res {
                warn!("Un des jobs s'est terminé avec une erreur : {}", e);
            }
        }
        info!("Tous les jobs ont été arrêtés.");
    }
    info!("Fin de la routine");

    Ok(())
}

async fn find_run_to_launch(pool: &SqlitePool) -> Result<Vec<Attempt>> {
    Attempt::get_pending_runs(pool)
        .await
        .context("Impossible de récupérer les tentatives à lancer")
}

/// Records the outcome of `run_script` in the database.
///
/// `run_script` never writes to the database itself: deciding whether the
/// attempt succeeded, failed mid-analysis, or could not even be launched is the
/// caller's responsibility. Preparation failures (git, I/O, form resolution, …)
/// happen before the analysis process exists, so they are reported as
/// "cannot analyse"; everything else is a regular analysis failure.
async fn finish_attempt(run_id: i64, result: Result<()>, pool: &SqlitePool) -> Result<()> {
    match result {
        Ok(()) => analysis::complete_success(run_id, pool).await,
        Err(e) => {
            let reason = format!("{e:#}");
            if is_preparation_error(&e) {
                analysis::fail_cannot_analyse_run(run_id, &reason, pool).await
            } else {
                analysis::complete_failure(run_id, &reason, pool).await
            }
        }
    }
}

/// Returns true when an error happened before the analysis process was started.
///
/// Those errors mean the run could not be analysed at all, which is distinct
/// from an analysis that ran and failed.
fn is_preparation_error(error: &anyhow::Error) -> bool {
    error.downcast_ref::<versionning::GitCheckError>().is_some()
        || error.downcast_ref::<std::io::Error>().is_some()
        || error.downcast_ref::<FormDefinitionError>().is_some()
        || error.downcast_ref::<AttemptError>().is_some()
        || error.downcast_ref::<RunDefinitionError>().is_some()
}

async fn send_sigint_then_kill(
    child: &mut Child,
    grace_time_seconds: Option<u64>,
) -> Result<ExitStatus> {
    use nix::errno::Errno;
    use nix::sys::signal::{Signal, kill, killpg};
    use nix::unistd::{Pid, getpgid};

    if let Some(status) = child
        .try_wait()
        .context("Impossible de vérifier l'état du processus")?
    {
        return Ok(status);
    }

    let pid = child
        .id()
        .context("Le processus n'a pas de PID (probablement déjà récupéré)")?;
    let pid_i32: i32 = pid.try_into().context("PID invalide")?;
    let nix_pid = Pid::from_raw(pid_i32);

    // Determine if the child is a process group leader
    let is_group_leader = match getpgid(Some(nix_pid)) {
        Ok(pgid) => pgid == nix_pid,
        Err(Errno::ESRCH) => {
            // Process died before we could check its PGID.
            // We set this to false, the subsequent signal will harmlessly fail with ESRCH.
            false
        }
        Err(e) => bail!(
            "Impossible d'obtenir l'identifiant du groupe de processus pour {}: {}",
            pid,
            e
        ),
    };

    // 1. Send SIGINT
    let sigint_result = if is_group_leader {
        killpg(nix_pid, Signal::SIGINT)
    } else {
        kill(nix_pid, Signal::SIGINT)
    };

    if let Err(e) = sigint_result {
        if e != Errno::ESRCH {
            bail!(
                "Impossible d'envoyer SIGINT au groupe de processus {}: {}",
                pid,
                e
            );
        }
    }

    let grace_time = Duration::from_secs(grace_time_seconds.unwrap_or(5));
    if let Ok(wait_result) = tokio::time::timeout(grace_time, child.wait()).await {
        return wait_result.context("Erreur en attendant le processus après SIGINT");
    }

    // 2. Timeout expired: send SIGKILL
    if is_group_leader {
        if let Err(e) = killpg(nix_pid, Signal::SIGKILL) {
            if e != Errno::ESRCH {
                bail!(
                    "Impossible d'envoyer SIGKILL au groupe de processus {}: {}",
                    pid,
                    e
                );
            }
        }
    } else {
        // Standard fallback for single process
        child
            .kill()
            .await
            .context("Impossible d'envoyer SIGKILL au processus")?;
    }

    // 3. Harvest the process
    child
        .wait()
        .await
        .context("Erreur en récupérant le processus après SIGKILL")
}

async fn tee_stream_to_file_and_cache<R>(
    stream: R,
    file_path: &Path,
    progress_cache: RunProgressCache,
    run_id: i64,
    attempt_number: u32,
    stream_type: &'static str,
    run_report: Option<RunReport>,
) -> Result<()>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    let mut file = tokio::fs::File::create(file_path).await.with_context(|| {
        format!(
            "Impossible de créer le fichier de log {}",
            file_path.display()
        )
    })?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();

    let mut parser = if stream_type == "stderr" {
        let parser = LogProgressParser::new(run_id, attempt_number);
        progress_cache.upsert(parser.state().clone()).await;
        Some(parser)
    } else {
        None
    };

    let (report_tx, report_handle) = match run_report {
        Some(report) => {
            let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(4096);
            let handle = tokio::spawn(async move {
                while let Some(line) = rx.recv().await {
                    report.observe(&line).await;
                }
                report.finish().await;
            });
            (Some(tx), Some(handle))
        }
        None => (None, None),
    };

    let read_result: Result<()> = async {
        loop {
            line.clear();
            let bytes_read = reader
                .read_line(&mut line)
                .await
                .with_context(|| format!("Impossible de lire la sortie {}", stream_type))?;
            if bytes_read == 0 {
                break;
            }

            file.write_all(line.as_bytes())
                .await
                .with_context(|| format!("Impossible d'écrire dans {}", file_path.display()))?;

            if let Some(parser) = parser.as_mut() {
                parser.parse_line(&line);
                progress_cache.upsert(parser.state().clone()).await;
            }

            // Reporting is best-effort: a slow report must never backpressure the
            // analysis by blocking this stderr drain loop.
            if let Some(tx) = report_tx.as_ref() {
                let _ = tx.try_send(line.clone());
            }
        }
        Ok(())
    }
    .await;

    drop(report_tx);
    if let Some(handle) = report_handle
        && let Err(e) = handle.await
    {
        warn!("La tâche d'écriture du rapport d'exécution a échoué: {e}");
    }
    read_result?;

    file.flush()
        .await
        .with_context(|| format!("Impossible de vider le tampon de {}", file_path.display()))?;
    Ok(())
}

/// This function may panic or return an error; the consequences are minimal.
///
/// A panic only affects one tokio task among others, part of the routine's global
/// JoinSet (the set that keeps track of every running job).
async fn run_script(
    attempt: &Attempt,
    pool: &SqlitePool,
    progress_cache: RunProgressCache,
    mut shutdown_rx: broadcast::Receiver<()>,
) -> Result<()> {
    let (git_manager, scripts_dir, logs_dir) = {
        let config = get_smaug_config();

        let scripts_dir = PathBuf::from(&config.scripts_dir);
        let logs_dir = PathBuf::from(&config.logs_dir);

        ensure_absolute_dir(&scripts_dir, "scripts_dir").await?;
        ensure_absolute_dir(&logs_dir, "logs_dir").await?;

        (config.pipelines, scripts_dir, logs_dir)
    };

    let script_id = attempt.attempt_full_name();
    let script_path = scripts_dir.join(format!("{}.sh", script_id));
    let stdout_path = logs_dir.join(format!("{}.stdout", script_id));
    let stderr_path = logs_dir.join(format!("{}.stderr", script_id));

    let run = attempt.get_run(pool).await?;
    let rendered = attempt
        .render_script(&run.run_name)
        .await
        .with_context(|| format!("Impossible de rendre le script de la tentative {script_id}"))?;

    // Async FS operations
    tokio::fs::create_dir_all(attempt.outdir())
        .await
        .context("Impossible de créer le dossier outdir")?;
    tokio::fs::create_dir_all(attempt.exports_dir())
        .await
        .context("Impossible de créer le dossier d'exportations")?;
    tokio::fs::create_dir_all(attempt.logs_dir())
        .await
        .context("Impossible de créer le dossier de logs")?;
    tokio::fs::write(&script_path, &rendered)
        .await
        .context("Impossible d'écrire le script rendu")?;

    // The report is written while the analysis runs; failing to create it must
    // not prevent the analysis from starting.
    let run_report = match RunReport::create(attempt).await {
        Ok(report) => Some(report),
        Err(e) => {
            warn!(
                "Impossible de créer le rapport d'exécution de la tentative {} du run {}: {e:#}",
                attempt.attempt_number, attempt.run_id
            );
            None
        }
    };

    // Clones the whole repo into the working directory. Now, PWD of the rendered script contains a `pipelines` directory.
    versionning::git_clone_to(&attempt.outdir(), &attempt.form_identification, git_manager).await?;

    let mut child = Command::new("bash")
        .arg("-l")
        .arg(&script_path)
        .current_dir(attempt.outdir())
        .process_group(0)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| {
            format!(
                "Erreur de lancement pour la tentative {} du run {}",
                attempt.attempt_number, attempt.run_id
            )
        })?;

    let stdout = child
        .stdout
        .take()
        .context("Impossible de récupérer la sortie standard")?;
    let stderr = child
        .stderr
        .take()
        .context("Impossible de récupérer la sortie d'erreur")?;

    let run_id = attempt.run_id;
    let attempt_number = attempt.attempt_number;
    let progress_cache_stdout = progress_cache.clone();
    let progress_cache_stderr = progress_cache;

    // Spawn multiplexers
    let stdout_handle = tokio::spawn(async move {
        tee_stream_to_file_and_cache(
            stdout,
            &stdout_path,
            progress_cache_stdout,
            run_id,
            attempt_number,
            "stdout",
            None,
        )
        .await
    });

    let stderr_handle = tokio::spawn(async move {
        tee_stream_to_file_and_cache(
            stderr,
            &stderr_path,
            progress_cache_stderr,
            run_id,
            attempt_number,
            "stderr",
            run_report,
        )
        .await
    });

    // Race between the end of the script and the shutdown signal.
    tokio::select! {
        status_res = child.wait() => {
            // Wait for the log multiplexers to flush before returning.
            async {
                let (stdout_result, stderr_result) =
                    tokio::try_join!(stdout_handle, stderr_handle)?;
                stdout_result?;
                stderr_result?;
                Ok::<(), anyhow::Error>(())
            }
            .await
            .map_err(|e| anyhow::anyhow!("Erreur lors de la lecture des logs: {e:#}"))?;
            match status_res {
                Ok(status) if status.success() => Ok(()),
                Ok(status) => Err(exit_status_error(status)),
                Err(e) => Err(e).context("Erreur en attendant la fin du script"),
            }
        }

        _ = shutdown_rx.recv() => {
            info!("La tentative {} du run {} a reçu un signal d'arrêt prématuré", attempt.attempt_number, attempt.run_id);
            let sigint_behavior: appstate::SigintBehavior = appstate::AppState::get(pool).await?;

            let outcome = match sigint_behavior {
                SigintBehavior::StopJobs => {
                    info!("Le job va être arrêté par SIGINT, 5s avant SIGKILL.");
                    match send_sigint_then_kill(&mut child, None).await {
                        Ok(status) if status.success() => Ok(()),
                        Ok(status) => Err(exit_status_error(status)),
                        Err(e) => Err(e).context("Erreur lors de l'envoi du signal d'arrêt"),
                    }
                }
                SigintBehavior::WaitForJobCompletion => {
                    info!("Attente de la fin du job avant l'arrêt...");
                    match child.wait().await {
                        Ok(status) if status.success() => Ok(()),
                        Ok(status) => Err(exit_status_error(status)),
                        Err(e) => Err(e).context("Erreur en attendant la fin du job"),
                    }
                }
            };

            // Crucial: wait for the logs to finish capturing the shutdown process.
            let logs_result: Result<()> = async {
                let (stdout_result, stderr_result) =
                    tokio::try_join!(stdout_handle, stderr_handle)?;
                stdout_result?;
                stderr_result?;
                Ok::<(), anyhow::Error>(())
            }
            .await
            .map_err(|e| anyhow::anyhow!("Erreur lors de la lecture des logs: {e:#}"));
            outcome.and(logs_result)
        }
    }
}

/// Builds the error describing an analysis process that exited with a failure.
fn exit_status_error(status: ExitStatus) -> anyhow::Error {
    let code = status
        .code()
        .map(|c| c.to_string())
        .unwrap_or_else(|| "?".to_string());
    anyhow::anyhow!("(exit code {})", code)
}

// Helper function to deduplicate directory validation
async fn ensure_absolute_dir(path: &Path, name: &str) -> Result<()> {
    if !path.is_absolute() {
        return Err(anyhow::anyhow!(
            "Le dossier `{}` est mal configuré, un chemin absolu est requis",
            name
        ));
    }
    if !path.exists() {
        tokio::fs::create_dir_all(path)
            .await
            .with_context(|| format!("Impossible de créer le dossier {}", name))?;
    }
    Ok(())
}
