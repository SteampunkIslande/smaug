use rocket::{State, get};
use serde::Serialize;
use sqlx::SqlitePool;

use crate::{
    auth::Authenticated,
    models::{AppState, AppStateConfigKey},
    routes::frontend::FrontendError,
    templates::{Template, context},
};

#[derive(Serialize)]
struct ConfigEntryForTemplate {
    key: AppStateConfigKey,
    input_html: String,
    description: &'static str,
}

#[get("/runtime_config")]
pub async fn runtime_config_page_get(
    pool: &State<SqlitePool>,
    auth: Authenticated,
) -> Result<Template, FrontendError> {
    if !auth.user.is_admin {
        return Err(FrontendError::AdminsOnly);
    }

    let mut entries = Vec::with_capacity(AppStateConfigKey::all().len());
    for key in AppStateConfigKey::all() {
        let value = AppState::get_str(*key, pool).await?;
        let input_html = key.value_input().render_html(key.as_str(), &value);
        entries.push(ConfigEntryForTemplate {
            key: *key,
            input_html,
            description: key.description(),
        });
    }

    Ok(Template::render(
        "admin/runtime_config",
        context! {
            entries => entries,
            user => auth.user,
        },
    ))
}
