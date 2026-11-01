use std::collections::HashSet;
use std::net::Ipv4Addr;

use anyhow::bail;
use clap::Parser;
use include_dir::Dir;
use include_dir::include_dir;
use log::error;
use log::info;
use rocket::Build;
use rocket::Config;
use rocket::Rocket;
use rocket::config::Shutdown;
use rocket::fs::FileServer;
use rocket::get;
use rocket::response::Redirect;
use rocket_include_dir::StaticFiles;
use smaug_lib::templates::{Template, context, minijinja_fairing};
use tokio::signal::unix::{SignalKind, signal};

use rocket::{catch, catchers, routes};

use anyhow::Result;
use smaug_lib::config;
use smaug_lib::pipeline_exec::{RunProgressCache, SqueueCache};
use smaug_lib::routes;
use sqlx::SqlitePool;
use tokio::sync::broadcast;

use crate::routine::run_routine_loop;

#[catch(401)]
async fn unauthorized() -> Template {
    Template::render(
        "errors/unauthorized",
        context! {
            title=> "Session expirée",
            h2=> "Session expirée",
            message=> "Votre session a expirée, veuillez vous reconnecter"
        },
    )
}

#[catch(404)]
async fn not_found() -> Template {
    Template::render(
        "errors/not_found",
        context! {
            title=> "Page introuvable",
            h2=> "Page introuvable",
            message=> "La page demandée n’existe pas ou a été déplacée."
        },
    )
}

#[get("/documentation")]
pub fn doc_redirect() -> Redirect {
    Redirect::permanent("/smaug/documentation/index.html")
}

async fn rocket(
    pool: SqlitePool,
    progress_cache: RunProgressCache,
    host: Option<Ipv4Addr>,
    port: Option<u16>,
) -> Rocket<Build> {
    let config = config::get_smaug_config();

    let shutdown_config = Shutdown {
        ctrlc: false,
        signals: HashSet::new(),
        ..Default::default()
    };

    let mut figment = Config::figment().merge(("shutdown", shutdown_config));
    if let Some(host) = host {
        figment = figment.merge(("address", host));
    }
    if let Some(port) = port {
        figment = figment.merge(("port", port));
    }

    let mut r = rocket::custom(figment)
        .register("/smaug", catchers![unauthorized, not_found])
        .mount("/smaug/uploads", routes![routes::backend::serve_upload])
        .mount("/smaug/logs", routes![routes::backend::serve_run_log])
        .mount("/smaug/files", routes![routes::backend::serve_run_file])
        .mount(
            "/smaug",
            routes![
                // Home page
                routes::frontend::welcome_page_get,
                // Login page
                routes::frontend::login_get,
                // Simple GET to log out; the only GET route that belongs to the backend
                routes::backend::logout_get,
                // Frontend routes: always return HTML
                routes::frontend::home_get,
                routes::frontend::password_edit_get,
                routes::frontend::new_run_get,
                // Display a specific run
                routes::frontend::show_run_get,
                // Display the list of runs
                routes::frontend::list_runs,
                // Advanced run search
                routes::frontend::search_run,
                // Edit a run
                routes::frontend::edit_run_get,
                // Redirect to the documentation
                doc_redirect
            ],
        )
        .mount(
            "/smaug/admin",
            // Admin-only routes
            routes![
                routes::frontend::register_get,
                routes::frontend::admin_dashboard_get,
                routes::frontend::edit_users,
                routes::frontend::edit_groups_for_user,
                routes::frontend::new_group,
                routes::frontend::forms_settings_get,
                routes::frontend::runtime_config_page_get,
            ],
        )
        .mount(
            "/smaug/api",
            routes![
                // Backend routes: always return JSON
                routes::backend::register_post,
                // Route updating the form cache from a webhook
                routes::backend::gitea_webhook_update_form_cache,
                // Route updating the form cache from a simple authenticated GET
                routes::backend::update_cache_get,
                routes::backend::login_post,
                routes::backend::newgroup_get,
                routes::backend::list_groups,
                routes::backend::get_all_forms_for_group,
                // Simple GET to list users
                routes::backend::list_users,
                // Simple GET to list the groups of a user
                routes::backend::list_groups_for_user,
                routes::backend::update_groups,
                routes::backend::get_all_forms,
                routes::backend::password_edit_post,
                routes::backend::newrun_post,
                routes::backend::editrun_post,
                routes::backend::validate_run_post,
                routes::backend::upload_post,
                routes::backend::list_runs_get,
                routes::backend::check_samplesheet,
                routes::backend::list_samples_from_samplesheet,
                // Route returning a run's progress information
                routes::backend::watch,
                // Route querying squeue directly
                routes::backend::get_squeue,
                // Route to search runs
                routes::backend::search_run_get,
                // Route to translate sequencer names
                routes::backend::prettify_seqname,
                // Route for all redirects
                routes::frontend::redirect_get,
                // Route to relaunch a run (once it is finished)
                routes::backend::retry_run_get,
                // Runtime configuration routes
                routes::backend::runtime_config_get,
                routes::backend::runtime_config_set,
            ],
        )
        .manage(pool)
        .manage(config)
        .manage(progress_cache)
        .manage(SqueueCache::new(30))
        .attach(minijinja_fairing());

    #[cfg(debug_assertions)]
    {
        use std::path::Path;
        let static_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("static")
            .canonicalize()
            .expect("Impossible de trouver le dossier static");
        r = r.mount("/smaug/static", FileServer::from(static_path));
    }
    #[cfg(not(debug_assertions))]
    {
        use rocket_include_dir::{Dir, StaticFiles, include_dir};
        static PROJECT_DIR: Dir = include_dir!("static");
        r = r.mount("/smaug/static", StaticFiles::from(&PROJECT_DIR));
    }
    static DOC_DIR: Dir = include_dir!("$CARGO_MANIFEST_DIR/documentation/book");
    r = r.mount("/smaug/documentation", StaticFiles::from(&DOC_DIR));
    r
}

#[derive(Parser)]
pub struct Web {
    /// The IP address to listen on
    #[arg(short = 'H', long = "host")]
    ip: Option<Ipv4Addr>,

    #[arg(short = 'p', long = "port")]
    port: Option<u16>,
}

impl Web {
    pub async fn run(self, pool: SqlitePool) -> Result<()> {
        let progress_cache = RunProgressCache::new();
        let rocket_app = match rocket(pool.clone(), progress_cache.clone(), self.ip, self.port)
            .await
            .ignite()
            .await
        {
            Ok(app) => app,
            Err(e) => {
                bail!("{}: {:?}", e, e.kind())
            }
        };
        let shutdown_handle = rocket_app.shutdown();

        // Channel used to tell the routine that it must stop
        let (routine_shutdown_tx, routine_shutdown_rx) = broadcast::channel(1);

        let mut sigterm =
            signal(SignalKind::terminate()).expect("Impossible d'écouter le signal SIGTERM");
        let mut sigint =
            signal(SignalKind::interrupt()).expect("Impossible d'écouter le signal SIGINT");
        let mut sigquit =
            signal(SignalKind::quit()).expect("Impossible d'écouter le signal SIGQUIT");
        // Spawn a dedicated task listening for OS signals
        tokio::spawn(async move {
            tokio::select! {
                _ = sigterm.recv() => info!("SIGTERM reçu."),
                _ = sigint.recv() => info!("SIGINT (Ctrl+C) reçu."),
                _ = sigquit.recv() => info!("SIGQUIT reçu."),
            }
            // Notify the Rocket runtime that it must shut down
            shutdown_handle.notify();

            // Send a shutdown signal to the routine
            let _ = routine_shutdown_tx.send(());
        });

        // Start the routine (job execution)
        let routine = run_routine_loop(&pool, progress_cache, routine_shutdown_rx);

        // Start the web application
        let webapp = rocket_app.launch();

        // Wait for both the web application and the routine to finish
        let (web_res, routine_res) = tokio::join!(webapp, routine);

        if let Err(e) = web_res {
            error!("Erreur de fermeture Rocket: {}", e);
        }
        if let Err(e) = routine_res {
            error!("Erreur de fermeture Routine: {}", e);
        }

        Ok(())
    }
}
