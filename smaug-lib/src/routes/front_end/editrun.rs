use std::collections::HashSet;
use std::ops::Not;

use anyhow::Context;
use rocket::State;
use rocket::get;
use sqlx::SqlitePool;

use crate::routes::frontend::FrontendError;
use crate::templates::{Template, context};

use crate::auth::Authenticated;
use crate::models::Group;
use crate::models::{FileIdentification, Run, RunStatus};

#[get("/editrun/<run_id>")]
pub async fn edit_run_get(
    auth: Authenticated,
    pool: &State<SqlitePool>,
    run_id: i64,
) -> Result<Template, FrontendError> {
    let mut run: Run = Run::get_run_from_id(run_id, pool)
        .await
        .with_context(|| format!("Impossible de lire le run {run_id}"))?;
    let form = match run.get_form().await {
        Ok(form) => form,
        Err(e) => {
            let (branch, form_path) = match &run.form_identification {
                FileIdentification::GitBranchAndPath { branch, file_path } => {
                    (branch.clone(), file_path.display().to_string())
                }
                FileIdentification::GitCommit {
                    commit_hash,
                    file_path,
                } => (commit_hash.clone(), file_path.display().to_string()),
                FileIdentification::None => (String::new(), String::new()),
            };
            log::warn!(
                "Le formulaire '{}' sur la branche '{}' n'est plus disponible pour le run {run_id}: {e:#}",
                form_path,
                branch
            );
            return Err(FrontendError::FormNotFound {
                branch,
                file_path: form_path,
            });
        }
    };
    let variables_html: Vec<String> = form.variables.iter().map(|v| v.to_html_safe()).collect();

    let variable_names: HashSet<&str> = form.variables.iter().map(|v| v.name.as_str()).collect();

    let previous_variable_names: HashSet<&str> =
        run.user_defined_vars.keys().map(String::as_str).collect();
    let warning = if previous_variable_names != variable_names {
        let (branch, form_path) = match &form.form_identification {
            FileIdentification::GitBranchAndPath { branch, file_path } => {
                (branch.as_str(), file_path.display().to_string())
            }
            FileIdentification::GitCommit {
                commit_hash,
                file_path,
            } => (commit_hash.as_str(), file_path.display().to_string()),
            FileIdentification::None => ("", String::new()),
        };
        Some(format!(
            "Le formulaire '{}' sur la branche '{}' n'a pas les mêmes variables qu'à l'exécution précédente",
            form_path, branch
        ))
    } else {
        None
    };

    // Drop variables that no longer exist in the current version of the form
    // (a variable may have been removed between two edits of a run).
    run.user_defined_vars = run
        .user_defined_vars
        .into_iter()
        .filter_map(|(k, v)| {
            if variable_names.contains(k.as_str()) {
                Some((k, v))
            } else {
                None
            }
        })
        .collect();
    let can_see_run = {
        if auth.user.is_admin {
            true
        } else {
            let current_user_groups: HashSet<i64> = Group::get_user_groups(pool, auth.user.id)
                .await?
                .iter()
                .map(|grp| grp.id)
                .collect();
            let run_user_name = run.get_user(pool).await?.username;
            let run_user_groups: HashSet<i64> = Group::get_user_groups(pool, run.user_id)
                .await
                .with_context(|| {
                    format!(
                        "Impossible d'obtenir les groupes de {run_user_name} (l'utilisateur qui a déclaré le run)"
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
        match run.status {
            RunStatus::Idle => Ok(Template::render(
                "common/editrun",
                context! {
                    run=> &run,
                    user=> &auth.user,
                    form=> &form,
                    user_defined_vars=> &variables_html,
                    warning=> warning,
                },
            )),
            status => {
                let status_str = match status {
                    RunStatus::Running => "Run en cours",
                    RunStatus::Success => "Run terminé",
                    RunStatus::Failure(_) => "Run échoué",
                    _ => "Run non éditable",
                };
                Ok(Template::render(
                    "common/error",
                    context! {
                        title=> status_str,
                        h2=> format!("Impossible d'éditer un {}.", status_str.to_lowercase()),
                        message=> "Seuls les runs à l'état 'A valider' peuvent être édités.",
                        user=> auth.user
                    },
                ))
            }
        }
    } else {
        Ok(Template::render(
            "common/error",
            context! {
                title=> "Accès refusé",
                h2=> "Accès refusé",
                message=> "Vous n'avez pas les permissions nécessaires pour éditer ce run",
                user=> auth.user
            },
        ))
    }
}
