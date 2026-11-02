use crate::auth::Authenticated;
use crate::models::User;
use crate::routes::frontend::FrontendError;
use crate::templates::{Template, context};
use rocket::{State, get};
use sqlx::SqlitePool;

#[get("/dashboard")]
pub async fn admin_dashboard_get(auth: Authenticated) -> Result<Template, FrontendError> {
    if !auth.user.is_admin {
        return Err(FrontendError::AdminsOnly);
    }
    Ok(Template::render(
        "admin/dashboard",
        context! {user=>auth.user},
    ))
}

/// The admin route to change a user's password
#[get("/passedit/<user_id>")]
pub async fn password_edit_get(
    auth: Authenticated,
    pool: &State<SqlitePool>,
    user_id: i64,
) -> Result<Template, FrontendError> {
    if user_id != auth.user.id && !auth.user.is_admin {
        return Err(FrontendError::AdminsOnly);
    }
    let edited_user = User::find_by_id(user_id, pool).await?;
    Ok(Template::render(
        "admin/passedit",
        context! { edited_user=>edited_user, user=>auth.user},
    ))
}

/// Route to show the admin a page to register new user.
#[get("/register")]
pub async fn register_get(auth: Authenticated) -> Result<Template, FrontendError> {
    if !auth.user.is_admin {
        return Err(FrontendError::AdminsOnly);
    }
    Ok(Template::render(
        "admin/register",
        context! {user => auth.user},
    ))
}
