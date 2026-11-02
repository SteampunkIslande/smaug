use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunProgress {
    pub run_id: i64,
    pub attempt_number: u32,
    pub current_step_string: Option<String>,
    pub current_progress: Option<i64>,
    pub last_output_line: Option<String>,
    pub done_jobs_count: Option<i64>,
    pub total_jobs_count: Option<i64>,
    pub slurm_job_name: Option<String>,
    pub log_available: bool,
    pub updated_at: OffsetDateTime,
}

impl RunProgress {
    pub fn new(run_id: i64, attempt_number: u32) -> Self {
        Self {
            run_id,
            attempt_number,
            current_step_string: None,
            current_progress: None,
            last_output_line: None,
            done_jobs_count: None,
            total_jobs_count: None,
            slurm_job_name: None,
            log_available: false,
            updated_at: OffsetDateTime::now_utc(),
        }
    }
}
