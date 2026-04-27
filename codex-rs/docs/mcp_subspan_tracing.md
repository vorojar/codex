# MCP Subspan Tracing [experimental]

This document describes Codex's experimental MCP client extension for ingesting child OpenTelemetry spans emitted by MCP servers.

- Status: experimental and subject to change without notice
- Capability: `codex/subspan-tracing`
- Supported version: `1`
- Supported transport: `stderr-jsonl`

## Purpose

Codex creates an `mcp.tools.call` span around each MCP tool call. Some MCP servers perform meaningful nested work that is useful to inspect as child spans of that call. The `codex/subspan-tracing` capability lets a server opt in to receiving the active W3C trace context for a tool call and emitting sanitized span records back to Codex.

This is intended for local stdio MCP servers. Telemetry records are out-of-band with respect to the MCP JSON-RPC stream and must not affect tool-call success or transport liveness.

## Capability Negotiation

Codex advertises client support during MCP `initialize`:

```json
{
  "capabilities": {
    "experimental": {
      "codex/subspan-tracing": {
        "version": 1,
        "transports": ["stderr-jsonl"]
      }
    }
  }
}
```

An MCP server opts in by returning a compatible experimental capability in its initialize result:

```json
{
  "capabilities": {
    "experimental": {
      "codex/subspan-tracing": {
        "version": 1,
        "transports": ["stderr-jsonl"],
        "tools": {
          "js": {
            "attributeProfile": "browser-use-v1"
          }
        }
      }
    }
  }
}
```

If `tools` is present, Codex enables subspan tracing only for those tool names. If `tools` is omitted, Codex treats the capability as applying to every tool exposed by that server.

Unknown versions or transports are ignored.

## Tool Call Metadata

For a negotiated server/tool pair, Codex adds `_meta["codex/subspan-tracing"]` while it is inside the active `mcp.tools.call` span:

```json
{
  "_meta": {
    "codex/subspan-tracing": {
      "enabled": true,
      "version": 1,
      "traceparent": "00-00000000000000000000000000000001-0000000000000002-01",
      "tracestate": "vendor=value"
    }
  }
}
```

`tracestate` is omitted when no tracestate is active. If tracing is not active, Codex does not add this metadata.

Servers should disable subspan emission for a tool call unless:

- `enabled` is `true`
- `version` is `1`
- `traceparent` is present and parseable

## Stderr Transport

For `stderr-jsonl`, each telemetry record is written to the MCP server process stderr as one line with this exact prefix:

```text
@codex-telemetry 
```

The rest of the line is a single JSON object:

```text
@codex-telemetry {"v":1,"type":"span","name":"example.work",...}
```

Normal stderr output must not use that prefix. Codex preserves ordinary stderr logging behavior for non-telemetry lines.

## Span Record Schema

Span records use schema version `v: 1`:

```json
{
  "v": 1,
  "type": "span",
  "name": "browser_use.tab.goto",
  "trace_id": "00000000000000000000000000000001",
  "span_id": "0000000000000010",
  "parent_span_id": "0000000000000002",
  "trace_flags": "01",
  "tracestate": "vendor=value",
  "start_unix_nanos": 1000000000,
  "end_unix_nanos": 2000000000,
  "attrs": {
    "browser_use.url": "https://example.com",
    "browser_use.timeout_ms": 2500
  }
}
```

Required fields:

- `v`: must be `1`
- `type`: must be `"span"`
- `name`: allowlisted span name
- `trace_id`: 32 lowercase or uppercase hex characters, nonzero
- `span_id`: 16 lowercase or uppercase hex characters, nonzero
- `parent_span_id`: 16 lowercase or uppercase hex characters, nonzero
- `trace_flags`: 2 hex characters
- `start_unix_nanos`: Unix timestamp in nanoseconds
- `end_unix_nanos`: Unix timestamp in nanoseconds, greater than or equal to `start_unix_nanos`

Optional fields:

- `tracestate`: W3C tracestate header value
- `attrs`: span attributes object

Codex also accepts `traceparent` for backward compatibility, but explicit IDs are the canonical protocol for reconstructed spans because they preserve parent-child relationships across records.

## Span IDs and Hierarchy

The first server-created span for a tool call should use:

- `trace_id` from the request `traceparent`
- `parent_span_id` from the request `traceparent`
- a newly generated `span_id`

Nested server spans should use the same `trace_id`, their own generated `span_id`, and the parent reconstructed span's `span_id` as `parent_span_id`.

## Sanitization

Servers must emit only sanitized, allowlisted attributes. Attribute values must be primitives:

- string
- integer or float
- boolean

Do not emit objects, arrays, cookies, credentials, auth headers, bearer tokens, full request/response payloads, arbitrary DOM text, or user-sensitive selectors. Codex applies its own allowlist and drops unsupported attributes, but servers should sanitize before writing records.

## Failure Behavior

Subspan telemetry is best effort:

- malformed telemetry lines are ignored
- unsupported versions or record types are ignored
- invalid span records are ignored or logged at warning level
- telemetry write failures must not fail the MCP tool call
- telemetry parser failures must not break MCP transport
- no telemetry is emitted or reconstructed when Codex tracing is inactive

## Current Attribute Profile

`browser-use-v1` is the initial attribute profile used by Browser Use instrumentation. Codex currently allows Browser Use, Node REPL, and JS-related span names and attribute keys needed for that profile. New profiles should be added deliberately with their own allowlist changes and tests.

