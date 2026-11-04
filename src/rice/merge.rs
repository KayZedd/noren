//! Edits inside other programs' config files that keep everything else byte-for-byte:
//! comments, ordering and formatting in JSONC (VS Code, Windows Terminal), and the user's own
//! rules around a marked block in CSS or `user.js`.

use jsonc_parser::ParseOptions;
use jsonc_parser::cst::{CstInputValue, CstObject, CstRootNode};
use serde_json::Value;

fn input(v: &Value) -> CstInputValue {
    match v {
        Value::Null => CstInputValue::Null,
        Value::Bool(b) => CstInputValue::Bool(*b),
        Value::Number(n) => CstInputValue::Number(n.to_string()),
        Value::String(s) => CstInputValue::String(s.clone()),
        Value::Array(a) => CstInputValue::Array(a.iter().map(input).collect()),
        Value::Object(o) => CstInputValue::Object(o.iter().map(|(k, v)| (k.clone(), input(v))).collect()),
    }
}

fn parse(text: &str) -> Result<CstRootNode, String> {
    let text = if text.trim().is_empty() { "{}\n" } else { text };
    CstRootNode::parse(text, &ParseOptions::default()).map_err(|e| e.to_string())
}

fn object_at(root: &CstRootNode, path: &[&str]) -> CstObject {
    path.iter().fold(root.object_value_or_set(), |obj, key| {
        obj.object_value_or_set(key)
    })
}

/// Edits to apply to one JSONC document.
#[derive(Clone, Debug, PartialEq)]
pub enum JsonEdit {
    /// Sets `path` (object keys from the root; the last is the property) to `value`, creating
    /// parent objects.
    Set { path: Vec<String>, value: Value },
    /// In the array at `path`, replaces the object whose `key` equals `value[key]`, or appends.
    Upsert {
        path: Vec<String>,
        key: String,
        value: Value,
    },
}

/// Applies `edits` to `text` (empty means `{}`), preserving everything they do not touch.
pub fn merge_jsonc(text: &str, edits: &[JsonEdit]) -> Result<String, String> {
    let root = parse(text)?;
    for edit in edits {
        match edit {
            JsonEdit::Set { path, value } => {
                let (last, parents) = path.split_last().ok_or("empty JSON path")?;
                let parents: Vec<&str> = parents.iter().map(String::as_str).collect();
                let obj = object_at(&root, &parents);
                match obj.get(last) {
                    Some(prop) => prop.set_value(input(value)),
                    None => {
                        obj.append(last, input(value));
                    }
                }
            }
            JsonEdit::Upsert { path, key, value } => {
                let (last, parents) = path.split_last().ok_or("empty JSON path")?;
                let parents: Vec<&str> = parents.iter().map(String::as_str).collect();
                let array = object_at(&root, &parents).array_value_or_set(last);
                let wanted = value
                    .get(key)
                    .ok_or_else(|| format!("upsert value has no {key:?}"))?;
                let existing = array
                    .elements()
                    .into_iter()
                    .filter_map(|el| el.as_object())
                    .find(|o| {
                        o.get(key)
                            .and_then(|p| p.value())
                            .and_then(|v| v.to_serde_value())
                            .is_some_and(|v| &v == wanted)
                    });
                match existing {
                    Some(obj) => {
                        obj.replace_with(input(value));
                    }
                    None => {
                        array.append(input(value));
                    }
                }
            }
        }
    }
    Ok(root.to_string())
}

/// Replaces the text between the `begin` and `end` marker lines with `body`, or appends a new
/// marked block. Text outside the markers is untouched.
pub fn replace_block(text: &str, begin: &str, end: &str, body: &str) -> String {
    let mut block = format!("{begin}\n{body}");
    if !block.ends_with('\n') {
        block.push('\n');
    }
    block += end;
    if let Some(start) = text.find(begin)
        && let Some(stop) = text[start..].find(end).map(|i| start + i + end.len())
    {
        return format!("{}{block}{}", &text[..start], &text[stop..]);
    }
    let sep = if text.is_empty() || text.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    format!("{text}{sep}{block}\n")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn set(path: &[&str], value: Value) -> JsonEdit {
        JsonEdit::Set {
            path: path.iter().map(|s| s.to_string()).collect(),
            value,
        }
    }

    #[test]
    fn set_keeps_comments_and_unrelated_keys() {
        let text = "{\n  // my font\n  \"editor.fontSize\": 14,\n  \"workbench.colorCustomizations\": {\n    \"foo\": \"#000000\" // keep\n  }\n}\n";
        let out = merge_jsonc(
            text,
            &[
                set(
                    &["workbench.colorCustomizations", "editor.background"],
                    json!("#111111"),
                ),
                set(&["workbench.colorCustomizations", "foo"], json!("#222222")),
            ],
        )
        .unwrap();
        assert!(out.contains("// my font") && out.contains("// keep"), "{out}");
        assert!(out.contains("\"editor.fontSize\": 14"), "{out}");
        assert!(out.contains("\"foo\": \"#222222\""), "{out}");
        assert!(out.contains("\"editor.background\": \"#111111\""), "{out}");
        let parsed: Value = jsonc_parser::parse_to_serde_value(&out, &Default::default()).unwrap();
        assert_eq!(
            parsed["workbench.colorCustomizations"]["editor.background"],
            "#111111"
        );
    }

    #[test]
    fn set_on_empty_or_missing_file_creates_the_document() {
        let out = merge_jsonc("", &[set(&["a", "b"], json!(1))]).unwrap();
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed, json!({"a": {"b": 1}}));
    }

    #[test]
    fn upsert_replaces_by_name_and_appends_otherwise() {
        let text = r#"{"schemes": [{"name": "Campbell", "x": 1}, {"name": "noren", "x": 1}]}"#;
        let edit = |name: &str| JsonEdit::Upsert {
            path: vec!["schemes".into()],
            key: "name".into(),
            value: json!({"name": name, "x": 2}),
        };
        let out = merge_jsonc(text, &[edit("noren"), edit("other")]).unwrap();
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            parsed["schemes"],
            json!([{"name": "Campbell", "x": 1}, {"name": "noren", "x": 2}, {"name": "other", "x": 2}])
        );
    }

    #[test]
    fn invalid_json_is_an_error_not_an_overwrite() {
        assert!(merge_jsonc("{ broken", &[set(&["a"], json!(1))]).is_err());
    }

    #[test]
    fn marked_block_is_replaced_in_place_or_appended() {
        let (b, e) = ("/* noren:begin */", "/* noren:end */");
        let first = replace_block("mine {}", b, e, "a {}");
        assert_eq!(first, "mine {}\n/* noren:begin */\na {}\n/* noren:end */\n");
        let second = replace_block(&(first.clone() + "after {}\n"), b, e, "b {}\n");
        assert_eq!(
            second,
            "mine {}\n/* noren:begin */\nb {}\n/* noren:end */\nafter {}\n"
        );
        assert_eq!(
            replace_block("", b, e, "x"),
            "/* noren:begin */\nx\n/* noren:end */\n"
        );
    }
}
