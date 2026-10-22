//! Persistent Windows user settings, shared by interactive and scripted commands.

use crate::{
    cli::{CompressionFormat, ConfigCommand, ConfigKey},
    interactive, Result,
};
use inquire::{Select, Text};
use serde_json::{json, Map, Value};
use std::{
    env, fs,
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

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
    if !document.is_object() {
        return Err("wslim configuration must be a JSON object".into());
    }
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

fn raw_value_in(document: &Value, key: ConfigKey) -> Option<String> {
    let path = match key {
        ConfigKey::BackupOutput => "/backup/output",
        ConfigKey::BackupFormat => "/backup/format",
        ConfigKey::BackupLevel => "/backup/level",
    };
    match document.pointer(path) {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) => Some(text.clone()),
        Some(value) => Some(value.to_string()),
    }
}

fn validate(key: ConfigKey, value: &str) -> Result<()> {
    match key {
        ConfigKey::BackupOutput => {
            if !Path::new(value).is_absolute() {
                return Err("backup.output must be an absolute Windows directory path".into());
            }
        }
        ConfigKey::BackupFormat => {
            if !matches!(value, "xz" | "gz") {
                return Err("backup.format must be xz or gz".into());
            }
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

fn set_in(document: &mut Value, key: ConfigKey, value: Option<&str>) -> Result<()> {
    if let Some(value) = value {
        validate(key, value)?;
    }
    let backup = document
        .as_object_mut()
        .expect("checked JSON object")
        .entry("backup")
        .or_insert_with(|| Value::Object(Map::new()));
    let fields = backup
        .as_object_mut()
        .ok_or("backup must be a JSON object in config.json")?;
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
    Ok(())
}

fn current_in(document: &Value) -> Result<Value> {
    Ok(json!({
        "backup.output": value_in(document, ConfigKey::BackupOutput)?,
        "backup.format": value_in(document, ConfigKey::BackupFormat)?.as_deref().unwrap_or("xz"),
        "backup.level": value_in(document, ConfigKey::BackupLevel)?
            .map(|value| value.parse::<u8>())
            .transpose()?
            .unwrap_or(6),
    }))
}

fn current() -> Result<Value> {
    current_in(&read_document(&file_path()?)?)
}

fn write_document_atomic(path: &Path, document: &Value) -> Result<()> {
    let contents = serde_json::to_vec_pretty(document)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let file_name = path
        .file_name()
        .ok_or("configuration path has no file name")?;
    let (temporary_path, mut temporary_file) = loop {
        let sequence = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let temporary_name = format!(
            "{}.{}.{}.tmp",
            file_name.to_string_lossy(),
            std::process::id(),
            sequence,
        );
        let temporary_path = path.with_file_name(temporary_name);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary_path)
        {
            Ok(file) => break (temporary_path, file),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    };

    let write_result = temporary_file
        .write_all(&contents)
        .and_then(|()| temporary_file.sync_all());
    drop(temporary_file);
    if let Err(error) = write_result {
        let _ = fs::remove_file(&temporary_path);
        return Err(error.into());
    }
    if let Err(error) = fs::rename(&temporary_path, path) {
        let _ = fs::remove_file(&temporary_path);
        return Err(error.into());
    }
    Ok(())
}

fn update_file(
    path: &Path,
    key: ConfigKey,
    value: Option<&str>,
    include_snapshot: bool,
) -> Result<(Option<String>, Option<Value>)> {
    let mut document = read_document(path)?;
    let previous = raw_value_in(&document, key);
    set_in(&mut document, key, value)?;
    let snapshot = if include_snapshot {
        Some(current_in(&document)?)
    } else {
        None
    };
    write_document_atomic(path, &document)?;
    Ok((previous, snapshot))
}

fn save(key: ConfigKey, value: Option<&str>) -> Result<()> {
    update_file(&file_path()?, key, value, false)?;
    Ok(())
}

fn show_settings() -> Result<()> {
    let settings = current()?;
    println!(
        "backup.output = {}",
        settings["backup.output"].as_str().unwrap_or("(not set)")
    );
    println!(
        "backup.format = {}",
        settings["backup.format"].as_str().unwrap_or("xz")
    );
    println!("backup.level = {}", settings["backup.level"]);
    Ok(())
}

pub(crate) fn execute(
    action: Option<ConfigCommand>,
    non_interactive: bool,
    json_output: bool,
) -> Result<()> {
    match action {
        Some(ConfigCommand::List) => {
            if json_output {
                println!("{}", current()?);
            } else {
                show_settings()?;
            }
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
                println!(
                    "{}",
                    match value {
                        Value::String(text) => text,
                        Value::Null => "(not set)".to_owned(),
                        other => other.to_string(),
                    }
                );
            }
        }
        Some(ConfigCommand::Set { key, value }) => set_value(key, &value, json_output)?,
        Some(ConfigCommand::Output { value }) => {
            set_value(
                ConfigKey::BackupOutput,
                &value.to_string_lossy(),
                json_output,
            )?;
        }
        Some(ConfigCommand::Unset { key }) => {
            let (_, snapshot) = update_file(&file_path()?, key, None, json_output)?;
            if let Some(snapshot) = snapshot {
                println!("{snapshot}");
            } else {
                println!("{} cleared", key.as_str());
            }
        }
        None => {
            if non_interactive || json_output {
                return Err("Use 'wslim config list|get|set|unset' in non-interactive mode".into());
            }
            interactive::ensure_terminal()?;
            loop {
                show_settings()?;
                let action = Select::new(
                    "Configure wslim:",
                    vec![
                        "Set backup.output",
                        "Set backup.format",
                        "Set backup.level",
                        "Clear backup.output",
                        "Reset backup.format",
                        "Reset backup.level",
                        "Exit",
                    ],
                )
                .prompt()?;
                match action {
                    "Set backup.output" => {
                        let previous = get(ConfigKey::BackupOutput)?.unwrap_or_default();
                        let label = format!(
                            "Windows backup directory (current: {}):",
                            if previous.is_empty() {
                                "not set"
                            } else {
                                &previous
                            }
                        );
                        let mut prompt = Text::new(&label);
                        if !previous.is_empty() {
                            prompt = prompt.with_default(&previous);
                        }
                        let selected = prompt.prompt()?;
                        save(ConfigKey::BackupOutput, Some(selected.trim()))?;
                    }
                    "Set backup.format" => {
                        let selected = Select::new("Default compression format:", vec!["xz", "gz"])
                            .with_starting_cursor(usize::from(
                                backup_format()? == CompressionFormat::Gz,
                            ))
                            .prompt()?;
                        save(ConfigKey::BackupFormat, Some(selected))?;
                    }
                    "Set backup.level" => {
                        let values = vec!["1", "3", "6", "9"];
                        let selected = Select::new("Default compression level:", values)
                            .with_starting_cursor(match backup_level()? {
                                1 => 0,
                                3 => 1,
                                9 => 3,
                                _ => 2,
                            })
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
    let (previous, snapshot) = update_file(&file_path()?, key, Some(value), json_output)?;
    if let Some(snapshot) = snapshot {
        println!("{snapshot}");
    } else {
        let previous = previous.unwrap_or_else(|| "not set".to_owned());
        println!("{} set to {} (previous: {previous})", key.as_str(), value);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_documented_defaults() {
        for value in ["1", "3", "6", "9"] {
            validate(ConfigKey::BackupLevel, value).unwrap();
        }
        assert!(validate(ConfigKey::BackupLevel, "4").is_err());
        assert!(validate(ConfigKey::BackupFormat, "zip").is_err());
    }

    #[test]
    fn reads_saved_level_and_format() {
        let value = json!({"backup":{"format":"gz","level":3}});
        assert_eq!(
            value_in(&value, ConfigKey::BackupFormat).unwrap(),
            Some("gz".into())
        );
        assert_eq!(
            value_in(&value, ConfigKey::BackupLevel).unwrap(),
            Some("3".into())
        );
    }

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let sequence = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = env::temp_dir().join(format!(
                "wslim-config-test-{}-{sequence}",
                std::process::id(),
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn config_path(&self) -> PathBuf {
            self.0.join("config.json")
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn repairs_invalid_previous_value() {
        let directory = TestDirectory::new();
        let path = directory.config_path();
        fs::write(&path, r#"{"backup":{"level":4}}"#).unwrap();

        let (previous, snapshot) =
            update_file(&path, ConfigKey::BackupLevel, Some("6"), false).unwrap();

        assert_eq!(previous.as_deref(), Some("4"));
        assert!(snapshot.is_none());
        assert_eq!(read_document(&path).unwrap()["backup"]["level"], json!(6));
    }

    #[test]
    fn json_set_rejects_invalid_unrelated_setting_without_writing() {
        let directory = TestDirectory::new();
        let path = directory.config_path();
        let original = r#"{"backup":{"format":"xz","level":4}}"#;
        fs::write(&path, original).unwrap();

        assert!(update_file(&path, ConfigKey::BackupFormat, Some("gz"), true).is_err());

        assert_eq!(fs::read_to_string(&path).unwrap(), original);
    }

    #[test]
    fn json_unset_rejects_invalid_unrelated_setting_without_writing() {
        let directory = TestDirectory::new();
        let path = directory.config_path();
        let original = r#"{"backup":{"format":"gz","level":4}}"#;
        fs::write(&path, original).unwrap();

        assert!(update_file(&path, ConfigKey::BackupFormat, None, true).is_err());

        assert_eq!(fs::read_to_string(&path).unwrap(), original);
    }

    #[test]
    fn human_update_allows_unrelated_invalid_setting() {
        let directory = TestDirectory::new();
        let path = directory.config_path();
        fs::write(&path, r#"{"backup":{"format":"xz","level":4}}"#).unwrap();

        update_file(&path, ConfigKey::BackupFormat, Some("gz"), false).unwrap();

        let updated = read_document(&path).unwrap();
        assert_eq!(updated["backup"]["format"], json!("gz"));
        assert_eq!(updated["backup"]["level"], json!(4));
    }

    #[test]
    fn snapshot_keeps_defaults_and_json_types() {
        let snapshot = current_in(&json!({})).unwrap();

        assert_eq!(snapshot["backup.output"], Value::Null);
        assert_eq!(snapshot["backup.format"], json!("xz"));
        assert_eq!(snapshot["backup.level"], json!(6));
        assert!(snapshot["backup.level"].is_number());
        assert!(snapshot["backup.format"].is_string());
    }

    #[test]
    fn update_preserves_unknown_fields() {
        let directory = TestDirectory::new();
        let path = directory.config_path();
        fs::write(
            &path,
            r#"{"backup":{"level":6,"custom":{"enabled":true}},"other":[1,2]}"#,
        )
        .unwrap();

        update_file(&path, ConfigKey::BackupFormat, Some("gz"), false).unwrap();

        let updated = read_document(&path).unwrap();
        assert_eq!(updated["backup"]["custom"]["enabled"], json!(true));
        assert_eq!(updated["other"], json!([1, 2]));
    }

    #[test]
    fn replaces_existing_file_successfully() {
        let directory = TestDirectory::new();
        let path = directory.config_path();
        fs::write(&path, r#"{"backup":{"level":6}}"#).unwrap();

        write_document_atomic(&path, &json!({"backup":{"level":3}})).unwrap();

        assert_eq!(read_document(&path).unwrap()["backup"]["level"], json!(3));
        assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 1);
    }

    #[cfg(windows)]
    #[test]
    fn failed_replacement_preserves_file_and_removes_temporary() {
        use std::os::windows::fs::OpenOptionsExt;

        let directory = TestDirectory::new();
        let path = directory.config_path();
        let original = r#"{"backup":{"level":6}}"#;
        fs::write(&path, original).unwrap();
        let locked_file = OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&path)
            .unwrap();

        assert!(write_document_atomic(&path, &json!({"backup":{"level":3}})).is_err());
        drop(locked_file);

        assert_eq!(fs::read_to_string(&path).unwrap(), original);
        assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 1);
    }
}
