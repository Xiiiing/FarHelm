//! Safe display projection shared by native history and streaming output.
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};
const PAGE_BYTES: usize = 500 * 1024;

#[derive(Default, PartialEq)]
enum TextMode {
    #[default]
    Normal,
    Url,
    LocalPath,
}

/// An incremental lexer shares exactly the same rules with history projection.
/// It holds at most a protocol/drive prefix; ordinary text is immediately visible.
#[derive(Default)]
pub struct StreamText {
    pending: String,
    previous: Option<char>,
    mode: TextMode,
    offset: usize,
}
fn url_character(c: char) -> bool {
    !c.is_whitespace() && !"<>\"`".contains(c)
}
fn path_character(c: char) -> bool {
    !c.is_whitespace() && !"[]()<>{}'\"`|，。；！？".contains(c)
}
impl StreamText {
    pub fn feed(&mut self, delta: &str, final_chunk: bool) -> (usize, String) {
        let mut output = String::new();
        for c in delta.chars() {
            if self.mode == TextMode::Url && url_character(c) {
                output.push(c);
                self.previous = Some(c);
                continue;
            }
            if self.mode == TextMode::LocalPath && path_character(c) {
                self.previous = Some(c);
                continue;
            }
            self.mode = TextMode::Normal;
            self.pending.push(c);
            self.flush(false, &mut output);
        }
        if final_chunk {
            self.flush(true, &mut output);
        }
        let offset = self.offset;
        self.offset += output.chars().count();
        (offset, output)
    }
    fn flush(&mut self, final_chunk: bool, output: &mut String) {
        while !self.pending.is_empty() {
            let mut wait = false;
            let mut url = false;
            let candidate = self.pending.to_ascii_lowercase();
            for prefix in ["http://", "https://", "mailto:"] {
                if prefix.starts_with(&candidate) {
                    wait = true;
                }
                if let Some(rest) = candidate.strip_prefix(prefix) {
                    if rest.is_empty() {
                        wait = true;
                    } else if rest.chars().next().is_some_and(url_character) {
                        url = true;
                    }
                }
            }
            if url {
                output.push_str(&self.pending);
                self.previous = self.pending.chars().next_back();
                self.pending.clear();
                self.mode = TextMode::Url;
                return;
            }
            let boundary = self
                .previous
                .is_none_or(|c| !c.is_alphanumeric() && c != '_' && c != '/');
            let first = self.pending.chars().next().expect("nonempty pending");
            let mut path_prefix = None;
            if boundary {
                if self.pending.starts_with('/') {
                    path_prefix = Some(1);
                } else if self.pending.starts_with("~/") {
                    path_prefix = Some(2);
                } else if first.is_ascii_alphabetic()
                    && (self.pending.starts_with(&format!("{first}:/"))
                        || self.pending.starts_with(&format!("{first}:\\")))
                {
                    path_prefix = Some(3);
                }
                if self.pending == "~"
                    || (first.is_ascii_alphabetic()
                        && (self.pending.len() == 1 || self.pending == format!("{first}:")))
                {
                    wait = true;
                }
            }
            if let Some(length) = path_prefix {
                let rest = &self.pending[length..];
                if rest.is_empty() {
                    wait = true;
                } else if rest.chars().next().is_some_and(path_character) {
                    output.push_str("[local path]");
                    self.previous = self.pending.chars().next_back();
                    self.pending.clear();
                    self.mode = TextMode::LocalPath;
                    return;
                }
            }
            if wait && !final_chunk {
                return;
            }
            output.push(first);
            self.previous = Some(first);
            self.pending.drain(..first.len_utf8());
        }
    }
}
pub fn redact_paths(text: &str) -> String {
    StreamText::default().feed(text, true).1
}

pub fn formal_title(value: &Value) -> Option<String> {
    let text = value
        .as_str()?
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if text.is_empty()
        || [
            "codex session",
            "untitled",
            "new conversation",
            "new chat",
            "未命名会话",
            "新会话",
        ]
        .contains(&text.to_lowercase().as_str())
    {
        None
    } else {
        Some(redact_paths(&text))
    }
}

fn summary(text: &str) -> String {
    let text = text.replace('\0', "");
    if text.len() <= 2048 {
        return text;
    }
    let mut end = 2045;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

pub fn normalise_turn(turn: &Value) -> Value {
    let mut items = Vec::new();
    for (index, item) in turn["items"].as_array().into_iter().flatten().enumerate() {
        let mut metadata = json!({});
        let (kind, text) = match item["type"].as_str() {
            Some("userMessage") => (
                "user_message",
                item["content"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|p| p["type"] == "text")
                    .filter_map(|p| p["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            Some("agentMessage") => (
                "assistant_message",
                item["text"].as_str().unwrap_or_default().to_owned(),
            ),
            Some("commandExecution") => {
                if let Some(status) = item["status"]
                    .as_str()
                    .filter(|s| ["completed", "failed", "inProgress", "declined"].contains(s))
                {
                    metadata["status"] = json!(status);
                }
                let exit = item["exitCode"].as_i64();
                if let Some(exit) = exit {
                    metadata["exit_code"] = json!(exit);
                    metadata["status"] = json!(if exit == 0 { "completed" } else { "failed" });
                }
                let duration = item["durationMs"].as_f64().filter(|n| *n >= 0.0);
                if let Some(duration) = duration {
                    metadata["duration_ms"] = json!(duration as u64);
                }
                let executable = item["command"]
                    .as_str()
                    .and_then(|s| s.split_whitespace().next())
                    .unwrap_or("command")
                    .trim_matches(['\'', '"'])
                    .rsplit(['/', '\\'])
                    .next()
                    .unwrap_or("command");
                let mut text = format!("{executable} …");
                if let Some(exit) = exit {
                    text.push_str(&format!(" · exit {exit}"));
                }
                if let Some(duration) = duration {
                    text.push_str(&format!(" · {duration} ms"));
                }
                if let Some(command) = item["command"].as_str() {
                    text.push_str(&format!("\n$ {command}"));
                }
                if let Some(output) = item["aggregatedOutput"].as_str() {
                    text.push_str(&format!("\n{output}"));
                }
                ("command_summary", text)
            }
            Some("fileChange") => {
                if let Some(status) = item["status"]
                    .as_str()
                    .filter(|s| ["completed", "failed", "inProgress", "declined"].contains(s))
                {
                    metadata["status"] = json!(status);
                }
                let text = item["changes"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|c| {
                        let name = c["path"]
                            .as_str()
                            .unwrap_or("file")
                            .rsplit(['/', '\\'])
                            .next()
                            .unwrap_or("file");
                        let kind = c["kind"]
                            .as_str()
                            .or_else(|| c["kind"]["type"].as_str())
                            .unwrap_or("update");
                        format!("{kind}: {name}\n{}", c["diff"].as_str().unwrap_or_default())
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                ("file_change_summary", text)
            }
            Some("plan") => (
                "assistant_message",
                format!("计划\n\n{}", item["text"].as_str().unwrap_or_default()),
            ),
            Some("enteredReviewMode") => (
                "assistant_message",
                format!(
                    "开始审查\n\n{}",
                    item["review"].as_str().unwrap_or_default()
                ),
            ),
            Some("exitedReviewMode") => (
                "assistant_message",
                format!(
                    "审查结果\n\n{}",
                    item["review"].as_str().unwrap_or_default()
                ),
            ),
            Some("contextCompaction") => ("assistant_message", "上下文已压缩".into()),
            Some("mcpToolCall") => (
                "command_summary",
                format!(
                    "MCP {} / {} · {}\n{}",
                    item["server"].as_str().unwrap_or_default(),
                    item["tool"].as_str().unwrap_or_default(),
                    item["status"].as_str().unwrap_or_default(),
                    item["result"].get("content").unwrap_or(&Value::Null)
                ),
            ),
            Some("collabAgentToolCall") => (
                "command_summary",
                format!(
                    "子任务 {} · {}\n{}",
                    item["tool"].as_str().unwrap_or_default(),
                    item["status"].as_str().unwrap_or_default(),
                    item["agentsStates"]
                ),
            ),
            Some("imageView") => {
                metadata["native_image_path"] = item["path"].clone();
                ("image", "Codex 图片".to_owned())
            }
            Some("imageGeneration") if item["savedPath"].is_string() => {
                metadata["native_image_path"] = item["savedPath"].clone();
                ("image", "Codex 生成的图片".to_owned())
            }
            _ => continue,
        };
        if text.is_empty() {
            continue;
        }
        metadata["item_id"] = item["id"]
            .as_str()
            .map_or_else(|| json!(format!("item-{index}")), |s| json!(s));
        if kind == "user_message"
            && let Some(client_id) = item["clientId"].as_str()
        {
            metadata["client_id"] = json!(client_id);
        }
        metadata["kind"] = json!(kind);
        metadata["text"] = json!(redact_paths(&text));
        items.push(metadata);
    }
    if !turn["error"].is_null() {
        items.push(json!({"item_id":format!("{}-error",turn["id"].as_str().unwrap_or("turn")),"kind":"error","text":summary(&redact_paths(turn["error"]["message"].as_str().unwrap_or("Codex turn failed")))}));
    }
    json!({"turn_id":turn["id"].as_str().unwrap_or_default(),"status":turn["status"].as_str().unwrap_or("unknown"),"started_at_unix":turn["startedAt"],"completed_at_unix":turn["completedAt"],"items":items})
}

pub fn decode_cursor(session: &str, cursor: Option<&str>) -> Result<(Option<String>, Value)> {
    let Some(raw) = cursor.and_then(|c| c.strip_prefix("fh1.")) else {
        return Ok((cursor.map(str::to_owned), Value::Null));
    };
    ensure!(raw.len() < 8192, "invalid_history_cursor");
    let value: Value = serde_json::from_slice(
        &URL_SAFE_NO_PAD
            .decode(raw)
            .context("invalid_history_cursor")?,
    )
    .context("invalid_history_cursor")?;
    ensure!(
        value["session"] == session
            && ["t", "i", "offset"]
                .iter()
                .all(|key| value[*key].as_u64().is_some()),
        "invalid_history_cursor"
    );
    Ok((value["upstream"].as_str().map(str::to_owned), value))
}

pub fn bounded_page(
    session: &str,
    turns: &[Value],
    upstream: Option<&str>,
    next: Option<&str>,
    resume: &Value,
) -> Result<Value> {
    let start = resume["t"].as_u64().unwrap_or(0) as usize;
    ensure!(
        resume.is_null() || start < turns.len(),
        "history_changed_reload"
    );
    let mut page = json!({"session_id":session,"turns":[],"next_cursor":next});
    for (ti, raw) in turns.iter().enumerate().skip(start) {
        let mut turn = normalise_turn(raw);
        ensure!(
            resume.is_null() || ti != start || turn["turn_id"] == resume["turn"],
            "history_changed_reload"
        );
        let items = turn["items"].take().as_array().cloned().unwrap_or_default();
        let turn_id = turn["turn_id"].clone();
        turn["items"] = json!([]);
        page["turns"].as_array_mut().expect("turns").push(turn);
        let pi = page["turns"].as_array().expect("turns").len() - 1;
        let first = if ti == start {
            resume["i"].as_u64().unwrap_or(0) as usize
        } else {
            0
        };
        ensure!(
            resume.is_null() || ti != start || first < items.len(),
            "history_changed_reload"
        );
        for (ii, item) in items.iter().enumerate().skip(first) {
            let offset = if ti == start && ii == first {
                resume["offset"].as_u64().unwrap_or(0) as usize
            } else {
                0
            };
            ensure!(
                resume.is_null() || ti != start || ii != first || item["item_id"] == resume["item"],
                "history_changed_reload"
            );
            let text = item["text"].as_str().unwrap_or_default();
            let chars = text.chars().collect::<Vec<_>>();
            ensure!(offset <= chars.len(), "invalid_history_offset");
            let mut fragment = item.clone();
            fragment["text"] = json!(chars[offset..].iter().collect::<String>());
            fragment["text_offset"] = json!(offset);
            fragment["text_complete"] = json!(true);
            let target = page["turns"][pi]["items"].as_array_mut().expect("items");
            let fi = target.len();
            target.push(fragment);
            if serde_json::to_vec(&page)?.len() <= PAGE_BYTES {
                continue;
            }
            let (mut low, mut high) = (0, chars.len() - offset);
            while low < high {
                let mid = (low + high).div_ceil(2);
                page["turns"][pi]["items"][fi]["text"] =
                    json!(chars[offset..offset + mid].iter().collect::<String>());
                if serde_json::to_vec(&page)?.len() <= PAGE_BYTES - 1024 {
                    low = mid;
                } else {
                    high = mid - 1;
                }
            }
            page["turns"][pi]["items"][fi]["text"] =
                json!(chars[offset..offset + low].iter().collect::<String>());
            page["turns"][pi]["items"][fi]["text_complete"] = json!(false);
            if low == 0 {
                let target = page["turns"][pi]["items"].as_array_mut().expect("items");
                target.pop();
                if target.is_empty() {
                    page["turns"].as_array_mut().expect("turns").pop();
                }
            }
            let cursor = json!({"session":session,"upstream":upstream,"t":ti,"i":ii,"offset":offset+low,"turn":turn_id,"item":item["item_id"]});
            page["next_cursor"] = json!(format!(
                "fh1.{}",
                URL_SAFE_NO_PAD.encode(serde_json::to_vec(&cursor)?)
            ));
            page["continuation"] = json!({"kind":if offset+low>0 {"message"} else {"history"},"turn_id":turn_id,"item_id":item["item_id"],"text_offset":offset+low});
            return Ok(page);
        }
    }
    Ok(page)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn paths_urls_math_and_split_stream_share_one_projection() {
        assert_eq!(
            redact_paths("HTTPS://example.org/a/b"),
            "HTTPS://example.org/a/b"
        );
        let raw = "**训练🙂** [文档](https://example.org/a/b?q=1) [本地](/home/private/result.json) `curl https://example.org/api` 公式 \\(a/b\\) $x/y$ C:\\private\\file.py\n";
        let expected = raw
            .replace("/home/private/result.json", "[local path]")
            .replace("C:\\private\\file.py", "[local path]");
        assert_eq!(redact_paths(raw), expected);
        let mut stream = StreamText::default();
        let mut result = String::new();
        for c in raw.chars() {
            let (offset, text) = stream.feed(&c.to_string(), false);
            assert_eq!(offset, result.chars().count());
            result.push_str(&text);
            assert!(!result.contains("private"));
        }
        result.push_str(&stream.feed("", true).1);
        assert_eq!(result, expected);
    }
    #[test]
    fn ordinary_streaming_text_does_not_wait_for_whitespace_and_paths_are_bounded() {
        let mut stream = StreamText::default();
        assert_eq!(stream.feed("正在检查项目", false).1, "正在检查项目");
        assert_eq!(stream.feed(" /home/", false).1, " [local path]");
        for _ in 0..10_000 {
            assert!(stream.feed("private", false).1.is_empty());
            assert!(stream.pending.len() <= 8);
        }
        assert_eq!(stream.feed(" 完成", false).1, " 完成");
        for raw in [
            "https://example.org/a/b",
            "[x](/private/file)",
            "C:\\private\\file.py",
            "\\(a/b\\)",
            "没有标点的中文",
            "http:/private",
            "~/secret",
            "a/b",
            "文本 C:/private 再继续",
        ] {
            let expected = redact_paths(raw);
            for width in [1, 3, 17] {
                let characters: Vec<_> = raw.chars().collect();
                let mut stream = StreamText::default();
                let mut text = String::new();
                for chunk in characters.chunks(width) {
                    text.push_str(&stream.feed(&chunk.iter().collect::<String>(), false).1);
                }
                text.push_str(&stream.feed("", true).1);
                assert_eq!(text, expected);
            }
        }
    }
    #[test]
    fn large_unicode_continuations_are_lossless_and_bound_to_identity() {
        let text = "训练🙂结果\n".repeat(100_000);
        let turns = vec![
            json!({"id":"turn","status":"completed","items":[{"id":"item","type":"agentMessage","text":text}]}),
        ];
        let mut cursor = None;
        let mut result = String::new();
        let mut pages = 0;
        loop {
            let (upstream, resume) = decode_cursor("s", cursor.as_deref()).unwrap();
            let page = bounded_page("s", &turns, upstream.as_deref(), None, &resume).unwrap();
            assert!(serde_json::to_vec(&page).unwrap().len() < 512 * 1024);
            for turn in page["turns"].as_array().unwrap() {
                for item in turn["items"].as_array().unwrap() {
                    assert_eq!(
                        item["text_offset"].as_u64().unwrap() as usize,
                        result.chars().count()
                    );
                    result.push_str(item["text"].as_str().unwrap());
                }
            }
            pages += 1;
            cursor = page["next_cursor"].as_str().map(str::to_owned);
            if cursor.is_none() {
                break;
            }
            assert!(decode_cursor("other", cursor.as_deref()).is_err());
            assert!(pages < 20);
        }
        assert!(pages > 1);
        assert_eq!(result, text);
        assert!(
            bounded_page(
                "s",
                &turns,
                None,
                None,
                &json!({"t":0,"i":0,"offset":0,"turn":"turn","item":"wrong"})
            )
            .is_err()
        );
    }
}
