use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Serialize, Deserialize, Clone)]
pub struct SavedTab {
    pub session: String,
    pub cwd: PathBuf,
    pub title: String,
}

#[derive(Serialize, Deserialize, Default)]
pub struct SavedWindow {
    pub tabs: Vec<SavedTab>,
    pub active: usize,
}

pub struct Store {
    path: PathBuf,
    file: File,
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

fn state_dir() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".local/state"))
        .join("claude-tabs")
}

fn project_dir_name(cwd: &str) -> String {
    cwd.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect()
}

fn conversation_path(session: &str) -> Option<PathBuf> {
    let name = format!("{session}.jsonl");
    fs::read_dir(home().join(".claude/projects"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path().join(&name))
        .find(|path| path.exists())
}

fn is_real_reply(entry: &serde_json::Value) -> bool {
    if entry["type"] != "assistant" || entry["message"]["model"] == "<synthetic>" {
        return false;
    }
    entry["message"]["content"].as_array().into_iter().flatten().any(|block| match block["type"].as_str() {
        Some("tool_use") => true,
        Some("text") => {
            let text = block["text"].as_str().unwrap_or("").trim();
            !text.is_empty() && !(text.len() < 200 && text.to_lowercase().contains("ponytail"))
        },
        _ => false,
    })
}

/// Deletes a conversation that never got a real reply from Claude
pub fn drop_if_boring(session: &str) {
    let Some(path) = conversation_path(session) else {
        return;
    };
    let Ok(file) = File::open(&path) else {
        return;
    };
    let has_reply = BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(&line).ok())
        .any(|entry| is_real_reply(&entry));
    if !has_reply {
        let _ = fs::remove_file(path);
    }
}

/// Finds a saved conversation and the folder it belongs to
pub fn find_conversation(session: &str) -> Option<PathBuf> {
    let path = conversation_path(session)?;
    let project = path.parent()?;
    let project_name = project.file_name()?.to_string_lossy().into_owned();
    BufReader::new(File::open(&path).ok()?)
        .lines()
        .map_while(Result::ok)
        .find_map(|line| {
            let value: serde_json::Value = serde_json::from_str(&line).ok()?;
            let cwd = value["cwd"].as_str()?;
            (project_dir_name(cwd) == project_name).then(|| PathBuf::from(cwd))
        })
        .or(Some(PathBuf::new()))
}

impl Store {
    /// Claims the most recently closed window's state, or a fresh one if none is restorable
    pub fn open() -> (Store, SavedWindow) {
        let dir = state_dir();
        let _ = fs::create_dir_all(&dir);
        let mut candidates: Vec<(SystemTime, PathBuf)> = fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| entry.path().extension().is_some_and(|e| e == "json"))
            .filter_map(|entry| Some((entry.metadata().ok()?.modified().ok()?, entry.path())))
            .collect();
        candidates.sort_by(|a, b| b.0.cmp(&a.0));

        for (_, path) in candidates {
            let Ok(file) = File::options().read(true).write(true).open(&path) else {
                continue;
            };
            if file.try_lock().is_err() {
                continue;
            }
            let mut saved: SavedWindow = fs::read(&path)
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                .unwrap_or_default();
            if saved.tabs.is_empty() {
                let _ = fs::remove_file(&path);
                continue;
            }
            saved.active = saved.active.min(saved.tabs.len() - 1);
            return (Store { path, file }, saved);
        }

        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let path = dir.join(format!("{nanos}-{}.json", std::process::id()));
        let file = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .expect("cannot create state file");
        let _ = file.try_lock();
        (Store { path, file }, SavedWindow::default())
    }

    pub fn save(&self, window: &SavedWindow) {
        let Ok(data) = serde_json::to_vec(window) else {
            return;
        };
        let mut file = &self.file;
        let _ = file.set_len(0);
        let _ = file.seek(SeekFrom::Start(0));
        let _ = file.write_all(&data);
    }

    pub fn touch(&self) {
        let _ = self.file.set_modified(SystemTime::now());
    }

    pub fn remove(&self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::is_real_reply;

    #[test]
    fn only_real_answers_count_as_replies() {
        let parse = |s: &str| serde_json::from_str::<serde_json::Value>(s).unwrap();
        let reply = |model: &str, content: &str| {
            parse(&format!(r#"{{"type":"assistant","message":{{"model":"{model}","content":{content}}}}}"#))
        };
        assert!(!is_real_reply(&reply("<synthetic>", r#"[{"type":"text","text":"No response requested."}]"#)));
        assert!(!is_real_reply(&reply("claude-opus-5-5", r#"[{"type":"text","text":"PONYTAIL ACTIVATED"}]"#)));
        assert!(!is_real_reply(&parse(r#"{"type":"user","message":{"content":"hi"}}"#)));
        assert!(is_real_reply(&reply("claude-opus-5-5", r#"[{"type":"text","text":"Here is the fix."}]"#)));
        assert!(is_real_reply(&reply("claude-opus-5-5", r#"[{"type":"tool_use","name":"Bash"}]"#)));
    }
}
