use log::error;
use minijinja::{Environment, Value};
use rocket::fairing::AdHoc;
use rocket::http::Status;
use rocket::request::Request;
use rocket::response::{self, Responder, content::RawHtml};

#[cfg(not(debug_assertions))]
use rust_embed::RustEmbed;

// Re-export of context
pub use minijinja::context;

/// These assets are contained in the final binary
#[cfg(not(debug_assertions))]
#[derive(RustEmbed)]
#[folder = "../static/templates"]
struct TemplateAssets;

pub struct Template {
    name: String,
    context: Value,
}

impl Template {
    /// Renders the template `name` with the context `context`.
    ///
    /// The file `static/templates/{name}.html.j2` must exist, otherwise an error is returned.
    pub fn render<S: Into<String>>(name: S, context: Value) -> Self {
        Self {
            name: name.into(),
            context,
        }
    }
}

/// Responder implementation so that Rocket knows how to return this Template
impl<'r, 'o: 'r> Responder<'r, 'o> for Template {
    fn respond_to(self, request: &'r Request<'_>) -> response::Result<'o> {
        // Retrieve the Minijinja environment from Rocket's global state, via the request
        let env = request.rocket().state::<Environment>().ok_or_else(|| {
            error!(
                "L'environnement Minijinja n'est pas géré par Rocket. As-tu oublié le fairing ?"
            );
            Status::InternalServerError
        })?;

        // Load the template
        let tmpl = env
            .get_template(&format!("{}.html.j2", self.name))
            .map_err(|e| {
                error!("Erreur de chargement du template '{}': {}", self.name, e);
                Status::InternalServerError
            })?;

        // Render with the context
        let rendered = tmpl.render(self.context).map_err(|e| {
            error!("Erreur lors du rendu du template '{}': {}", self.name, e);
            Status::InternalServerError
        })?;

        // Delegate the response to RawHtml to ensure the correct Content-Type (text/html)
        RawHtml(rendered).respond_to(request)
    }
}

#[cfg(debug_assertions)]
pub fn minijinja_fairing() -> AdHoc {
    AdHoc::try_on_ignite("Minijinja Embedded Templates", |rocket| async {
        use std::path::Path;

        let mut env = Environment::new();

        // Template root at run time
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let template_root = match manifest_dir
            .join("..")
            .join("static")
            .join("templates")
            .canonicalize()
        {
            Ok(r) => r,
            Err(e) => {
                log::error!("Impossible de trouver le dossier des templates: {}", e);
                return Err(rocket);
            }
        };
        eprintln!("{}", template_root.display());

        if !template_root.exists() {
            log::error!("Dossier templates introuvable: {}", template_root.display());
            return Err(rocket);
        }

        for entry in walkdir::WalkDir::new(&template_root) {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    log::error!("Walkdir error: {}", e);
                    continue;
                }
            };

            let path = entry.path();
            if !path.is_file() {
                continue;
            }

            // Relative path used as the template name
            let rel = match path.strip_prefix(&template_root) {
                Ok(r) => r,
                Err(_) => continue,
            };

            // Keep the same prefix as RustEmbed so that the `render!` calls remain identical
            let template_name = rel.display().to_string();
            log::info!("Adding template {}", template_name);

            match std::fs::read_to_string(path) {
                Ok(content) => {
                    if let Err(e) = env.add_template_owned(template_name, content) {
                        log::error!("Erreur lors de l'ajout d'un template: {}", e);
                        return Err(rocket);
                    }
                }
                Err(e) => {
                    log::error!("Impossible de lire {}: {}", path.display(), e);
                    return Err(rocket);
                }
            }
        }

        let rocket = rocket.manage(env);
        Ok(rocket)
    })
}

#[cfg(not(debug_assertions))]
pub fn minijinja_fairing() -> AdHoc {
    AdHoc::try_on_ignite("Minijinja Embedded Templates", |rocket| async {
        let mut env = Environment::new();

        for file_path in TemplateAssets::iter() {
            if let Some(file) = TemplateAssets::get(&file_path) {
                if let Ok(content) = std::str::from_utf8(&file.data) {
                    if let Err(e) =
                        env.add_template_owned(file_path.to_string(), content.to_string())
                    {
                        error!("Erreur lors de l'ajout d'un template: {}", e);
                        return Err(rocket);
                    }
                }
            }
        }

        let rocket = rocket.manage(env);
        Ok(rocket)
    })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use minijinja::context;
    use walkdir::WalkDir;

    fn load_env() -> Environment<'static> {
        let mut env = Environment::new();

        let template_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("static")
            .join("templates")
            .canonicalize()
            .unwrap();
        for file_path in WalkDir::new(&template_root) {
            let file_path = file_path.unwrap();
            if file_path.file_type().is_file() {
                let rel = file_path.path().strip_prefix(&template_root).unwrap();

                let content = std::fs::read_to_string(file_path.path()).unwrap();
                env.add_template_owned(rel.display().to_string(), content)
                    .unwrap_or_else(|e| panic!("parse {}: {e}", file_path.file_name().display()));
            }
        }
        env
    }

    #[test]
    fn all_templates_parse() {
        let env = load_env();
        assert!(env.get_template("base.html.j2").is_ok());
        assert!(env.get_template("common/run_base.html.j2").is_ok());
        assert!(env.get_template("common/welcome.html.j2").is_ok());
        assert!(env.get_template("admin/dashboard.html.j2").is_ok());
        assert!(env.get_template("common/runningrun.html.j2").is_ok());
    }

    #[test]
    fn runtime_config_translates_enum_values() {
        use crate::models::appstate::{ActiveRoutine, ConfigItem, SigintBehavior};

        let env = load_env();
        let html = env
            .get_template("admin/runtime_config.html.j2")
            .unwrap()
            .render(context! {
                entries => vec![
                    context! {
                        key => "on_sigint",
                        input_html => SigintBehavior::value_input().render_html("on_sigint", "StopJobs"),
                        description => "Signal behavior",
                    },
                    context! {
                        key => "active_routine",
                        input_html => ActiveRoutine::value_input().render_html("active_routine", "RoutineOff"),
                        description => "Routine state",
                    },
                    context! {
                        key => "shutdown_date",
                        input_html => crate::models::appstate::ValueInput::Date.render_html("shutdown_date", "2026-01-01"),
                        description => "Date",
                    },
                ]
            })
            .unwrap();

        assert!(html.contains("Arrêter les jobs"));
        assert!(html.contains("Routine désactivée"));
        assert!(html.contains("value=\"2026-01-01\""));
    }

    #[test]
    fn welcome_renders_from_base() {
        let env = load_env();
        let html = env
            .get_template("common/welcome.html.j2")
            .unwrap()
            .render(context! {})
            .unwrap();
        assert!(html.contains("<!DOCTYPE html>"));
        assert!(html.contains("Pipelines et outils d’analyses génomiques"));
        assert!(html.contains("/smaug/login"));
        assert!(html.contains("/smaug/static/css/app.css"));
        assert!(html.contains("/smaug/static/img/dragon.png"));
        assert!(!html.contains("Déconnexion"));
    }

    #[test]
    fn login_renders_without_banner() {
        let env = load_env();
        let html = env
            .get_template("common/login.html.j2")
            .unwrap()
            .render(context! {})
            .unwrap();
        assert!(html.contains("Connexion | SMAUG"));
        assert!(html.contains("login-form"));
        assert!(html.contains("pages/login.js"));
    }

    #[test]
    fn error_pages_render() {
        let env = load_env();
        let unauthorized = env
            .get_template("errors/unauthorized.html.j2")
            .unwrap()
            .render(context! { title => "x", h2 => "y", message => "z" })
            .unwrap();
        assert!(unauthorized.contains("state-card"));
        assert!(unauthorized.contains("Se connecter"));

        let admin_only = env
            .get_template("errors/admin_only.html.j2")
            .unwrap()
            .render(context! {})
            .unwrap();
        assert!(admin_only.contains("Accès protégé"));
    }

    #[test]
    fn inherited_pages_render_with_minimal_context() {
        let env = load_env();
        let pages = [
            "admin/register.html.j2",
            "admin/dashboard.html.j2",
            "admin/users_list.html.j2",
            "common/home.html.j2",
            "common/listruns.html.j2",
            "common/searchrun.html.j2",
            "common/redirect.html.j2",
            "common/error.html.j2",
            "common/pendingrun.html.j2",
            "common/successrun.html.j2",
            "common/failurerun.html.j2",
            "common/idlerun.html.j2",
            "common/runningrun.html.j2",
            "common/editrun.html.j2",
            "admin/groups_list.html.j2",
            "admin/passedit.html.j2",
        ];
        let ctx = context! {
            title => "t",
            h2 => "h",
            message => "m",
            seconds => 5,
            target_url => "/",
            user => context! { username => "u", id => 1 },
            edited_user => context! { username => "u", id => 1 },
            user_groups => Vec::<minijinja::Value>::new(),
            is_admin => false,
            pipelines_struct => context! {},
            edit_mode => false,
            form => context! {
                name => "f",
                description => "",
                branch => "main",
                file_path => "forms/f.yaml",
                variables => Vec::<minijinja::Value>::new(),
            },
            run => context! {
                run_id => 1,
                run_name => "r",
                status => "Idle",
                branch_name => "main",
                form_path => "forms/f.yaml",
                creation_date => "",
                attempt_count => 0,
                user_defined_vars => context! {},
            },
            attempt => context! {
                attempt_number => 1,
                commit_hash => "abc1234",
                comment => "",
                user_defined_vars => context! {},
            },
            history => Vec::<minijinja::Value>::new(),
            fail_reason => "",
        };
        for page in pages {
            env.get_template(page)
                .unwrap_or_else(|e| panic!("load {page}: {e}"))
                .render(ctx.clone())
                .unwrap_or_else(|e| panic!("render {page}: {e}"));
        }

        let running_page = env
            .get_template("common/runningrun.html.j2")
            .unwrap()
            .render(ctx)
            .unwrap();
        assert!(running_page.contains("id=\"run-data\""));
        assert!(running_page.contains("/smaug/static/js/pages/run.js"));
        assert!(running_page.contains("slurm-jobs-table"));
    }
}
