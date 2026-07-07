# Plan: Integrate Vision Analysis into CodexPlusPlus

## Problem

When CodexPlusPlus is connected to a text-only LLM backend (e.g. DeepSeek V4 series), sending an image in chat causes the session to become permanently unusable. The image data (base64) gets embedded in the conversation history, and subsequent requests carry that corrupted data, breaking the session irrecoverably.

## Root Cause

CodexPlusPlus sends the full message history (including `image_url` content blocks) to the upstream API regardless of whether the model supports vision. Text-only models cannot process `image_url` blocks, and the raw image data pollutes the context.

## Solution

Before sending a request to the upstream API, detect whether the target model supports vision:

- If **yes**: pass the request as-is (normal behavior)
- If **no**: strip `image_url` blocks from messages, call a vision analysis API to get text descriptions, inject the descriptions back, then forward the cleaned request

This is the same logic as `image-router`, but integrated natively into CodexPlus++'s request pipeline -- no external proxy needed.

## Key Touch Points

| File | Role |
|------|------|
| `crates/codex-plus-core/src/bridge.rs` | Request construction and forwarding -- the main interception point |
| `crates/codex-plus-core/src/http_client.rs` | HTTP client used to call the vision analysis API |
| `crates/codex-plus-core/src/models.rs` | Model capability definitions (vision support flag) |
| `crates/codex-plus-core/src/model_catalog.rs` | Model catalog with per-model capabilities |
| `apps/codex-plus-manager/src/` | UI for configuring vision analysis (API key, model, toggle) |
| `apps/codex-plus-manager/src-tauri/src/commands.rs` | Tauri commands for persisting config |

## Implementation Steps

### Step 1: Model capability detection

Add a `supports_vision: bool` field to the model catalog, default `false`. Vision-capable models (e.g. GPT-4o, Claude 3.5 Sonnet, Gemini 2.0 Flash) get `true`.

### Step 2: Vision analysis module

Create a new module `crates/codex-plus-core/src/vision.rs`:

```
detect_image_blocks(messages) -> Vec<(index, image_url)>
analyze_image(image_url, api_key, model, base_url) -> String (text description)
replace_image_blocks(messages, analysis_results) -> cleaned messages
```

Use the existing `http_client` module for API calls. Support OpenAI-compatible vision API format.

### Step 3: Intercept in bridge.rs

In the request-building path in `bridge.rs`, before serializing and sending:

```rust
if !model.supports_vision && has_image_blocks(&messages) {
    let analysis = vision::analyze_all_images(&messages, &vision_config).await;
    messages = vision::replace_with_descriptions(messages, analysis);
}
```

The interception must happen **before** the request is sent upstream, and **before** tool/MCP skill invocation (which happens after the model responds).

### Step 4: Configuration

Add three settings (stored via existing settings infrastructure):

- `vision_api_key` -- API key for the vision provider
- `vision_model` -- model name (e.g. `qwen-vl-plus`)
- `vision_base_url` -- API endpoint
- `vision_enabled` -- toggle

Add corresponding UI in the Manager frontend.

### Step 5: Session recovery

When a session has been corrupted by previous image messages (history contains raw image data), the same logic applies: on the next request, the proxy strips images and replaces them with descriptions, recovering the session automatically.

## Files to Create

- `crates/codex-plus-core/src/vision.rs`

## Files to Modify

- `crates/codex-plus-core/src/bridge.rs` -- add image detection + replacement
- `crates/codex-plus-core/src/models.rs` -- add `supports_vision` field
- `crates/codex-plus-core/src/model_catalog.rs` -- populate vision capability
- `crates/codex-plus-core/src/lib.rs` -- register new module
- `crates/codex-plus-core/src/settings.rs` -- add vision config fields
- `apps/codex-plus-manager/src/App.tsx` -- add vision config UI
- `apps/codex-plus-manager/src-tauri/src/commands.rs` -- expose settings to UI

## Verification

1. Send an image to a text-only model (e.g. DeepSeek V4) -- image should be replaced with a text description, session should remain usable
2. Send an image to a vision-capable model (e.g. GPT-4o) -- image should pass through untouched
3. Send a text-only message -- no behavior change
4. Recover a previously corrupted session -- old image data should be cleaned up on the next request
