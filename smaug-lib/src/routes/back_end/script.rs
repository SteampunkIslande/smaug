use std::path::PathBuf;

use rocket::State;
use rocket::get;
use rocket::http::{ContentType, Status};
use sqlx::SqlitePool;

use crate::auth::Authenticated;
use crate::config::get_smaug_config;
use crate::models::{Attempt, Run, RunStatus};

use super::logs::can_user_see_run;

/// Serves the rendered script of an attempt to users allowed to see the run.
///
/// Runs that are being analysed, have succeeded or have failed already have
/// their rendered script on disk in `scripts_dir`. Pending attempts and idle
/// runs have no script yet, so it is rendered on demand with jinja.
#[get("/<run_id>/<attempt_number>")]
pub async fn serve_run_script(
    auth: Authenticated,
    pool: &State<SqlitePool>,
    run_id: i64,
    attempt_number: u32,
) -> Result<(ContentType, String), Status> {
    let run = Run::get_run_from_id(run_id, pool)
        .await
        .map_err(|_| Status::NotFound)?;

    if !can_user_see_run(&auth, &run, pool).await? {
        return Err(Status::Forbidden);
    }

    let attempt = match run.status {
        RunStatus::Idle => Attempt::get_hypothetic_attempt(&run).map_err(|_| Status::NotFound)?,
        _ => Attempt::get_attempt_from_number(attempt_number, run_id, pool)
            .await
            .map_err(|_| Status::NotFound)?,
    };

    let script = match attempt.status {
        RunStatus::Running | RunStatus::Success | RunStatus::Failure(_) => {
            let script_path = PathBuf::from(&get_smaug_config().scripts_dir)
                .join(format!("{}.sh", attempt.attempt_full_name()));
            match tokio::fs::read_to_string(script_path).await {
                Ok(script) => script,
                // The script may not have been written yet, or may have been
                // cleaned up: fall back to rendering it.
                Err(_) => render_script(&attempt, &run).await?,
            }
        }
        RunStatus::Pending | RunStatus::Idle => render_script(&attempt, &run).await?,
    };

    Ok((ContentType::Plain, script))
}

async fn render_script(attempt: &Attempt, run: &Run) -> Result<String, Status> {
    attempt
        .render_script(&run.run_name)
        .await
        .map_err(|_| Status::InternalServerError)
}
