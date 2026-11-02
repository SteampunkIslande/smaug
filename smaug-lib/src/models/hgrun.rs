use crate::models::CachedForm;
use crate::models::FileIdentification;
use crate::models::Form;
use crate::models::Group;
use crate::models::User;

use crate::utils::format_french_date;
use anyhow::{Context, Result};
use rocket::form::FromFormField;
use rocket::form::ValueField;
use serde::{Deserialize, Serialize};
use serde_json;
use serde_json::Value;
use serde_json::json;
use sqlx::Row;
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::fmt::Display;
use std::path::PathBuf;
use std::str::FromStr;
use time::{OffsetDateTime, UtcOffset};

/// Errors that callers may want to recognise when describing a run.
///
/// Run functions return `anyhow::Result`; a bad status or a refused edition are
/// *user* errors, so they keep a name.///
/// These types are the *named* errors of the layer; the functions themselves
/// return `anyhow::Result`. The `InternalError` variant only exists to give the
/// edge errors (FrontendError / AuthError) a single `#[from] anyhow::Error`
/// conversion point; it is not produced by the model layer itself.
#[derive(Debug, wherror::Error)]
pub enum RunDefinitionError {
    #[error("Le run est dans un statut invalide")]
    InvalidRunStatusError,
    #[error("Le run ne peut pas être édité, seul un run 'A valider' peut l'être")]
    UneditableRun,
    #[error("RunID manquant!")]
    MissingRunID,
    #[error("{location}: {source}")]
    InternalError {
        #[from]
        source: anyhow::Error,
        location: &'static std::panic::Location<'static>,
    },
}

#[derive(Debug, Default, Serialize, Deserialize, Clone, PartialEq)]
pub enum RunStatus {
    /// The form was created but not yet validated
    #[default]
    Idle,
    /// The form was validated but the analysis has not started yet
    Pending,
    /// The form was validated and the analysis is running
    Running,
    /// The form was validated and the analysis completed successfully
    Success,
    /// The form was validated but the analysis failed with an error
    Failure(String),
}

#[rocket::async_trait]
impl<'r> FromFormField<'r> for RunStatus {
    fn default() -> Option<Self> {
        Some(RunStatus::Idle)
    }
    fn from_value(field: ValueField<'r>) -> rocket::form::Result<'r, Self> {
        // Failure alone (front-end filter) means all failure statuses:
        // the reason is empty, and the `Failure:%` prefix filter covers them all
        if field.value == "Failure" {
            return Ok(RunStatus::Failure(String::new()));
        }

        Ok(Self::from_str(field.value)
            .map_err(|_| rocket::form::Error::validation("Statut de run invalide"))?)
    }
}

impl Display for RunStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunStatus::Idle => write!(f, "Idle"),
            RunStatus::Pending => write!(f, "Pending"),
            RunStatus::Running => write!(f, "Running"),
            RunStatus::Success => write!(f, "Success"),
            RunStatus::Failure(reason) => write!(f, "Failure:{}", reason),
        }
    }
}

impl FromStr for RunStatus {
    type Err = RunDefinitionError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "Idle" => Ok(RunStatus::Idle),
            "Pending" => Ok(RunStatus::Pending),
            "Running" => Ok(RunStatus::Running),
            "Success" => Ok(RunStatus::Success),
            s if s.starts_with("Failure:") => Ok(RunStatus::Failure(s[8..].to_string())),
            _ => Err(RunDefinitionError::InvalidRunStatusError),
        }
    }
}

/// Created by users.
/// On any user's home page, there is a list of runs submitted by the user
/// There is also a button that the user can press to get to route '/newrun/groupname'
#[derive(Debug, Serialize, Default, Deserialize, Clone, PartialEq)]
pub struct Run {
    /// Auto-incremented Run ID (database defined).
    pub run_id: Option<i64>,
    /// Id of the user who created this run. Not modifiable with new attempts.
    pub user_id: i64,

    /// A user-defined name for this run
    /// Not modifiable with new attempts (ignored by the edit_run associated function).
    pub run_name: String,

    /// Date of creation of this run in the database. Not modifiable with new attempts.
    pub creation_date: Option<OffsetDateTime>,

    /// Current status of the run. Reflects the status of the latest attempt.
    pub status: RunStatus,

    /// Number of attempts made for this run. Incremented each time a new attempt is created.
    pub attempt_count: u32,

    /// User defined variables
    pub user_defined_vars: HashMap<String, String>,

    #[serde(flatten)]
    pub form_identification: FileIdentification,
}

impl Run {
    /// Creates a new HgRun from the ID of an HgFormDef and other required parameters
    pub async fn new_run(mut run_submission: Run, pool: &SqlitePool) -> Result<i64> {
        // Creation date (regardless of what the user had submitted)
        run_submission.creation_date = Some(OffsetDateTime::now_utc());

        let user_defined_vars_str = serde_json::to_string(&run_submission.user_defined_vars)
            .context("Impossible de sérialiser les variables du run")?;

        // Insert into the Runs table
        let form = run_submission.get_form().await?;
        // Make sure the form is properly filled in (the injected vars
        // `outdir`/`attempt_number` are added at execution time)
        form.check_validity(Some(&run_submission.user_defined_vars))
            .await
            .context("Le formulaire soumis est invalide")?;

        match run_submission.form_identification {
            FileIdentification::GitBranchAndPath { branch, file_path } => {
                let run_id = sqlx::query(
                    r#"
                    INSERT INTO Runs (user_id, run_name, creation_date, status, attempt_count, branch_name, user_defined_vars, form_path)
                    VALUES (?,?,?,?,?,?,?,?)
                    RETURNING run_id
                    "#,
                )
                .bind(&run_submission.user_id)
                .bind(&run_submission.run_name)
                .bind(&run_submission.creation_date)
                .bind(RunStatus::Idle.to_string())
                .bind(0)
                .bind(&branch)
                .bind(&user_defined_vars_str)
                .bind(&file_path.to_string_lossy())
                .fetch_one(pool)
                .await
                .with_context(|| {
                    format!(
                        "Impossible d'insérer le run `{}` de l'utilisateur {}",
                        run_submission.run_name, run_submission.user_id
                    )
                })?
                .try_get("run_id")?;

                Ok(run_id)
            }
            _ => anyhow::bail!(
                "Impossible de créer un nouveau run sans faire référence à une branche et un nom de fichier"
            ),
        }
    }

    /// Edits an HgRun from its ID and other required parameters
    pub async fn edit_run(
        run_id: i64,
        user_defined_vars: HashMap<String, String>,
        pool: &SqlitePool,
    ) -> Result<()> {
        let user_defined_vars_str = serde_json::to_string(&user_defined_vars)
            .context("Impossible de sérialiser les variables du run")?;

        let run: Run = Self::get_run_from_id(run_id, pool)
            .await
            .with_context(|| format!("Impossible de lire le run {run_id}"))?;
        if !matches!(run.status, RunStatus::Idle) {
            return Err(RunDefinitionError::UneditableRun.into());
        }

        let form = run.get_form().await?;
        // Make sure the form is properly filled in
        form.check_validity(Some(&user_defined_vars))
            .await
            .context("Le formulaire édité est invalide")?;

        sqlx::query(
            r#"
            UPDATE Runs SET user_defined_vars = ? WHERE run_id = ?
            "#,
        )
        .bind(&user_defined_vars_str)
        .bind(run_id)
        .execute(pool)
        .await
        .with_context(|| format!("Impossible de mettre à jour les variables du run {run_id}"))?;

        Ok(())
    }

    /// Serializes the run into a table row for the front end.
    ///
    /// The output must match `Run::runs_table_header` column by column.
    fn to_json(&self, user_name: &str) -> Option<Value> {
        let local_offset = UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC);
        let creation_date = self
            .creation_date
            .unwrap_or(OffsetDateTime::now_local().unwrap_or(OffsetDateTime::now_utc()))
            .to_offset(local_offset);
        let creation_date_str = format_french_date(&creation_date, false);
        Some(json!([
            // Column 1 - Run name
            json!({
                "content": &self.run_name,
                "href": Some(format!("/smaug/show/run/{}", self.run_id?)),
            }),
            // Column 2 - User
            json!({
                "content": user_name,
            }),
            // Column 3 - Run date
            json!({
                "content": creation_date_str,
            }),
            // Column 4 - Status
            json!({
                "content": match self.status {
                    RunStatus::Idle => "A valider",
                    RunStatus::Failure(_) => "Echec",
                    RunStatus::Pending => "En attente",
                    RunStatus::Running => "Analyses en cours",
                    RunStatus::Success => "Succès"
                },
                "status": match self.status {
                    RunStatus::Idle => "idle",
                    RunStatus::Failure(_) => "failure",
                    RunStatus::Pending => "pending",
                    RunStatus::Running => "running",
                    RunStatus::Success => "success"
                },
            }),
            // Column 5 - Attempts
            json!({
                "content": &self.attempt_count.to_string(),
            })
        ]))
    }

    /// Header of the runs table.
    ///
    /// Each column carries a semantic `kind`, from which the front end derives
    /// the cell classes. The output must match `Run::to_json` column by column.
    fn runs_table_header() -> Value {
        json!([
            json!({"content": "Nom du run", "kind": "content"}),
            json!({"content": "Utilisateur", "kind": "content"}),
            json!({"content": "Date du run", "kind": "content"}),
            json!({"content": "Statut", "kind": "badge"}),
            json!({"content": "Tentative", "kind": "numeric"}),
        ])
    }

    /// Builds the JSON expected by the front end: title, table header,
    /// paginated run rows and pagination.
    fn runs_payload(
        title: &str,
        table: Vec<Value>,
        current_page: i64,
        page_size: i64,
        total_count: i64,
    ) -> Value {
        let total_pages = ((total_count + page_size - 1) / page_size).max(1);

        json!({
            "title": title,
            "header": Self::runs_table_header(),
            "table": table,
            "pagination": {
                "current_page": current_page,
                "total_pages": total_pages,
                "page_size": page_size,
                "total_count": total_count
            }
        })
    }

    /// SQL core shared by `Run::list_runs` and `Run::search_runs`.
    ///
    /// Dynamically builds the WHERE clause from:
    ///
    /// 1. visibility: only an admin (`user = None`) sees all runs, a
    ///    standard user only sees the runs of forms associated with at least
    ///    one of their groups (the `branch_name`/`form_path` pairs being resolved from
    ///    the `FormDef`/`FormDefHasGroup` cache, see `Run::visible_form_pairs`);
    /// 2. the optional filters passed in `filters`.
    ///
    /// Returns JSON ready for the front end: `{title, header, table, pagination}`.
    async fn fetch_runs_table(
        pool: &SqlitePool,
        groups: Option<Vec<Group>>,
        page_size: Option<i64>,
        page: Option<i64>,
        title: &str,
        filters: RunFilters<'_>,
    ) -> Result<Value> {
        let limit = page_size.unwrap_or(5).max(1);
        let current_page = page.unwrap_or(1).max(1);
        let offset = (current_page - 1) * limit;

        let mut where_parts: Vec<String> = Vec::new();
        let mut where_binds: Vec<String> = Vec::new();

        // 1. Visibility by groups (if groups matter, i.e. if the user is not an admin)
        if let Some(groups) = groups {
            let form_pairs = CachedForm::visible_form_pairs(pool, groups).await?;

            if form_pairs.is_empty() {
                // The user has access to no form: empty list
                return Ok(Self::runs_payload(
                    title,
                    Vec::new(),
                    current_page,
                    limit,
                    0,
                ));
            }

            where_parts.push(format!(
                "({})",
                form_pairs
                    .iter()
                    .map(|_| "(r.branch_name = ? AND r.form_path = ?)".to_string())
                    .collect::<Vec<_>>()
                    .join(" OR ")
            ));
            for (branch, form_path) in &form_pairs {
                where_binds.push(branch.clone());
                where_binds.push(form_path.clone());
            }
        }

        // 2. Optional filters
        if let Some(status) = filters.status {
            where_parts.push("r.status LIKE ?".to_string());
            where_binds.push(format!("{status}%"));
        }
        if let Some(date_from) = filters.date_from {
            where_parts.push("r.creation_date >= ?".to_string());
            where_binds.push(date_from.to_string());
        }
        if let Some(date_to) = filters.date_to {
            where_parts.push("r.creation_date <= ?".to_string());
            where_binds.push(date_to.to_string());
        }
        if let Some(run_name_search) = filters.run_name_search {
            where_parts.push("r.run_name LIKE ?".to_string());
            where_binds.push(format!("%{run_name_search}%"));
        }

        let where_sql = if where_parts.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", where_parts.join(" AND "))
        };

        // Total count for pagination
        let count_query_str = format!("SELECT COUNT(r.run_id) as total FROM Runs r{where_sql}");

        // Paginated results, sorted by creation date, most recent first
        let base_query_str = format!(
            "SELECT r.run_id, r.user_id, r.run_name, r.creation_date, r.status,
             r.attempt_count, r.user_defined_vars, r.form_path, r.branch_name, u.username
             FROM Runs r
             INNER JOIN Users u ON r.user_id = u.id{where_sql}
             ORDER BY r.creation_date DESC
             LIMIT ? OFFSET ?"
        );

        let mut count_query = sqlx::query(&count_query_str);
        let mut base_query = sqlx::query(&base_query_str);
        for bind in &where_binds {
            count_query = count_query.bind(bind);
            base_query = base_query.bind(bind);
        }

        let total_count: i64 = count_query
            .fetch_one(pool)
            .await
            .context("Impossible de compter les runs visibles")?
            .try_get("total")?;

        let rows = base_query
            .bind(limit)
            .bind(offset)
            .fetch_all(pool)
            .await
            .context("Impossible de lister les runs visibles")?;

        let mut table = Vec::with_capacity(rows.len());
        for row in &rows {
            if let Some(row_json) = run_row_to_json(row) {
                table.push(row_json);
            }
        }

        Ok(Self::runs_payload(
            title,
            table,
            current_page,
            limit,
            total_count,
        ))
    }

    /// Lists visible runs, paginated and sorted by descending creation date.
    ///
    /// A non-admin user only sees the runs of forms associated with at
    /// least one of their groups; passing `user = None` (admin) disables this filter.
    ///
    /// # Arguments
    ///
    /// - `pool` - database connection
    /// - `user` - the requesting user; `None` means `admin`, so sees everything
    /// - `groups` - the groups the user is a member of; if `None` for a
    ///   non-admin user, their groups are read from the database
    /// - `page_size` - number of runs per page (default: 5)
    /// - `page` - page to display, starting at 1 (default: 1)
    /// - `status` - optional status filter, applied as a prefix
    ///   (`LIKE '{status}%'`, which also covers `Failure:reason`); `None` applies no filter
    ///
    /// # Returns
    ///
    /// - The JSON ready for the front end to consume: `{title, header, table, pagination}`
    ///
    /// # Errors
    ///
    /// In case of an SQL error, an error reading the forms cache (`FormDef`), or
    /// an error reading the user's groups if `groups` is `None`.
    pub async fn list_runs(
        pool: &SqlitePool,
        page_size: Option<i64>,
        page: Option<i64>,
        status: Option<RunStatus>,
        groups: Option<Vec<Group>>,
    ) -> Result<Value> {
        let title = if groups.is_none() {
            "Liste des runs (tous les groupes)"
        } else {
            "Liste des runs de vos groupes"
        };

        let status_str = status.map(|s| s.to_string());

        Self::fetch_runs_table(
            pool,
            groups,
            page_size,
            page,
            title,
            RunFilters {
                status: status_str.as_deref(),
                date_from: None,
                date_to: None,
                run_name_search: None,
            },
        )
        .await
    }

    /// Paginated search of visible runs, sorted by descending creation date.
    ///
    /// Same visibility as `Run::list_runs`: a non-admin user only sees the
    /// runs of forms associated with at least one of their groups.
    ///
    /// # Arguments
    ///
    /// - `pool` - database connection
    /// - `user` - the requesting user; `None` means "admin, sees everything"
    /// - `groups` - the groups the user is a member of; if `None` for a
    ///   non-admin user, their groups are read from the database
    /// - `page_size` - number of runs per page (default: 5)
    /// - `page` - page to display, starting at 1 (default: 1)
    /// - `status` - status prefix to search for (`LIKE '{status}%'`)
    /// - `date_from` / `date_to` - creation date bounds in `YYYY-MM-DD` format
    /// - `run_name_search` - substring to find in the run name
    ///
    /// # Returns
    ///
    /// - The JSON ready for the front end to consume: `{title, header, table, pagination}`
    ///
    /// # Errors
    ///
    /// In case of an SQL error, an error reading the forms cache (`FormDef`), or
    /// an error reading the user's groups if `groups` is `None`.
    #[allow(clippy::too_many_arguments)]
    pub async fn search_runs(
        pool: &SqlitePool,
        page_size: Option<i64>,
        page: Option<i64>,
        status: Option<&str>,
        date_from: Option<&str>,
        date_to: Option<&str>,
        run_name_search: Option<&str>,
        groups: Option<Vec<Group>>,
    ) -> Result<Value> {
        let title = if groups.is_none() {
            "Résultats de la recherche (tous les groupes)"
        } else {
            "Résultats de la recherche de vos groupes"
        };

        Self::fetch_runs_table(
            pool,
            groups,
            page_size,
            page,
            title,
            RunFilters {
                status,
                date_from,
                date_to,
                run_name_search,
            },
        )
        .await
    }

    /// Instantiates an HgRun from its run_id by retrieving it from the database
    pub async fn get_run_from_id(run_id: i64, pool: &SqlitePool) -> Result<Self> {
        let row = sqlx::query(
            r#"
            SELECT * FROM Runs WHERE run_id = ?
            "#,
        )
        .bind(run_id)
        .fetch_one(pool)
        .await
        .with_context(|| format!("Impossible de lire le run {run_id}"))?;

        let run_name = row.try_get("run_name")?;
        let creation_date = row.try_get("creation_date")?;

        let user_id: i64 = row.try_get("user_id")?;
        // Retrieve the user, just to make sure the user_id is valid
        let _ = sqlx::query_as::<_, User>(
            r#"
            SELECT id, usermail, username, password_hash, created_at, last_login, is_admin
            FROM Users WHERE id = ?
            "#,
        )
        .bind(user_id)
        .fetch_optional(pool)
        .await?
        .context("Utilisateur non trouvé!")?;

        // Deserialize the user-defined variables
        let user_defined_vars: HashMap<String, String> =
            serde_json::from_str(row.try_get("user_defined_vars")?)?;

        // Parse the status
        let status: RunStatus = RunStatus::from_str(row.try_get("status")?)
            .map_err(anyhow::Error::from)
            .with_context(|| format!("Statut invalide pour le run {run_id}"))?;
        // Number of attempts
        let attempt_count = row.try_get("attempt_count")?;
        // The branch name to use for execution
        let branch_name: String = row.try_get("branch_name")?;

        let form_path: String = row.try_get("form_path")?;

        // Build the HgRun instance
        let hgrun = Run {
            run_id: Some(run_id),
            user_id,
            run_name,
            creation_date,
            status,
            attempt_count,
            user_defined_vars,
            form_identification: FileIdentification::GitBranchAndPath {
                branch: branch_name,
                file_path: PathBuf::from_str(&form_path).with_context(|| {
                    format!(
                        "Impossible de convertir {} en chemin de fichier!",
                        form_path
                    )
                })?,
            },
        };

        Ok(hgrun)
    }

    pub async fn get_form(&self) -> Result<Form> {
        Form::get_form(self.form_identification.clone()).await
    }

    pub async fn get_user(&self, pool: &SqlitePool) -> Result<User> {
        User::find_by_id(self.user_id, pool)
            .await
            .with_context(|| format!("Impossible de lire l'utilisateur {}", self.user_id))
    }

    //--------- Here are the associated functions (soon to become methods) that are crucial to the state machine

    /// Validates the form for this run: Idle -> Pending
    ///
    /// This function only modifies the HgRun table.
    ///
    /// It is the caller's responsibility to update the Attempts table **after** this function.
    pub(super) async fn validate_form(run_id: i64, pool: &SqlitePool) -> Result<()> {
        // Increment the number of attempts
        sqlx::query(
            r#"
            UPDATE Runs SET (attempt_count, status) = (attempt_count + 1, ?) WHERE run_id = ?
            "#,
        )
        .bind(RunStatus::Pending.to_string())
        .bind(run_id)
        .execute(pool)
        .await
        .with_context(|| format!("Impossible de passer le run {run_id} en attente"))?;

        Ok(())
    }

    /// Starts the run: Pending -> Running (called by the monitoring routine)
    pub(super) async fn start_run(run_id: i64, pool: &SqlitePool) -> Result<()> {
        // Update the status to Running
        sqlx::query(
            r#"
            UPDATE Runs SET status = ? WHERE run_id = ?
            "#,
        )
        .bind(RunStatus::Running.to_string())
        .bind(run_id)
        .execute(pool)
        .await
        .with_context(|| format!("Impossible de démarrer le run {run_id}"))?;

        Ok(())
    }

    /// Completes the run successfully: Running -> Success
    pub(super) async fn complete_success(run_id: i64, pool: &SqlitePool) -> Result<()> {
        // Update the status to Success
        sqlx::query(
            r#"
            UPDATE Runs SET status = ? WHERE run_id = ?
            "#,
        )
        .bind(RunStatus::Success.to_string())
        .bind(run_id)
        .execute(pool)
        .await
        .with_context(|| format!("Impossible de terminer avec succès le run {run_id}"))?;

        Ok(())
    }

    /// Completes the run with a failure: Running -> Failure
    pub(super) async fn complete_failure(
        run_id: i64,
        reason: &str,
        pool: &SqlitePool,
    ) -> Result<()> {
        // Update the status to Failure
        sqlx::query(
            r#"
            UPDATE Runs SET status = ? WHERE run_id = ?
            "#,
        )
        .bind(RunStatus::Failure(reason.to_string()).to_string())
        .bind(run_id)
        .execute(pool)
        .await
        .with_context(|| format!("Impossible de marquer le run {run_id} en échec"))?;

        Ok(())
    }

    /// Restarts the run: Success/Failure -> Idle
    pub(super) async fn relaunch_run(run_id: i64, pool: &SqlitePool) -> Result<()> {
        // Update the status to Idle
        sqlx::query(
            r#"
            UPDATE Runs SET status = ? WHERE run_id = ?
            "#,
        )
        .bind("Idle")
        .bind(run_id)
        .execute(pool)
        .await
        .with_context(|| format!("Impossible de relancer le run {run_id}"))?;

        Ok(())
    }
}

/// Filter criteria shared by `Run::list_runs` and `Run::search_runs`.
struct RunFilters<'a> {
    status: Option<&'a str>,
    date_from: Option<&'a str>,
    date_to: Option<&'a str>,
    run_name_search: Option<&'a str>,
}

/// Rebuilds a `Run` from a row of the listing query,
/// then serializes it for the front-end table.
fn run_row_to_json(row: &sqlx::sqlite::SqliteRow) -> Option<Value> {
    let form_identification = FileIdentification::GitBranchAndPath {
        branch: row.try_get("branch_name").ok()?,
        file_path: PathBuf::from_str(&row.try_get::<String, _>("form_path").ok()?).ok()?,
    };
    let run = Run {
        run_id: row.try_get("run_id").ok()?,
        user_id: row.try_get("user_id").ok()?,
        run_name: row.try_get("run_name").ok()?,
        creation_date: row.try_get("creation_date").ok()?,
        status: RunStatus::from_str(row.try_get("status").ok()?).ok()?,
        attempt_count: row.try_get("attempt_count").ok()?,
        user_defined_vars: serde_json::from_str(row.try_get("user_defined_vars").ok()?).ok()?,
        form_identification,
    };
    let username: String = row.try_get("username").ok()?;

    run.to_json(&username)
}
