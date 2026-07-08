//! Record-type command handlers (`xv type ...`). Config-only: never talks
//! to a secrets backend.

use crate::cli::commands::TypeCommands;
use crate::config::Config;
use crate::error::{CrosstacheError, Result};
use crate::records::RecordType;
use crate::utils::format::OutputFormat;

pub(crate) async fn execute_type_command(command: TypeCommands, config: Config) -> Result<()> {
    let format = config.runtime_output_format;
    match command {
        TypeCommands::List => execute_type_list(config, format).await,
        TypeCommands::Show { name } => execute_type_show(config, name, format).await,
    }
}

/// Renders one field's compact table cell: `username*` for required,
/// `[password]` for secret, `[password]•` for primary (secret + required).
fn render_field(field: &crate::records::FieldDef) -> String {
    let mut s = String::new();
    if field.kind == crate::records::FieldKind::Secret {
        s.push('[');
        s.push_str(&field.name);
        s.push(']');
    } else {
        s.push_str(&field.name);
    }
    if field.required {
        s.push('*');
    }
    if field.primary {
        s.push('•');
    }
    s
}

fn render_fields(record_type: &RecordType) -> String {
    record_type
        .fields
        .iter()
        .map(render_field)
        .collect::<Vec<_>>()
        .join(", ")
}

#[derive(Debug, Clone, serde::Serialize, tabled::Tabled)]
struct TypeListRow {
    #[tabled(rename = "NAME")]
    name: String,
    #[tabled(rename = "SOURCE")]
    source: String,
    #[tabled(rename = "FIELDS")]
    fields: String,
}

pub(crate) async fn execute_type_list(config: Config, format: OutputFormat) -> Result<()> {
    use crate::utils::format::TableFormatter;

    let mut types = config.resolve_record_types().await?;
    types.sort_by(|a, b| a.name.cmp(&b.name));

    let resolved = format.resolve_for_stdout();
    if resolved == OutputFormat::Json {
        println!(
            "{}",
            serde_json::to_string_pretty(&types).map_err(|e| CrosstacheError::serialization(
                format!("JSON serialization failed: {e}")
            ))?
        );
        return Ok(());
    }
    if resolved == OutputFormat::Yaml {
        println!(
            "{}",
            serde_yaml::to_string(&types).map_err(|e| CrosstacheError::serialization(format!(
                "YAML serialization failed: {e}"
            )))?
        );
        return Ok(());
    }

    let rows: Vec<TypeListRow> = types
        .iter()
        .map(|t| TypeListRow {
            name: t.name.clone(),
            source: t.source.to_string(),
            fields: render_fields(t),
        })
        .collect();

    let formatter = TableFormatter::new(
        format,
        config.no_color,
        config.template.clone(),
        config.runtime_columns.clone(),
    );
    println!("{}", formatter.format_table(&rows)?);
    Ok(())
}

pub(crate) async fn execute_type_show(
    config: Config,
    name: String,
    format: OutputFormat,
) -> Result<()> {
    let types = config.resolve_record_types().await?;
    let Some(record_type) = crate::records::find_type(&types, &name) else {
        let mut known: Vec<&str> = types.iter().map(|t| t.name.as_str()).collect();
        known.sort_unstable();
        return Err(CrosstacheError::config(format!(
            "unknown type '{name}'. Known types: {}",
            known.join(", ")
        )));
    };

    let resolved = format.resolve_for_stdout();
    if resolved == OutputFormat::Json {
        println!(
            "{}",
            serde_json::to_string_pretty(record_type).map_err(|e| {
                CrosstacheError::serialization(format!("JSON serialization failed: {e}"))
            })?
        );
        return Ok(());
    }
    if resolved == OutputFormat::Yaml {
        println!(
            "{}",
            serde_yaml::to_string(record_type).map_err(|e| CrosstacheError::serialization(
                format!("YAML serialization failed: {e}")
            ))?
        );
        return Ok(());
    }

    println!("{}  ({})", record_type.name, record_type.source);
    println!(
        "{:<20} {:<10} {:<10} {:<10}",
        "FIELD", "KIND", "REQUIRED", "PRIMARY"
    );
    for field in &record_type.fields {
        let kind = match field.kind {
            crate::records::FieldKind::Metadata => "metadata",
            crate::records::FieldKind::Secret => "secret",
        };
        println!(
            "{:<20} {:<10} {:<10} {:<10}",
            field.name,
            kind,
            if field.required { "yes" } else { "no" },
            if field.primary { "yes" } else { "no" },
        );
    }
    Ok(())
}
