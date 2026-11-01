use std::collections::HashSet;
use std::ops::Not;

use anyhow::Context;
use rocket::State;
use rocket::get;
use serde::Serialize;
use sqlx::SqlitePool;
use time::UtcOffset;

use crate::routes::frontend::FrontendError;
use crate::templates::{Template, context};

use crate::auth::Authenticated;
use crate::models::Attempt;
use crate::models::Group;
use crate::models::Run;
use crate::models::RunStatus;
use crate::utils;

#[derive(Debug, Serialize)]
pub struct ExportFile {
    pub name: String,
    pub url: String,
}

#[derive(Debug, Serialize)]
pub struct ExportSection {
    pub title: String,
    pub files: Vec<ExportFile>,
}

/// Lists the files produced by a script in `exports-<attempt_number>`.
///
/// Each immediate subdirectory becomes a section; files stored directly in
/// `exports-<attempt_number>` are grouped under a generic "Fichiers" section.
/// Bulk files such as logs live in `logs-<attempt_number>` instead: they are
/// served by the files API but not listed here, so they cannot bloat the page.
fn list_export_files(attempt: &Attempt) -> Vec<ExportSection> {
    let export_dir = attempt.exports_dir();

    let Ok(entries) = std::fs::read_dir(&export_dir) else {
        return Vec::new();
    };

    let mut sections: Vec<ExportSection> = Vec::new();
    let mut root_files: Vec<ExportFile> = Vec::new();

    let mut entries: Vec<_> = entries.filter_map(Result::ok).collect();
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let name = entry.file_name().to_string_lossy().to_string();
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };

        if file_type.is_dir() {
            let mut files: Vec<ExportFile> = std::fs::read_dir(&path)
                .into_iter()
                .flatten()
                .filter_map(Result::ok)
                .filter(|entry| entry.file_type().map(|t| t.is_file()).unwrap_or(false))
                .map(|entry| {
                    let file_name = entry.file_name().to_string_lossy().to_string();
                    ExportFile {
                        url: format!(
                            "/smaug/files/{}/{}/exports/{}/{}",
                            attempt.run_id,
                            attempt.attempt_number,
                            urlencode(&name),
                            urlencode(&file_name)
                        ),
                        name: file_name,
                    }
                })
                .collect();
            files.sort_by(|a, b| a.name.cmp(&b.name));
            if !files.is_empty() {
                sections.push(ExportSection { title: name, files });
            }
        } else if file_type.is_file() {
            root_files.push(ExportFile {
                url: format!(
                    "/smaug/files/{}/{}/exports/{}",
                    attempt.run_id,
                    attempt.attempt_number,
                    urlencode(&name)
                ),
                name,
            });
        }
    }

    root_files.sort_by(|a, b| a.name.cmp(&b.name));
    if !root_files.is_empty() {
        sections.push(ExportSection {
            title: "Fichiers".to_string(),
            files: root_files,
        });
    }

    sections
}

/// Percent-encodes a single path component for use in a URL.
fn urlencode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char)
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

#[get("/show/run/<run_id>?<attempt_number>")]
pub async fn show_run_get(
    auth: Authenticated,
    pool: &State<SqlitePool>,
    run_id: i64,
    attempt_number: Option<u32>,
) -> Result<Template, FrontendError> {
    // Get sequencers list for edit form

    let run: Run = Run::get_run_from_id(run_id, pool)
        .await
        .with_context(|| format!("Impossible d'obtenir le run {}", run_id))?;

    let can_see_run = {
        if auth.user.is_admin {
            true
        } else {
            let current_user_groups: HashSet<i64> = Group::get_user_groups(pool, auth.user.id)
                .await
                .with_context(|| {
                    format!(
                        "Impossible d'obtenir les groupes de {} (vous)",
                        auth.user.username
                    )
                })?
                .iter()
                .map(|grp| grp.id)
                .collect();
            let run_user_groups: HashSet<i64> = Group::get_user_groups(pool, run.user_id)
                    .await
                    .with_context(|| {
                        format!(
                            "Impossible d'obtenir les groupes de {} (l'utilisateur qui a déclaré le run)",
                            run.user_id
                        )
                    })?
                    .iter()
                    .map(|grp| grp.id)
                    .collect();
            // If there is a common group between the groups the form was declared for, the user that declared the run, and the authenticated user's groups, then you can see/edit the run
            intersection::hash_set::intersection([current_user_groups, run_user_groups])
                .is_empty()
                .not()
        }
    };

    if can_see_run {
        // Retrieve the attempt history for navigation
        let history = Attempt::list_attempts_for_run(run_id, pool)
            .await
            .with_context(|| {
                format!(
                    "Impossible d'obtenir la liste des tentatives du run {}",
                    run_id
                )
            })?;

        let local_offset = UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC);

        let run_creation_date = if let Some(ref run_creation_date) = run.creation_date {
            utils::format_french_date(&run_creation_date.to_offset(local_offset), true)
        } else {
            "".to_string()
        };

        // EDIT MODE: If the run is Idle AND the latest attempt is requested
        if run.status == RunStatus::Idle && attempt_number.is_none() {
            let attempt = Attempt::get_hypothetic_attempt(&run)?;
            let form = &run.get_form().await.with_context(|| {
                "Le formulaire avec lequel ce run a été initialement déclaré n'est plus disponible"
            })?;

            Ok(Template::render(
                "common/idlerun",
                context! {
                    run=> &run,
                    form=> &form,
                    run_creation_date,
                    attempt=> &attempt,
                    history=> history,
                    user=> auth.user,
                },
            ))
        } else {
            // VIEW MODE: Non-Idle run OR specific attempt requested
            let attempt_number = attempt_number.unwrap_or(run.attempt_count);

            // Look up the requested attempt
            let attempt = Attempt::get_attempt_from_number(attempt_number, run_id, pool)
                .await
                .with_context(|| {
                    format!(
                        "Impossible d'obtenir la tentative {} pour le run {}",
                        attempt_number, run_id
                    )
                })?;

            let attempt_form = attempt.get_form().await.with_context(|| {
                "Le formulaire avec lequel ce run a été initialement déclaré n'est plus disponible"
            })?;

            let attempt_date =
                utils::format_french_date(&attempt.attempt_date.to_offset(local_offset), true);

            let exports = list_export_files(&attempt);

            // Display the template corresponding to the status of the ATTEMPT (not the Run)
            match attempt.status {
                RunStatus::Pending => Ok(Template::render(
                    "common/pendingrun",
                    context! {
                        run=> &run,
                        form=> &attempt_form,
                        run_creation_date,
                        attempt_date,
                        attempt=> &attempt,
                        history=> &history,
                        exports=> &exports,
                        user=> auth.user,
                    },
                )),
                RunStatus::Running => Ok(Template::render(
                    "common/runningrun",
                    context! {
                        run=> &run,
                        form=> &attempt_form,
                        run_creation_date,
                        attempt_date,
                        attempt=> &attempt,
                        history=> &history,
                        exports=> &exports,
                        user=> auth.user,
                    },
                )),
                RunStatus::Success => Ok(Template::render(
                    "common/successrun",
                    context! {
                        run=> &run,
                        form=> &attempt_form,
                        run_creation_date,
                        attempt_date,
                        attempt=> &attempt,
                        history=> &history,
                        exports=> &exports,
                        user=> auth.user,
                    },
                )),
                RunStatus::Failure(ref fail_reason) => Ok(Template::render(
                    "common/failurerun",
                    context! {
                        run=> &run,
                        form=> &attempt_form,
                        run_creation_date,
                        attempt_date,
                        attempt=> &attempt,
                        history=> &history,
                        exports=> &exports,
                        user=> auth.user,
                        fail_reason=> fail_reason,
                    },
                )),
                RunStatus::Idle => Err(anyhow::anyhow!(
                    "Une tentative ne peut pas être en état 'A valider'"
                )
                .into()),
            }
        }
    } else {
        Ok(Template::render(
            "common/error",
            context! {
                title=> "Accès refusé",
                h2=> "Accès refusé",
                message=> "Vous n'avez pas les permissions nécessaires pour voir ce run"
            },
        ))
    }
}

#[get("/show/runs")]
pub async fn list_runs(auth: Authenticated) -> Template {
    Template::render("common/listruns", context! {user => auth.user})
}

#[get("/search/run")]
pub async fn search_run(auth: Authenticated) -> Template {
    Template::render("common/searchrun", context! {user => auth.user})
}
