//! Media extraction from tool output.
//!
//! This module provides utilities for parsing media payloads that may be
//! embedded in tool output, such as images, audio, or video files.

use crate::types::ContentPart;
use serde::Deserialize;

/// Internal payload structure for parsing media from tool output.
///
/// This struct represents the JSON format used to encode media data
/// within tool result content.
#[derive(Deserialize)]
struct MediaPayload {
    #[serde(rename = "type")]
    kind: String,
    mime: String,
    data: String,
    #[serde(default)]
    message: Option<String>,
}

/// Extracts media parts from tool output content.
///
/// Parses the content string as a JSON media payload and converts it
/// to appropriate [`ContentPart`] items. If the content is not a valid
/// media payload, returns an empty vector.
///
/// # Arguments
///
/// * `content` - The tool output content to parse
///
/// # Returns
///
/// A vector of [`ContentPart`] items extracted from the content
pub fn extract_media_parts(content: &str) -> Vec<ContentPart> {
    let parsed: MediaPayload = match serde_json::from_str(content) {
        Ok(value) => value,
        Err(_) => return Vec::new(),
    };

    if parsed.kind != "media" {
        return Vec::new();
    }

    let mut parts = Vec::new();
    if let Some(text) = parsed.message {
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            parts.push(ContentPart::Text {
                text: trimmed.to_string(),
            });
        }
    }

    let mime = parsed.mime.as_str();
    let content_part = if mime.starts_with("image/") {
        ContentPart::Image {
            data: parsed.data,
            mime_type: parsed.mime,
        }
    } else if mime.starts_with("audio/") {
        ContentPart::Audio {
            data: parsed.data,
            mime_type: parsed.mime,
        }
    } else if mime.starts_with("video/") {
        ContentPart::Video {
            data: parsed.data,
            mime_type: parsed.mime,
        }
    } else {
        ContentPart::File {
            data: parsed.data,
            mime_type: parsed.mime,
        }
    };
    parts.push(content_part);

    parts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ContentPart;

    #[test]
    fn extract_media_parts_parses_image() {
        let json = r#"{"type":"media","mime":"image/png","data":"ZmFrZQ==","message":"Image read successfully"}"#;
        let parts = extract_media_parts(json);
        assert_eq!(parts.len(), 2);
        match &parts[0] {
            ContentPart::Text { text } => assert!(text.contains("Image")),
            _ => panic!("first part should be text"),
        }
        match &parts[1] {
            ContentPart::Image { mime_type, data } => {
                assert_eq!(mime_type, "image/png");
                assert_eq!(data, "ZmFrZQ==");
            }
            _ => panic!("second part should be image"),
        }
    }

    #[test]
    fn extract_media_parts_ignores_non_media() {
        let json = r#"{"type":"text","text":"hello"}"#;
        let parts = extract_media_parts(json);
        assert!(parts.is_empty());
    }
}
