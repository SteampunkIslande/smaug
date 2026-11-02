use crate::config::get_smaug_config;
use crate::models::{FileIdentification, Form};
use crate::models::{Run, RunStatus};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json;
use sqlx::Row;
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::path::PathBuf;
use std::str::FromStr;
use time::OffsetDateTime;

/// Errors of an attempt that callers may want to recognise.
///
/// The attempt functions return `anyhow::Result`; the only error kept here is the
/// one a caller can act upon.///
/// These types are the *named* errors of the layer; the functions themselves
/// return `anyhow::Result`. The `InternalError` variant only exists to give the
/// edge errors (FrontendError / AuthError) a single `#[from] anyhow::Error`
/// conversion point; it is not produced by the model layer itself.
#[derive(wherror::Error, Debug)]
pub enum AttemptError {
    #[error("Aucun run n'est attaché à cette tentative")]
    NoRunAttached,
    #[error("{location}: {source}")]
    InternalError {
        #[from]
        source: anyhow::Error,
        location: &'static std::panic::Location<'static>,
    },
}

/// Created every time we attempt to analyze an HgRun.
///
/// Copies the editable data from HgRun at the time of attempt.
///
/// All of its fields are read-only except for comment and status.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Attempt {
    pub run_id: i64,
    pub attempt_number: u32,
    pub attempt_date: OffsetDateTime,

    pub user_defined_vars: HashMap<String, String>,

    pub form_identification: FileIdentification,

    /// One of the only two editable fields
    pub status: RunStatus,
    /// One of the only two editable fields
    pub comment: String,
}

impl Attempt {
    /// Creates a new attempt from an HgRun
    pub(super) async fn new_attempt(
        run_id: i64,
        current_commit_hash: &str,
        pool: &SqlitePool,
    ) -> Result<()> {
        let run: Run = Run::get_run_from_id(run_id, pool)
            .await
            .with_context(|| format!("Impossible de lire le run {run_id}"))?;

        let FileIdentification::GitBranchAndPath { file_path, .. } = run.form_identification else {
            anyhow::bail!(
                "Une nouvelle tentative ne peut être créée qu'à partir d'un formulaire défini par une branche et un chemin."
            );
        };

        let attempt_date = OffsetDateTime::now_utc();
        let user_defined_vars = serde_json::to_string(&run.user_defined_vars)
            .context("Impossible de sérialiser les variables du run")?;

        sqlx::query(
            r#"
            INSERT INTO Attempts (run_id, attempt_number, attempt_date, user_defined_vars, commit_hash, form_path, status, comment)
            VALUES (?, ?, ?, ?, ?, ?, ?, '')
            "#,
        )
        .bind(run.run_id)
        .bind(run.attempt_count) // No increment, the run we are trying to analyze has already incremented its `attempt_count`
        .bind(attempt_date)
        .bind(user_defined_vars)
        .bind(current_commit_hash)
        .bind(file_path.display().to_string())
        .bind(run.status.to_string())
        .execute(pool)
        .await
        .with_context(|| {
            format!(
                "Impossible d'insérer la tentative {} du run {run_id}",
                run.attempt_count
            )
        })?;

        Ok(())
    }

    /// Creates a hypothetical attempt from an HgRun (without inserting it into the database)
    ///
    /// Used to preview the data of an attempt before actually creating it.
    /// Handy for the API, it standardizes the jinja2 environment.
    pub fn get_hypothetic_attempt(run: &Run) -> Result<Attempt> {
        Ok(Attempt {
            attempt_number: run.attempt_count + 1,
            run_id: run.run_id.context(AttemptError::NoRunAttached)?,
            attempt_date: OffsetDateTime::now_utc(),
            user_defined_vars: run.user_defined_vars.clone(),
            form_identification: run.form_identification.clone(),
            status: RunStatus::Idle,
            comment: String::new(),
        })
    }

    pub async fn get_run(&self, pool: &SqlitePool) -> Result<Run> {
        Run::get_run_from_id(self.run_id, pool)
            .await
            .with_context(|| format!("Impossible de lire le run {}", self.run_id))
    }

    pub async fn get_form(&self) -> Result<Form> {
        Form::get_form(self.form_identification.clone()).await
    }

    /// Auto-computed output directory for this attempt. Single definition point
    /// for the injected `outdir` jinja variable.
    ///
    /// Less complete than attempt_full_name, so that every run ends up in the same folder, no matter how many attempts
    pub fn outdir(&self) -> PathBuf {
        PathBuf::from(&get_smaug_config().analysis_dir).join(format!("run-{:0>8}", self.run_id))
    }

    /// Directory of the curated files listed on the run page.
    pub fn exports_dir(&self) -> PathBuf {
        self.outdir()
            .join(format!("exports-{:0>8}", self.attempt_number))
    }

    /// Directory of the bulk files (logs, intermediates, ...) served by the
    /// files API but deliberately not listed on the run page.
    pub fn logs_dir(&self) -> PathBuf {
        self.outdir()
            .join(format!("logs-{:0>8}", self.attempt_number))
    }

    pub fn attempt_full_name(&self) -> String {
        format!(
            "run-{:0>8}-tentative-{:0>8}",
            self.run_id, self.attempt_number
        )
    }

    /// Path of the sqlite report describing the snakemake jobs of this attempt.
    pub fn run_report_path(&self) -> PathBuf {
        self.outdir().join(format!(
            "smaug-run-report-attempt-{}.db",
            self.attempt_number
        ))
    }

    pub async fn render_script(&self, run_name: &str) -> Result<String> {
        let form = self.get_form().await?;
        let outdir = self.outdir().display().to_string();
        let exports_dir = self.exports_dir().display().to_string();
        let logs_dir = self.logs_dir().display().to_string();
        // Add { user defined variables } as well as run-related mandatory/convenient variables: [attempt number, output directory (that will be $PWD), export and log directories, run number]
        let ctx = form.jinja_context(
            &self.user_defined_vars,
            self.attempt_number,
            &outdir,
            &exports_dir,
            &logs_dir,
            self.run_id,
            run_name,
        );
        form.render_template(&ctx).await
    }

    /// Retrieves an HgAttempt from its attempt_number and run_id
    pub async fn get_attempt_from_number(
        attempt_number: u32,
        run_id: i64,
        pool: &SqlitePool,
    ) -> Result<Self> {
        let row = sqlx::query(
            r#"
            SELECT * FROM Attempts WHERE attempt_number = ? AND run_id = ?
            "#,
        )
        .bind(attempt_number)
        .bind(run_id)
        .fetch_one(pool)
        .await
        .with_context(|| {
            format!("Impossible de lire la tentative {attempt_number} du run {run_id}")
        })?;

        // Deserialize the user-defined variables
        let user_defined_vars: HashMap<String, String> =
            serde_json::from_str(row.try_get("user_defined_vars")?)
                .context("Impossible de désérialiser les variables de la tentative")?;

        let status = RunStatus::from_str(row.try_get::<String, _>("status")?.as_str())
            .map_err(anyhow::Error::from)
            .context("Statut de tentative invalide")?;

        // Build the HgAttempt instance
        let attempt = Attempt {
            attempt_number,
            run_id,
            attempt_date: row.try_get("attempt_date")?,
            user_defined_vars,
            form_identification: FileIdentification::GitCommit {
                commit_hash: row.try_get("commit_hash")?,
                file_path: PathBuf::from_str(row.try_get("form_path")?)
                    .context("Chemin de formulaire invalide")?,
            },
            status,
            comment: row.try_get("comment")?,
        };

        Ok(attempt)
    }

    pub async fn get_pending_runs(pool: &SqlitePool) -> Result<Vec<Self>> {
        let rows = sqlx::query(
            r#"
            SELECT * FROM Attempts WHERE status = 'Pending' ORDER BY attempt_number DESC
            "#,
        )
        .fetch_all(pool)
        .await
        .context("Impossible de lister les tentatives en attente")?;

        Ok(rows
            .into_iter()
            .filter_map(|row| {
                Some(Attempt {
                    attempt_number: row.try_get("attempt_number").ok()?,
                    run_id: row.try_get("run_id").ok()?,
                    attempt_date: row.try_get("attempt_date").ok()?,
                    user_defined_vars: serde_json::from_str(row.try_get("user_defined_vars").ok()?)
                        .ok()?,
                    form_identification: FileIdentification::GitCommit {
                        commit_hash: row.try_get("commit_hash").ok()?,
                        file_path: PathBuf::from_str(row.try_get("form_path").ok()?).ok()?,
                    },
                    status: RunStatus::Pending,
                    comment: row.try_get("comment").ok()?,
                })
            })
            .collect())
    }

    /// Retrieves all attempts for a run_id
    pub async fn list_attempts_for_run(run_id: i64, pool: &SqlitePool) -> Result<Vec<Self>> {
        let rows = sqlx::query(
            r#"
            SELECT * FROM Attempts WHERE run_id = ? ORDER BY attempt_number DESC
            "#,
        )
        .bind(run_id)
        .fetch_all(pool)
        .await
        .with_context(|| format!("Impossible de lister les tentatives du run {run_id}"))?;

        Ok(rows
            .into_iter()
            .filter_map(|row| {
                // Deserialize the user-defined variables
                let user_defined_vars: HashMap<String, String> =
                    serde_json::from_str(row.try_get("user_defined_vars").ok()?).ok()?;

                // Parse the status
                let status_str: String = row.try_get("status").ok()?;
                let status = RunStatus::from_str(status_str.as_str()).ok()?;
                Some(Attempt {
                    attempt_number: row.try_get("attempt_number").ok()?,
                    run_id: row.try_get("run_id").ok()?,
                    attempt_date: row.try_get("attempt_date").ok()?,
                    user_defined_vars,
                    form_identification: FileIdentification::GitCommit {
                        commit_hash: row.try_get("commit_hash").ok()?,
                        file_path: PathBuf::from_str(row.try_get("form_path").ok()?).ok()?,
                    },
                    status,
                    comment: row.try_get("comment").ok()?,
                })
            })
            .collect())
    }

    pub(super) async fn start_run(run_id: i64, pool: &SqlitePool) -> Result<()> {
        let run = Run::get_run_from_id(run_id, pool)
            .await
            .with_context(|| format!("Impossible de lire le run {run_id}"))?;
        sqlx::query(r#"UPDATE Attempts SET status = ? WHERE run_id = ? AND attempt_number = ?"#)
            .bind("Running")
            .bind(run_id)
            .bind(run.attempt_count)
            .execute(pool)
            .await
            .with_context(|| {
                format!(
                    "Impossible de démarrer la tentative {} du run {run_id}",
                    run.attempt_count
                )
            })?;
        Ok(())
    }

    /// The attempt `attempt_number` completed successfully.
    pub(super) async fn complete_success(run_id: i64, pool: &SqlitePool) -> Result<()> {
        let run = Run::get_run_from_id(run_id, pool)
            .await
            .with_context(|| format!("Impossible de lire le run {run_id}"))?;
        sqlx::query(
            r#"
            UPDATE Attempts SET status = ? WHERE run_id = ? AND attempt_number = ?
            "#,
        )
        .bind("Success")
        .bind(run_id)
        .bind(run.attempt_count)
        .execute(pool)
        .await
        .with_context(|| {
            format!(
                "Impossible de terminer avec succès la tentative {} du run {run_id}",
                run.attempt_count
            )
        })?;
        Ok(())
    }

    /// The attempt `attempt_number` failed.
    pub(super) async fn complete_failure(
        run_id: i64,
        reason: &str,
        pool: &SqlitePool,
    ) -> Result<()> {
        let run = Run::get_run_from_id(run_id, pool)
            .await
            .with_context(|| format!("Impossible de lire le run {run_id}"))?;
        sqlx::query(
            r#"
            UPDATE Attempts SET status = ? WHERE run_id = ? AND attempt_number = ?
            "#,
        )
        .bind(format!("Failure:{}", reason))
        .bind(run_id)
        .bind(run.attempt_count)
        .execute(pool)
        .await
        .with_context(|| {
            format!(
                "Impossible de marquer en échec la tentative {} du run {run_id}",
                run.attempt_count
            )
        })?;
        Ok(())
    }

    /// Updates the comment of an attempt
    pub(super) async fn update_comment(
        run_id: i64,
        attempt_number: u32,
        new_comment: &str,
        pool: &SqlitePool,
    ) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE Attempts SET comment = ? WHERE attempt_number = ? AND run_id = ?
            "#,
        )
        .bind(new_comment)
        .bind(attempt_number)
        .bind(run_id)
        .execute(pool)
        .await
        .with_context(|| {
            format!("Impossible de commenter la tentative {attempt_number} du run {run_id}")
        })?;
        Ok(())
    }
}
