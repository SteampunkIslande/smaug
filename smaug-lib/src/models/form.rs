use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{Context, Result};
use log::warn;
use minijinja::Environment;
use minijinja::{UndefinedBehavior, Value};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use sqlx::SqlitePool;

use crate::config::get_smaug_config;
use crate::{config::SmaugConfig, models::groups, pipeline_exec::versionning};

macro_rules! additional_vars {
    (
        // Match a comma-separated list of identifiers and their types
        $($var_name:ident: $var_type:ty),* $(,)?
    ) => {
        // 1. Automatically generate the list of reserved names
        pub const INJECTED_VARS: &'static [&'static str] = &[
            $( stringify!($var_name) ),*
        ];

        // 2. Generate the method signature and body
        pub fn jinja_context(
            &self,
            user_vars: &HashMap<String, String>,
            $( $var_name: $var_type ),*
        ) -> HashMap<String, String> {
            let mut ctx = user_vars.clone();

            $(
                // stringify! converts the variable name to a string literal at compile time
                ctx.insert(stringify!($var_name).to_string(), $var_name.to_string());
            )*

            ctx
        }
    };
}

/// Errors of a form definition that callers may want to recognise: they are the
/// validation errors shown to the user when a form does not fit a run.
///
/// Form functions return `anyhow::Result`; every other failure (git, I/O,
/// yaml, jinja, database, …) travels as an `anyhow::Error` with context.///
/// These types are the *named* errors of the layer; the functions themselves
/// return `anyhow::Result`. The `InternalError` variant only exists to give the
/// edge errors (FrontendError / AuthError) a single `#[from] anyhow::Error`
/// conversion point; it is not produced by the model layer itself.
#[derive(wherror::Error, Debug)]
pub enum FormDefinitionError {
    #[error("La variable `{}` apparaît en plusieurs exemplaires",.0)]
    DuplicateVarError(String),
    #[error("Les variables suivantes sont requises:{}",.0)]
    MissingVar(String),
    #[error("Erreur de formulaire: {0}")]
    InvalidData(String),
    #[error("Nom de variable réservé: {}",.0)]
    ReservedVarName(String),
    #[error("{location}: {source}")]
    InternalError {
        #[from]
        source: anyhow::Error,
        location: &'static std::panic::Location<'static>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(tag = "type")]
pub enum VariableType {
    ListFromURL {
        source: String,
    },
    ValuesList {
        values: Vec<String>,
    },
    /// With this type, the user is prompted a date and the date is returned
    DateEdit,
    /// By default, a variable is set by the user from a text field
    #[default]
    LineEdit,
    /// The user will have to upload a file to the server. It is the html's responsibility to post a valid, server-side file name as the variable value
    ExistingFile,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct Variable {
    pub name: String,
    pub title: String,
    pub description: String,
    #[serde(flatten)]
    pub variable_type: VariableType,
}

impl Variable {
    pub fn to_html_safe(&self) -> String {
        let Variable {
            name,
            title,
            description,
            variable_type,
        } = self;

        let description = description.replace('\n', "<br>");

        let widget: String = match variable_type {
            VariableType::DateEdit => {
                format!(r#"<input type="date" id="{name}" name="{name}">"#)
            }
            VariableType::ExistingFile => {
                format!(r#"<input type="file" id="{name}" name="{name}">"#)
            }
            VariableType::ListFromURL { source } => {
                // Render an empty select with a data-source attribute.
                // The JS reads `dataset.source`, fetch the URL, and populate the <option>s.
                format!(r#"<select id="{name}" name="{name}" data-source="{source}"></select>"#)
            }
            VariableType::ValuesList { values } => {
                let options = values
                    .iter()
                    .map(|v| format!(r#"<option value="{v}">{v}</option>"#))
                    .collect::<Vec<String>>()
                    .join("\n");

                format!(
                    r#"<select id="{name}" name="{name}">
                    {options}
                    <option value="" disabled selected value>---Choisissez une valeur---</option>
                    </select>"#
                )
            }
            VariableType::LineEdit => {
                format!(r#"<input type="text" id="{name}" name="{name}">"#)
            }
        };

        format!(
            r#"<div class="card-container" id="{name}-card">
            <label>{title}</label>
            <span>{description}</span>
            {widget}
        </div>"#
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "exec_type")]
pub enum FormExecType {
    #[serde(rename = "shell")]
    Shell { exec: String },
    #[serde(rename = "script")]
    Script { script_name: PathBuf },
}

impl Default for FormExecType {
    fn default() -> Self {
        Self::Shell {
            exec: Default::default(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "form_id")]
pub enum FileIdentification {
    GitBranchAndPath {
        branch: String,
        file_path: PathBuf,
    },
    GitCommit {
        commit_hash: String,
        file_path: PathBuf,
    },
    #[default]
    None,
}

impl FileIdentification {
    pub fn path(&self) -> &Path {
        match self {
            Self::GitBranchAndPath { file_path, .. } => &file_path,
            Self::GitCommit { file_path, .. } => &file_path,
            Self::None => Path::new(""),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct Form {
    pub name: String,
    pub description: String,
    pub variables: Vec<Variable>,
    #[serde(flatten)]
    pub exec_type: FormExecType,
    pub workdir: String,
    #[serde(flatten)]
    #[serde(skip_deserializing)]
    pub form_identification: FileIdentification,
    pub groups: Option<Vec<String>>,
}

impl Form {
    /// Build a list of all valid forms hosted at the configured git repository
    ///
    /// # Arguments
    ///
    /// - `config` (`&SmaugConfig`) - This global webapp's configuration.
    ///
    /// # Returns
    ///
    /// - `Result<Vec<Form>>` - A vector of all valid forms
    ///
    /// # Errors
    ///
    /// A lot of things can go wrong here, but you'll know when something goes wrong!
    pub async fn get_all_form_defs(config: &SmaugConfig) -> Result<Vec<Form>> {
        let SmaugConfig { pipelines, .. } = config;
        let all_branches = versionning::get_all_branches(pipelines)
            .await
            .context("Impossible de lister les branches")?;

        let mut all_forms: Vec<Form> = Vec::new();

        for branch_name in all_branches.into_iter() {
            // 1. List yaml files on this branch. A deleted or unreachable branch must
            // not discard every other branch's forms: log it and keep going.
            let items = match versionning::list_yaml_forms(&pipelines, &branch_name).await {
                Ok(items) => items,
                Err(e) => {
                    warn!(
                        "Impossible de lister les formulaires de la branche {}: {}",
                        branch_name, e
                    );
                    continue;
                }
            };
            // For each form in given branch
            for dir_item in items {
                let form_identification = FileIdentification::GitBranchAndPath {
                    branch: branch_name.to_string(), // Clone is mandatory
                    file_path: dir_item.path,        // MOVED: No clone needed!
                };

                // parse yaml
                let mut form: Form = match yaml_serde::from_str(&dir_item.content) {
                    Ok(f) => f,
                    Err(e) => {
                        // Read the path back out of the enum you just created
                        let path_for_warn = match &form_identification {
                            FileIdentification::GitBranchAndPath { file_path, .. } => file_path,
                            FileIdentification::GitCommit {
                                file_path: form_path,
                                ..
                            } => form_path,
                            _ => &PathBuf::new(),
                        };

                        warn!(
                            "Impossible de sérialiser {} sur la branche {}: {}",
                            path_for_warn.display(),
                            branch_name,
                            e
                        );
                        continue;
                    }
                };

                // At this point, form identification was set to None, which is the (invalid) default for the FormIdentification type.
                form.form_identification = form_identification;
                all_forms.push(form);
            }
        }

        Ok(all_forms)
    }

    pub async fn get_form(form_identification: FileIdentification) -> Result<Form> {
        let SmaugConfig { pipelines, .. } = crate::config::get_smaug_config();

        let content = versionning::git_get_file(&pipelines, &form_identification)
            .await
            .with_context(|| {
                format!(
                    "Impossible de récupérer le formulaire {}",
                    form_identification.path().display()
                )
            })?;
        let mut form: Form = yaml_serde::from_str(&content).with_context(|| {
            format!(
                "Le formulaire {} n'est pas un YAML valide",
                form_identification.path().display()
            )
        })?;
        form.form_identification = form_identification;
        // Check validity of this form by making sure there are no syntax error in the template definition
        form.check_validity(None)
            .await
            .context("Le formulaire est invalide")?;
        Ok(form)
    }

    // Define variable names (and types) that will be added at launch time
    // It automatically defines the list of reserved names as a const &'static [&'static str]
    additional_vars!(attempt_number: u32, outdir: &str, exports_dir: &str, logs_dir: &str, run_id: i64, run_name:&str);

    /// Resolve the raw jinja template source for this form.
    /// - `Shell`: inline `exec` string.
    /// - `Script`: file fetched from the repo. `script_name` is relative to the
    ///   repo root (no `..` traversal allowed) and resolved at the same
    ///   ref/commit as `self.form_identification`.
    pub async fn template_source(&self) -> Result<String> {
        match &self.exec_type {
            FormExecType::Shell { exec } => Ok(exec.clone()),
            FormExecType::Script { script_name } => {
                if script_name.is_absolute()
                    || script_name.components().any(|c| {
                        matches!(
                            c,
                            std::path::Component::ParentDir | std::path::Component::RootDir
                        )
                    })
                {
                    return Err(FormDefinitionError::InvalidData(format!(
                        "script_name `{}` doit être relatif à la racine du dépôt, sans `..`",
                        script_name.display()
                    ))
                    .into());
                }
                let SmaugConfig { pipelines, .. } = get_smaug_config();
                let script_ident = match &self.form_identification {
                    FileIdentification::GitBranchAndPath { branch, .. } => {
                        FileIdentification::GitBranchAndPath {
                            branch: branch.clone(),
                            file_path: script_name.clone(),
                        }
                    }
                    FileIdentification::GitCommit { commit_hash, .. } => {
                        FileIdentification::GitCommit {
                            commit_hash: commit_hash.clone(),
                            file_path: script_name.clone(),
                        }
                    }
                    FileIdentification::None => {
                        return Err(FormDefinitionError::InvalidData(
                            "Identification du formulaire invalide".into(),
                        )
                        .into());
                    }
                };
                versionning::git_get_file(&pipelines, &script_ident)
                    .await
                    .context("Impossible de lire le fichier de script")
            }
        }
    }

    /// Render the template with the given context, returning the final script.
    pub async fn render_template(&self, context: &impl Serialize) -> Result<String> {
        let template = self.template_source().await?;
        let mut env = Environment::new();
        env.set_undefined_behavior(UndefinedBehavior::SemiStrict);
        match env.render_str(&template, context) {
            Ok(s) => Ok(s),
            Err(e) => match e.kind() {
                minijinja::ErrorKind::UndefinedError => Err(FormDefinitionError::MissingVar(
                    Self::list_missing_vars(&template, context)?.join(", "),
                )
                .into()),
                _ => Err(e).context("Impossible de rendre le template du formulaire"),
            },
        }
    }

    /// Makes sure this form is valid under given circumstances
    ///
    /// - `None`: definition-time check. Rejects reserved var names
    ///   (`outdir`, `attempt_number`) in `self.variables`, then renders the
    ///   template with every declared var set to `""` plus injected vars.
    /// - `Some(vars)`: submission-time check. Renders with the provided user
    ///   vars plus injected placeholders.
    pub async fn check_validity(&self, variables: Option<&HashMap<String, String>>) -> Result<()> {
        match variables {
            None => {
                if let Some(v) = self
                    .variables
                    .iter()
                    .find(|v| Self::INJECTED_VARS.contains(&v.name.as_str()))
                {
                    return Err(FormDefinitionError::ReservedVarName(v.name.to_string()).into());
                }
                let ctx: HashMap<&str, Option<&str>> = self
                    .variables
                    .iter()
                    .map(|v| (v.name.as_str(), None))
                    .chain(Self::INJECTED_VARS.iter().map(|n| (*n, None)))
                    .collect();
                self.render_template(&ctx).await.map(|_| ())?;
            }
            Some(vars) => {
                let ctx = self.jinja_context(vars, 0, "", "", "", 0, "");
                self.render_template(&ctx).await.map(|_| ())?;
            }
        }
        Ok(())
    }

    fn list_missing_vars<T>(template: &str, context: &T) -> Result<Vec<String>, minijinja::Error>
    where
        T: Serialize,
    {
        // Create an isolated environment to parse the template string
        let env = Environment::new();
        let tmpl = env.template_from_str(template)?;

        // Get undeclared variables witin template
        let undeclared_vars = tmpl.undeclared_variables(false);

        // Serialize the context into a MiniJinja Value
        let context_value = Value::from_serialize(context);

        let mut missing = Vec::new();

        for var in undeclared_vars {
            // Attempt to get the attribute from the context.
            // If the context is an object/map, get_attr fetches the value.
            // It returns Ok(UNDEFINED) if the key is missing, or Err if the context
            // type doesn't support attribute lookup.
            match context_value.get_attr(&var) {
                Ok(val) if !val.is_undefined() => {
                    // The variable is present in the context
                }
                _ => {
                    // The variable is missing or context is not an object
                    missing.push(var);
                }
            }
        }

        // Sort to ensure deterministic output
        missing.sort();
        Ok(missing)
    }
}

/// A form preview, cached in the database.
///
/// A form is defined, as the source of truth, by its pair
/// (`branch`, `file_path`) in the git repository: this structure retains
/// exactly that pair, with a name and a description, but leaves the
/// logic (variable definitions, execution mode) to the YAML definition file.
///
/// The visibility of a `FormDef` for a group is carried by the
/// `FormDefHasGroup` table (a simple SQL join). This table is rebuilt
/// by `FormDef::refresh_cache`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CachedForm {
    /// The ID of a form. **Do not** use it to identify a form as such.
    pub form_def_id: i64,
    #[serde(flatten)]
    #[serde(skip_deserializing)]
    pub form_identification: FileIdentification,
    pub name: String,
    pub description: String,
}

/// A proxy for a form definition
///
/// Form definitions are stored in a web-served git repository, and can be referred to in two main ways:
/// - (branch name, file path)
/// - (commit hash, file path)
///
/// Both are great, but not so convenient: having to do a http query and a full form parse
/// only for the user to get the file identification, name and description, is not really worth it.
///
/// Hence, this proxy, that stores those three fields of interest and show them to the user in the 'launch' page.
impl CachedForm {
    /// Rebuild the FormDef cache from the actual Form repository.
    ///
    /// This is an all-in-one transaction: it deletes every stale cached FormDef,
    /// then re-inserts all the forms found on the git repository
    /// (see `Form::get_all_form_defs`), and rebuilds `FormDefHasGroup` from the
    /// `groups` field of each form definition.
    ///
    /// # Arguments
    ///
    /// - `pool` (`&SqlitePool`) - A handle to the database
    /// - `config` (`&SmaugConfig`) - Application configuration (git repository, mainly)
    ///
    /// # Returns
    ///
    /// - `Result<Vec<CachedForm>>` - On success, the whole cache (which is equal to
    ///   the `CachedForm`s found in the repository)
    ///
    /// # Errors
    ///
    /// Returns an error in case of database or git repository issues.
    pub async fn refresh_cache(pool: &SqlitePool, config: &SmaugConfig) -> Result<Vec<Self>> {
        let all_forms = Form::get_all_form_defs(config).await?;
        Self::rebuild_cache_from_forms(pool, all_forms)
            .await
            .context("Impossible de reconstruire le cache des formulaires")
    }

    /// Synchronously rebuilds `FormDefs` and `FormDefHasGroup` from an
    /// already-scanned list of form definitions (see `Form::get_all_form_defs`).
    pub(crate) async fn rebuild_cache_from_forms(
        pool: &SqlitePool,
        all_forms: Vec<Form>,
    ) -> Result<Vec<Self>> {
        let all_groups = groups::Group::get_groups_with_ids(pool).await?;

        let mut tx = pool
            .begin()
            .await
            .context("Impossible d'ouvrir la transaction de reconstruction du cache")?;

        sqlx::query("DELETE FROM FormDefHasGroup")
            .execute(&mut *tx)
            .await
            .context("Impossible de vider la table FormDefHasGroup")?;
        sqlx::query("DELETE FROM FormDefs")
            .execute(&mut *tx)
            .await
            .context("Impossible de vider la table FormDefs")?;
        sqlx::query("DELETE from sqlite_sequence where name='FormDefs'")
            .execute(&mut *tx)
            .await
            .context("Impossible de réinitialiser les IDs des formulaires")?;

        let mut cached = Vec::new();
        for form in all_forms {
            let Form {
                name,
                description,
                groups,
                form_identification: FileIdentification::GitBranchAndPath { branch, file_path },
                ..
            } = form
            else {
                continue;
            };

            let form_def_id: i64 = sqlx::query(
                r#"
                INSERT INTO FormDefs (branch, file_path, name, description) VALUES (?, ?, ?, ?)
                RETURNING form_def_id
                "#,
            )
            .bind(&branch)
            .bind(&file_path.to_string_lossy())
            .bind(&name)
            .bind(&description)
            .fetch_one(&mut *tx)
            .await
            .with_context(|| format!("Impossible d'insérer le formulaire {branch}/{file_path:?}"))?
            .try_get("form_def_id")?;

            if let Some(groups) = groups {
                for group_name in groups {
                    match all_groups.iter().find(|g| g.name == group_name) {
                        Some(group) => {
                            sqlx::query(
                                r#"INSERT INTO FormDefHasGroup (form_def_id, group_id) VALUES (?, ?)"#,
                            )
                            .bind(form_def_id)
                            .bind(group.id)
                            .execute(&mut *tx)
                            .await
                            .with_context(|| {
                                format!(
                                    "Impossible d'associer le formulaire {branch}/{file_path:?} au groupe {}",
                                    group.name
                                )
                            })?;
                        }
                        None => log::warn!(
                            "Le formulaire {}/{} attend le groupe {}, qui n'existe pas en base",
                            branch,
                            file_path.display(),
                            group_name,
                        ),
                    }
                }
            }

            cached.push(CachedForm {
                form_def_id,
                form_identification: FileIdentification::GitBranchAndPath { branch, file_path },
                description,
                name,
            });
        }

        tx.commit()
            .await
            .context("Impossible de valider la reconstruction du cache")?;

        Ok(cached)
    }

    /// All the cached `FormDef`s (from the database only, no HTTP request)
    pub async fn get_all(pool: &SqlitePool) -> Result<Vec<Self>> {
        Ok(sqlx::query(
            r#"
            SELECT form_def_id, branch, file_path, name, description FROM FormDefs ORDER BY branch, file_path
            "#,
        )
        .fetch_all(pool)
        .await
        .context("Impossible de lister les formulaires en cache")?
        .iter()
        .filter_map(form_def_from_row)
        .collect())
    }

    /// Lists (branch, file_path) pairs that are visible to given groups.
    ///
    /// Until now, that info was scattered in form definitions; it now lives in the
    /// `FormDefs` and `FormDefHasGroup` tables, and is resolved with a simple SQL query.
    ///
    /// # Arguments
    ///
    /// - `pool` (`&SqlitePool`) - A handle to the database
    /// - `groups` (`Vec<Group>`) - The groups doing the search
    ///
    /// # Returns
    ///
    /// - `Result<Vec<(String, String)>>` - The list of visible (branch, file_path) pairs
    ///
    /// # Errors
    ///
    /// Returns an error in case of database issues.
    pub async fn visible_form_pairs(
        pool: &SqlitePool,
        groups: Vec<groups::Group>,
    ) -> Result<Vec<(String, String)>> {
        let group_ids: Vec<i64> = groups.into_iter().map(|g| g.id).collect();

        if group_ids.is_empty() {
            return Ok(Vec::new());
        }

        let placeholders = vec!["?"; group_ids.len()].join(", ");

        let query_str = format!(
            r#"
            SELECT DISTINCT fd.branch, fd.file_path
            FROM FormDefs fd
            JOIN FormDefHasGroup fhg ON fhg.form_def_id = fd.form_def_id
            WHERE fhg.group_id IN ({placeholders})
            ORDER BY fd.branch, fd.file_path
            "#
        );

        let mut query = sqlx::query(&query_str);

        for group_id in group_ids {
            query = query.bind(group_id);
        }

        Ok(query
            .fetch_all(pool)
            .await
            .context("Impossible de lister les formulaires visibles pour ces groupes")?
            .into_iter()
            .filter_map(|row| Some((row.try_get("branch").ok()?, row.try_get("file_path").ok()?)))
            .collect())
    }

    /// The group ids that a (branch, file_path) pair is associated with.
    ///
    /// This is a simple SQL query, and does not require any HTTP request to the
    /// git repository: the association lives in the `FormDefHasGroup` table. A
    /// form that is not cached (or has no group) returns an empty list; it is
    /// then considered admin-only.
    ///
    /// # Arguments
    ///
    /// - `pool` (`&SqlitePool`) - A handle to the database
    /// - `branch` (`&str`) - The branch the form lives on
    /// - `form_path` (`&str`) - The path to the form yaml file, within said branch
    ///
    /// # Returns
    ///
    /// - `Result<Vec<i64>>` - On success, the list of group ids that are associated with this form
    ///
    /// # Errors
    ///
    /// Returns an error in case of database issues.
    pub async fn get_group_ids(
        pool: &SqlitePool,
        branch: &str,
        form_path: &str,
    ) -> Result<Vec<i64>> {
        Ok(sqlx::query(
            r#"
            SELECT fhg.group_id
            FROM FormDefs fd
            JOIN FormDefHasGroup fhg ON fhg.form_def_id = fd.form_def_id
            WHERE fd.branch = ? AND fd.file_path = ?
            "#,
        )
        .bind(branch)
        .bind(form_path)
        .fetch_all(pool)
        .await
        .with_context(|| {
            format!("Impossible de lire les groupes du formulaire {branch}/{form_path}")
        })?
        .into_iter()
        .filter_map(|row| row.try_get("group_id").ok())
        .collect())
    }

    /// Returns a tuple of (cached form defs with an assigned group, cached form defs
    /// with no assigned group).
    /// Form defs with no assigned group are available to admin users only.
    ///
    /// Both lists come straight from the cache, with a simple SQL query each.
    ///
    /// # Arguments
    ///
    /// - `pool` (`&SqlitePool`) - A handle to the database
    /// - `group_id` (`i64`) - Group ID to retrieve available forms to
    ///
    /// # Returns
    ///
    /// - `Result<Vec<CachedForm>>` - form defs visible to the given group (or the
    ///   form defs with no assigned group when `group_id` is `None`).
    ///
    /// # Errors
    ///
    /// Returns an error in case of database issues.
    pub async fn get_form_defs_for_group(
        pool: &SqlitePool,
        group_id: Option<i64>,
    ) -> Result<Vec<CachedForm>> {
        let forms = match group_id {
            Some(group_id) => sqlx::query(
                r#"
                SELECT fd.form_def_id, fd.branch, fd.file_path, name, description
                FROM FormDefs fd
                JOIN FormDefHasGroup fhg ON fhg.form_def_id = fd.form_def_id
                WHERE fhg.group_id = ?
                ORDER BY fd.branch, fd.file_path
                "#,
            )
            .bind(group_id)
            .fetch_all(pool)
            .await
            .with_context(|| {
                format!("Impossible de lister les formulaires du groupe {group_id}")
            })?,
            None => sqlx::query(
                r#"
                SELECT fd.form_def_id, fd.branch, fd.file_path, name, description
FROM FormDefs fd
WHERE NOT EXISTS (
    SELECT 1 
    FROM FormDefHasGroup fhg 
    WHERE fhg.form_def_id = fd.form_def_id
)
ORDER BY fd.branch, fd.file_path"#,
            )
            .fetch_all(pool)
            .await
            .context("Impossible de lister les formulaires sans groupe")?,
        };

        Ok(forms.iter().filter_map(form_def_from_row).collect())
    }
}

/// Builds a `FormDef` from a row selecting `form_def_id`, `branch` and `file_path`
fn form_def_from_row(row: &sqlx::sqlite::SqliteRow) -> Option<CachedForm> {
    let branch = row.try_get("branch").ok()?;
    let file_path = PathBuf::from_str(&row.try_get::<String, _>("file_path").ok()?).ok()?;
    Some(CachedForm {
        form_def_id: row.try_get("form_def_id").ok()?,
        form_identification: FileIdentification::GitBranchAndPath { branch, file_path },
        description: row.try_get("description").ok()?,
        name: row.try_get("name").ok()?,
    })
}

#[cfg(test)]
mod test {

    use std::fmt::{Debug, Display};

    use crate::models::VariableType::{DateEdit, ExistingFile, ValuesList};

    use super::*;

    #[test]
    fn test_form_serialize() {
        let expected = Form {
                        name: "Smaug-v3 simple".to_string(),
                        description: "Démultiplexe à partir d'un dossier de run brut, \
                        lance l'analyse smaug avec le BED spécifié, puis copie \
                        le résultat sur le NAS."
                            .to_string(),
                        variables: vec![
                            Variable {
                                name: "bed".to_string(),
                                title: "BED".to_string(),
                                description: "Le fichier BED à utiliser pour cette analyse".to_string(),
                                variable_type: VariableType::ListFromURL {
                                    source: "/smaug/api/aux/list_beds".to_string()
                                },
                            },
                            Variable {
                                name: "genome".to_string(),
                                title: "Génome".to_string(),
                                description: "Le génome à utiliser".to_string(),
                                variable_type: ValuesList {
                                    values: vec!["hg19".to_string(), "hg38".to_string()]
                                }
                            },
                            Variable {
                                name: "indir".to_string(),
                                title: "Dossier BCL".to_string(),
                                description: "Le dossier de run brut Illumina".to_string(),
                                variable_type: VariableType::ListFromURL {
                                    source: "/smaug/api/aux/list_illumina_dirs".to_string()
                                },
                            },
                            Variable {
                                name: "panel".to_string(),
                                title: "Nom du panel".to_string(),
                                description: "Le nom du panel".to_string(),
                                variable_type: VariableType::ListFromURL {
                                    source: "/smaug/api/aux/list_panels".to_string()
                                },
                            },
                            Variable {
                                name: "sample_sheet".to_string(),
                                title: "SampleSheet".to_string(),
                                description: "La samplesheet, espèce de neuneu".to_string(),
                                variable_type: VariableType::ExistingFile,
                            }
                        ],
                        form_identification: FileIdentification::None,

                        workdir: "outdir".to_string(),
                        groups: Some(vec!["Admin".to_string(),"Génétique".to_string()]),
                        exec_type: FormExecType::Shell { exec: r#"# Copie de la samplesheet dans le bon dossier
rsync {{ sample_sheet }} {{ indir }}/SampleSheet.csv

# Démultiplexage
snakemake -s pipelines/demul/Snakefile --config indir={{ indir }} outdir={{ outdir }}

# Copie vers MOABI 
rsync -a --info=progress2 "{{ outdir }}/fastq" user@depot:/where/it/should/go

# Smaug v3
snakemake -s pipelines/smaug-v3/Snakefile --configfile pipelines/smaug-v3/config.yaml --config indir={{ outdir }} outdir={{ outdir }}/tentative-{{ attempt_number }}

# Copie NAS
rsync -a --info=progress2 "{{ outdir }}/{bam,vcf,reports}" /mnt/nas/analysis/{{ panel }}
"#.to_string() }};
        let form: Form = yaml_serde::from_str(include_str!("../../../.forms/smaug-basique.yaml"))
            .expect("Cannot serialize");

        let expected_str = serde_json::to_string(&expected).expect("Cannot serialize");
        let form_str = serde_json::to_string(&form).expect("Cannot serialize back");

        assert_eq_display_helper(&expected_str, &form_str);
    }

    /// Just a helper to print what the assert_eq actually got on the left side.
    /// Applies to types that implement the Display trait.
    #[track_caller]
    fn assert_eq_display_helper<T>(left: &T, right: &T)
    where
        T: Display + Debug + PartialEq<T> + ?Sized,
    {
        assert_eq!(
            left, right,
            "\n------BEGIN_LEFT------\n{}\n-------END_LEFT-------\n\n------BEGIN_RIGHT------\n{}\n-------END_RIGHT-------\n",
            left, right
        );
    }

    #[test]
    fn test_vars_to_html_values_list() {
        // Test a variable of type ValuesList
        let v = Variable {
            title: "Une variable".to_string(),
            name: "v1".to_string(),
            description: "Une simple description".to_string(),
            variable_type: ValuesList {
                values: vec!["A".to_string(), "B".to_string()],
            },
        };
        let v_html = v.to_html_safe();
        assert_eq_display_helper(
            v_html.as_str(),
            r#"<div class="card-container" id="v1-card">
            <label>Une variable</label>
            <span>Une simple description</span>
            <select id="v1" name="v1">
                    <option value="A">A</option>
<option value="B">B</option>
                    <option value="" disabled selected value>---Choisissez une valeur---</option>
                    </select>
        </div>"#,
        );
    }

    #[test]
    fn test_vars_to_html_existing_file() {
        //
        let v = Variable {
            title: "Une autre variable".to_string(),
            name: "v2".to_string(),
            description: "Whatever".to_string(),
            variable_type: ExistingFile,
        };
        let v_html = v.to_html_safe();
        assert_eq_display_helper(
            v_html.as_str(),
            r#"<div class="card-container" id="v2-card">
            <label>Une autre variable</label>
            <span>Whatever</span>
            <input type="file" id="v2" name="v2">
        </div>"#,
        );
    }

    #[test]
    fn test_vars_to_html_date_edit() {
        // Test a variable of type ValuesList
        let v1 = Variable {
            title: "Une variable".to_string(),
            name: "v1".to_string(),
            description: "Une simple description".to_string(),
            variable_type: DateEdit,
        };
        let v1_html = v1.to_html_safe();
        assert_eq_display_helper(
            v1_html.as_str(),
            r#"<div class="card-container" id="v1-card">
            <label>Une variable</label>
            <span>Une simple description</span>
            <input type="date" id="v1" name="v1">
        </div>"#,
        );
    }
}

/// Tests for the SQL cache (`FormDefs` / `FormDefHasGroup`)
#[cfg(test)]
mod form_def_cache_tests {
    use std::{collections::HashMap, path::Path};

    use sqlx::SqlitePool;

    use crate::models::{
        CachedForm, FileIdentification, Form, FormDefinitionError, FormExecType, Group,
    };

    async fn test_pool() -> SqlitePool {
        // The in-memory cache lives per connection: a single connection in the pool
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("pool en mémoire");
        crate::db::run_migrations(&pool).await.expect("migrations");
        pool
    }

    fn form(name: &str, branch: &str, file_path: &str, groups: &[&str]) -> Form {
        Form {
            name: name.to_string(),
            description: String::new(),
            variables: vec![],
            exec_type: Default::default(),
            workdir: String::new(),
            form_identification: FileIdentification::GitBranchAndPath {
                branch: branch.to_string(),
                file_path: Path::new(file_path).to_path_buf(),
            },
            groups: Some(groups.iter().map(|s| s.to_string()).collect()),
        }
    }

    fn form_with_shell(
        name: &str,
        branch: &str,
        file_path: &str,
        groups: &[&str],
        shell: &str,
    ) -> Form {
        Form {
            name: name.to_string(),
            description: String::new(),
            variables: vec![],
            exec_type: FormExecType::Shell {
                exec: shell.to_string(),
            },
            workdir: String::new(),
            form_identification: FileIdentification::GitBranchAndPath {
                branch: branch.to_string(),
                file_path: Path::new(file_path).to_path_buf(),
            },
            groups: Some(groups.iter().map(|s| s.to_string()).collect()),
        }
    }

    fn group(id: i64) -> Group {
        Group {
            id,
            name: format!("g{id}"),
        }
    }

    #[tokio::test]
    async fn test_cache_lists_and_visibility() {
        let pool = test_pool().await;

        let bio = Group::add_group(&pool, "Bio").await.expect("add Bio");
        let chimie = Group::add_group(&pool, "Chimie").await.expect("add Chimie");

        let cached = CachedForm::rebuild_cache_from_forms(
            &pool,
            vec![
                form("F1", "main", ".forms/f1.yaml", &["Bio"]),
                form("F2", "dev", ".forms/f2.yaml", &[]),
                // Group that does not exist in the database: ignored
                form("F3", "main", ".forms/f3.yaml", &["Fantome"]),
            ],
        )
        .await
        .expect("rebuild cache");

        assert_eq!(cached.len(), 3);

        let all = CachedForm::get_all(&pool).await.expect("get_all");
        assert_eq!(
            all,
            vec![
                CachedForm {
                    form_def_id: all[0].form_def_id,
                    form_identification: FileIdentification::GitBranchAndPath {
                        branch: "dev".into(),
                        file_path: ".forms/f2.yaml".into()
                    },
                    description: "".to_string(),
                    name: "F2".to_string()
                },
                CachedForm {
                    form_def_id: all[1].form_def_id,
                    form_identification: FileIdentification::GitBranchAndPath {
                        branch: "main".into(),
                        file_path: ".forms/f1.yaml".into()
                    },
                    description: "".to_string(),
                    name: "F1".to_string()
                },
                CachedForm {
                    form_def_id: all[2].form_def_id,
                    form_identification: FileIdentification::GitBranchAndPath {
                        branch: "main".into(),
                        file_path: ".forms/f3.yaml".into()
                    },
                    description: "".to_string(),
                    name: "F3".to_string()
                },
            ]
        );

        let forms_with_group = CachedForm::get_form_defs_for_group(&pool, Some(bio))
            .await
            .expect("for group");
        assert_eq!(forms_with_group.len(), 1);
        assert_eq!(
            forms_with_group[0].form_identification,
            FileIdentification::GitBranchAndPath {
                branch: "main".to_string(),
                file_path: Path::new(".forms/f1.yaml").to_path_buf()
            }
        );

        let forms_without_group = CachedForm::get_form_defs_for_group(&pool, None)
            .await
            .expect("for no group");
        assert_eq!(forms_without_group.len(), 2);

        assert_eq!(
            CachedForm::get_group_ids(&pool, "main", ".forms/f1.yaml")
                .await
                .expect("ids"),
            vec![bio]
        );
        assert!(
            CachedForm::get_group_ids(&pool, "dev", ".forms/f2.yaml")
                .await
                .expect("ids")
                .is_empty()
        );
        assert!(
            CachedForm::get_group_ids(&pool, "main", ".forms/f3.yaml")
                .await
                .expect("ids")
                .is_empty()
        );

        assert_eq!(
            CachedForm::visible_form_pairs(&pool, vec![group(bio)])
                .await
                .expect("pairs"),
            vec![("main".to_string(), ".forms/f1.yaml".to_string())]
        );
        assert!(
            CachedForm::visible_form_pairs(&pool, vec![group(chimie)])
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            CachedForm::visible_form_pairs(&pool, vec![])
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn test_cache_refresh_purges_stale_entries() {
        let pool = test_pool().await;

        Group::add_group(&pool, "Bio").await.expect("add Bio");

        CachedForm::rebuild_cache_from_forms(
            &pool,
            vec![form("F1", "main", ".forms/f1.yaml", &["Bio"])],
        )
        .await
        .expect("first rebuild");

        // A new scan now finds only F1, with no group
        CachedForm::rebuild_cache_from_forms(
            &pool,
            vec![form("F1", "main", ".forms/f1.yaml", &[])],
        )
        .await
        .expect("second rebuild");

        let all = CachedForm::get_all(&pool).await.expect("get_all");
        assert_eq!(all.len(), 1);
        assert_eq!(
            all[0].form_identification,
            FileIdentification::GitBranchAndPath {
                branch: "main".to_string(),
                file_path: Path::new(".forms/f1.yaml").to_path_buf()
            }
        );

        // The old association (F1 -> Bio) must no longer exist
        assert!(
            CachedForm::get_group_ids(&pool, "main", ".forms/f1.yaml")
                .await
                .expect("ids")
                .is_empty()
        );

        let forms_with_group = CachedForm::get_form_defs_for_group(&pool, Some(1))
            .await
            .expect("for group");
        let forms_without_group = CachedForm::get_form_defs_for_group(&pool, None)
            .await
            .expect("for no group");
        assert!(forms_with_group.is_empty());
        assert_eq!(forms_without_group.len(), 1);
    }

    #[tokio::test]
    async fn test_render() {
        let ctx: HashMap<&str, &str> = [("a", "valueA"), ("b", "valueB")].into_iter().collect();
        let simple_form = form_with_shell(
            "whatever",
            "main",
            "pipelineA/form.yaml",
            &["admin", "genet"],
            "une {{a}} erreur {{b}} manifeste {{c}}",
        );
        let Err(e) = simple_form.render_template(&ctx).await else {
            panic!("Should be an error")
        };
        match e.downcast::<FormDefinitionError>() {
            Ok(FormDefinitionError::MissingVar(v)) if v.as_str() == "c" => {}
            _ => {
                panic!(r#"Expected FormDefinitionError::MissingVar("c")"#)
            }
        }

        let simple_form = form_with_shell(
            "whatever",
            "main",
            "pipelineA/form.yaml",
            &["admin", "genet"],
            "une {{a}} erreur {{b}} manifeste {{c}}",
        );

        let Err(e) = simple_form
            .render_template(&HashMap::<&str, &str>::new())
            .await
        else {
            panic!("Should be an error")
        };
        match e.downcast::<FormDefinitionError>() {
            Ok(FormDefinitionError::MissingVar(v)) if v.as_str() == "a, b, c" => {}
            Ok(FormDefinitionError::MissingVar(v)) => {
                panic!("Got: {}", v)
            }
            _ => {
                panic!(r#"Expected FormDefinitionError::MissingVar("c")"#)
            }
        }
    }
}
