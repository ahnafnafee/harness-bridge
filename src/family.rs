use crate::{
    ir::{Event, EventKind, Role, SessionRef, WriteOpts, WriteOutcome},
    providers::Provider,
};
use serde_json::json;
use std::collections::{HashMap, HashSet, VecDeque};

pub fn migrate(
    source: &dyn Provider,
    destination: &dyn Provider,
    root: SessionRef,
    opts: &WriteOpts,
    include_children: bool,
    max_chars: usize,
    prune: bool,
) -> anyhow::Result<WriteOutcome> {
    let mut queue = VecDeque::from([(root, None::<String>, 0_usize)]);
    let mut seen = HashSet::new();
    let mut mapping = HashMap::new();
    let mut locations = HashMap::new();
    let mut plans = Vec::new();
    while let Some((reference, parent, depth)) = queue.pop_front() {
        anyhow::ensure!(
            seen.insert(reference.id.clone()),
            "duplicate or cyclic child session {:?}",
            reference.id
        );
        anyhow::ensure!(
            seen.len() <= 1024,
            "session family exceeds the 1024-session safety bound"
        );
        if include_children {
            for child in source.children(&reference)? {
                queue.push_back((child, Some(reference.id.clone()), depth + 1));
            }
        }
        let mut session = source.read(&reference).map_err(|error| {
            anyhow::anyhow!("cannot read family member {}: {error:#}", reference.id)
        })?;
        if parent.is_none() {
            if let Some(original_parent) = session.parent_session.take() {
                session.events.push(Event::at(
                    Some(session.created_ms),
                    EventKind::Meta {
                        kind: "source-parent".into(),
                        data: json!({"parent_session":original_parent}),
                    },
                ));
            }
        } else {
            session.parent_session = parent.as_ref().and_then(|id| mapping.get(id).cloned());
            anyhow::ensure!(
                session.parent_session.is_some(),
                "parent has not been preflighted"
            );
            session.events.push(Event::at(
                Some(session.created_ms),
                EventKind::Meta {
                    kind: "migration-parent-depth".into(),
                    data: json!({"depth":depth, "location":parent.as_ref().and_then(|id| locations.get(id))}),
                },
            ));
        }
        let policy = crate::resume::prepare(&mut session, max_chars, prune)
            .map_err(|error| anyhow::anyhow!("family member {}: {error:#}", reference.id))?;
        let options = WriteOpts {
            cwd: opts.cwd.clone(),
            name: if parent.is_none() {
                opts.name.clone()
            } else {
                None
            },
            dry_run: true,
        };
        let mut outcome = destination.write(&session, &options)?;
        outcome.extra["resume_context_policy"] = policy;
        outcome.extra["parent_session"] = json!(session.parent_session);
        mapping.insert(reference.id.clone(), outcome.native_id.clone());
        locations.insert(reference.id.clone(), outcome.location.clone());
        plans.push((reference.id, session, options, outcome));
    }
    if plans.len() > 1 {
        let links:Vec<_> = plans.iter().skip(1).map(|(id,s,_,o)| json!({"source_id":id,"destination_id":o.native_id,"parent_session":s.parent_session})).collect();
        let note = Event::at(Some(plans[0].1.updated_ms),EventKind::Message {role:Role::User,
            text:format!("Imported session family: these are saved historical child sessions, not running agents. Destination parent links use the translated IDs below. Native child display is destination-dependent.\n{}",serde_json::to_string(&links)?),
            source_kind:Some("migration-session-family".into())});
        let (_, root, options, outcome) = &mut plans[0];
        if root.resume_events.is_none() {
            root.resume_events = Some(root.events.clone());
        }
        root.resume_events.as_mut().unwrap().push(note.clone());
        root.events.push(note);
        let mut policy = crate::resume::prepare(root, max_chars, prune)?;
        let retained_chars = policy
            .get("retained_chars")
            .unwrap_or(&policy["input_chars"])
            .clone();
        let previous = &outcome.extra["resume_context_policy"];
        if previous["pruned"] == true {
            policy["pruned"] = json!(true);
            policy["input_chars"] = previous["input_chars"].clone();
            for key in ["reasoning_events_removed", "tool_outputs_shortened"] {
                policy[key] =
                    json!(previous[key].as_u64().unwrap_or(0) + policy[key].as_u64().unwrap_or(0));
            }
            if policy.get("retained_chars").is_none() {
                policy["retained_chars"] = retained_chars;
            }
        }
        *outcome = destination.write(root, options)?;
        outcome.extra["resume_context_policy"] = policy;
    }
    let mut outcomes = Vec::new();
    for (_, session, mut options, preview) in plans {
        options.dry_run = opts.dry_run;
        let mut outcome = if opts.dry_run {
            preview
        } else {
            let mut outcome = destination.write(&session, &options)?;
            outcome.extra["resume_context_policy"] = preview.extra["resume_context_policy"].clone();
            outcome
        };
        outcome.extra["parent_session"] = json!(session.parent_session);
        outcomes.push(outcome);
    }
    let mut root = outcomes.remove(0);
    root.extra["child_sessions"] = serde_json::to_value(outcomes)?;
    Ok(root)
}
