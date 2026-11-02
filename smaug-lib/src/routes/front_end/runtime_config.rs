use rocket::{State, get};
use serde::Serialize;
use sqlx::SqlitePool;

use crate::{
    auth::Authenticated,
    models::{AppState, AppStateConfigKey, appstate::ValueInput},
    routes::frontend::FrontendError,
    templates::{Template, context},
};

#[derive(Serialize)]
struct ConfigEntryForTemplate {
    key: AppStateConfigKey,
    value: String,
    value_input: ValueInput,
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
        entries.push(ConfigEntryForTemplate {
            key: *key,
            value,
            value_input: key.value_input(),
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
