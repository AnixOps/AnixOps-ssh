//! Saved command snippets. They are plain text that the frontend pastes into
//! a terminal; nothing here runs them.

use crate::hosts::{load_config, open_store};
use easyssh_core::CommandSnippet;

const MAX_NAME: usize = 100;
const MAX_CONTENT: usize = 16 * 1024;

#[tauri::command]
pub fn snippets_list() -> Result<Vec<CommandSnippet>, String> {
    Ok(load_config()?.snippets)
}

/// Creates or updates a snippet and returns its id.
#[tauri::command]
pub fn snippet_save(id: Option<String>, name: String, content: String) -> Result<String, String> {
    let snippet = validated(id, &name, &content)?;
    // Only this snippet is written: a host saved elsewhere meanwhile is safe.
    open_store()?
        .save_snippet(&snippet)
        .map_err(|error| error.to_string())?;
    Ok(snippet.id)
}

#[tauri::command]
pub fn snippet_delete(id: String) -> Result<(), String> {
    open_store()?
        .remove_snippet(&id)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn validated(id: Option<String>, name: &str, content: &str) -> Result<CommandSnippet, String> {
    let name = name.trim();
    let content = content.trim_end();
    if name.is_empty() || name.chars().count() > MAX_NAME || name.chars().any(char::is_control) {
        return Err(format!(
            "the name must be 1 to {MAX_NAME} characters on one line"
        ));
    }
    if content.trim().is_empty() || content.len() > MAX_CONTENT || content.contains('\0') {
        return Err("the command must not be empty or longer than 16 KB".to_owned());
    }
    Ok(CommandSnippet {
        id: id.unwrap_or_else(|| format!("snippet-{}", chrono::Utc::now().timestamp_millis())),
        name: name.to_owned(),
        content: content.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::validated;

    #[test]
    fn trims_and_keeps_multiline_commands() {
        let snippet = validated(None, "  Disk usage ", "df -h\nfree -m\n\n").unwrap();
        assert_eq!(snippet.name, "Disk usage");
        assert_eq!(snippet.content, "df -h\nfree -m");
        assert!(snippet.id.starts_with("snippet-"));
    }

    #[test]
    fn rejects_empty_or_multiline_names() {
        assert!(validated(None, " ", "ls").is_err());
        assert!(validated(None, "a\nb", "ls").is_err());
        assert!(validated(None, "ok", "   ").is_err());
    }
}
