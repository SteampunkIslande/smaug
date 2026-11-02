use std::path::PathBuf;

use rocket::fs::NamedFile;
use rocket::http::Status;
use rocket::request::FromParam;
use rocket::{State, get};
use sqlx::SqlitePool;

use crate::auth::Authenticated;
use crate::models::{Attempt, Run};

use super::logs::can_user_see_run;

/// Folder exposed by the files API.
#[derive(Debug, Clone, Copy)]
pub enum FileScope {
    /// Curated files listed on the run page.
    Exports,
    /// Bulk files such as snakemake logs, which must not be listed on the page.
    Logs,
}

impl<'a> FromParam<'a> for FileScope {
    type Error = &'a str;

    fn from_param(param: &'a str) -> Result<Self, Self::Error> {
        match param {
            "exports" => Ok(Self::Exports),
            "logs" => Ok(Self::Logs),
            other => Err(other),
        }
    }
}

impl FileScope {
    fn directory(self, attempt: &Attempt) -> PathBuf {
        match self {
            Self::Exports => attempt.exports_dir(),
            Self::Logs => attempt.outdir(),
        }
    }
}

/// Serves a file produced by an attempt.
///
/// The analysis directory is laid out as
/// `<analysis_dir>/run-<run_id>/<scope>-<attempt_number>/...`.
///
/// `exports` holds the curated files listed on the run page. `logs` holds bulk
/// files such as snakemake logs: they are served by this route but never listed
/// on the page, so a pipeline may produce thousands of them without bloating the
/// HTML.
///
/// Paths may be arbitrarily deep. Canonicalization plus the `starts_with` check
/// keeps `..` and symlinks from escaping the selected directory.
#[get("/<run_id>/<attempt_number>/<scope>/<path..>")]
pub async fn serve_run_file(
    auth: Authenticated,
    pool: &State<SqlitePool>,
    run_id: i64,
    attempt_number: u32,
    scope: FileScope,
    path: PathBuf,
) -> Result<NamedFile, Status> {
    if path.components().count() == 0 {
        return Err(Status::NotFound);
    }

    let run = Run::get_run_from_id(run_id, pool)
        .await
        .map_err(|_| Status::NotFound)?;

    if !can_user_see_run(&auth, &run, pool).await? {
        return Err(Status::Forbidden);
    }

    // The attempt must exist for this run, so a forged attempt number cannot
    // expose files left over in the run directory.
    let attempt = Attempt::get_attempt_from_number(attempt_number, run_id, pool)
        .await
        .map_err(|_| Status::NotFound)?;

    let base_dir = scope
        .directory(&attempt)
        .canonicalize()
        .map_err(|_| Status::NotFound)?;
    let file_path = base_dir
        .join(&path)
        .canonicalize()
        .map_err(|_| Status::NotFound)?;

    if !file_path.starts_with(&base_dir) || !file_path.is_file() {
        return Err(Status::NotFound);
    }

    NamedFile::open(file_path)
        .await
        .map_err(|_| Status::NotFound)
}

#[cfg(test)]
mod tests {
    use rocket::request::FromParam;

    use super::FileScope;

    #[test]
    fn parses_known_scopes() {
        assert!(matches!(
            FileScope::from_param("exports"),
            Ok(FileScope::Exports)
        ));
        assert!(matches!(FileScope::from_param("logs"), Ok(FileScope::Logs)));
        assert!(FileScope::from_param("other").is_err());
    }
}
