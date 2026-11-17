# Chat

A personal agent IDE, in the vein of [Gitflow](../gitflow): native GPUI, same palette and rhythm.

Projects are pinned down the far left, the selected project's threads sit beside them, and the
workspace is tabbed: the chat is pinned first, with files and diffs opening next to it. The right
panel is tabbed by icon for context, changes and files.

Run `cargo run` for a dev build.

## Agents

Each thread runs a coding agent over the [Agent Client Protocol](https://agentclientprotocol.com).
Claude Code and Codex are set up out of the box through their ACP adapters, which `npx` fetches on
first use, so Node needs to be installed and each agent's own CLI signed in.

The model, reasoning effort and permission mode in the composer are whatever the agent's session
offers; changing one applies to the live session, mid-thread.

Any command that speaks ACP on stdio can be added under `agents` in
`~/Library/Application Support/Chat/settings.json`:

```json
{
  "agents": [
    { "name": "Claude Code", "command": "npx", "args": ["-y", "@agentclientprotocol/claude-agent-acp"] },
    { "name": "Codex", "command": "npx", "args": ["-y", "@agentclientprotocol/codex-acp"] },
    { "name": "Mock", "command": "python3", "args": ["/path/to/chat/scripts/mock-agent.py"] }
  ]
}
```

Threads are saved under `threads/` in the same folder, one file per project, and come back on
launch. Each keeps its agent's session id: when a thread is shown again, the agent is asked to
reopen that session (`session/resume`, or `session/load` with the replay ignored), so it still
remembers the conversation. Agents that can do neither start fresh, and the thread says so.
At most four agents run at once; the least recently shown idle one is stopped first.

Fonts are set in Settings (⌘ ,).

`scripts/mock-agent.py` is a stand-in agent for working on the UI without spending tokens.
Set `CHAT_ACP_LOG=1` to print every protocol message to stderr.
`CHAT_THEME=dark` or `light` pins the appearance instead of following the system.
