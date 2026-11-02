use rocket::get;

use crate::routes::frontend::FrontendError;
use crate::templates::{Template, context};

use crate::auth::Authenticated;

#[get("/groups/new")]
pub async fn new_group(auth: Authenticated) -> Result<Template, FrontendError> {
    if !auth.user.is_admin {
        Err(FrontendError::AdminsOnly)
    } else {
        Ok(Template::render(
            "admin/new_group",
            context! {user => auth.user},
        ))
    }
}
