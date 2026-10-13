# ChatGPT export reader: design

## Goal

Read ChatGPT web conversations from the official data export and write them as native sessions in any harness-bridge destination, mainly Claude Code, so a chat can be continued locally with its history.

Non-goals: writing to ChatGPT or to claude.ai web chats (neither has an import), and reading live ChatGPT through its private API or a browser session.

## Input

`--chatgpt-export <path>` is a global option, like the other store overrides. It accepts the export `.zip`, its extracted folder, or a `conversations.json` file. Conversations come from `conversations.json` and any `conversations-*.json` shards at the export root, since both layouts exist. The export text is loaded once per run and kept as raw JSON slices, so discovery parses only titles and dates and a read parses only the selected conversation.

## Discovery

One `SessionRef` per conversation: the id is `conversation_id` (falling back to `id`), with the title, creation and update times converted from seconds to milliseconds, no cwd, and the id as the locator. Newest first.

## Conversion

The visible chat is the branch from `current_node` up to the root. Without `current_node`, descend from the root through the last child. Missing nodes and cycles fail the read.

| Export record | Session event |
| :-- | :-- |
| null message, `system` author, hidden metadata, `user_editable_context`, `model_editable_context`, `reasoning_recap` | skipped |
| user message | `TurnStart` (closing the previous turn) and a user `Message` |
| assistant `thoughts` | `Reasoning` |
| assistant with a `recipient` other than `all` | `ToolCall`: id from the message, name reduced to `[A-Za-z0-9_-]` (64 max), arguments as the JSON object text or `{"input": text}` |
| other assistant content | assistant `Message` |
| tool message after an open call | `ToolResult`; consecutive outputs for one call are joined |
| tool message with no open call | assistant `Message` labeled with the tool name |

Writers require every result to follow its call, and Claude requires every call to get a result. A call still open when the next user or assistant message arrives, or at the end, gets a result saying the export recorded no output.

Content rendering covers `text` and `multimodal_text` parts, `code`, `execution_output`, `tether_browsing_display`, `tether_quote`, `thoughts`, `system_error`, `sonic_webpage`, and audio transcriptions. Other types render as a short placeholder naming the type. Private-use citation markers (spans from U+E200 to U+E201) are removed.

## Attachments

Asset references come from `image_asset_pointer` parts (`file-service://` and `sediment://` pointers) and from `metadata.attachments`. An index of every file in the export maps an asset id to the file whose name starts with that id. A found file is referenced as `chatgpt-attachments/<conversation id>/<file name>`, relative to the destination base directory. A missing file renders as a placeholder that says it was not in the export.

`migrate --from chatgpt` requires `--cwd`, because a ChatGPT chat has no working directory. After a successful write it copies the referenced files into that folder and reports what it copied; `--dry-run` reports the plan without copying. Portable exports keep the references but carry no files.

## Errors

A missing `--chatgpt-export`, an export without conversation files, malformed JSON, and an unknown conversation id each fail with a message naming the problem. Writing to `chatgpt` fails with a message that ChatGPT has no import.

## Changes outside the provider

- `providers/mod.rs`: the module, `Homes::chatgpt_export`, and `provider_named("chatgpt")`.
- `cli.rs`: the global option; `list` includes ChatGPT when the option is set; `migrate` requires `--cwd` for ChatGPT and copies attachments.
- `portable.rs`: `chatgpt` is an accepted source.
- `Cargo.toml`: `zip` with deflate only, and the `raw_value` feature of `serde_json`.
- Docs: README, AGENTS.md, portable format, development guide.

## Testing

Fixture tests cover branch selection, each content type, tool pairing and the synthetic no-output result, unpaired output, name reduction, citation removal, attachments found, missing and copied, zip, folder and file inputs, the sharded layout, discovery order, error paths, portable source acceptance, and a ChatGPT-to-Claude write read back by the Claude reader. A final check resumes a converted chat with the real Claude Code CLI.
