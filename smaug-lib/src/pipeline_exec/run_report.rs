use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use log::warn;
use sqlx::Row;
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::Mutex;

use crate::models::Attempt;

/// State of a Snakemake job in the execution report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RuleStatus {
    #[default]
    Unknown,
    Running,
    Submitted,
    Completed,
    Failed,
}

impl RuleStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Running => "running",
            Self::Submitted => "submitted",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }

    fn from_db(value: &str) -> Self {
        match value {
            "running" => Self::Running,
            "submitted" => Self::Submitted,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            _ => Self::Unknown,
        }
    }

    /// A later, more advanced state must not be replaced by a weaker one.
    fn priority(self) -> u8 {
        match self {
            Self::Unknown => 0,
            Self::Running => 1,
            Self::Submitted => 2,
            Self::Completed => 3,
            Self::Failed => 4,
        }
    }

    fn merge(self, other: Self) -> Self {
        if other.priority() >= self.priority() {
            other
        } else {
            self
        }
    }
}

/// Incrementally records the snakemake jobs of an attempt in a sqlite report.
///
/// The report is written while the analysis runs, from the lines snakemake
/// writes on stderr. It lives next to the attempt's output so that it survives
/// the run and can be inspected afterwards.
#[derive(Clone)]
pub struct RunReport {
    inner: Arc<RunReportInner>,
}

struct RunReportInner {
    pool: SqlitePool,
    outdir: PathBuf,
    state: Mutex<ReportState>,
}

#[derive(Default)]
struct ReportState {
    /// UUID of the current snakemake workflow, emitted as `SLURM run ID: <uuid>`.
    slurm_job_name: Option<String>,
    /// Job block currently being printed by snakemake.
    block: Option<JobBlock>,
}

#[derive(Default)]
struct JobBlock {
    inputs: Vec<String>,
    log_paths: Vec<String>,
    wildcards: Vec<String>,
    jobid: Option<i64>,
    slurm_job_id: Option<String>,
    rule_name: Option<String>,
    status: RuleStatus,
    field: Option<BlockField>,
}

#[derive(Clone, Copy)]
enum BlockField {
    Inputs,
    Logs,
}

struct SlurmSubmission {
    snakemake_job_id: i64,
    slurm_job_id: String,
    log_path: Option<String>,
}

struct FinishedJob {
    jobid: i64,
    rule_name: Option<String>,
}

impl RunReport {
    /// Creates the report database in the attempt output directory and prepares
    /// its schema.
    pub async fn create(attempt: &Attempt) -> Result<Self> {
        let path = attempt.run_report_path();
        let pool = open_pool(&path, true).await.with_context(|| {
            format!(
                "Impossible de créer le rapport d'exécution {}",
                path.display()
            )
        })?;
        ensure_schema(&pool).await?;

        Ok(Self {
            inner: Arc::new(RunReportInner {
                pool,
                outdir: attempt.outdir(),
                state: Mutex::new(ReportState::default()),
            }),
        })
    }

    /// Consumes one stderr line. Failures are logged and never stop the analysis.
    pub async fn observe(&self, raw_line: &str) {
        let line = strip_ansi(raw_line);
        if line.trim().is_empty() {
            return;
        }

        let mut state = self.inner.state.lock().await;
        if let Err(e) = self.observe_locked(&mut state, &line).await {
            warn!("Impossible d'écrire le rapport d'exécution: {e:#}");
        }
    }

    /// Persists the last job block once snakemake has stopped writing.
    pub async fn finish(&self) {
        let mut state = self.inner.state.lock().await;
        if let Err(e) = self.flush_block(&mut state).await {
            warn!("Impossible d'écrire le rapport d'exécution: {e:#}");
        }
    }

    async fn observe_locked(&self, state: &mut ReportState, line: &str) -> Result<()> {
        let trimmed = line.trim();

        if is_step_marker(trimmed) {
            // Each step starts a new Slurm job (usually a new Snakemake
            // invocation). Close the previous block and forget the previous
            // run ID: the next workflow must publish its own `SLURM run ID`.
            self.flush_block(state).await?;
            state.slurm_job_name = None;
            return Ok(());
        }

        if let Some(job_name) = parse_slurm_run_id(trimmed) {
            if state.slurm_job_name.as_deref() != Some(job_name.as_str()) {
                // The previous workflow, if any, must not inherit the jobs of
                // the next one.
                self.flush_block(state).await?;
                state.slurm_job_name = Some(job_name);
            }
            return Ok(());
        }

        if let Some(submission) = parse_slurm_submission(trimmed) {
            let mut block = match state.block.take() {
                Some(block)
                    if block.jobid.is_none()
                        || block.jobid == Some(submission.snakemake_job_id) =>
                {
                    block
                }
                Some(other) => {
                    self.persist(state, other).await?;
                    JobBlock::default()
                }
                None => JobBlock::default(),
            };

            block.jobid = Some(submission.snakemake_job_id);
            block.slurm_job_id = Some(submission.slurm_job_id);
            block.status = block.status.merge(RuleStatus::Submitted);
            if let Some(path) = submission.log_path {
                block.log_paths.push(path);
            }
            self.persist(state, block).await?;
            return Ok(());
        }

        if let Some(finished) = parse_finished_job(trimmed) {
            let mut block = match state.block.take() {
                Some(block) if block.jobid.is_none() || block.jobid == Some(finished.jobid) => {
                    block
                }
                Some(other) => {
                    self.persist(state, other).await?;
                    JobBlock::default()
                }
                None => JobBlock::default(),
            };

            block.jobid = Some(finished.jobid);
            block.status = block.status.merge(RuleStatus::Completed);
            if let Some(rule_name) = finished.rule_name {
                block.rule_name = Some(rule_name);
            }
            self.persist(state, block).await?;
            return Ok(());
        }

        if let Some(rule_name) = parse_error_rule_header(trimmed) {
            // Keep the block open: the properties that follow (`jobid`, `log`,
            // `input`, ...) complete or correct what was seen before the error.
            let mut block = state.block.take().unwrap_or_default();
            block.rule_name = Some(rule_name);
            block.status = block.status.merge(RuleStatus::Failed);
            state.block = Some(block);
            return Ok(());
        }

        if let Some(rule_name) = parse_rule_header(trimmed) {
            self.flush_block(state).await?;
            state.block = Some(JobBlock {
                rule_name: Some(rule_name),
                status: RuleStatus::Running,
                ..JobBlock::default()
            });
            return Ok(());
        }

        let Some(block) = state.block.as_mut() else {
            return Ok(());
        };

        if let Some((key, value)) = parse_block_property(line) {
            match key {
                "input" => {
                    block.field = Some(BlockField::Inputs);
                    append_values(&mut block.inputs, value);
                }
                "log" => {
                    block.field = Some(BlockField::Logs);
                    append_values(&mut block.log_paths, value);
                }
                "wildcards" => {
                    block.field = None;
                    block.wildcards.extend(wildcard_values(value));
                }
                "jobid" => {
                    block.field = None;
                    block.jobid = value.trim().parse().ok();
                }
                _ => {
                    block.field = None;
                }
            }
            return Ok(());
        }

        // Continuation line of a multi-line `input:` or `log:` property.
        if is_indented(line)
            && let Some(field) = block.field
        {
            match field {
                BlockField::Inputs => append_values(&mut block.inputs, line),
                BlockField::Logs => append_values(&mut block.log_paths, line),
            }
        }

        Ok(())
    }

    async fn flush_block(&self, state: &mut ReportState) -> Result<()> {
        if let Some(block) = state.block.take() {
            self.persist(state, block).await?;
        }
        Ok(())
    }

    async fn persist(&self, state: &ReportState, block: JobBlock) -> Result<()> {
        let Some(snakemake_job_id) = block.jobid else {
            return Ok(());
        };

        let workflow = state.slurm_job_name.clone().unwrap_or_default();
        let rule_name = block.rule_name.unwrap_or_default();

        let mut log_paths = block
            .log_paths
            .iter()
            .map(|path| absolutize(&self.inner.outdir, path))
            .collect::<Vec<_>>();
        log_paths.sort();
        log_paths.dedup();

        let input_paths = block
            .inputs
            .iter()
            .map(|path| absolutize(&self.inner.outdir, path))
            .collect::<Vec<_>>();
        let mut input_sizes = Vec::with_capacity(input_paths.len());
        for path in &input_paths {
            let size = tokio::fs::metadata(path)
                .await
                .map(|meta| meta.len().to_string())
                .unwrap_or_else(|_| "0".to_string());
            input_sizes.push(size);
        }

        let new_log_paths = join_paths(&log_paths);
        let new_input_paths = join_paths(&input_paths);
        let new_input_sizes = input_sizes.join(",");

        // TODO: Not necessary, should get around this: all the necessary information come as a block (all the lines get written to stderr in the same time...)
        let existing = sqlx::query(
            r#"
            SELECT id, slurm_job_id, rule_name, status, log_paths, wildcards,
                   input_paths, input_sizes
            FROM snakemake
            WHERE slurm_job_name = ? AND snakemake_job_id = ?
            ORDER BY id
            LIMIT 1
            "#,
        )
        .bind(&workflow)
        .bind(snakemake_job_id)
        .fetch_optional(&self.inner.pool)
        .await
        .context("Impossible de chercher le job dans le rapport d'exécution")?;

        if let Some(row) = existing {
            let row_id: i64 = row.try_get("id")?;
            let old_status = RuleStatus::from_db(&row.try_get::<String, _>("status")?);
            let status = old_status.merge(block.status);

            let old_rule_name: String = row.try_get("rule_name")?;
            let rule_name = if rule_name.is_empty() {
                old_rule_name
            } else {
                rule_name
            };

            let slurm_job_id: Option<String> = block
                .slurm_job_id
                .or_else(|| row.try_get("slurm_job_id").ok().flatten());

            let mut merged_logs = split_list(&row.try_get::<String, _>("log_paths")?);
            merged_logs.extend(split_list(&new_log_paths));
            merged_logs.sort();
            merged_logs.dedup();

            let mut merged_wildcards = split_list(&row.try_get::<String, _>("wildcards")?);
            merged_wildcards.extend(block.wildcards.iter().cloned());
            merged_wildcards.dedup();

            let old_input_paths: String = row.try_get("input_paths")?;
            let old_input_sizes: String = row.try_get("input_sizes")?;
            let (input_paths, input_sizes) = if new_input_paths.is_empty() {
                (old_input_paths, old_input_sizes)
            } else if new_input_paths == old_input_paths {
                // A duplicated block (typically the error report) carries the
                // same inputs: keep the sizes measured when they still existed.
                (new_input_paths, old_input_sizes)
            } else {
                (new_input_paths, new_input_sizes)
            };

            sqlx::query(
                r#"
                UPDATE snakemake
                SET slurm_job_id = ?, rule_name = ?, status = ?, log_paths = ?,
                    wildcards = ?, input_paths = ?, input_sizes = ?
                WHERE id = ?
                "#,
            )
            .bind(&slurm_job_id)
            .bind(&rule_name)
            .bind(status.as_str())
            .bind(merged_logs.join(","))
            .bind(merged_wildcards.join(","))
            .bind(&input_paths)
            .bind(&input_sizes)
            .bind(row_id)
            .execute(&self.inner.pool)
            .await
            .context("Impossible de mettre à jour le rapport d'exécution")?;
        } else {
            let wildcards = block.wildcards.join(",");
            sqlx::query(
                r#"
                INSERT INTO snakemake
                    (slurm_job_name, slurm_job_id, snakemake_job_id, rule_name,
                     status, log_paths, wildcards, input_paths, input_sizes)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                "#,
            )
            .bind(&workflow)
            .bind(&block.slurm_job_id)
            .bind(snakemake_job_id)
            .bind(&rule_name)
            .bind(block.status.as_str())
            .bind(&new_log_paths)
            .bind(&wildcards)
            .bind(&new_input_paths)
            .bind(&new_input_sizes)
            .execute(&self.inner.pool)
            .await
            .context("Impossible d'insérer dans le rapport d'exécution")?;
        }

        Ok(())
    }
}

async fn open_pool(path: &Path, create_if_missing: bool) -> Result<SqlitePool> {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(create_if_missing)
        .busy_timeout(Duration::from_secs(5));
    SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .context("Impossible d'ouvrir la base du rapport d'exécution")
}

async fn ensure_schema(pool: &SqlitePool) -> Result<()> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS snakemake (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            slurm_job_name TEXT,
            slurm_job_id TEXT,
            snakemake_job_id INTEGER NOT NULL,
            rule_name TEXT NOT NULL DEFAULT '',
            status TEXT NOT NULL DEFAULT 'unknown',
            log_paths TEXT NOT NULL DEFAULT '',
            wildcards TEXT NOT NULL DEFAULT '',
            input_paths TEXT NOT NULL DEFAULT '',
            input_sizes TEXT NOT NULL DEFAULT ''
        )
        "#,
    )
    .execute(pool)
    .await
    .context("Impossible de créer la table `snakemake` du rapport d'exécution")?;

    // Reports created before the status column was introduced are migrated in
    // place: they remain readable and new updates can enrich them.
    let columns: HashSet<String> = sqlx::query("PRAGMA table_info(snakemake)")
        .fetch_all(pool)
        .await
        .context("Impossible de lire le schéma du rapport d'exécution")?
        .into_iter()
        .filter_map(|row| row.try_get::<String, _>("name").ok())
        .collect();

    for (name, definition) in [
        ("rule_name", "TEXT NOT NULL DEFAULT ''"),
        ("status", "TEXT NOT NULL DEFAULT 'unknown'"),
    ] {
        if !columns.contains(name) {
            sqlx::query(&format!(
                "ALTER TABLE snakemake ADD COLUMN {name} {definition}"
            ))
            .execute(pool)
            .await
            .with_context(|| format!("Impossible d'ajouter la colonne {name} au rapport"))?;
        }
    }

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_snakemake_job ON snakemake(slurm_job_name, snakemake_job_id)",
    )
    .execute(pool)
    .await
    .context("Impossible de créer l'index du rapport d'exécution")?;

    Ok(())
}

fn parse_slurm_run_id(line: &str) -> Option<String> {
    let value = line.trim_start().strip_prefix("SLURM run ID:")?;
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

/// Recognises the step banner printed by the pipeline script (`## STEP ...`).
fn is_step_marker(line: &str) -> bool {
    let Some(rest) = line.strip_prefix("##") else {
        return false;
    };
    let mut parts = rest.split_whitespace();
    matches!(
        parts.next(),
        Some(keyword)
            if keyword.eq_ignore_ascii_case("step") || keyword.eq_ignore_ascii_case("phase")
    )
}

/// Parses `Job <snakemake_job_id> has been submitted with SLURM jobid <id> (log: <path>)`.
fn parse_slurm_submission(line: &str) -> Option<SlurmSubmission> {
    let rest = line.strip_prefix("Job ")?;
    let (snakemake_job_id, rest) = rest.split_once(" has been submitted with SLURM jobid ")?;
    let (slurm_job_id, rest) = match rest.split_once(" (log:") {
        Some((id, rest)) => (id, Some(rest)),
        None => (rest, None),
    };
    let slurm_job_id = slurm_job_id.trim();
    if slurm_job_id.is_empty() {
        return None;
    }
    let log_path = rest.and_then(|rest| {
        let path = rest.trim();
        let path = path.strip_suffix('.').unwrap_or(path);
        let path = path.strip_suffix(')').unwrap_or(path);
        let path = path.trim();
        (!path.is_empty()).then(|| path.to_string())
    });

    Some(SlurmSubmission {
        snakemake_job_id: snakemake_job_id.trim().parse().ok()?,
        slurm_job_id: slurm_job_id.to_string(),
        log_path,
    })
}

/// Parses both `Finished jobid: 12 (Rule: align)` and `Finished job 12.`.
fn parse_finished_job(line: &str) -> Option<FinishedJob> {
    if let Some(rest) = line.strip_prefix("Finished jobid:") {
        let rest = rest.trim();
        let (jobid, rule_name) = match rest.split_once('(') {
            Some((jobid, rule)) => {
                let rule = rule
                    .trim()
                    .strip_prefix("Rule:")
                    .unwrap_or(rule.trim())
                    .trim()
                    .trim_end_matches(')')
                    .trim();
                (jobid.trim(), (!rule.is_empty()).then(|| rule.to_string()))
            }
            None => (rest, None),
        };
        return Some(FinishedJob {
            jobid: jobid.parse().ok()?,
            rule_name,
        });
    }

    let rest = line.strip_prefix("Finished job ")?;
    let jobid = rest.trim().trim_end_matches('.').trim();
    Some(FinishedJob {
        jobid: jobid.parse().ok()?,
        rule_name: None,
    })
}

fn parse_rule_header(line: &str) -> Option<String> {
    let line = line.strip_suffix(':')?;
    let rule_name = line
        .strip_prefix("localrule ")
        .or_else(|| line.strip_prefix("rule "))
        .or_else(|| line.strip_prefix("checkpoint "))
        .or_else(|| line.strip_prefix("localcheckpoint "))?;
    let rule_name = rule_name.trim();
    (!rule_name.is_empty()).then(|| rule_name.to_string())
}

fn parse_error_rule_header(line: &str) -> Option<String> {
    let rest = line.strip_prefix("Error in rule ")?;
    let rule_name = rest.strip_suffix(':')?.trim();
    (!rule_name.is_empty()).then(|| rule_name.to_string())
}

fn parse_block_property(line: &str) -> Option<(&str, &str)> {
    if !is_indented(line) {
        return None;
    }
    let (key, value) = line.trim_start().split_once(':')?;
    let key = key.trim();
    if key.is_empty() || key.contains(' ') {
        return None;
    }
    Some((key, value.trim()))
}

fn is_indented(line: &str) -> bool {
    line.starts_with(' ') || line.starts_with('\t')
}

fn append_values(values: &mut Vec<String>, raw: &str) {
    for value in raw.split(',') {
        let value = value.trim();
        if !value.is_empty() {
            values.push(value.to_string());
        }
    }
}

/// Snakemake prints `key=value, key2=value2`; only the values are kept.
fn wildcard_values(raw: &str) -> Vec<String> {
    raw.split(',')
        .filter_map(|pair| {
            let pair = pair.trim();
            if pair.is_empty() {
                return None;
            }
            Some(
                pair.split_once('=')
                    .map(|(_, value)| value.trim().to_string())
                    .unwrap_or_else(|| pair.to_string()),
            )
        })
        .collect()
}

fn split_list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect()
}

fn join_paths(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(",")
}

fn absolutize(outdir: &Path, path: &str) -> PathBuf {
    // Storage-plugin inputs may be URLs: prefixing them with the workdir would
    // turn them into nonsense paths.
    if path.contains("://") {
        return PathBuf::from(path);
    }

    let path = Path::new(path);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        outdir.join(path)
    }
}

fn strip_ansi(line: &str) -> String {
    lazy_regex::regex!(r"\x1b\[[0-9;?]*[ -/]*[@-~]")
        .replace_all(line, "")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_report(dir: &Path) -> RunReport {
        let pool = open_pool(&dir.join("report.db"), true).await.unwrap();
        ensure_schema(&pool).await.unwrap();

        RunReport {
            inner: Arc::new(RunReportInner {
                pool,
                outdir: dir.to_path_buf(),
                state: Mutex::new(ReportState::default()),
            }),
        }
    }

    fn temp_dir() -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("smaug-run-report-{}-{unique}", std::process::id()))
    }

    #[tokio::test]
    async fn observes_indented_blocks_and_flushes_pending_job() {
        let dir = temp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("in.txt"), b"abc").unwrap();
        let report = test_report(&dir).await;

        report.observe("localrule process:").await;
        report.observe("    input: in.txt").await;
        report.observe("    log: job.log").await;
        report.observe("    jobid: 7").await;
        report.observe("    wildcards: sample=A").await;
        report.finish().await;

        let row = sqlx::query(
            "SELECT snakemake_job_id, rule_name, status, wildcards, input_paths, input_sizes, log_paths FROM snakemake",
        )
        .fetch_one(&report.inner.pool)
        .await
        .unwrap();

        assert_eq!(row.get::<i64, _>("snakemake_job_id"), 7);
        assert_eq!(row.get::<String, _>("rule_name"), "process");
        assert_eq!(row.get::<String, _>("status"), "running");
        assert_eq!(row.get::<String, _>("wildcards"), "A");
        assert_eq!(
            row.get::<String, _>("input_paths"),
            dir.join("in.txt").display().to_string()
        );
        assert_eq!(row.get::<String, _>("input_sizes"), "3");
        assert_eq!(
            row.get::<String, _>("log_paths"),
            dir.join("job.log").display().to_string()
        );

        drop(report);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn completion_updates_the_existing_job() {
        let dir = temp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let report = test_report(&dir).await;

        report.observe("SLURM run ID: workflow").await;
        report.observe("localrule process:").await;
        report.observe("    jobid: 7").await;
        report.observe("Finished jobid: 7 (Rule: process)").await;
        report.finish().await;

        let row = sqlx::query("SELECT status, rule_name FROM snakemake")
            .fetch_one(&report.inner.pool)
            .await
            .unwrap();
        assert_eq!(row.get::<String, _>("status"), "completed");
        assert_eq!(row.get::<String, _>("rule_name"), "process");

        drop(report);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn failed_job_keeps_the_error_block_metadata() {
        let dir = temp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("in.txt"), b"abc").unwrap();
        let report = test_report(&dir).await;

        report.observe("SLURM run ID: workflow").await;
        report.observe("localrule process:").await;
        report.observe("    input: in.txt").await;
        report.observe("    jobid: 7").await;
        report.observe("Error in rule process:").await;
        report.observe("    message: None").await;
        report.observe("    jobid: 7").await;
        report.observe("    log: failed.log").await;
        report.finish().await;

        let rows = sqlx::query("SELECT snakemake_job_id, status, log_paths FROM snakemake")
            .fetch_all(&report.inner.pool)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get::<i64, _>("snakemake_job_id"), 7);
        assert_eq!(rows[0].get::<String, _>("status"), "failed");
        assert_eq!(
            rows[0].get::<String, _>("log_paths"),
            dir.join("failed.log").display().to_string()
        );

        drop(report);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn duplicate_error_blocks_merge_instead_of_duplicating_rows() {
        let dir = temp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let report = test_report(&dir).await;

        for _ in 0..2 {
            report.observe("Error in rule process:").await;
            report.observe("    jobid: 7").await;
            report.observe("    log: failed.log").await;
        }
        report.finish().await;

        let rows = sqlx::query("SELECT status FROM snakemake")
            .fetch_all(&report.inner.pool)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get::<String, _>("status"), "failed");

        drop(report);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn remote_submission_and_completion_update_the_same_row() {
        let dir = temp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("in.txt"), b"abc").unwrap();
        std::fs::write(dir.join("slurm.log"), b"").unwrap();
        let report = test_report(&dir).await;

        report.observe("SLURM run ID: workflow").await;
        report.observe("rule align:").await;
        report.observe("    input: in.txt").await;
        report.observe("    log: rule.log").await;
        report.observe("    jobid: 3").await;
        report
            .observe("Job 3 has been submitted with SLURM jobid 999 (log: slurm.log).")
            .await;
        report.observe("Finished jobid: 3 (Rule: align)").await;
        report.finish().await;

        let rows =
            sqlx::query("SELECT slurm_job_name, slurm_job_id, status, log_paths FROM snakemake")
                .fetch_all(&report.inner.pool)
                .await
                .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get::<String, _>("slurm_job_name"), "workflow");
        assert_eq!(rows[0].get::<String, _>("slurm_job_id"), "999");
        assert_eq!(rows[0].get::<String, _>("status"), "completed");
        let log_paths = rows[0].get::<String, _>("log_paths");
        assert!(log_paths.contains(&dir.join("rule.log").display().to_string()));
        assert!(log_paths.contains(&dir.join("slurm.log").display().to_string()));

        drop(report);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn step_marker_resets_the_slurm_job_name() {
        let dir = temp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let report = test_report(&dir).await;

        report.observe("SLURM run ID: workflow-1").await;
        report.observe("rule first:").await;
        report.observe("    jobid: 1").await;
        report.observe("## STEP second").await;
        report.observe("SLURM run ID: workflow-2").await;
        report.observe("rule second:").await;
        report.observe("    jobid: 2").await;
        report.finish().await;

        let rows =
            sqlx::query("SELECT slurm_job_name, snakemake_job_id FROM snakemake ORDER BY id")
                .fetch_all(&report.inner.pool)
                .await
                .unwrap();

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].get::<String, _>("slurm_job_name"), "workflow-1");
        assert_eq!(rows[0].get::<i64, _>("snakemake_job_id"), 1);
        assert_eq!(rows[1].get::<String, _>("slurm_job_name"), "workflow-2");
        assert_eq!(rows[1].get::<i64, _>("snakemake_job_id"), 2);

        drop(report);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn migration_of_old_reports() {
        use std::collections::HashSet;
        let dir = temp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("old-report.db");
        let pool = open_pool(&path, true).await.unwrap();
        sqlx::query(
            r#"
            CREATE TABLE snakemake (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                slurm_job_name TEXT,
                slurm_job_id TEXT,
                snakemake_job_id INTEGER NOT NULL,
                log_paths TEXT NOT NULL DEFAULT '',
                wildcards TEXT NOT NULL DEFAULT '',
                input_paths TEXT NOT NULL DEFAULT '',
                input_sizes TEXT NOT NULL DEFAULT ''
            )
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();

        ensure_schema(&pool).await.unwrap();

        let columns: HashSet<String> = sqlx::query("PRAGMA table_info(snakemake)")
            .fetch_all(&pool)
            .await
            .unwrap()
            .into_iter()
            .filter_map(|row| row.try_get::<String, _>("name").ok())
            .collect();
        assert!(columns.contains("rule_name"));
        assert!(columns.contains("status"));

        drop(pool);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parses_slurm_markers() {
        assert_eq!(
            parse_slurm_run_id("SLURM run ID: 8cb8c359-3930-40ec-a5df-3da736d2b4e9"),
            Some("8cb8c359-3930-40ec-a5df-3da736d2b4e9".to_string())
        );

        let submission = parse_slurm_submission(
            "Job 8 has been submitted with SLURM jobid 321 (log: /tmp/rule_a/x/321.log).",
        )
        .unwrap();
        assert_eq!(submission.snakemake_job_id, 8);
        assert_eq!(submission.slurm_job_id, "321");
        assert_eq!(
            submission.log_path.as_deref(),
            Some("/tmp/rule_a/x/321.log")
        );
    }

    #[test]
    fn parses_finished_jobs() {
        let finished = parse_finished_job("Finished jobid: 12 (Rule: align)").unwrap();
        assert_eq!(finished.jobid, 12);
        assert_eq!(finished.rule_name.as_deref(), Some("align"));

        let finished = parse_finished_job("Finished job 0.").unwrap();
        assert_eq!(finished.jobid, 0);
        assert_eq!(finished.rule_name, None);
    }

    #[test]
    fn parses_rule_blocks() {
        assert_eq!(
            parse_rule_header("rule process:").as_deref(),
            Some("process")
        );
        assert_eq!(
            parse_rule_header("localrule init:").as_deref(),
            Some("init")
        );
        assert_eq!(parse_rule_header("Finished jobid: 3 (Rule: init)"), None);
        assert_eq!(
            parse_error_rule_header("Error in rule process:").as_deref(),
            Some("process")
        );
        assert_eq!(parse_error_rule_header("Error in rule process"), None);

        assert_eq!(
            parse_block_property("    input: a.txt, b.txt"),
            Some(("input", "a.txt, b.txt"))
        );
        assert_eq!(parse_block_property("jobid: 3"), None);
    }

    #[test]
    fn recognises_step_markers() {
        assert!(is_step_marker("## STEP align"));
        assert!(is_step_marker("## phase two"));
        assert!(!is_step_marker("## PROGRESS 10"));
        assert!(!is_step_marker("STEP align"));
    }

    #[test]
    fn keeps_only_wildcard_values() {
        assert_eq!(
            wildcard_values("sample=job_001, lane=L1"),
            vec!["job_001".to_string(), "L1".to_string()]
        );
    }
}
