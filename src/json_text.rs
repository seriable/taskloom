//! Pulls JSON out of free-form model replies.
//!
//! Model answers often wrap the JSON in a Markdown fence, put a sentence in
//! front of it, or append a second value or a prose footer after it. Callers
//! that read JSON from model text use these helpers instead of carrying their
//! own `find('{')`/`rfind('}')` variant.

use serde_json::Value;

/// Returns the first complete JSON object in `text`.
///
/// Accepts a reply wrapped in a Markdown fence (with or without a language
/// tag, any case) and prose before or after the object. Only an object is
/// returned; a top-level array or scalar yields `None`. Strict JSON: no
/// trailing-comma repair and no truncation salvage (see
/// [`parse_first_json_lenient`] for that).
pub fn first_json_object(text: &str) -> Option<Value> {
    let body = strip_code_fence(text);
    if let Ok(value @ Value::Object(_)) = serde_json::from_str::<Value>(body) {
        return Some(value);
    }
    object_from_first_brace(body)
}

/// Parses the balanced object that starts at the first `{` of `text`, ignoring
/// anything before it and anything after its matching `}`.
pub fn object_from_first_brace(text: &str) -> Option<Value> {
    let start = text.find('{')?;
    let span = first_json_value_span(&text[start..]).ok()?;
    serde_json::from_str::<Value>(span)
        .ok()
        .filter(Value::is_object)
}

/// Removes a surrounding Markdown code fence (` ``` `, ` ```json `,
/// ` ```JSON `, ...) and surrounding whitespace. Text that does not start with
/// a fence is only trimmed.
pub fn strip_code_fence(text: &str) -> &str {
    let trimmed = text.trim();
    let Some(rest) = trimmed.strip_prefix("```") else {
        return trimmed;
    };
    let rest = rest
        .trim_start_matches(|character: char| character.is_ascii_alphanumeric())
        .trim_end();
    rest.strip_suffix("```").unwrap_or(rest).trim()
}

/// Returns the text of the first balanced JSON object or array in `content`.
/// Brackets inside string literals do not count.
pub fn first_json_value_span(content: &str) -> Result<&str, &'static str> {
    let content = content.trim();
    let Some((start, root)) = content.char_indices().find_map(|(index, character)| {
        matches!(character, '{' | '[').then_some((index, character))
    }) else {
        return Err("response does not contain a JSON object or array");
    };
    let close = match root {
        '{' => '}',
        '[' => ']',
        _ => unreachable!(),
    };
    let mut stack = vec![close];
    let mut in_string = false;
    let mut escaped = false;

    for (index, character) in content[start..].char_indices().skip(1) {
        let absolute_index = start + index;
        if in_string {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_string = false;
            }
            continue;
        }

        match character {
            '"' => in_string = true,
            '{' => stack.push('}'),
            '[' => stack.push(']'),
            '}' | ']' => {
                if stack.pop() != Some(character) {
                    return Err("JSON brackets are not balanced");
                }
                if stack.is_empty() {
                    let end = absolute_index + character.len_utf8();
                    return Ok(&content[start..end]);
                }
            }
            _ => {}
        }
    }

    Err("JSON value is incomplete")
}

/// Most lenient parse: the whole text, else the first object or array (whichever
/// comes first), repairing trailing commas, and finally salvaging a reply that
/// was truncated mid-JSON. Accepts arrays as well as objects.
pub fn parse_first_json_lenient(content: &str) -> Result<Value, serde_json::Error> {
    let trimmed = content.trim();
    if let Ok(value) = parse_json_value_lenient(trimmed) {
        return Ok(value);
    }
    let object_start = trimmed.find('{');
    let array_start = trimmed.find('[');
    let extracted = match (object_start, array_start) {
        (Some(object_start), Some(array_start)) if array_start < object_start => {
            parse_json_container_at(trimmed, array_start, '[', ']')
        }
        (Some(object_start), _) => parse_json_container_at(trimmed, object_start, '{', '}'),
        (None, Some(array_start)) => parse_json_container_at(trimmed, array_start, '[', ']'),
        (None, None) => None,
    };
    if let Some(Ok(value)) = extracted {
        return Ok(value);
    }
    // Final fallback: the response was very likely truncated mid-JSON (the model hit
    // its output token cap). Salvage every complete leading element by cutting at the
    // last closed container and re-closing the still-open ones, so we keep the good
    // segments instead of discarding the whole response into a transcript fallback.
    if let Some(salvaged) = salvage_truncated_json(trimmed) {
        if let Ok(value) = parse_json_value_lenient(&salvaged) {
            return Ok(value);
        }
    }
    parse_json_value_lenient(trimmed)
}

/// Repair a JSON payload that was cut off mid-value (e.g. an LLM response truncated
/// at its token limit). Walks the text tracking string/bracket state, finds the last
/// position where a nested value finished cleanly while still inside an outer
/// container, drops the dangling partial element, and appends the closers for the
/// containers that were still open there. Returns None when nothing complete can be
/// salvaged (e.g. truncated before the first element finished).
fn salvage_truncated_json(input: &str) -> Option<String> {
    let mut stack: Vec<char> = Vec::new();
    let mut in_string = false;
    let mut escaped = false;
    // (byte index just after a clean close, snapshot of still-open containers)
    let mut best_cut: Option<(usize, Vec<char>)> = None;
    for (idx, ch) in input.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' | '[' => stack.push(ch),
            '}' | ']' => {
                stack.pop();
                // A nested value just closed and we are still inside an outer
                // container -> safe boundary that keeps all complete elements so far.
                if !stack.is_empty() {
                    best_cut = Some((idx + ch.len_utf8(), stack.clone()));
                }
            }
            _ => {}
        }
    }
    // Balanced and not mid-string -> not truncated; nothing for us to salvage.
    if stack.is_empty() && !in_string {
        return None;
    }
    let (cut_idx, remaining) = best_cut?;
    let mut out = input[..cut_idx].to_string();
    for opener in remaining.iter().rev() {
        out.push(match opener {
            '{' => '}',
            '[' => ']',
            _ => return None,
        });
    }
    Some(out)
}

fn parse_json_container_at(
    trimmed: &str,
    start: usize,
    open: char,
    close: char,
) -> Option<Result<Value, serde_json::Error>> {
    let mut depth = 0_i32;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, ch) in trimmed[start..].char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        if ch == '"' {
            in_string = true;
            continue;
        }
        if ch == open {
            depth += 1;
        } else if ch == close {
            depth -= 1;
            if depth == 0 {
                let end = start + offset + ch.len_utf8();
                return Some(parse_json_value_lenient(&trimmed[start..end]));
            }
        }
    }
    None
}

fn parse_json_value_lenient(input: &str) -> Result<Value, serde_json::Error> {
    match serde_json::from_str::<Value>(input) {
        Ok(value) => Ok(value),
        Err(strict_error) => {
            let repaired = strip_json_trailing_commas(input);
            if repaired == input {
                return Err(strict_error);
            }
            serde_json::from_str::<Value>(&repaired)
        }
    }
}

fn strip_json_trailing_commas(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_string = false;
    let mut escaped = false;
    for ch in input.chars() {
        if in_string {
            out.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }

        match ch {
            '"' => {
                in_string = true;
                out.push(ch);
            }
            '}' | ']' => {
                while out.chars().last().is_some_and(|last| last.is_whitespace()) {
                    out.pop();
                }
                if out.ends_with(',') {
                    out.pop();
                }
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
    out
}

/// The JSON object in a plain-text answer: fenced, prefixed or bare.
pub fn object_in_text(text: &str) -> Result<Value, String> {
    let trimmed = strip_code_fence(text.trim()).trim();
    if let Ok(value) = serde_json::from_str(trimmed) {
        return Ok(value);
    }
    if let Some(value) = object_from_first_brace(trimmed) {
        return Ok(value);
    }
    let start = trimmed
        .find('{')
        .ok_or_else(|| "the text answer did not contain a JSON object".to_string())?;
    let end = trimmed
        .rfind('}')
        .ok_or_else(|| "the text answer held an incomplete JSON object".to_string())?;
    if end < start {
        return Err("the text answer held an incomplete JSON object".to_string());
    }
    serde_json::from_str(&trimmed[start..=end])
        .map_err(|error| format!("decode the text answer's JSON failed: {error}"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_text_answer_gives_up_its_json_object() {
        assert_eq!(
            super::object_in_text(
                "好的：
```json
{\"a\": 1}
```"
            )
            .unwrap(),
            serde_json::json!({ "a": 1 })
        );
        assert_eq!(
            super::object_in_text(
                "```json
{\"a\": 2}
```"
            )
            .unwrap(),
            serde_json::json!({ "a": 2 })
        );
        assert!(super::object_in_text("没有对象").is_err());
    }

    use super::*;

    #[test]
    fn first_json_object_reads_fenced_and_prose_wrapped_objects() {
        for text in [
            "{\"a\":1}",
            "```json
    {\"a\":1}
    ```",
            "```JSON
    {\"a\":1}
    ```",
            "```
    {\"a\":1}
    ```",
            "以下是结果：{\"a\":1} 以上。",
            "{\"a\":1}
    {\"b\":2}",
        ] {
            assert_eq!(
                first_json_object(text),
                Some(serde_json::json!({"a": 1})),
                "{text}"
            );
        }
    }

    #[test]
    fn first_json_object_rejects_missing_incomplete_or_non_object_json() {
        assert_eq!(first_json_object("没有结构化结果"), None);
        assert_eq!(first_json_object("{\"a\":"), None);
        assert_eq!(first_json_object("[1,2]"), None);
    }

    #[test]
    fn object_from_first_brace_respects_braces_inside_strings() {
        let value = object_from_first_brace("说明 {\"text\":\"含 } 字符\"} 尾巴 }").unwrap();
        assert_eq!(value["text"], "含 } 字符");
    }

    #[test]
    fn strip_code_fence_only_trims_unfenced_text() {
        assert_eq!(strip_code_fence("  {\"a\":1} "), "{\"a\":1}");
        assert_eq!(
            strip_code_fence(
                "```json
    [1]
    ```"
            ),
            "[1]"
        );
        assert_eq!(
            strip_code_fence(
                "```
    {}"
            ),
            "{}"
        );
    }
    #[test]
    fn extracts_json_value_with_nested_strings_and_trailing_json() {
        let json = first_json_value_span(
            r#"说明：{"groups":[{"start_shot_index":1,"end_shot_index":2,"semantic_summary":"包含 } 和 [ 字符","confidence":0.8}]} {"ignored":true}"#,
        )
        .unwrap();

        assert_eq!(
            json,
            r#"{"groups":[{"start_shot_index":1,"end_shot_index":2,"semantic_summary":"包含 } 和 [ 字符","confidence":0.8}]}"#
        );
    }
    #[test]
    fn parse_first_json_object_ignores_extra_text() {
        let value = parse_first_json_lenient(r#"{"segments":[{"index":1}]} trailing"#).unwrap();
        assert_eq!(value["segments"][0]["index"].as_i64(), Some(1));
    }
    #[test]
    fn parse_first_json_object_repairs_trailing_commas() {
        let value = parse_first_json_lenient(
            r#"
            ```json
            {
              "semantic_segments": [
                {"index": 1, "start_s": 0, "end_s": 5,},
                {"index": 2, "start_s": 5, "end_s": 9,},
              ],
            }
            ```
            "#,
        )
        .unwrap();
        assert_eq!(value["semantic_segments"][0]["index"].as_i64(), Some(1));
        assert_eq!(value["semantic_segments"][1]["end_s"].as_i64(), Some(9));
    }
    #[test]
    fn parse_first_json_object_salvages_truncated_segments() {
        // Simulates the live failure: the model hit its output cap and the JSON was
        // cut off mid-element. The complete leading segments must be salvaged instead
        // of the whole response being discarded into a transcript fallback.
        let truncated = r#"{
      "segments": [
    {"index": 1, "start_s": 0.0, "end_s": 3.4, "title": "北方四岛地图", "main_entity": "北方四岛"},
    {"index": 2, "start_s": 3.4, "end_s": 8.0, "title": "斯大林会谈", "main_entity": "斯大林"},
    {"index": 3, "start_s": 8.0, "end_s": 12.0, "title": "航母战斗"#;
        let value = parse_first_json_lenient(truncated).unwrap();
        let segments = value["segments"].as_array().expect("segments array");
        assert_eq!(segments.len(), 2, "only the two complete segments survive");
        assert_eq!(segments[0]["main_entity"].as_str(), Some("北方四岛"));
        assert_eq!(segments[1]["main_entity"].as_str(), Some("斯大林"));
    }
    #[test]
    fn parse_first_json_object_extracts_markdown_array() {
        let value = parse_first_json_lenient(
            r#"
            ```json
            [
              {"index": 1, "start_s": 0, "end_s": 5},
              {"index": 2, "start_s": 5, "end_s": 10}
            ]
            ```
            "#,
        )
        .unwrap();

        assert_eq!(value.as_array().map(Vec::len), Some(2));
        assert_eq!(value[1]["end_s"].as_i64(), Some(10));
    }
    #[test]
    fn strip_json_trailing_commas_keeps_commas_inside_strings() {
        let value = parse_first_json_lenient(
            r#"{"segments":[{"index":1,"script_text":"保留逗号, 和 }, 字符",}],}"#,
        )
        .unwrap();
        assert_eq!(
            value["segments"][0]["script_text"].as_str(),
            Some("保留逗号, 和 }, 字符")
        );
    }
}
