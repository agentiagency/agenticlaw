use std::fs;
use std::path::Path;

use crate::types::Record;

pub struct ParseResult {
    pub records: Vec<Record>,
    pub errors: Vec<ParseError>,
}

pub struct ParseError {
    pub line: usize,
    pub message: String,
}

pub fn parse_session(path: &Path) -> Result<ParseResult, std::io::Error> {
    let content = fs::read_to_string(path)?;
    Ok(parse_lines(&content))
}

pub fn parse_lines(content: &str) -> ParseResult {
    let mut records = Vec::new();
    let mut errors = Vec::new();

    for (i, line) in content.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<Record>(line) {
            Ok(record) => records.push(record),
            Err(e) => errors.push(ParseError {
                line: i + 1,
                message: e.to_string(),
            }),
        }
    }

    ParseResult { records, errors }
}
