//! A provider-native workflow's opening (`resolveWorkflowOpening`, `deriveWorkflowGroups`,
//! `workflowOpeningHeadline` in `packages/client-runtime/src/state/subagentRuntime.ts`).

use serde_json::Value;

/// A built opening for the timeline: the block, and its members as subagent cards by phase.
#[derive(Clone, Debug, PartialEq)]
pub struct WorkflowRow {
    pub opening: Opening,
    pub phases: Vec<(String, usize, Vec<std::rc::Rc<crate::timeline::SubagentCard>>)>,
    pub unphased: Vec<std::rc::Rc<crate::timeline::SubagentCard>>,
    /// [`Opening::headline`], built with the row rather than per frame.
    pub headline: (String, Option<String>, Option<String>),
    /// [`script_markdown`] of the opening's script.
    pub script_markdown: gpui_kit::SharedString,
    /// The script is long enough to fold.
    pub foldable: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Opening {
    pub message_id: String,
    pub script: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub driver: Option<String>,
    pub phases: Vec<Phase>,
    pub unphased: Vec<Member>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Phase {
    pub index: i64,
    pub title: String,
    pub members: Vec<Member>,
    pub settled: usize,
    /// A member still works (or idles, resumable): the phase has not finished.
    pub running: bool,
}

impl Phase {
    /// `phaseStatusLabel`'s failure: a member of a finished phase failed.
    pub fn failed(&self) -> bool {
        !self.running && self.members.iter().any(|m| m.status == "failed")
    }

    /// `phaseElapsed` through `deriveSubagentElapsedMs`: from the first member's start, until
    /// now while running, else until the last member's end.
    pub fn elapsed_ms(&self, now_ms: i64) -> Option<i64> {
        let started = self.members.iter().filter_map(|m| m.started_ms).min()?;
        let end = if self.running { now_ms } else { self.members.iter().filter_map(|m| m.completed_ms).max()? };
        Some(end.saturating_sub(started).max(0))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Member {
    pub id: String,
    pub title: String,
    pub status: String,
    pub detail: Option<String>,
    pub started_ms: Option<i64>,
    pub completed_ms: Option<i64>,
    pub child_thread_id: Option<String>,
}

impl Member {
    /// `deriveSubagentElapsedMs`: until now while it works, else until it ended.
    pub fn elapsed_ms(&self, now_ms: i64) -> Option<i64> {
        let started = self.started_ms?;
        let end = if is_active(&self.status) { now_ms } else { self.completed_ms? };
        Some(end.saturating_sub(started).max(0))
    }
}

/// `isOrchestrationV2WorkActive`.
pub fn is_active(status: &str) -> bool {
    matches!(status, "pending" | "running" | "waiting")
}

pub fn is_terminal(status: &str) -> bool {
    matches!(status, "completed" | "failed" | "cancelled" | "interrupted")
}

/// `isWorkflowScriptMessage`: the server's mark, else a script opening with `export const meta`.
pub fn is_workflow_script(text: &str, mark: Option<bool>) -> bool {
    if let Some(mark) = mark {
        return mark;
    }
    let mut rest = text.trim_start();
    loop {
        if let Some(line) = rest.strip_prefix("//") {
            rest = line.split_once('\n').map_or("", |(_, after)| after).trim_start();
        } else if let Some(block) = rest.strip_prefix("/*") {
            let Some((_, after)) = block.split_once("*/") else { return false };
            rest = after.trim_start();
        } else {
            break;
        }
    }
    let Some(rest) = rest.strip_prefix("export") else { return false };
    let rest = rest.trim_start();
    let Some(rest) = rest.strip_prefix("const") else { return false };
    let rest = rest.trim_start();
    rest.strip_prefix("meta").is_some_and(|after| !after.starts_with(|c: char| c.is_alphanumeric() || c == '_'))
}

/// A workflow coordinator, as its subagent record names it.
#[derive(Clone, Copy)]
pub struct Coordinator<'a> {
    pub id: &'a str,
    pub status: &'a str,
    pub completed_ms: Option<i64>,
    /// `OrchestrationV2SubagentWorkflow`.
    pub workflow: &'a Value,
}

/// The members of a coordinator's workflow (`workflowMembersToRuntime`).
fn members(coordinator: Coordinator<'_>) -> Vec<(Member, Option<i64>, Option<String>, i64)> {
    let coordinator_status = coordinator.status;
    let coordinator_done = coordinator.completed_ms;
    let Some(agents) = coordinator.workflow["agents"].as_array() else { return Vec::new() };
    agents
        .iter()
        .map(|agent| {
            let state = agent["state"].as_str().unwrap_or("queued");
            let status = match state {
                "queued" | "running" if is_terminal(coordinator_status) => coordinator_status.to_owned(),
                "queued" => "pending".to_owned(),
                other => other.to_owned(),
            };
            let started = agent["startedAt"].as_i64();
            let completed = is_terminal(&status)
                .then(|| {
                    agent["completedAt"]
                        .as_i64()
                        .or_else(|| started?.checked_add(agent["durationMs"].as_i64()?))
                        .or(coordinator_done)
                })
                .flatten();
            let result = agent["result"].as_str().map(str::to_owned);
            let prompt = agent["prompt"].as_str().map(str::to_owned);
            // `WorkflowMemberRow`: a settled member shows its error or result, else its prompt.
            let detail = if is_terminal(&status) { result.or(prompt) } else { prompt };
            let detail = detail
                .map(|d| d.replace('`', "").split_whitespace().collect::<Vec<_>>().join(" "))
                .filter(|d| !d.is_empty());
            let index = agent["index"].as_i64().unwrap_or_default();
            let member = Member {
                id: format!("{}:agent:{index}", coordinator.id),
                title: agent["label"].as_str().unwrap_or("Agent").to_owned(),
                status,
                detail,
                started_ms: started,
                completed_ms: completed,
                child_thread_id: agent["childThreadId"].as_str().map(str::to_owned),
            };
            (member, agent["phaseIndex"].as_i64(), agent["phaseTitle"].as_str().map(str::to_owned), index)
        })
        .collect()
}

/// `deriveWorkflowGroups` for one coordinator: its phases with members, by index, and the
/// members no phase holds, each in agent order.
pub fn groups(coordinator: Coordinator<'_>) -> (Vec<Phase>, Vec<Member>) {
    let mut all = members(coordinator);
    all.sort_by_key(|(_, _, _, index)| *index);
    let mut titles: Vec<(i64, String)> = Vec::new();
    for (_, phase, title, _) in &all {
        let Some(phase) = phase else { continue };
        let title = title.clone().unwrap_or_else(|| format!("Phase {}", phase.saturating_add(1)));
        match titles.iter_mut().find(|(i, _)| i == phase) {
            Some(entry) => entry.1 = title,
            None => titles.push((*phase, title)),
        }
    }
    for declared in coordinator.workflow["phases"].as_array().into_iter().flatten() {
        let (Some(index), Some(title)) = (declared["index"].as_i64(), declared["title"].as_str()) else { continue };
        match titles.iter_mut().find(|(i, _)| *i == index) {
            Some(entry) => entry.1 = title.to_owned(),
            None => titles.push((index, title.to_owned())),
        }
    }
    titles.sort_by_key(|(index, _)| *index);
    let mut phases = Vec::new();
    for (index, title) in titles {
        let members: Vec<Member> =
            all.iter().filter(|(_, phase, ..)| *phase == Some(index)).map(|(m, ..)| m.clone()).collect();
        // Both the opening block and the lineage row show only phases with members.
        if members.is_empty() {
            continue;
        }
        let settled = members.iter().filter(|m| is_terminal(&m.status)).count();
        let running = members.iter().any(|m| is_active(&m.status) || m.status == "idle");
        phases.push(Phase { index, title, members, settled, running });
    }
    let unphased = all.into_iter().filter(|(_, phase, ..)| phase.is_none()).map(|(m, ..)| m).collect();
    (phases, unphased)
}

/// `resolveWorkflowOpening` over the parent projection's `subagents`, when loaded.
pub fn opening(thread_id: &str, message_id: &str, script: &str, parent_subagents: Option<&[Value]>) -> Opening {
    let coordinator = parent_subagents
        .and_then(|subagents| subagents.iter().find(|s| s["childThreadId"].as_str() == Some(thread_id)));
    let text = |key: &str| coordinator.and_then(|c| c[key].as_str()).filter(|v| !v.is_empty()).map(str::to_owned);
    let (phases, unphased) = coordinator
        .filter(|c| c.get("workflow").is_some_and(|w| !w.is_null()))
        .map(|c| {
            groups(Coordinator {
                id: c["id"].as_str().unwrap_or_default(),
                status: c["status"].as_str().unwrap_or_default(),
                completed_ms: c["completedAt"].as_str().and_then(crate::timestamp::parse_ms),
                workflow: &c["workflow"],
            })
        })
        .unwrap_or_default();
    Opening {
        message_id: message_id.to_owned(),
        script: script.to_owned(),
        name: coordinator.and_then(|c| c.pointer("/workflow/name")).and_then(Value::as_str).map(str::to_owned),
        description: text("title"),
        driver: text("driver"),
        phases,
        unphased,
    }
}

impl Opening {
    /// `workflowOpeningHeadline`: the title, a description unlike it, and how the agents stand.
    pub fn headline(&self) -> (String, Option<String>, Option<String>) {
        let title = self.name.clone().or_else(|| self.description.clone()).unwrap_or_else(|| "Workflow".into());
        let members: Vec<&Member> = self.phases.iter().flat_map(|p| &p.members).chain(&self.unphased).collect();
        let mut counts = [("working", 0), ("done", 0), ("failed", 0), ("stopped", 0)];
        for member in &members {
            let slot = match member.status.as_str() {
                "completed" => 1,
                "failed" => 2,
                status if is_terminal(status) => 3,
                _ => 0,
            };
            counts[slot].1 += 1;
        }
        let agents = (!members.is_empty()).then(|| {
            let parts: Vec<String> =
                counts.iter().filter(|(_, n)| *n > 0).map(|(key, n)| format!("{n} {key}")).collect();
            format!("{} {} · {}", members.len(), if members.len() == 1 { "agent" } else { "agents" }, parts.join(" · "))
        });
        let description = self.description.clone().filter(|d| *d != title);
        (title, description, agents)
    }
}

/// `scriptMarkdown`: the script in a fence longer than any backtick run inside it.
pub fn script_markdown(script: &str) -> String {
    let mut longest = 2;
    let mut run = 0;
    for c in script.chars() {
        run = if c == '`' { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    let fence = "`".repeat(longest + 1);
    format!("{fence}javascript\n{}\n{fence}", script.trim_end())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn workflow_scripts_open_with_their_meta_export() {
        assert!(is_workflow_script("// plan\n/* x */\nexport const meta = {}", None));
        assert!(!is_workflow_script("export const metadata = 1", None));
        assert!(!is_workflow_script("hello", None));
        assert!(is_workflow_script("hello", Some(true)));
    }

    #[test]
    fn the_opening_reads_its_coordinator_from_the_parent() {
        let parent = vec![json!({
            "id": "s1", "childThreadId": "t", "title": "Review the diff", "status": "running", "driver": "claudeAgent",
            "workflow": {"name": "review", "phases": [{"index": 0, "title": "Find"}],
                "agents": [
                    {"index": 0, "label": "bugs", "state": "completed", "phaseIndex": 0, "result": "2 bugs", "startedAt": 1000, "durationMs": 4000},
                    {"index": 1, "label": "perf", "state": "running", "phaseIndex": 0, "prompt": "Look for  slow paths"},
                    {"index": 2, "label": "verify", "state": "queued", "phaseIndex": 1, "phaseTitle": "Verify"}
                ]}
        })];
        let opening = opening("t", "m", "export const meta = {}", Some(&parent));
        assert_eq!(opening.phases.len(), 2);
        assert_eq!((opening.phases[0].title.as_str(), opening.phases[0].settled), ("Find", 1));
        assert_eq!(opening.phases[0].members[0].completed_ms, Some(5000));
        assert_eq!(opening.phases[0].members[1].detail.as_deref(), Some("Look for slow paths"));
        assert_eq!(
            opening.headline(),
            ("review".into(), Some("Review the diff".into()), Some("3 agents · 2 working · 1 done".into()))
        );
        assert_eq!(script_markdown("a ``` b"), "````javascript\na ``` b\n````");
    }

    #[test]
    fn extreme_server_numbers_do_not_overflow() {
        let parent = vec![json!({
            "id": "s1", "childThreadId": "t", "status": "completed",
            "workflow": {"agents": [
                {"index": 0, "label": "a", "state": "completed", "phaseIndex": i64::MAX, "startedAt": i64::MAX, "durationMs": 1}
            ]}
        })];
        let opening = opening("t", "m", "export const meta = {}", Some(&parent));
        assert_eq!(opening.phases[0].title, format!("Phase {}", i64::MAX));
        assert_eq!(opening.phases[0].members[0].completed_ms, None);
    }
}
