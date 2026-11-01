use crate::templates::{Template, context};
use rocket::get;

#[get("/redirect?<message>&<title>&<target_url>&<seconds>")]
pub async fn redirect_get(
    message: String,
    title: String,
    target_url: String,
    seconds: Option<i64>,
) -> Template {
    let seconds = seconds.unwrap_or(5);
    Template::render(
        "common/redirect",
        context! {
            title=> title,
            message=> message,
            target_url=> target_url,
            seconds=> seconds,
        },
    )
}
