use diacritics::remove_diacritics;

use time::format_description::StaticFormatDescription;
use time::macros::format_description;
use time::{Month, OffsetDateTime, Weekday};

pub static NICE_DATETIME_FORMAT: StaticFormatDescription =
    format_description!("[weekday repr:short] [day padding:none] [month repr:short] [year]");

/// Utility function to clean a string
fn clean_string(input: &str) -> String {
    // Remove all whitespace characters (spaces, tabs, newlines, etc.)
    let no_whitespace: String = input.chars().filter(|c| !c.is_whitespace()).collect();
    remove_diacritics(&no_whitespace)
}

/// Errors of a samplesheet that the caller can act upon: the input is simply not a
/// valid Illumina samplesheet. Everything else is a bug and would be a panic.
#[derive(Debug, wherror::Error)]
pub enum SamplesheetError {
    #[error("{0}")]
    InvalidSamplesheet(String),
}

pub fn correct_samplesheet(input: &str) -> Result<(Vec<String>, String), SamplesheetError> {
    // Split into lines while preserving the order
    let lines: Vec<&str> = input.lines().collect();
    let mut cleaned_lines: Vec<String> = Vec::new();
    let mut in_data = false;
    let mut column_names: Vec<String> = Vec::new();
    let mut sample_col_index: Option<usize> = None;
    let mut samples: Vec<String> = Vec::new();

    let mut i: usize = 0;
    while i < lines.len() {
        let line = lines[i];

        // Keep empty lines as they are
        if line.is_empty() {
            cleaned_lines.push(line.to_string());
            i += 1;
            continue;
        }

        // Before the [Data] section, keep them as they are
        if !in_data && line != "[Data]" {
            cleaned_lines.push(line.to_string());
            i += 1;
            continue;
        }

        // Start of the [Data] section
        if line == "[Data]" {
            in_data = true;
            cleaned_lines.push(line.to_string());

            // Find the first non-empty line that follows as the header
            i += 1;
            let mut header_line_opt: Option<&str> = None;
            while i < lines.len() {
                if lines[i].is_empty() {
                    cleaned_lines.push(lines[i].to_string());
                    i += 1;
                    continue;
                }
                header_line_opt = Some(lines[i]);
                break;
            }

            let header_line = match header_line_opt {
                Some(h) => h,
                None => {
                    return Err(SamplesheetError::InvalidSamplesheet(
                        "Aucune ligne d'en-tête trouvée après [Data]".to_string(),
                    ));
                }
            };

            column_names = header_line.split(',').map(|s| s.to_string()).collect();

            // Cleaned header
            let clean_column_names: Vec<String> =
                column_names.iter().map(|c| clean_string(c)).collect();
            cleaned_lines.push(clean_column_names.join(","));

            // Determine the index of the "sample" column

            sample_col_index = column_names.iter().position(|col| col == "Sample_ID");

            if sample_col_index.is_none() {
                return Err(SamplesheetError::InvalidSamplesheet(
                    "Impossible d'extraire la liste des échantillons: aucune colonne 'sample' trouvée"
                        .to_string(),
                ));
            }

            // Move to the line after the header to read the data
            i += 1;
            continue;
        }

        // In the Data section: process lines until the end or a new section
        if in_data && !column_names.is_empty() {
            if line.starts_with('[') {
                // new section encountered: leave Data mode and keep the line
                in_data = false;
                cleaned_lines.push(line.to_string());
                i += 1;
                continue;
            }

            let values: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
            if values.len() != column_names.len() {
                return Err(SamplesheetError::InvalidSamplesheet(format!(
                    "Le nombre de colonnes diffère à la ligne {}",
                    i + 1
                )));
            }

            // Build a cleaned line respecting the number of columns (pad if necessary)
            let mut cleaned_values: Vec<String> = Vec::new();
            for col_idx in 0..column_names.len() {
                let v = values.get(col_idx).copied().unwrap_or("");
                cleaned_values.push(clean_string(v));
            }
            cleaned_lines.push(cleaned_values.join(","));

            // Extract the sample if present
            if let Some(sample_idx) = sample_col_index
                && let Some(val) = values.get(sample_idx)
            {
                let cleaned_sample = clean_string(val);
                if !cleaned_sample.is_empty() {
                    samples.push(cleaned_sample);
                }
            }

            i += 1;
            continue;
        }

        // General case: move on
        cleaned_lines.push(line.to_string());
        i += 1;
    }

    if samples.is_empty() {
        return Err(SamplesheetError::InvalidSamplesheet(
            "Impossible d'extraire la liste des échantillons: aucune valeur trouvée".to_string(),
        ));
    }

    let cleaned = cleaned_lines.join("\n");
    Ok((samples, cleaned))
}

pub fn format_french_date(date: &OffsetDateTime, with_time: bool) -> String {
    // Extract just the date part if it is a datetime

    let weekday = match date.weekday() {
        Weekday::Monday => "lun.",
        Weekday::Tuesday => "mar.",
        Weekday::Wednesday => "mer.",
        Weekday::Thursday => "jeu.",
        Weekday::Friday => "ven.",
        Weekday::Saturday => "sam.",
        Weekday::Sunday => "dim.",
    };
    let month_name = match date.month() {
        Month::January => "janv.",
        Month::February => "fév.",
        Month::March => "mars",
        Month::April => "avr.",
        Month::May => "mai",
        Month::June => "juin",
        Month::July => "juill.",
        Month::August => "août",
        Month::September => "sept.",
        Month::October => "oct.",
        Month::November => "nov.",
        Month::December => "déc.",
    };

    if with_time {
        format!(
            "{} {:0>2} {} {} ({:0>2}h{:0>2})",
            weekday,
            date.day(),
            month_name,
            date.year(),
            date.hour(),
            date.minute()
        )
    } else {
        format!("{} {} {} {}", weekday, date.day(), month_name, date.year())
    }
}

/// Converts the name of a local file into a name served by the static route
/// # Arguments
/// * `filename` - The full path of the local file (on the server)
/// * `base_path` - The base path to strip (the name of the upload directory on the server, served statically)
/// * `prefix` - The prefix (as an absolute path) used in the static route (e.g. "/uploads")
pub fn filename_to_static_served_name(
    filename: &str,
    base_path: &str,
    prefix: &str,
) -> Option<String> {
    let path = std::path::Path::new(filename).canonicalize().ok()?;
    path.strip_prefix(base_path)
        .ok()
        .map(|rel_path| format!("{}/{}", prefix, rel_path.to_string_lossy()))
}
