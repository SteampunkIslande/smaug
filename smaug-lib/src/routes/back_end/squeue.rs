use rocket::serde::json::Json;
use rocket::{State, get};

use crate::pipeline_exec::{JobStatus, RunProgressCache, SqueueCache, SqueueSnapshot};
use crate::routes::ApiResponse;

/// Returns the Slurm queue for one attempt.
///
/// The job name is resolved server-side from the progress cache: the browser
/// does not have to pass a value parsed from another endpoint, which used to
/// make the two views disagree.
#[get("/squeue?<run_id>&<attempt_number>&<job_name>")]
pub async fn get_squeue(
    run_id: Option<i64>,
    attempt_number: Option<u32>,
    job_name: Option<String>,
    progress: &State<RunProgressCache>,
    cache: &State<SqueueCache>,
) -> Json<ApiResponse<Vec<JobStatus>>> {
    let (Some(run_id), Some(attempt_number)) = (run_id, attempt_number) else {
        return Json(ApiResponse::error(
            "Les paramètres `run_id` et `attempt_number` sont requis",
        ));
    };

    let job_name = match job_name.map(|name| name.trim().to_string()) {
        Some(name) if !name.is_empty() => name,
        _ => progress
            .get_or_default(run_id, attempt_number)
            .await
            .slurm_job_name
            .unwrap_or_default(),
    };

    match cache.get(run_id, attempt_number, &job_name).await {
        SqueueSnapshot::Available { jobs, .. } => Json(ApiResponse::success(jobs)),
        SqueueSnapshot::NoJobName => Json(ApiResponse::error(
            "Aucun nom de job SLURM n’est encore associé à cette tentative",
        )),
        SqueueSnapshot::Unavailable { message, .. } => Json(ApiResponse::error(message)),
    }
}
