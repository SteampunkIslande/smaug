use crate::auth::Authenticated;
use crate::templates::{Template, context};
use rocket::get;

/// Route to show the admin a page to register new user.
#[get("/editusers")]
pub async fn edit_users(auth: Authenticated) -> Template {
    if !auth.user.is_admin {
        return Template::render(
            "errors/admin_only",
            context! {user_name=>auth.user.username},
        );
    }
    Template::render("admin/users_list", context! {user => auth.user})
}
