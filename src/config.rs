//! Persistent Windows user settings, shared by interactive and scripted commands.

use crate::{cli::{CompressionFormat, ConfigCommand, ConfigKey}, interactive, Result};
use inquire::{Select, Text};
use serde_json::{json, Map, Value};
use std::{env, fs, path::{Path, PathBuf}};

fn file_path() -> Result<PathBuf> {
    let local = env::var_os("LOCALAPPDATA")
        .ok_or("LOCALAPPDATA is unavailable; cannot locate the wslim configuration")?;
    Ok(PathBuf::from(local).join("wslim").join("config.json"))
}

fn read_document(path: &Path) -> Result<Value> {
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(json!({})),
        Err(error) => return Err(error.into()),
    };
    let document: Value = serde_json::from_str(&content)?;
    if !document.is_object() { return Err("wslim configuration must be a JSON object".into()); }
    Ok(document)
}

fn value_in(document: &Value, key: ConfigKey) -> Result<Option<String>> {
    let path = match key {
        ConfigKey::BackupOutput => "/backup/output",
        ConfigKey::BackupFormat => "/backup/format",
        ConfigKey::BackupLevel => "/backup/level",
    };
    let value = document.pointer(path);
    let text = match value {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::String(text)) if !text.trim().is_empty() => text.clone(),
        Some(Value::Number(number)) if key == ConfigKey::BackupLevel => number.to_string(),
        _ => return Err(format!("Invalid {} in config.json", key.as_str()).into()),
    };
    validate(key, &text)?;
    Ok(Some(text))
}

fn validate(key: ConfigKey, value: &str) -> Result<()> {
    match key {
        ConfigKey::BackupOutput => {
            if !Path::new(value).is_absolute() {
                return Err("backup.output must be an absolute Windows directory path".into());
            }
        }
        ConfigKey::BackupFormat => {
            if !matches!(value, "xz" | "gz") { return Err("backup.format must be xz or gz".into()); }
        }
        ConfigKey::BackupLevel => {
            if !matches!(value, "1" | "3" | "6" | "9") {
                return Err("backup.level must be one of: 1, 3, 6, 9".into());
            }
        }
    }
    Ok(())
}

fn get(key: ConfigKey) -> Result<Option<String>> {
    value_in(&read_document(&file_path()?)?, key)
}

pub(crate) fn backup_output() -> Result<Option<PathBuf>> {
    Ok(get(ConfigKey::BackupOutput)?.map(PathBuf::from))
}

pub(crate) fn backup_format() -> Result<CompressionFormat> {
    match get(ConfigKey::BackupFormat)?.as_deref() {
        Some("gz") => Ok(CompressionFormat::Gz),
        _ => Ok(CompressionFormat::Xz),
    }
}

pub(crate) fn backup_level() -> Result<u8> {
    match get(ConfigKey::BackupLevel)? {
        Some(value) => Ok(value.parse()?),
        None => Ok(6),
    }
}

fn save(key: ConfigKey, value: Option<&str>) -> Result<()> {
    if let Some(value) = value { validate(key, value)?; }
    let file = file_path()?;
    let mut document = read_document(&file)?;
    let backup = document.as_object_mut().expect("checked JSON object")
        .entry("backup").or_insert_with(|| Value::Object(Map::new()));
    let fields = backup.as_object_mut().ok_or("backup must be a JSON object in config.json")?;
    let field = match key {
        ConfigKey::BackupOutput => "output",
        ConfigKey::BackupFormat => "format",
        ConfigKey::BackupLevel => "level",
    };
    if let Some(value) = value {
        let json_value = if key == ConfigKey::BackupLevel {
            json!(value.parse::<u8>()?)
        } else {
            json!(value)
        };
        fields.insert(field.to_owned(), json_value);
    } else {
        fields.remove(field);
    }
    if let Some(parent) = file.parent() { fs::create_dir_all(parent)?; }
    fs::write(file, serde_json::to_vec_pretty(&document)?)?;
    Ok(())
}

fn current() -> Result<Value> {
    Ok(json!({
        "backup.output": get(ConfigKey::BackupOutput)?,
        "backup.format": backup_format()?.as_str(),
        "backup.level": backup_level()?,
    }))
}

fn show_settings() -> Result<()> {
    let settings = current()?;
    println!("backup.output = {}", settings["backup.output"].as_str().unwrap_or("(not set)"));
    println!("backup.format = {}", settings["backup.format"].as_str().unwrap_or("xz"));
    println!("backup.level = {}", settings["backup.level"]);
    Ok(())
}

pub(crate) fn execute(action: Option<ConfigCommand>, non_interactive: bool, json_output: bool) -> Result<()> {
    match action {
        Some(ConfigCommand::List) => {
            if json_output { println!("{}", current()?); }
            else { show_settings()?; }
        }
        Some(ConfigCommand::Get { key }) => {
            // Keep JSON types consistent with `config list` for GUI consumers.
            let value = match key {
                ConfigKey::BackupOutput => json!(get(key)?),
                ConfigKey::BackupFormat => json!(backup_format()?.as_str()),
                ConfigKey::BackupLevel => json!(backup_level()?),
            };
            if json_output {
                let mut fields = Map::new();
                fields.insert(key.as_str().to_owned(), value);
                println!("{}", Value::Object(fields));
            } else {
                println!("{}", match value {
                    Value::String(text) => text,
                    Value::Null => "(not set)".to_owned(),
                    other => other.to_string(),
                });
            }
        }
        Some(ConfigCommand::Set { key, value }) => set_value(key, &value, json_output)?,
        Some(ConfigCommand::Output { value }) => {
            set_value(ConfigKey::BackupOutput, &value.to_string_lossy(), json_output)?;
        }
        Some(ConfigCommand::Unset { key }) => {
            save(key, None)?;
            if json_output { println!("{}", current()?); }
            else { println!("{} cleared", key.as_str()); }
        }
        None => {
            if non_interactive || json_output {
                return Err("Use 'wslim config list|get|set|unset' in non-interactive mode".into());
            }
            interactive::ensure_terminal()?;
            loop {
                show_settings()?;
                let action = Select::new("Configure wslim:", vec![
                    "Set backup.output", "Set backup.format", "Set backup.level",
                    "Clear backup.output", "Reset backup.format", "Reset backup.level", "Exit",
                ]).prompt()?;
                match action {
                    "Set backup.output" => {
                        let previous = get(ConfigKey::BackupOutput)?.unwrap_or_default();
                        let label = format!("Windows backup directory (current: {}):", if previous.is_empty() { "not set" } else { &previous });
                        let mut prompt = Text::new(&label);
                        if !previous.is_empty() { prompt = prompt.with_default(&previous); }
                        let selected = prompt.prompt()?;
                        save(ConfigKey::BackupOutput, Some(selected.trim()))?;
                    }
                    "Set backup.format" => {
                        let selected = Select::new("Default compression format:", vec!["xz", "gz"])
                            .with_starting_cursor(usize::from(backup_format()? == CompressionFormat::Gz))
                            .prompt()?;
                        save(ConfigKey::BackupFormat, Some(selected))?;
                    }
                    "Set backup.level" => {
                        let values = vec!["1", "3", "6", "9"];
                        let selected = Select::new("Default compression level:", values)
                            .with_starting_cursor(match backup_level()? { 1 => 0, 3 => 1, 9 => 3, _ => 2 })
                            .prompt()?;
                        save(ConfigKey::BackupLevel, Some(selected))?;
                    }
                    "Clear backup.output" => save(ConfigKey::BackupOutput, None)?,
                    "Reset backup.format" => save(ConfigKey::BackupFormat, None)?,
                    "Reset backup.level" => save(ConfigKey::BackupLevel, None)?,
                    _ => break,
                }
            }
        }
    }
    Ok(())
}

fn set_value(key: ConfigKey, value: &str, json_output: bool) -> Result<()> {
    let previous = get(key)?.unwrap_or_else(|| "not set".to_owned());
    save(key, Some(value))?;
    if json_output { println!("{}", current()?); }
    else { println!("{} set to {} (previous: {previous})", key.as_str(), value); }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_documented_defaults() {
        for value in ["1", "3", "6", "9"] { validate(ConfigKey::BackupLevel, value).unwrap(); }
        assert!(validate(ConfigKey::BackupLevel, "4").is_err());
        assert!(validate(ConfigKey::BackupFormat, "zip").is_err());
    }

    #[test]
    fn reads_saved_level_and_format() {
        let value = json!({"backup":{"format":"gz","level":3}});
        assert_eq!(value_in(&value, ConfigKey::BackupFormat).unwrap(), Some("gz".into()));
        assert_eq!(value_in(&value, ConfigKey::BackupLevel).unwrap(), Some("3".into()));
    }
}
