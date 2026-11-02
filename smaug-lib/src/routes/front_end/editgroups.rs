use crate::routes::frontend::FrontendError;
use crate::templates::{Template, context};
use crate::{auth::Authenticated, models::User};
use anyhow::Context;
use rocket::{State, get};
use sqlx::SqlitePool;

/// Route to show the admin a page to register new user.
#[get("/groupedit/<user_id>")]
pub async fn edit_groups_for_user(
    auth: Authenticated,
    pool: &State<SqlitePool>,
    user_id: i64,
) -> Result<Template, FrontendError> {
    if !auth.user.is_admin {
        return Err(FrontendError::AdminsOnly);
    }
    let user = User::find_by_id(user_id, pool)
        .await
        .context("Utilisateur introuvable")?;
    Ok(Template::render(
        "admin/groups_list",
        context! {edited_user => user, user => auth.user},
    ))
}
