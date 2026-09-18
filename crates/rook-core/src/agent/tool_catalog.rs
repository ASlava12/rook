//! The model-visible tool catalog and schemas.

use super::{
    AgentLoop, CHANGES_THINGS, DELEGATE, DOCS, FIND_SKILL, FORGET, LOAD_SKILL, MAX_DEPTH, PLAN, RECALL,
    REMEMBER, STANCE, SUBAGENTS, VERIFY, WRITE_SKILL,
};
use rook_llm::ToolSpec;
use rook_tools::policy::Stance;
use serde_json::json;

impl<'a> AgentLoop<'a> {
    pub fn tool_specs(&self) -> Vec<ToolSpec> {
        let lazy = self.rook.config.agent.lazy_tools;
        let mut specs = if lazy { self.tools.stubs() } else { self.tools.specs() };
        let checking = self.checking;
        let mut push = |spec: ToolSpec| {
            if checking && CHANGES_THINGS.contains(&spec.name.as_str()) {
                return;
            }
            specs.push(if lazy { spec.stub() } else { spec })
        };
        push(ToolSpec {
            name: crate::results::READ_RESULT.into(),
            description:
                "Read saved results or command output in byte pages; omit result_id to list result IDs."
                    .into(),
            parameters: json!({"type":"object", "properties":{
                "result_id":{"type":"integer"}, "session":{"type":"string", "description":"Optional direct child session."}, "offset":{"type":"integer"},
                "limit":{"type":"integer"}, "source":{"type":"string","enum":["result","output"]}
            }}),
        });
        if self.rook.config.agent.todo_tool {
            push(ToolSpec {
                name: PLAN.into(),
                description: "Write the plan for this task as a checklist, replacing whatever was there. Keep it current: mark a step done as soon as it is."
                    .into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "steps": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "step": { "type": "string" },
                                    "done": { "type": "boolean" }
                                },
                                "required": ["step"]
                            }
                        }
                    },
                    "required": ["steps"]
                }),
            });
        }
        push(ToolSpec {
            name: DOCS.into(),
            description: "Look a technology up in the documentation kept here, fetching it \
                          first if it is not. Ask before saying how a library or protocol \
                          behaves: what a model remembers is a year old and does not say so. \
                          Answers cite the local copy and the page each passage came from."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "topic": { "type": "string", "description": "redis, tokio, the http spec" },
                    "version": { "type": "string", "description": "omit for the current one" },
                    "question": { "type": "string", "description": "what is being asked; it picks the passages" },
                    "page": { "type": "integer", "description": "read one page whole, by its number in the answer" },
                    "refresh": { "type": "boolean", "description": "read the site again anyway" }
                },
                "required": ["topic"]
            }),
        });
        push(ToolSpec {
            name: LOAD_SKILL.into(),
            description: "Load a skill's full instructions into context by name. An unknown \
                          name comes back with the skills that do match it, so a description \
                          works when the exact name is not known."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": { "name": { "type": "string" } },
                "required": ["name"]
            }),
        });
        push(ToolSpec {
            name: FIND_SKILL.into(),
            description: "Search the configured sources for a skill, and install one by name. \
                          For when nothing here covers what is being asked and fetching beats \
                          writing from scratch. Installing is approved like any write: it puts \
                          instructions on the machine that later sessions follow."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "words to match against a name or description" },
                    "install": { "type": "string", "description": "the exact name to install. Search first: a name no source offers comes back with the closest one." }
                }
            }),
        });
        push(ToolSpec {
            name: WRITE_SKILL.into(),
            description: "Write down a repeatable procedure so a later session does not work it \
                          out again. For what took real effort — a build incantation, a platform \
                          quirk — not for what this conversation already says. `requires` scopes \
                          it to where it holds."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "lower-case, hyphenated" },
                    "description": { "type": "string", "description": "when to use it, in one line" },
                    "body": { "type": "string", "description": "markdown instructions" },
                    "keywords": { "type": "array", "items": { "type": "string" } },
                    "files": {
                        "type": "object",
                        "additionalProperties": { "type": "string" },
                        "description": "By relative name: a script the body runs, a template it fills."
                    },
                    "requires": {
                        "type": "object",
                        "properties": {
                            "os": { "type": "array", "items": { "type": "string" } },
                            "arch": { "type": "array", "items": { "type": "string" } },
                            "userland": { "type": "array", "items": { "type": "string" } },
                            "language": { "type": "object", "additionalProperties": { "type": "string" } },
                            "tool": { "type": "object", "additionalProperties": { "type": "string" } }
                        }
                    }
                },
                "required": ["name", "description", "body"]
            }),
        });
        if self.rook.config.memory.enabled {
            push(ToolSpec {
                name: REMEMBER.into(),
                description: "Remember something for future sessions. Use it for durable facts — preferences, conventions, decisions — not for what is already in this conversation."
                    .into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "text": { "type": "string", "description": "One self-contained fact." },
                        "tags": { "type": "array", "items": { "type": "string" } },
                        "scope": { "type": "string", "enum": ["global", "project"], "default": "project" },
                        "pinned": { "type": "boolean", "description": "Recall this ahead of anything that merely matches. It still costs context, so pin only what is true every turn." }
                    },
                    "required": ["text"]
                }),
            });
            push(ToolSpec {
                name: FORGET.into(),
                description: "Drop a remembered fact by its id, once it is wrong or stale.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": { "id": { "type": "string" } },
                    "required": ["id"]
                }),
            });
            push(ToolSpec {
                name: RECALL.into(),
                description: "Search memory for facts beyond the ones already in context.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": { "query": { "type": "string" } },
                    "required": ["query"]
                }),
            });
        }
        if self.depth < MAX_DEPTH {
            push(ToolSpec {
                name: crate::worktrees::TOOL.into(),
                description: "Review or remove a delegated worktree; read files to transfer selected edits."
                    .into(),
                parameters: json!({"type":"object", "properties":{
                    "session":{"type":"string"}, "action":{"type":"string","enum":["status","diff","read","remove"]},
                    "path":{"type":"string"}, "offset":{"type":"integer"}, "limit":{"type":"integer"},
                    "discard":{"type":"boolean"}
                }, "required":["session"]}),
            });
            push(ToolSpec {
                name: VERIFY.into(),
                description: "Have a claim checked by an agent that did not make it and cannot edit anything. Use it before reporting work done."
                    .into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "claim": {
                            "type": "string",
                            "description": "Stated so it can be wrong: `the tests pass`, not `the code is better`."
                        },
                        "settles": { "type": "string", "description": "What would decide it — a command, a file." }
                    },
                    "required": ["claim"]
                }),
            });
            // The endpoints are named in the argument they constrain, and not
            // in the system prompt: the prompt is the front of every request
            // and prompt caching is a prefix match, so a list that moved with
            // the configuration would invalidate everything behind it on the
            // turn somebody edited a file.
            //
            // Not in this tool's own description either, which was the first
            // attempt. Under lazy loading only the first sentence of that is
            // advertised, so the names were dropped exactly where they would
            // have been read — and put back in the full schema, which is what
            // a model fetches before it calls. Here they arrive with the field
            // and only where the field exists.
            let endpoints: Vec<&str> = self.rook.config.models.keys().map(String::as_str).collect();
            let mut delegate_args = json!({
                    "type": "object",
                    "properties": {
                        // A bare `task` is still accepted, and deliberately not
                        // advertised: a model that saw both filled both, which
                        // ran every sub-task twice.
                        "tasks": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "One assignment per entry, run at the same time. A sub-agent cannot see this conversation, so each must stand alone."
                        },
                        "context": {
                            "type": "string",
                            "default": "none",
                            "description": "What it starts with. `recent` is the last few \
                                            exchanges; anything else is passed verbatim — put \
                                            here what it would otherwise read."
                        },
                        "isolation": {"type":"string", "enum":["shared","worktree"],
                            "description":"Default shared. worktree requires a clean Git root; edits are retained separately for review."},
                        "max_steps": { "type": "integer" },
                        // One word rather than a model and an effort, which
                        // are never chosen apart: the question a caller can
                        // actually answer is whether this is legwork or
                        // judgement, and the two knobs follow from it. Two
                        // fields also cost eighty tokens on every eager
                        // request, which is most of what the whole list has
                        // left.
                        "care": {
                            "type": "string",
                            "enum": ["quick", "careful"],
                            "description": "`careful` gives it this turn's model and reasoning. \
                                            Default: legwork."
                        },
                        "wait": {
                            "type": "boolean",
                            "default": true,
                            "description": "False answers at once and leaves them running, for \
                                            `subagents` to read."
                        }
                    }
            });
            // Only offered where there is a choice to make. One endpoint, or
            // none named, and this field is a question with one answer that
            // costs tokens on every request to ask.
            if !endpoints.is_empty()
                && let Some(properties) = delegate_args["properties"].as_object_mut()
            {
                properties.insert(
                    "model".into(),
                    json!({
                        "type": "string",
                        "enum": endpoints,
                        "description": "Which configured endpoint to run it on. Left out, it \
                                        goes to whichever has room."
                    }),
                );
            }
            push(ToolSpec {
                name: DELEGATE.into(),
                description: "Hand a self-contained sub-task to a fresh agent and get back only its conclusion. Use it when a step would otherwise fill this conversation with detail you do not need to keep — a wide search, a long file survey, an independent verification."
                    .into(),
                parameters: delegate_args,
            });
            // Only where there is more to ask for.
            if self.policy.stance() < Stance::Free {
                push(ToolSpec {
                    name: STANCE.into(),
                    description: "Ask for more latitude for the rest of this run. A person decides.".into(),
                    parameters: json!({
                        "type": "object",
                        "properties": {
                            "to": { "type": "string", "enum": ["assist", "autonomous", "free"] },
                            "why": { "type": "string" }
                        },
                        "required": ["to"]
                    }),
                });
            }
            push(ToolSpec {
                name: SUBAGENTS.into(),
                description:
                    "Where sub-agents left running got to, and their results. No id answers for all.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "id": { "type": "string" },
                        "say": { "type": "string", "description": "A remark it sees at its next step." },
                        "wait_secs": { "type": "integer", "description": "Answer when it lands, or after this." }
                    }
                }),
            });
        }
        // Sorted so the rendered prefix is byte-identical between turns:
        // tools render first, and a reordered list invalidates everything.
        specs.sort_by(|a, b| a.name.cmp(&b.name));
        specs
    }
}
