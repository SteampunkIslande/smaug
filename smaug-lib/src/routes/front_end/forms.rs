use crate::templates::{Template, context};
use rocket::get;

use crate::auth::Authenticated;

#[get("/forms")]
pub async fn forms_settings_get(auth: Authenticated) -> Template {
    if !auth.user.is_admin {
        Template::render("errors/admin_only", context! {user => auth.user})
    } else {
        Template::render("admin/forms", context! {user => auth.user})
    }
}
