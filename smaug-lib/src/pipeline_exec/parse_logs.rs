use anyhow::Result;
use lazy_regex::Regex;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use crate::models::RunProgress;

pub struct LogProgressParser {
    state: RunProgress,
    re_ansi: Regex,
    re_step: Regex,
    re_progress: Regex,
    re_job_name: Regex,
    re_snakemake: Regex,
    re_rsync: Regex,
}

impl LogProgressParser {
    pub fn new(run_id: i64, attempt_number: u32) -> Self {
        let mut state = RunProgress::new(run_id, attempt_number);
        state.log_available = true;
        Self::from_progress(state)
    }

    /// Builds a parser around an already known progress snapshot.
    ///
    /// Used when the cache receives an explicit `RunProgress` instead of
    /// replaying log lines.
    pub fn from_progress(state: RunProgress) -> Self {
        Self {
            state,
            re_ansi: Regex::new(r"\x1b\[[0-9;?]*[ -/]*[@-~]").unwrap(),
            re_step: Regex::new(r"(?i)^##\s+(?:STEP|PHASE)\b\s*(.*?)\s*$").unwrap(),
            re_progress: Regex::new(r"(?i)^##\s+PROGRESS\s+(\d{1,3})\s*%?\s*$").unwrap(),
            re_job_name: Regex::new(r"(?i)^\s*SLURM\s+run\s+ID:\s+(.+?)\s*$").unwrap(),
            re_snakemake: Regex::new(r"(?i)(\d+)\s+of\s+(\d+)\s+steps\s+\((\d{1,3})%\)\s+done")
                .unwrap(),
            re_rsync: Regex::new(r"(?i)^\s*[\d,.]+\s+(\d{1,3})%\s+.*(?:xfr#|to-chk|ir-chk)")
                .unwrap(),
        }
    }

    pub fn state(&self) -> &RunProgress {
        &self.state
    }

    pub fn state_mut(&mut self) -> &mut RunProgress {
        &mut self.state
    }

    pub fn parse_line(&mut self, raw_line: &str) {
        let stripped = self.re_ansi.replace_all(raw_line, "");
        let line = stripped.trim();

        if line.is_empty() {
            return;
        }

        if let Some(cap) = self.re_step.captures(line) {
            let step = cap[1].trim();
            self.state.current_step_string = (!step.is_empty()).then(|| step.to_string());
            self.state.current_progress = None;
            self.state.done_jobs_count = None;
            self.state.slurm_job_name = None;
            return;
        }

        if let Some(cap) = self.re_progress.captures(line) {
            self.state.current_progress = Some(cap[1].parse::<i64>().unwrap_or(0).clamp(0, 100));
            return;
        }

        if let Some(cap) = self.re_job_name.captures(line) {
            self.state.slurm_job_name = Some(cap[1].trim().to_string());
            return;
        }

        if let Some(cap) = self.re_snakemake.captures(line) {
            let done = cap[1].parse::<i64>().unwrap_or(0);
            let _total = cap[2].parse::<i64>().unwrap_or(0);
            let percent = cap[3].parse::<i64>().unwrap_or(0).clamp(0, 100);

            self.state.current_progress = Some(percent);
            self.state.done_jobs_count = Some(done);

            // Keep `slurm_job_name` even at 100 %: the squeue tracker still needs
            // it to collect the last states, and it is only replaced when the
            // next workflow or step starts.

            self.state.last_output_line = Some(truncate_line(line, 500));
            return;
        }

        if let Some(cap) = self.re_rsync.captures(line) {
            self.state.current_progress = Some(cap[1].parse::<i64>().unwrap_or(0).clamp(0, 100));
            self.state.last_output_line = Some(truncate_line(line, 500));
            return;
        }

        self.state.last_output_line = Some(truncate_line(line, 500));
    }

    pub fn parse_text(run_id: i64, attempt_number: u32, content: &str) -> RunProgress {
        let mut parser = Self::new(run_id, attempt_number);
        // Snakemake emits progress updates as regular newline-terminated lines,
        // never with carriage returns.
        for raw_line in content.lines() {
            parser.parse_line(raw_line);
        }
        parser.state
    }
}

fn truncate_line(line: &str, max_chars: usize) -> String {
    let mut truncated: String = line.chars().take(max_chars).collect();
    if line.chars().count() > max_chars {
        truncated.push('…');
    }
    truncated
}

pub async fn find_errors<T>(log_path: T) -> Result<Vec<String>>
where
    T: AsRef<Path>,
{
    let file = File::open(&log_path)?;
    let reader = BufReader::new(file).lines();

    let re_error = Regex::new(r"^##\s+ERROR\s+(.+)$").unwrap();
    let mut errors: Vec<String> = Vec::new();
    for line in reader {
        let line = line?;
        if let Some(cap) = re_error.captures(&line) {
            errors.push(cap[1].trim().to_string());
        }
    }
    Ok(errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_markers_and_snakemake_progress() {
        let log = "## STEP Demultiplexage\n\
                   rsync started\n\
                   ## SLURM_JOB_NAME demux\n\
                   2 of 10 steps (20%) done\n";

        let parsed = LogProgressParser::parse_text(1, 1, log);

        assert!(parsed.log_available);
        assert_eq!(
            parsed.current_step_string.as_deref(),
            Some("Demultiplexage")
        );
        assert_eq!(parsed.current_progress, Some(20));
        assert_eq!(parsed.done_jobs_count, Some(2));
        assert_eq!(parsed.slurm_job_name.as_deref(), None); //Doesn't parse if it's not the snakemake way of naming slurm job
        assert_eq!(
            parsed.last_output_line.as_deref(),
            Some("2 of 10 steps (20%) done")
        );
    }

    #[test]
    fn new_step_resets_phase_state() {
        let log = "## STEP first\n\
                   ## SLURM_JOB_NAME first-job\n\
                   1 of 2 steps (50%) done\n\
                   ## STEP second\n";

        let parsed = LogProgressParser::parse_text(1, 1, log);

        assert_eq!(parsed.current_step_string.as_deref(), Some("second"));
        assert_eq!(parsed.current_progress, None);
        assert_eq!(parsed.done_jobs_count, None);
        assert_eq!(parsed.slurm_job_name, None);
    }

    #[test]
    fn bare_step_marker_still_resets_the_slurm_job_name() {
        let log = "SLURM run ID: workflow-1\n## STEP\n";

        let parsed = LogProgressParser::parse_text(1, 1, log);

        assert_eq!(parsed.slurm_job_name, None);
        assert_eq!(parsed.current_step_string, None);
    }

    #[test]
    fn parses_explicit_progress_and_rsync_fallback() {
        let log = "## STEP copy\n\
                   ## PROGRESS 25\n\
                   \x20 1,024  50%  2.00MB/s 0:00:01 (xfr#1, to-chk=1/2)\n";

        let parsed = LogProgressParser::parse_text(1, 1, log);

        assert_eq!(parsed.current_progress, Some(50));
        assert!(
            parsed
                .last_output_line
                .as_deref()
                .unwrap()
                .contains("xfr#1")
        );
    }

    #[test]
    fn emits_slurm_run_id_for_squeue_tracking() {
        let log = "## STEP run\n\
                   SLURM run ID: 8cb8c359-3930-40ec-a5df-3da736d2b4e9\n\
                   10 of 10 steps (100%) done\n";

        let parsed = LogProgressParser::parse_text(1, 1, log);

        assert_eq!(parsed.current_progress, Some(100));
        assert_eq!(parsed.done_jobs_count, Some(10));
        assert_eq!(
            parsed.slurm_job_name.as_deref(),
            Some("8cb8c359-3930-40ec-a5df-3da736d2b4e9")
        );
    }
}
