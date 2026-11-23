//! Adding, changing and removing the sources of the knowledge map, shared by the CLI
//! (`kuroko sources add|update|remove`) and the MCP tools (`source_add`, `source_update`,
//! `source_remove`). The GUI builds the same sources in its form (Knowledge › Sources) and saves
//! the whole map; the host checks the map on every save, whoever sends it.
//!
//! A new source starts disabled, without audiences and without external processing; who may
//! read it is changed only through the authorization (`source_authorize`, `kuroko sources
//! audience|enable`), never here.

use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

/// The fields that say who may read a source.
const AUTHORIZATION: [&str; 4] = [
    "enabled",
    "external_processing",
    "allowed_conversations",
    "allowed_senders",
];
/// What says which kind of source it is; a change keeps it.
const IDENTITY: [&str; 2] = ["id", "kind"];

/// Adds `resource` to `map`'s sources, disabled and without audiences: authorization fields it
/// leaves out start that way, and any it sets otherwise is refused. Returns its ID.
pub(crate) fn add(map: &mut Value, mut resource: Value) -> Result<String> {
    let fields = resource
        .as_object_mut()
        .context("A source is a JSON object.")?;
    let id = fields
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .context("A source needs an id.")?
        .to_owned();
    for (key, initial) in [
        ("enabled", json!(false)),
        ("external_processing", json!(false)),
        ("allowed_conversations", json!([])),
        ("allowed_senders", json!([])),
    ] {
        let value = fields.entry(key).or_insert_with(|| initial.clone());
        ensure!(
            *value == initial,
            "Add sources disabled, without audiences or external processing; authorize them afterward with source_authorize."
        );
    }
    fields.entry("topics").or_insert_with(|| json!([]));
    fields.entry("description").or_insert_with(|| json!(id));
    let sources = sources(map)?;
    ensure!(
        !sources.iter().any(|r| r["id"] == id.as_str()),
        "A source with that ID already exists."
    );
    check(&resource)?;
    sources.push(resource);
    Ok(id)
}

/// Changes what source `id` reads and how it is described: `change` holds its fields to
/// replace (`description`, `topics`, `urls`…), and `tool` the tool's settings to replace,
/// keeping its type. Its ID, kind and authorization never change here.
pub(crate) fn update(map: &mut Value, id: &str, change: &Value) -> Result<()> {
    let change = change
        .as_object()
        .context("A change is a JSON object of the fields to replace.")?;
    ensure!(!change.is_empty(), "Say which fields to change.");
    let source = sources(map)?
        .iter_mut()
        .find(|r| r["id"] == id)
        .context("Unknown source.")?;
    let mut changed = source.clone();
    for (key, value) in change {
        ensure!(
            !AUTHORIZATION.contains(&key.as_str()),
            "Change who may read a source with source_authorize."
        );
        ensure!(
            !IDENTITY.contains(&key.as_str()),
            "A source's ID and kind cannot change; remove it and add another."
        );
        if key == "tool" {
            let tool = changed["tool"]
                .as_object_mut()
                .context("This source is not a tool.")?;
            for (setting, value) in value
                .as_object()
                .context("tool is an object of the settings to replace.")?
            {
                ensure!(
                    setting != "type" || tool.get("type") == Some(value),
                    "A tool's type cannot change; remove the source and add another."
                );
                tool.insert(setting.clone(), value.clone());
            }
        } else {
            changed[key] = value.clone();
        }
    }
    check(&changed)?;
    *source = changed;
    Ok(())
}

/// Removes source `id`, and from the contexts, cases and organization that list it. A source a
/// question reads is not removed: change or remove that question first.
pub(crate) fn remove(map: &mut Value, id: &str) -> Result<()> {
    let used = map["questions"].as_array().into_iter().flatten().any(|q| {
        q["sources"]
            .as_array()
            .is_some_and(|s| s.iter().any(|s| s == id))
    });
    ensure!(
        !used,
        "A question reads this source. Change or remove that question first."
    );
    let sources = sources(map)?;
    let before = sources.len();
    sources.retain(|r| r["id"] != id);
    ensure!(sources.len() < before, "Unknown source.");
    let forget = |list: &mut Value| {
        if let Some(ids) = list.as_array_mut() {
            ids.retain(|s| s != id);
        }
    };
    // The retired cases of a map without questions list sources too.
    for list in ["contexts", "cases"] {
        for item in map[list].as_array_mut().into_iter().flatten() {
            forget(&mut item["sources"]);
        }
    }
    if map["organization"].is_object() {
        forget(&mut map["organization"]["sources"]);
    }
    Ok(())
}

fn sources(map: &mut Value) -> Result<&mut Vec<Value>> {
    if map["resources"].is_null() {
        map["resources"] = json!([]);
    }
    map["resources"]
        .as_array_mut()
        .context("The map's sources are not a list.")
}
/// The source as the map reads it, with a valid tool: the whole map is checked again when it
/// is saved.
fn check(resource: &Value) -> Result<()> {
    let parsed: crate::knowledge::Resource =
        serde_json::from_value(resource.clone()).context("That is not a valid source.")?;
    if let crate::knowledge::Access::Tool { tool } = &parsed.access {
        tool.validate()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map() -> Value {
        json!({"resources":[{"id":"notes","description":"Notes","topics":[],"enabled":true,"external_processing":true,"allowed_conversations":["*"],"kind":"notes"}],
            "contexts":[{"id":"billing","sources":["notes","agent-sessions"]}],
            "cases":[{"id":"question","sources":["agent-sessions"]}],
            "organization":{"sources":["agent-sessions"]},
            "questions":[{"id":"q","text":"?","sources":["notes"]}]})
    }

    #[test]
    fn a_new_source_starts_disabled_and_without_audiences() {
        let mut map = map();
        let id = add(
            &mut map,
            json!({"id":"agent-sessions","kind":"tool","tool":{"type":"agent_sessions","sessions":10,"exclude_projects":["hobby-game"]}}),
        )
        .unwrap();
        assert_eq!(id, "agent-sessions");
        let added = &map["resources"][1];
        assert_eq!(added["enabled"], false);
        assert_eq!(added["external_processing"], false);
        assert_eq!(added["allowed_conversations"], json!([]));
        assert_eq!(added["allowed_senders"], json!([]));
        for granted in [
            json!({"id":"x","kind":"notes","enabled":true}),
            json!({"id":"x","kind":"notes","external_processing":true}),
            json!({"id":"x","kind":"notes","allowed_conversations":["*"]}),
            json!({"id":"x","kind":"notes","allowed_senders":["someone"]}),
        ] {
            assert!(add(&mut map, granted).is_err());
        }
        // An existing ID and an invalid tool are refused.
        assert!(add(&mut map, json!({"id":"notes","kind":"notes"})).is_err());
        assert!(
            add(
                &mut map,
                json!({"id":"y","kind":"tool","tool":{"type":"agent_sessions","sessions":90}})
            )
            .is_err()
        );
        assert_eq!(map["resources"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn a_change_keeps_the_kind_and_never_touches_the_authorization() {
        let mut map = map();
        add(
            &mut map,
            json!({"id":"agent-sessions","kind":"tool","tool":{"type":"agent_sessions","sessions":10}}),
        )
        .unwrap();
        update(
            &mut map,
            "agent-sessions",
            &json!({"description":"My agents","tool":{"exclude_projects":["hobby-game","drafts/sketchbook"]}}),
        )
        .unwrap();
        let source = &map["resources"][1];
        assert_eq!(source["description"], "My agents");
        assert_eq!(source["tool"]["sessions"], 10);
        assert_eq!(
            source["tool"]["exclude_projects"],
            json!(["hobby-game", "drafts/sketchbook"])
        );
        for refused in [
            json!({"enabled":true}),
            json!({"allowed_conversations":["*"]}),
            json!({"kind":"notes"}),
            json!({"id":"other"}),
            json!({"tool":{"type":"activity_record"}}),
            json!({"tool":{"exclude_projects":["../etc"]}}),
            json!({}),
        ] {
            assert!(
                update(&mut map, "agent-sessions", &refused).is_err(),
                "{refused}"
            );
        }
        assert!(update(&mut map, "missing", &json!({"description":"x"})).is_err());
        assert_eq!(map["resources"][1]["enabled"], false);
    }

    #[test]
    fn a_removed_source_leaves_its_contexts_unless_a_question_reads_it() {
        let mut map = map();
        add(
            &mut map,
            json!({"id":"agent-sessions","kind":"tool","tool":{"type":"agent_sessions"}}),
        )
        .unwrap();
        assert!(remove(&mut map, "notes").is_err());
        remove(&mut map, "agent-sessions").unwrap();
        assert_eq!(map["resources"].as_array().unwrap().len(), 1);
        assert_eq!(map["contexts"][0]["sources"], json!(["notes"]));
        assert_eq!(map["organization"]["sources"], json!([]));
        assert_eq!(map["cases"][0]["sources"], json!([]));
        assert!(remove(&mut map, "agent-sessions").is_err());
    }
}
