use std::collections::HashSet;
use std::fmt::Display;
use std::path::PathBuf;

use rocket::fs::NamedFile;
use rocket::http::Status;
use rocket::request::FromParam;
use rocket::{State, get};
use sqlx::SqlitePool;

use crate::auth::Authenticated;
use crate::config::get_smaug_config;
use crate::models::{Group, Run};

#[derive(Debug)]
pub enum OutputTarget {
    Stdout,
    Stderr,
}

impl Display for OutputTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Stderr => "stderr",
            Self::Stdout => "stdout",
        })
    }
}

impl<'a> FromParam<'a> for OutputTarget {
    type Error = &'a str;
    fn from_param(param: &'a str) -> Result<Self, Self::Error> {
        match param {
            "stdout" => Ok(Self::Stdout),
            "stderr" => Ok(Self::Stderr),
            _ => Err(param),
        }
    }
}

/// Serves the stdout/stderr logs of an attempt to users allowed to see the run.
///
/// Only `run-<run_id>-tentative-<attempt_number>.{stdout,stderr}` files are
/// exposed, so the route cannot be used to browse the whole logs directory.
#[get("/<run_id>/<attempt_number>/<output_target>")]
pub async fn serve_run_log(
    auth: Authenticated,
    pool: &State<SqlitePool>,
    run_id: i64,
    attempt_number: u32,
    output_target: OutputTarget,
) -> Result<NamedFile, Status> {
    let filename = format!(
        "run-{:0>8}-tentative-{:0>8}.{}",
        run_id, attempt_number, output_target
    );
    let logs_dir = PathBuf::from(&get_smaug_config().logs_dir)
        .canonicalize()
        .map_err(|_| Status::NotFound)?;
    let file_path = logs_dir
        .join(&filename)
        .canonicalize()
        .map_err(|_| Status::NotFound)?;

    // The attempt must exist for this run; this also prevents serving leftovers
    // from an unrelated file that happens to follow the naming convention.
    crate::models::Attempt::get_attempt_from_number(attempt_number, run_id, pool)
        .await
        .map_err(|_| Status::NotFound)?;
    let run = crate::models::Run::get_run_from_id(run_id, pool)
        .await
        .map_err(|_| Status::NotFound)?;

    let cansee_run = self::can_user_see_run(&auth, &run, pool).await?;
    if !cansee_run {
        return Err(Status::Unauthorized);
    }

    NamedFile::open(file_path)
        .await
        .map_err(|_| Status::NotFound)
}

pub(crate) async fn can_user_see_run(
    auth: &Authenticated,
    run: &Run,
    pool: &SqlitePool,
) -> Result<bool, Status> {
    if auth.user.is_admin {
        return Ok(true);
    }

    let current_user_groups: HashSet<i64> = Group::get_user_groups(pool, auth.user.id)
        .await
        .map_err(|_| Status::InternalServerError)?
        .iter()
        .map(|group| group.id)
        .collect();
    let run_user_groups: HashSet<i64> = Group::get_user_groups(pool, run.user_id)
        .await
        .map_err(|_| Status::InternalServerError)?
        .iter()
        .map(|group| group.id)
        .collect();

    Ok(!intersection::hash_set::intersection([current_user_groups, run_user_groups]).is_empty())
}
