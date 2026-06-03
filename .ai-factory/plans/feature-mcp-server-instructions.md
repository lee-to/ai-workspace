# Implementation Plan: MCP Server Instructions for Context Maintenance

Branch: feature/mcp-server-instructions
Created: 2026-06-03

## Settings
- Testing: yes
- Logging: verbose
- Docs: yes

## Scope
Add first-class operating guidance to the `ai-workspace` MCP server so agents understand how to discover shared context, work within project/group scope, and update durable context when implementation changes create long-lived knowledge.

The first implementation should stay intentionally small:

- Add MCP `initialize` server instructions as the primary delivery mechanism.
- Keep existing tool names and schemas stable.
- Do not add MCP prompts, resources, watchers, or automatic git/diff tracking in this pass.
- Document prompts/resources as future work if useful, but do not implement them yet.

## References
- MCP lifecycle docs show `initialize` as the server capability negotiation step and allow an `instructions` field in newer protocol versions: https://modelcontextprotocol.io/specification/2025-06-18/basic/lifecycle
- MCP schema describes `InitializeResult.instructions` as a hint for clients to improve model understanding of the server and its features: https://modelcontextprotocol.io/specification/2025-06-18/schema
- Current server implementation: `src/mcp/mod.rs`
- Current MCP tool implementations: `src/mcp/tools.rs`
- Current MCP docs: `docs/mcp-server.md`
- Current README MCP usage section: `README.md`

## Key Decisions
- The guidance belongs in MCP `initialize` first because it is server-level behavior, not one more user-controlled workflow command.
- The instruction text should describe a decision rule, not a blanket auto-update rule: update shared context only when changes create durable knowledge future agents or related projects need.
- The initial wording should prefer existing tools:
  - Start tasks with `workspace_context`.
  - Use `workspace_read`, `workspace_search_fulltext`, `workspace_service_graph`, `workspace_events`, and CodeGraph tools before broad scans when applicable.
  - Use `project_file_write` only for in-scope durable Markdown context updates.
  - Run or request CodeGraph sync when changed Rust code should be discoverable through `codegraph_*` tools.
- Protocol compatibility must be handled deliberately because the server currently advertises `2024-11-05`, while the documented `instructions` field is explicit in the newer MCP schema.

## Open Questions
- Should the implementation keep `protocolVersion: "2024-11-05"` and include `instructions` as a permissive extra result field, or bump to a newer protocol version after compatibility review?
- Should instruction text be static, or should a later feature make it configurable from a shared file/note?
- Should a future tool expose a dedicated "context maintenance checklist" separate from initialize instructions?

## Commit Plan
- **Commit 1** (after tasks 1-3): `feat: add mcp server usage instructions`
- **Commit 2** (after tasks 4-6): `test: cover mcp instructions and docs`

## Tasks

### Phase 1: Contract and Compatibility

- [x] Task 1: Define the MCP server instruction contract.

  Deliverable: decide the exact instruction text and the protocol compatibility approach before editing runtime behavior.

  Expected behavior:
  - Identify whether adding `instructions` while keeping `protocolVersion: "2024-11-05"` is acceptable for current clients, or whether the server should move to a newer supported protocol version.
  - Keep the instruction text concise enough to fit initialize metadata without crowding tool schemas.
  - Include concrete agent workflow guidance:
    - Call `workspace_context` at task start when workspace context may matter.
    - Prefer shared context and CodeGraph tools before unbounded file scans.
    - Update durable shared Markdown context only when project knowledge, architecture, setup, cross-project contracts, or conventions change.
    - Keep access within MCP scope and shared/project-wide policy.
  - Document the decision inline in code only if the compatibility choice is non-obvious.

  Files: `src/mcp/mod.rs`, optional `docs/mcp-server.md`.

  Logging requirements:
  - No new runtime logs are required for text-only contract definition.
  - If protocol negotiation behavior changes, keep existing `INFO` initialize logging and add `DEBUG` detail for selected protocol/instruction mode.
  - Errors are not expected in this task; any compatibility blocker should be surfaced before implementation.

- [x] Task 2: Add server instructions to the MCP initialize response.

  Deliverable: update `handle_initialize` so the MCP server returns a stable `instructions` field alongside capabilities and server info.

  Expected behavior:
  - `initialize` response includes concise instructions that teach agents how to use ai-workspace projects, groups, shared items, service graph/events, and CodeGraph.
  - Instructions explicitly avoid blanket context rewrites after every code edit.
  - Instructions steer context updates toward `project_file_write` for durable Markdown artifacts inside in-scope projects.
  - Existing capabilities and server info remain unchanged unless Task 1 requires a protocol version adjustment.
  - The implementation keeps presentation-layer behavior in `src/mcp/mod.rs`; no database changes are needed.

  Files: `src/mcp/mod.rs`.

  Logging requirements:
  - Preserve `INFO` log for MCP initialize.
  - Add `DEBUG` log only if useful to show that server instructions were included or which instruction variant was selected.
  - Any serialization failure should continue to surface through existing JSON-RPC response handling.

- [x] Task 3: Keep tool descriptions aligned with the new workflow.

  Deliverable: review and minimally adjust MCP tool descriptions only where they contradict or under-explain the new instruction workflow.

  Expected behavior:
  - `workspace_context` remains the recommended starting point for workspace metadata.
  - `project_file_write` description remains clear that it writes regular files inside scope, shares them, and indexes Markdown.
  - CodeGraph descriptions continue to recommend CodeGraph before grep when populated.
  - Do not rename tools, change schemas, or add new tools in this task.

  Files: `src/mcp/mod.rs`.

  Logging requirements:
  - No new runtime logging required.
  - If any tool schema changes unexpectedly become necessary, log tool calls at existing `INFO` boundaries and preserve current `DEBUG` argument logging style in `src/mcp/tools.rs`.

### Phase 2: Tests and Regression Coverage

- [x] Task 4: Add unit coverage for initialize instructions.

  Deliverable: extend MCP unit tests so the initialize response verifies the new instructions contract.

  Expected behavior:
  - `handle_initialize_returns_capabilities` or a new focused test asserts `instructions` exists and is a non-empty string.
  - Test verifies the text names the key workflow concepts without depending on brittle full-string equality:
    - `workspace_context`
    - `project_file_write`
    - durable/shared context update guidance
    - scoped access/safety
  - Existing initialize tests continue to pass.

  Files: `src/mcp/mod.rs`.

  Logging requirements:
  - No test-time logs required.
  - Assertion messages should name the missing instruction concept so failures are actionable.

- [x] Task 5: Add integration coverage for MCP initialize over stdio.

  Deliverable: extend `tests/mcp_tests.rs` to verify real server initialize output includes instructions.

  Expected behavior:
  - `test_mcp_initialize` checks the stdio JSON-RPC response includes a non-empty `instructions` string.
  - The integration test verifies the important workflow terms by substring or structured helper, not exact full text.
  - The test does not require any registered projects or groups; instructions are server behavior, independent of DB contents.

  Files: `tests/mcp_tests.rs`.

  Logging requirements:
  - No new runtime logs required.
  - Test failure messages should include the missing term and response field name.

### Phase 3: Documentation and Verification

- [x] Task 6: Document MCP server instructions and context maintenance workflow.

  Deliverable: update user-facing docs so users understand what the MCP server tells agents and what behavior to expect.

  Expected behavior:
  - `docs/mcp-server.md` protocol section mentions server instructions in initialize output.
  - Docs add a short "Agent workflow" or equivalent section:
    - start with `workspace_context` when task context may span projects/groups,
    - read/search shared context before broad scans,
    - update durable shared Markdown context only for lasting project knowledge,
    - sync CodeGraph when changed Rust code should be discoverable.
  - `README.md` MCP usage section briefly mentions that ai-workspace now provides startup instructions to guide context discovery and maintenance.
  - Future work explicitly separates prompts/resources/configurable instruction text from this first implementation if mentioned.

  Files: `docs/mcp-server.md`, `README.md`.

  Logging requirements:
  - No runtime logging required for docs.
  - Documentation should avoid promising automatic updates after every code edit.

- [x] Task 7: Run project verification checklist.

  Deliverable: format, lint, test, audit, and fix any regressions introduced by the implementation.

  Expected behavior:
  - Run the required commands in order:
    1. `cargo fmt`
    2. `cargo clippy`
    3. `cargo test`
    4. `cargo audit`
  - If `cargo audit` is unavailable or blocked by advisory DB/network constraints, report the exact blocker and do not mark it as passed.
  - Ensure docs and tests match final protocol behavior.

  Files: any files touched by previous tasks if verification requires fixes.

  Logging requirements:
  - No new runtime logging required.
  - Final implementation summary must include each command and outcome.

## Out of Scope
- MCP prompts/list or prompts/get support.
- MCP resources/list or resources/read support.
- Automatic file watcher or git diff observer.
- Automatic context rewriting after every code edit.
- New database tables for instruction storage.
- User-configurable instruction templates.

## Risks and Edge Cases
- Some MCP clients may ignore `instructions`; docs should frame it as guidance, not enforcement.
- Protocol version handling must be conservative to avoid breaking clients that expect `2024-11-05`.
- Overly long instructions can consume prompt context and reduce tool usability.
- Too-aggressive wording may cause agents to rewrite context files unnecessarily.
- `project_file_write` can create files, so instructions must clearly scope it to durable, intentional context updates.
