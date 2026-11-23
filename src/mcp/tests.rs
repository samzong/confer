use super::capabilities;
use serde_json::Value;

fn contains_identifier(haystack: &str, ident: &str) -> bool {
    haystack
        .split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_')
        .any(|token| token == ident)
}

fn extract_signature_block(skill: &str) -> &str {
    let heading = "### Tool Signatures and Lazy MCP Calls";
    let after_heading = skill
        .split_once(heading)
        .expect("SKILL.md must contain Tool Signatures heading")
        .1;
    let after_open = after_heading
        .split_once("```text\n")
        .expect("Tool Signatures section must contain a ```text block")
        .1;
    after_open
        .split_once("\n```")
        .expect("signature code block must close")
        .0
}

#[test]
fn capabilities_expose_the_eight_room_tools_and_sync_with_bundled_skill() {
    let skill = include_str!("../../skills/confer/SKILL.md");
    let sig_block = extract_signature_block(skill);
    let sig_lines: Vec<&str> = sig_block.lines().collect();
    let seat_spec_line = sig_lines
        .iter()
        .copied()
        .find(|line| line.starts_with("SeatSpec ="))
        .expect("signature block must define SeatSpec");
    let tools = capabilities().unwrap().tools;
    let mut names = tools
        .iter()
        .map(|tool| tool.name.to_string())
        .collect::<Vec<_>>();
    names.sort();

    assert_eq!(
        names,
        [
            "add_seat",
            "cancel_delivery",
            "clear_uncertain_delivery",
            "create_room",
            "list_rooms",
            "retire_seat",
            "send_message",
            "wait_output",
        ]
    );

    for tool in &tools {
        let tool_name = tool.name.as_ref();
        let prefix = format!("{tool_name}(");
        let tool_line = sig_lines
            .iter()
            .copied()
            .find(|line| line.starts_with(&prefix))
            .unwrap_or_else(|| panic!("signature block is missing line for {tool_name}"));
        let Value::Object(top_props) = &tool.input_schema["properties"] else {
            panic!("tool {tool_name} has no top-level properties");
        };
        for prop in top_props.keys() {
            assert!(
                contains_identifier(tool_line, prop),
                "signature line for {tool_name} is missing property {prop}: {tool_line}"
            );
        }
        for defs_key in ["$defs", "definitions"] {
            if let Some(Value::Object(defs)) = tool.input_schema.get(defs_key) {
                for (def_name, def) in defs {
                    if def_name.contains("SeatSpec")
                        && let Value::Object(def_map) = def
                        && let Some(Value::Object(def_props)) = def_map.get("properties")
                    {
                        for prop in def_props.keys() {
                            assert!(
                                contains_identifier(seat_spec_line, prop),
                                "SeatSpec signature line is missing property {prop}: {seat_spec_line}"
                            );
                        }
                    }
                }
            }
        }
    }

    let by_name = |target: &str| {
        tools
            .iter()
            .find(|tool| tool.name == target)
            .unwrap()
            .input_schema
            .clone()
    };
    for (tool_name, array_prop) in [
        ("create_room", "seats"),
        ("send_message", "recipients"),
        ("wait_output", "delivery_ids"),
        ("cancel_delivery", "delivery_ids"),
    ] {
        let schema = by_name(tool_name);
        let prop_schema = &schema["properties"][array_prop];
        assert_eq!(
            prop_schema["type"], "array",
            "{tool_name}.{array_prop} schema must stay native array: {prop_schema}"
        );
        assert!(
            prop_schema.get("anyOf").is_none() && prop_schema.get("oneOf").is_none(),
            "{tool_name}.{array_prop} schema must not expose union types: {prop_schema}"
        );
    }
    let add_seat_schema = by_name("add_seat");
    let seat_prop = &add_seat_schema["properties"]["seat"];
    assert!(
        seat_prop.get("$ref").is_some() || seat_prop.get("type") == Some(&Value::from("object")),
        "add_seat.seat schema must stay native object: {seat_prop}"
    );
    assert!(
        seat_prop.get("anyOf").is_none() && seat_prop.get("oneOf").is_none(),
        "add_seat.seat schema must not expose union types: {seat_prop}"
    );
}
