# Plan: Integrate Vision Analysis into CodexPlusPlus

## Problem

When CodexPlusPlus is connected to a text-only LLM backend (e.g. DeepSeek V4 series), sending an image in chat causes the session to become permanently unusable. The image data (base64) gets embedded in the conversation history, and subsequent requests carry that corrupted data, breaking the session irrecoverably (`unknown variant 'image_url'` / `unknown variant 'input_image'`).

## Root Cause

CodexPlusPlus sends the full message history (including `image_url` or `input_image` content blocks from the Responses API) to the upstream API regardless of whether the model supports vision. Text-only models cannot process image blocks, and the raw image data pollutes the context.

## Solution

Before sending a request to the upstream API, detect whether the target model has VLM (Vision Language Model) analysis enabled:

- If **disabled**: pass the request as-is (normal behavior; vision-capable models see images directly)
- If **enabled**:
  1. Collect **all** image URLs from **all** messages
  2. Strip `image_url`/`input_image` blocks from **all** messages (text-only model compatibility)
  3. Call a VLM API to get text descriptions of **all** collected images
  4. Inject the descriptions into the last user message (as `{"type": "text", "text": "..."}`)
  5. Forward the cleaned request

> **Why re-analyze all images every request?** Codex's context compaction can summarize away previously-injected VLM descriptions. Re-analyzing all images ensures VLM context survives compression. The VLM is fast (~<1s per image) and the token cost is negligible.

## Key Touch Points

| File | Role |
|------|------|
| `crates/codex-plus-core/src/vision.rs` | **New module**: VLM analysis pipeline (detection, stripping, API call, injection) |
| `crates/codex-plus-core/src/protocol_proxy.rs` | Interception point in `upstream_request_parts` (lines 742鈥?59) |
| `crates/codex-plus-core/src/settings.rs` | VLM config fields on `RelayProfile`: `vlm_api_key`, `vlm_model`, `vlm_base_url`, per-model `model_vlm` JSON |
| `crates/codex-plus-core/src/lib.rs` | Module registration: `pub mod vision;` |
| `crates/codex-plus-core/src/model_suffix.rs` | `parse_window_token` 鈫?`pub(crate)`  |
| `apps/codex-plus-manager/src/App.tsx` | UI: per-model `Use VLM` checkbox in model config, VLM provider section, layout updates |
| ``apps/codex-plus-manager/src/model-windows.ts`` | ``ModelWindowRow`` gains ``vlm: boolean``; serialize/deserialize handle ``modelVlm`` param |
| `apps/codex-plus-manager/src/model-windows.test.ts` | Unit tests for VLM serialization helpers |
| `apps/codex-plus-manager/src/styles.css` | CSS: layout styles for model row actions and VLM section |
| `crates/codex-plus-core/src/ccs_import.rs` | Default VLM field values in relay_profile_from_ccs() |
| `crates/codex-plus-core/src/provider_import.rs` | Default VLM field values in relay_profile_from_request() |

## Architecture: VLM Module (vision.rs)

### Public API

```rust
should_process(model: &str, model_vlm_json: &str) -> bool
  // Returns true if the model's VLM toggle is "on" in the JSON.
  // Looks up the model key in model_vlm JSON: {"deepseek-v4-pro": true, ...}

strip_image_blocks(messages, vlm_config) -> async
  // Orchestrator: collect 鈫?strip 鈫?analyze 鈫?inject
collect_urls(msg)→ Vec<String>
  // Reads a single message, collects all image_url/input_image URLs.
  // Does NOT mutate the message (read-only).
  // 4. inject_analysis(messages, result): inject text into last user message
strip_all_images(messages)
  // Iterates ALL messages, removes image_url/input_image parts from content arrays.
  // Stripped messages are pure text. No URL collection.

```rust
collect_and_strip(messages) 鈫?Vec<String>
  // Iterates ALL messages, removes image_url/input_image parts from content arrays.
  // Returns all collected URLs. Stripped messages are pure text.

analyze_all(urls, config)→ Result<String, String>
  // Calls VLM API with images in parallel batches (BATCH_SIZE=5).
  // Each batch sends: [{"type":"text","text":"describe..."},{"type":"image_url","image_url":{"url":"..."}}]
  // Returns combined analysis text from all batches.

inject_analysis(messages, &result)
  // Inserts {"type":"text","text":"<VLM result>"} at the end of the last user message's content array.
  // On failure: inserts a placeholder text explaining VLM call failed.
```

### Key design properties

| Property | Detail |
|----------|--------|
| **Order** | collect BEFORE strip (so URLs are saved before deletion) |
| **Scope** | Only images from the LATEST user message are analyzed; historical images are already described |
| **Format** | VLM uses OpenAI-compatible Chat Completions format internally |
| **Batching** | 5 images per batch (`BATCH_SIZE = 5`) |
| **Limit** | `max_tokens: 2048` per VLM call |
| **Client** | Uses existing `http_client::proxied_client()` for proxy/timeout support |

## Decision Logic

```
model has Use VLM enabled & VLM API key/model/URL not empty?
  ├──── Yes → For each array key ["messages", "input"]:
  鈹?        strip_image_blocks(arr, vlm_config)
  鈹?        (collect 鈫?strip 鈫?analyze 鈫?inject)
  鈹斺攢鈹€ No  鈫?Skip entire VLM block. Images pass through as-is.
```

### Behavior by model type

| Upstream model | VLM enabled | What happens |
|----------------|-------------|--------------|
| **Vision model** (e.g. GPT-5, Claude) | 鉂?No | Images passed through as-is |
| **Vision model** (e.g. GPT-5, Claude) | 鉁?Yes | Images stripped, VLM re-analyzes (unnecessary but works) |
| **Text-only model** (e.g. DeepSeek) | 鉂?No | Upstream errors (`unknown variant 'image_url'`) 鈥?user must enable VLM |
| **Text-only model** (e.g. DeepSeek) | 鉁?Yes | Images stripped 鈫?VLM analyzes 鈫?descriptions injected 鈫?works |

### Per-model VLM persistence

The `model_vlm` field on `RelayProfile` is a JSON string:
```json
{"deepseek-v4-pro": true, "deepseek-v4": false, "qwen-vl": true}
```

- Managed by `model-windows.ts`: `modelWindowRowsFromProfile()` / `serializeModelWindowRows()`
- Rendered as a `鈽?Use VLM` checkbox per model in the supplier config page
- The checkbox appears on the second line of each model row, before the Delete button

## Request Pipeline (protocol_proxy.rs)

### VLM injection

```
1. Convert (if needed):
   ChatCompletions protocol  鈫?responses_to_chat_completions(request_json)
   Responses protocol         鈫?keep as-is

2. VLM check:
   body.model non-empty
   && should_process(model, relay.model_vlm)   鈫?per-model VLM toggle
   && relay.vlm_api_key non-empty              鈫?VLM provider configured

3. If VLM triggered:
   for key in ["messages", "input"]:           鈫?handles both Response & Chat formats
     strip_image_blocks(arr, vlm_config)       鈫?async: collect鈫抯trip鈫抋nalyze鈫抜nject

4. Forward to upstream (URL + body + wire_api)
> **Chat Completions path** (open_chat_completions_proxy_request) applies identical VLM logic directly on the request body's messages array.

### What the proxy does NOT do

- 鉂?**No message truncation** 鈥?previously added truncation code removed (line 762鈥?88 deleted)
- 鉂?**No config.toml modification** 鈥?does not write `model_context_window` / `auto_compact_limit`
- 鉂?**No state tracking** 鈥?each request processed independently; no VLM result caching

## Error Handling & Edge Cases

### Design principle
VLM must never break the user request. If VLM fails, strip the image and use a fallback placeholder. The upstream model must always receive a valid request.

### Edge case matrix

| Scenario | Behavior |
|----------|----------|
| No models have `Use VLM` checked | VLM block skipped entirely |
| VLM enabled but API key empty | `vlm_api_key.is_empty()` 鈫?VLM block skipped |
| VLM API call fails (network timeout) | Strip image, insert failure placeholder in Chinese |
| VLM API returns 401/403 (bad key) | Same as above |
| VLM API returns 429 (rate limited) | Same as above |
| VLM API returns empty content | Same as above |
| Multiple images in one message | All collected and analyzed in batches of 5 |
| Message has no images at all | `collect_and_strip` returns empty 鈫?return early |
| Base64 data URL image | Same process as URL; VLM API handles size limits |
| Streaming request | VLM runs before forwarding. Streaming unaffected. |
| Responses API (`input_image`) | Both `messages` and `input` arrays are processed |
| Chat Completions API (`image_url`) | Same logic applies; `messages` array processed |
| Concurrent requests | Each request is independent. No shared state. |
| Conversation exceeds context window | Proxy does NOT truncate. User should start new session or configure Codex compression. |

### VLM failure fallback

When VLM fails, `strip_image_blocks` still removes all `image_url`/`input_image` blocks and inserts a Chinese placeholder text into the last user message, telling the upstream model that VLM analysis failed.

## Context Window Handling

The proxy **does not** truncate messages. If a conversation exceeds the upstream model's limit:

- Codex Desktop may have context compaction (if `model_auto_compact_token_limit` is configured)
- The proxy's VLM re-processes all images every request (~2K tokens per image, negligible compared to conversation size)
- User can start a new session or configure compression in Codex settings

> **Note**: Writing `model_context_window` + `model_auto_compact_token_limit` to `~/.codex/config.toml` or the Codex Desktop catalog may trigger compaction, but this is outside the proxy's scope.

## Data Directory Isolation

During development, the fork uses a separate data directory to avoid interfering with the user's production Codex++ installation.

| Aspect | Production (`D:/codex++`) | This Fork |
|--------|--------------------------|-----------|
| Binary location | `D:/codex++/` | `target/debug/` (project-local) |
| Data directory | `~/.codex/` | `~/.codex-plus/` (or `$CODEX_PLUS_HOME`) |

**Changes in `crates/codex-plus-core/src/codex_home.rs`:**

1. **New env var `CODEX_PLUS_HOME`**: Checked first, before `CODEX_HOME`. Unlike `CODEX_HOME`, this one does NOT require the directory to pre-exist.
2. **Changed default**: From `~/.codex` to `~/.codex-plus`, so the fork's config, sessions, and SQLite data don't mix with production.

**Before release / upstream PR:** Revert `codex_home.rs` to upstream defaults (remove `CODEX_PLUS_HOME` logic, change default back to `~/.codex`).

## Files Changed 鈥?Summary

### Created
- `crates/codex-plus-core/src/vision.rs` 鈥?Entire VLM analysis module (~245 lines)

### Modified

| File | Changes |
|------|---------|
| `crates/codex-plus-core/src/protocol_proxy.rs` | `upstream_request_parts`: ~30 lines added 鈥?VLM check and injection for both `messages` and `input` arrays |
| `crates/codex-plus-core/src/settings.rs` | Serialization/deserialization of VLM fields in relay config |
| `crates/codex-plus-core/src/lib.rs` | `pub mod vision;` |
| `crates/codex-plus-core/src/model_suffix.rs` | `parse_window_token` 鈫?`pub(crate)`  |
| `apps/codex-plus-manager/src/App.tsx` | Model row: `Use VLM` checkbox before Delete button; VLM provider config section (key/model/URL) |
| `apps/codex-plus-manager/src/model-windows.ts` | `ModelWindowRow` gains `vlm: boolean`; serialize/deserialize handle `modelVlm` param |
| `apps/codex-plus-manager/src/model-windows.test.ts` | Tests for VLM serialization: updated assertions, added modelVlm parsing test |
| `apps/codex-plus-manager/src/styles.css` | VLM section styles: .relay-model-row-actions, .relay-vlm-section |
| `crates/codex-plus-core/src/ccs_import.rs` | VLM field defaults (4 lines) in relay_profile_from_ccs() |
| `crates/codex-plus-core/src/provider_import.rs` | VLM field defaults (4 lines) in relay_profile_from_request() |
| `.gitignore` | Added plan.md to gitignore |
| `crates/codex-plus-core/src/codex_home.rs` | `CODEX_PLUS_HOME` env var; default 鈫?`~/.codex-plus/` |
| `apps/codex-plus-launcher/build.rs` | Windows SDK toolkit path for resource compilation |

## Future Work

- Per-model VLM model selection (currently all VLM-enabled models share one VLM provider config)
- Proxy-level message truncation preserving tool_calls/tool pairing (complex, deferred)