use serde_json::{Map, Value, json};
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ApplicationLogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
    Off,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApplicationLogFormat {
    Pretty,
    Json,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApplicationLogDestination {
    Stderr,
    Stdout,
    File,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApplicationLoggingConfig {
    pub level: ApplicationLogLevel,
    pub format: ApplicationLogFormat,
    pub destination: ApplicationLogDestination,
    pub file: Option<PathBuf>,
}

#[derive(Clone, Debug)]
pub struct ApplicationLogEvent {
    pub level: ApplicationLogLevel,
    pub target: &'static str,
    pub message: &'static str,
    pub fields: Vec<(&'static str, String)>,
}

enum ApplicationLogWriter {
    Stderr,
    Stdout,
    File(File),
}

struct ApplicationLogger {
    config: ApplicationLoggingConfig,
    writer: ApplicationLogWriter,
}

static APPLICATION_LOGGER: OnceLock<Mutex<Option<ApplicationLogger>>> = OnceLock::new();
#[cfg(test)]
static TEST_LOGGING_MUTEX: OnceLock<Mutex<()>> = OnceLock::new();

#[cfg(test)]
pub(crate) fn test_logging_guard() -> std::sync::MutexGuard<'static, ()> {
    TEST_LOGGING_MUTEX
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Default for ApplicationLoggingConfig {
    fn default() -> Self {
        Self {
            level: ApplicationLogLevel::Off,
            format: ApplicationLogFormat::Pretty,
            destination: ApplicationLogDestination::Stderr,
            file: None,
        }
    }
}

impl ApplicationLoggingConfig {
    pub fn enabled(&self) -> bool {
        self.level != ApplicationLogLevel::Off
    }

    pub fn validate(&self) -> io::Result<()> {
        if self.destination == ApplicationLogDestination::File && self.file.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "logging destination 'file' requires --log-file or logging.file",
            ));
        }
        Ok(())
    }

    pub fn from_parts(
        level: Option<&str>,
        format: Option<&str>,
        destination: Option<&str>,
        file: Option<PathBuf>,
    ) -> io::Result<Self> {
        let mut config = Self::default();
        if let Some(level) = level {
            config.level = ApplicationLogLevel::parse(level)?;
        }
        if let Some(format) = format {
            config.format = ApplicationLogFormat::parse(format)?;
        }
        if let Some(destination) = destination {
            config.destination = ApplicationLogDestination::parse(destination)?;
        }
        config.file = file;
        config.validate()?;
        Ok(config)
    }
}

impl ApplicationLogLevel {
    pub fn parse(raw: &str) -> io::Result<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "trace" => Ok(Self::Trace),
            "debug" => Ok(Self::Debug),
            "info" => Ok(Self::Info),
            "warn" | "warning" => Ok(Self::Warn),
            "error" => Ok(Self::Error),
            "off" | "none" | "disabled" => Ok(Self::Off),
            other => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "Unsupported logging level '{other}'; expected trace, debug, info, warn, error, or off"
                ),
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Trace => "trace",
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
            Self::Off => "off",
        }
    }
}

impl ApplicationLogFormat {
    pub fn parse(raw: &str) -> io::Result<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "pretty" => Ok(Self::Pretty),
            "json" => Ok(Self::Json),
            other => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("Unsupported logging format '{other}'; expected pretty or json"),
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pretty => "pretty",
            Self::Json => "json",
        }
    }
}

impl ApplicationLogDestination {
    pub fn parse(raw: &str) -> io::Result<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "stderr" => Ok(Self::Stderr),
            "stdout" => Ok(Self::Stdout),
            "file" => Ok(Self::File),
            other => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "Unsupported logging destination '{other}'; expected stderr, stdout, or file"
                ),
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stderr => "stderr",
            Self::Stdout => "stdout",
            Self::File => "file",
        }
    }
}

impl ApplicationLogEvent {
    pub fn new(
        level: ApplicationLogLevel,
        target: &'static str,
        message: &'static str,
        fields: Vec<(&'static str, String)>,
    ) -> Self {
        Self {
            level,
            target,
            message,
            fields,
        }
    }
}

pub fn init_application_logging(config: ApplicationLoggingConfig) -> io::Result<()> {
    config.validate()?;
    if !config.enabled() {
        shutdown_application_logging();
        return Ok(());
    }

    let writer = match config.destination {
        ApplicationLogDestination::Stderr => ApplicationLogWriter::Stderr,
        ApplicationLogDestination::Stdout => ApplicationLogWriter::Stdout,
        ApplicationLogDestination::File => {
            let path = config.file.clone().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "logging destination 'file' requires --log-file or logging.file",
                )
            })?;
            if let Some(parent) = path.parent() {
                if !parent.as_os_str().is_empty() {
                    std::fs::create_dir_all(parent)?;
                }
            }
            ApplicationLogWriter::File(OpenOptions::new().create(true).append(true).open(path)?)
        }
    };

    let logger = ApplicationLogger { config, writer };
    *logger_slot()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(logger);
    Ok(())
}

pub fn application_logging_is_configured() -> bool {
    logger_slot()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .is_some()
}

pub fn shutdown_application_logging() {
    *logger_slot()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
}

pub fn emit_application_log(event: ApplicationLogEvent) {
    emit_tracing_event(&event);

    let mut guard = logger_slot()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(logger) = guard.as_mut() else {
        return;
    };
    if !logger.should_log(event.level) {
        return;
    }
    let rendered = render_event(&logger.config, &event);
    let _ = logger.writer.write_line(&rendered);
}

fn emit_tracing_event(event: &ApplicationLogEvent) {
    if event.level == ApplicationLogLevel::Off {
        return;
    }

    let fields = structured_fields_json(&event.fields);
    match event.level {
        ApplicationLogLevel::Trace => tracing::trace!(
            target: "wardrobe_application",
            application_target = event.target,
            application_fields = %fields,
            "{}",
            event.message
        ),
        ApplicationLogLevel::Debug => tracing::debug!(
            target: "wardrobe_application",
            application_target = event.target,
            application_fields = %fields,
            "{}",
            event.message
        ),
        ApplicationLogLevel::Info => tracing::info!(
            target: "wardrobe_application",
            application_target = event.target,
            application_fields = %fields,
            "{}",
            event.message
        ),
        ApplicationLogLevel::Warn => tracing::warn!(
            target: "wardrobe_application",
            application_target = event.target,
            application_fields = %fields,
            "{}",
            event.message
        ),
        ApplicationLogLevel::Error => tracing::error!(
            target: "wardrobe_application",
            application_target = event.target,
            application_fields = %fields,
            "{}",
            event.message
        ),
        ApplicationLogLevel::Off => {}
    }
}

fn logger_slot() -> &'static Mutex<Option<ApplicationLogger>> {
    APPLICATION_LOGGER.get_or_init(|| Mutex::new(None))
}

impl ApplicationLogger {
    fn should_log(&self, event_level: ApplicationLogLevel) -> bool {
        self.config.enabled() && event_level >= self.config.level
    }
}

impl ApplicationLogWriter {
    fn write_line(&mut self, line: &str) -> io::Result<()> {
        match self {
            Self::Stderr => {
                let mut stderr = io::stderr().lock();
                writeln!(stderr, "{line}")
            }
            Self::Stdout => {
                let mut stdout = io::stdout().lock();
                writeln!(stdout, "{line}")
            }
            Self::File(file) => {
                writeln!(file, "{line}")?;
                file.flush()
            }
        }
    }
}

fn render_event(config: &ApplicationLoggingConfig, event: &ApplicationLogEvent) -> String {
    match config.format {
        ApplicationLogFormat::Pretty => render_pretty_event(event),
        ApplicationLogFormat::Json => render_json_event(event),
    }
}

fn render_pretty_event(event: &ApplicationLogEvent) -> String {
    let fields = event
        .fields
        .iter()
        .map(|(key, value)| format!("{key}={}", quote_if_needed(value)))
        .collect::<Vec<_>>()
        .join(" ");
    if fields.is_empty() {
        format!(
            "{} {} {}",
            timestamp_millis(),
            event.level.as_str(),
            event.message
        )
    } else {
        format!(
            "{} {} {} target={} {}",
            timestamp_millis(),
            event.level.as_str(),
            event.message,
            event.target,
            fields
        )
    }
}

fn render_json_event(event: &ApplicationLogEvent) -> String {
    let mut object = Map::new();
    object.insert("ts_ms".to_string(), json!(timestamp_millis()));
    object.insert("level".to_string(), json!(event.level.as_str()));
    object.insert("target".to_string(), json!(event.target));
    object.insert("message".to_string(), json!(event.message));
    for (key, value) in &event.fields {
        object.insert((*key).to_string(), json!(value));
    }
    Value::Object(object).to_string()
}

fn structured_fields_json(fields: &[(&'static str, String)]) -> String {
    let mut object = Map::new();
    for (key, value) in fields {
        object.insert((*key).to_string(), json!(value));
    }
    Value::Object(object).to_string()
}

fn quote_if_needed(value: &str) -> String {
    if value.chars().any(char::is_whitespace) {
        format!("{value:?}")
    } else {
        value.to_string()
    }
}

fn timestamp_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logging_config_validation_rejects_invalid_values() {
        assert!(ApplicationLogLevel::parse("verbose").is_err());
        assert!(ApplicationLogFormat::parse("xml").is_err());
        assert!(ApplicationLogDestination::parse("syslog").is_err());
        assert!(
            ApplicationLoggingConfig::from_parts(Some("info"), Some("json"), Some("file"), None)
                .is_err()
        );
    }

    #[test]
    fn logging_config_parses_supported_values() {
        let config =
            ApplicationLoggingConfig::from_parts(Some("debug"), Some("json"), Some("stderr"), None)
                .expect("logging config should parse");
        assert_eq!(config.level, ApplicationLogLevel::Debug);
        assert_eq!(config.format, ApplicationLogFormat::Json);
        assert_eq!(config.destination, ApplicationLogDestination::Stderr);
    }

    #[test]
    fn embedded_logging_is_not_configured_by_default() {
        let _guard = test_logging_guard();
        shutdown_application_logging();
        assert!(!application_logging_is_configured());
    }

    #[test]
    fn application_logging_writes_file_without_terminal() {
        let _guard = test_logging_guard();
        shutdown_application_logging();
        let path = std::env::temp_dir().join(format!(
            "wardrobe_application_logging_{}.log",
            timestamp_millis()
        ));
        let config = ApplicationLoggingConfig::from_parts(
            Some("info"),
            Some("json"),
            Some("file"),
            Some(path.clone()),
        )
        .expect("file logging config should parse");

        init_application_logging(config).expect("file logging should initialize");
        emit_application_log(ApplicationLogEvent::new(
            ApplicationLogLevel::Info,
            "wardrobe_test",
            "startup",
            vec![("operation", "test".to_string())],
        ));
        shutdown_application_logging();

        let contents = std::fs::read_to_string(&path).expect("log file should be readable");
        assert!(contents.contains("\"message\":\"startup\""));
        assert!(contents.contains("\"operation\":\"test\""));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn pretty_and_json_renderers_include_structured_fields() {
        let event = ApplicationLogEvent::new(
            ApplicationLogLevel::Info,
            "wardrobe_test",
            "command_finished",
            vec![
                ("command", "read".to_string()),
                ("success", "true".to_string()),
            ],
        );
        let pretty = render_event(&ApplicationLoggingConfig::default(), &event);
        assert!(pretty.contains("command=read"));
        let json = render_event(
            &ApplicationLoggingConfig {
                format: ApplicationLogFormat::Json,
                ..ApplicationLoggingConfig::default()
            },
            &event,
        );
        assert!(json.contains("\"command\":\"read\""));
        assert!(json.contains("\"target\":\"wardrobe_test\""));
    }
}
