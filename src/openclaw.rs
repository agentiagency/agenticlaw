use std::path::Path;

use crate::parser::{self, ParseError};
use crate::session::Session;
use crate::transform::{self, SessionEvent};

/// Maximum format version this build of agenticlaw supports.
pub const MAX_SUPPORTED_VERSION: u32 = 3;

pub struct OpenclawSession {
    version: u32,
    id: String,
    timestamp: String,
    cwd: Option<String>,
    events: Vec<SessionEvent>,
    pub parse_errors: Vec<ParseError>,
}

impl OpenclawSession {
    pub fn from_jsonl(path: &Path) -> Result<Self, std::io::Error> {
        let content = std::fs::read_to_string(path)?;
        Ok(Self::from_str(&content))
    }

    pub fn from_str(content: &str) -> Self {
        let result = parser::parse_lines(content);
        let events = transform::transform(result.records);

        // Extract header info from events
        let (version, id, timestamp, cwd) = events
            .iter()
            .find_map(|e| {
                if let SessionEvent::Header {
                    version,
                    id,
                    timestamp,
                    cwd,
                } = e
                {
                    Some((*version, id.clone(), timestamp.clone(), cwd.clone()))
                } else {
                    None
                }
            })
            .unwrap_or_default();

        let mut parse_errors = result.errors;

        if version > MAX_SUPPORTED_VERSION {
            parse_errors.insert(
                0,
                ParseError {
                    line: 1,
                    message: format!(
                        "JSONL format version {} is newer than supported version {}. \
                     Output may be incomplete. Upgrade agenticlaw to parse this file correctly.",
                        version, MAX_SUPPORTED_VERSION
                    ),
                },
            );
        }

        OpenclawSession {
            version,
            id,
            timestamp,
            cwd,
            events,
            parse_errors,
        }
    }
}

impl Session for OpenclawSession {
    fn id(&self) -> &str {
        &self.id
    }

    fn timestamp(&self) -> &str {
        &self.timestamp
    }

    fn cwd(&self) -> Option<&str> {
        self.cwd.as_deref()
    }

    fn events(&self) -> &[SessionEvent] {
        &self.events
    }
}
