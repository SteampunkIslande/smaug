use std::str::FromStr;

use rocket::serde::Serialize;
use serde::ser::SerializeSeq;

/// Core trait for all configuration items (both Enums and Dates)
pub trait ConfigItem:
    Sized + FromStr<Err = anyhow::Error> + std::fmt::Display + Send + Sync + 'static
{
    const DB_KEY: &'static str;
    const DESCRIPTION: &'static str;
    const DEFAULT_STR: &'static str;

    fn default_value() -> Self;
    fn value_input() -> ValueInput;
}

/// A selectable value and its human-readable label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComboOption {
    pub value: &'static str,
    pub label: &'static str,
}

/// Input type for rendering HTML form elements in the Admin UI.
/// Only implements Serialize (for API responses), not Deserialize.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValueInput {
    /// Renders an HTML `<select>` with predefined options
    Combo(&'static [ComboOption]),
    /// Renders an HTML `<input type="date">`
    Date,
}

// Manual Serialize implementation because `&'static [ComboOption]` doesn't implement Deserialize
impl Serialize for ValueInput {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            ValueInput::Combo(options) => {
                let mut seq = serializer.serialize_seq(Some(options.len()))?;
                for option in *options {
                    seq.serialize_element(option.value)?;
                }
                seq.end()
            }
            ValueInput::Date => serializer.serialize_str("date"),
        }
    }
}

impl ValueInput {
    /// Renders the appropriate HTML input for this config value.
    pub fn render_html(&self, name: &str, current_value: &str) -> String {
        let name = html_escape::encode_double_quoted_attribute(name);
        match self {
            ValueInput::Combo(options) => {
                let mut html = format!(r#"<select class="config-select" name="{name}">"#);
                for option in *options {
                    let selected = if option.value.eq_ignore_ascii_case(current_value) {
                        " selected"
                    } else {
                        ""
                    };
                    let value = html_escape::encode_double_quoted_attribute(option.value);
                    let label = html_escape::encode_text(option.label);
                    html.push_str(&format!(
                        r#"<option value="{value}"{selected}>{label}</option>"#
                    ));
                }
                html.push_str("</select>");
                html
            }
            ValueInput::Date => {
                let current_value = html_escape::encode_double_quoted_attribute(current_value);
                format!(
                    r#"<input type="date" class="config-input" name="{name}" value="{current_value}">"#
                )
            }
        }
    }
}
