# Web chat exports reader: design

## Goal

Read conversations from the official data exports of ChatGPT, claude.ai and Gemini, and write them as native sessions in any harness-bridge destination, mainly Claude Code, so a web chat can be continued locally with its history.

Non-goals: writing back to any web app (none of them has an import), and reading live chats through private APIs or a browser session.

## Sources

| Provider | Export | Conversations file | Branches |
| :-- | :-- | :-- | :-- |
| `chatgpt` | ChatGPT Settings > Data controls > Export data | `conversations.json`, plus `conversations-*.json` shards when present | node tree: `mapping`, `parent`, `current_node` |
| `claude-web` | claude.ai Settings > Privacy > Export data | `conversations.json` | message tree: `parent_message_uuid`; older exports are flat lists |
| `gemini-web` | Google Takeout > My Activity > Gemini Apps, with Activity records set to JSON | `My Activity/Gemini Apps/MyActivity.json` | none: one prompt and response per record |

The three providers are read-only. Each is a format inside one shared provider, `providers/web`, so loading, attachments, turn assembly and tool pairing are written once.

## Input

`--export <path>` is a global option. It accepts the export `.zip`, its extracted folder, or the conversations file itself; for a file, its folder is the export root. The format is detected from the content: a `MyActivity.json` under `Gemini Apps` means `gemini-web`, and a `conversations.json` whose first conversation has `mapping` or `chat_messages` means `chatgpt` or `claude-web`. A provider given a different format fails with the detected name. A Takeout with only `MyActivity.html` fails with instructions to export Activity records as JSON. `.tgz` archives fail with instructions to extract first.

`list --export <path>` lists the detected provider; `list --provider <name> --export <path>` lists that one. The export is parsed once per run into typed records that keep only the fields used.

## Discovery

One `SessionRef` per conversation: provider-native id, title, creation and update times in milliseconds, no cwd, newest first. Gemini titles come from the first prompt.

## Conversion

Shared rules: a user message closes the previous turn and opens a new one. A `ToolResult` always follows its `ToolCall`: output with no open call becomes an assistant message labeled with the tool, and a call still open when the next message arrives, or at the end, gets a result saying the export recorded no output. Tool names are reduced to `[A-Za-z0-9_-]`, 64 characters at most. Times come from the records.

### chatgpt

The visible chat is the path from `current_node` to the root; without it, descend from the root through the last child. Missing nodes and cycles fail the read.

| Record | Event |
| :-- | :-- |
| null message, `system` author, hidden metadata, `user_editable_context`, `model_editable_context`, `reasoning_recap` | skipped |
| user | user `Message` |
| assistant `thoughts` | `Reasoning` |
| assistant with `recipient` other than `all` | `ToolCall`: id from the message, arguments as the JSON object text or `{"input": text}` |
| other assistant | assistant `Message` |
| tool | `ToolResult` for the open call; consecutive outputs for one call are joined |

Rendering covers `text` and `multimodal_text` parts, `code`, `execution_output`, `tether_browsing_display`, `tether_quote`, `thoughts`, `system_error`, `sonic_webpage` and audio transcriptions; other types become a short placeholder naming the type. Citation markers (spans from U+E200 to U+E201) are removed.

### claude-web

The visible chat is the path from `current_leaf_message_uuid` when present, otherwise from the latest message without children, up through `parent_message_uuid`. A flat export without parent links keeps its list order. `human` and `assistant` senders map to user and assistant. Content blocks: `text` becomes message text, `thinking` becomes `Reasoning`, `tool_use` becomes a `ToolCall` (its own id when present, otherwise one derived from the message and position) and `tool_result` becomes its `ToolResult` (matched by `tool_use_id`, otherwise to the open call). A message without `content` uses its `text`. Attachments with `extracted_content` are inlined in the user message under their file name.

### gemini-web

Records are grouped by the conversation id in `titleUrl` (the last path segment after `/app/`). Records without one form groups of consecutive records less than 30 minutes apart. Each record becomes a user prompt and an assistant response, in time order. The prompt is the `title` after its `Prompted ` prefix, a `Request` entry in `details`, or the text of a `userInteractions` request; the response is the `safeHtmlItem` HTML converted to text, a `Response` entry, or a `userInteractions` response. Records with neither are skipped.

## Attachments

References: ChatGPT `image_asset_pointer` parts and `metadata.attachments`, claude.ai `files`, Gemini `attachedFiles` and `imageFile`. An index of the export's files resolves a ChatGPT asset id to the file whose name starts with that id, and the other formats by file name. A found file is referenced as `<provider>-attachments/<conversation id>/<file name>`, relative to the destination base directory; a missing one renders as a placeholder saying it was not in the export.

`migrate` from a web provider requires `--cwd`, because web chats have no working directory. After a successful write it copies the referenced files into that folder and adds the counts to its output; `--dry-run` reports the plan without copying. Portable exports keep the references but carry no files.

## Errors

A missing `--export`, an unrecognized or mismatched export, malformed JSON and an unknown conversation id each fail with a message naming the problem. Writing to a web provider fails with a message that the web app has no import.

## Changes outside the provider

- `providers/mod.rs`: the module, `Homes::export`, and `provider_named` for the three names.
- `cli.rs`: the global option; `list` handles `--export`; `migrate` requires `--cwd` for web sources and copies attachments.
- `portable.rs`: the three names are accepted sources.
- `Cargo.toml`: `zip` with deflate only.
- Docs: README, AGENTS.md, portable format, development guide.

## Testing

Fixture tests per format cover branch selection, each content type, tool pairing and the no-output result, unlabeled output, name reduction, citation removal, attachments found, missing and copied, Gemini grouping by id and by time, all three Gemini record shapes, HTML to text, and zip, folder and file inputs. Shared tests cover detection and its errors, discovery order, the read-only write error, portable source acceptance, and a web-to-Claude write read back by the Claude reader. A final check resumes a converted chat with the real Claude Code CLI.
