//! Explicit, human-supplied mission plans; no model may widen these write sets at dispatch.

use std::collections::BTreeMap;
use std::path::Path;

use kaisa_domain::{Plan, PlannedTask, RepoPath, Role, TaskId, WriteSet};
use serde_json::Value;

pub(crate) fn load(path: &Path) -> Result<(Plan, BTreeMap<TaskId, String>), String> {
    let text = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
    let value: Value = serde_json::from_str(&text).map_err(|error| error.to_string())?;
    let tasks = value["tasks"]
        .as_array()
        .ok_or("plan needs a tasks array")?;
    let mut planned = Vec::new();
    let mut instructions = BTreeMap::new();
    for task in tasks {
        let id = TaskId::try_new(task["id"].as_str().ok_or("task needs an id")?)
            .map_err(|error| error.to_string())?;
        let instruction = task["instruction"]
            .as_str()
            .ok_or("task needs an instruction")?;
        let role = match task.get("role").and_then(Value::as_str) {
            None | Some("developer") => Role::Developer,
            Some("documentation") => Role::Documentation,
            Some("tester") => Role::Tester,
            _ => return Err("mission task role must be developer, tester or documentation".into()),
        };
        let write_set = WriteSet::new(
            task["write_set"]
                .as_array()
                .ok_or("task needs a write_set array")?
                .iter()
                .map(|path| {
                    RepoPath::try_new(path.as_str().ok_or("write_set entries must be strings")?)
                        .map_err(|error| error.to_string())
                })
                .collect::<Result<_, _>>()?,
        );
        let depends_on = match task.get("depends_on") {
            None => Vec::new(),
            Some(value) => value
                .as_array()
                .ok_or("depends_on must be an array")?
                .iter()
                .map(|value| {
                    TaskId::try_new(value.as_str().ok_or("dependency must be a task id")?)
                        .map_err(|error| error.to_string())
                })
                .collect::<Result<_, _>>()?,
        };
        instructions.insert(id.clone(), instruction.to_owned());
        planned.push(PlannedTask {
            id,
            role,
            write_set,
            depends_on,
        });
    }
    if planned.is_empty() {
        return Err("a mission needs at least one task".into());
    }
    Plan::new(planned)
        .map(|plan| (plan, instructions))
        .map_err(|error| error.to_string())
}
