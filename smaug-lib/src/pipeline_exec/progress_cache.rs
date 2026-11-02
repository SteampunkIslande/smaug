use crate::models::RunProgress;
use crate::pipeline_exec::parse_logs::LogProgressParser;
use std::collections::HashMap;
use std::sync::Arc;
use time::OffsetDateTime;
use tokio::sync::RwLock;

/// Shared, live progress of every analysis.
///
/// The cache owns the parser of each attempt, so a stderr line is applied
/// directly to the cached state instead of cloning the whole `RunProgress` for
/// every line.
#[derive(Clone, Default)]
pub struct RunProgressCache {
    entries: Arc<RwLock<HashMap<(i64, u32), LogProgressParser>>>,
}

impl RunProgressCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn get_or_default(&self, run_id: i64, attempt_number: u32) -> RunProgress {
        self.entries
            .read()
            .await
            .get(&(run_id, attempt_number))
            .map(|parser| parser.state().clone())
            .unwrap_or_else(|| RunProgress::new(run_id, attempt_number))
    }

    /// Creates the entry as soon as the log sink is attached.
    ///
    /// Without this, an attempt whose stderr has not produced a line yet would
    /// look like it has no log at all, even though the log file exists.
    pub async fn ensure_attempt(&self, run_id: i64, attempt_number: u32) {
        self.entries
            .write()
            .await
            .entry((run_id, attempt_number))
            .or_insert_with(|| LogProgressParser::new(run_id, attempt_number));
    }

    /// Applies one stderr line to the progress of an attempt.
    pub async fn observe_stderr(&self, run_id: i64, attempt_number: u32, line: &str) {
        let mut entries = self.entries.write().await;
        let parser = entries
            .entry((run_id, attempt_number))
            .or_insert_with(|| LogProgressParser::new(run_id, attempt_number));
        parser.parse_line(line);
        parser.state_mut().updated_at = OffsetDateTime::now_utc();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn missing_entry_returns_default_progress() {
        let cache = RunProgressCache::new();

        let missing = cache.get_or_default(1, 1).await;
        assert!(!missing.log_available);
        assert_eq!(missing.current_progress, None);
    }

    #[tokio::test]
    async fn attempts_are_cached_separately() {
        let cache = RunProgressCache::new();

        cache.observe_stderr(1, 1, "## STEP first").await;
        cache.observe_stderr(1, 2, "## STEP second").await;

        assert_eq!(
            cache
                .get_or_default(1, 1)
                .await
                .current_step_string
                .as_deref(),
            Some("first")
        );
        assert_eq!(
            cache
                .get_or_default(1, 2)
                .await
                .current_step_string
                .as_deref(),
            Some("second")
        );
    }

    #[tokio::test]
    async fn stderr_lines_update_the_cached_snapshot_in_place() {
        let cache = RunProgressCache::new();

        cache.observe_stderr(1, 1, "## STEP align").await;
        cache
            .observe_stderr(1, 1, "SLURM run ID: workflow-uuid")
            .await;
        cache.observe_stderr(1, 1, "3 of 10 steps (30%) done").await;

        let loaded = cache.get_or_default(1, 1).await;

        assert!(loaded.log_available);
        assert_eq!(loaded.current_step_string.as_deref(), Some("align"));
        assert_eq!(loaded.current_progress, Some(30));
        assert_eq!(loaded.done_jobs_count, Some(3));
        assert_eq!(loaded.slurm_job_name.as_deref(), Some("workflow-uuid"));
    }

    #[tokio::test]
    async fn ensure_marks_the_log_as_available_before_any_line() {
        let cache = RunProgressCache::new();
        cache.ensure_attempt(1, 1).await;

        let loaded = cache.get_or_default(1, 1).await;

        assert!(loaded.log_available);
        assert_eq!(loaded.current_progress, None);
    }
}
