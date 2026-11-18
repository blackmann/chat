#!/usr/bin/env python3
"""A stand-in ACP agent for working on the UI without spending tokens.

Speaks just enough of the Agent Client Protocol (v1, JSON-RPC over stdio) to
drive every part of a thread: streamed text, thoughts, tool calls, an edit that
asks permission first, a plan, usage, switchable model / effort / mode, and
reloading an earlier session (which replays it, as real agents do).
A prompt containing "sign in" is refused the way a signed-out agent refuses one.

Add it to settings.json under "agents":
    {"name": "Mock", "command": "python3", "args": ["scripts/mock-agent.py"]}
"""
import json
import sys
import threading
import time

lock = threading.Lock()
turn_lock = threading.Lock()
active_turns = {}
cancelled = threading.Event()
replies = {}  # request id -> (event, result holder), for requests we send
next_id = [1000]
state = {"model": "large", "effort": "high", "mode": "ask"}
turns = [0]
client = {"forms": False}  # what the client said it can do


def send(message):
    with lock:
        sys.stdout.write(json.dumps({"jsonrpc": "2.0", **message}) + "\n")
        sys.stdout.flush()


def update(session, body):
    send({"method": "session/update", "params": {"sessionId": session, "update": body}})


def ask(method, params):
    with lock:
        next_id[0] += 1
        rid = next_id[0]
    done, holder = threading.Event(), {}
    replies[rid] = (done, holder)
    send({"id": rid, "method": method, "params": params})
    while not done.wait(0.05):
        if cancelled.is_set():
            return None
    return holder.get("result")


def select(option_id, name, category, current, values):
    return {
        "id": option_id,
        "name": name,
        "category": category,
        "type": "select",
        "currentValue": current,
        "options": [{"value": v, "name": n, "description": d} for v, n, d in values],
    }


def options():
    return [
        select("mode", "Mode", "mode", state["mode"], [
            ("ask", "Ask first", "Confirms each edit"),
            ("auto", "Auto review", "Edits freely, you review after"),
        ]),
        select("model", "Model", "model", state["model"], [
            ("large", "Mock Large", "Slow and thorough"),
            ("small", "Mock Small", "Quick"),
        ]),
        select("effort", "Reasoning effort", "thought_level", state["effort"], [
            ("low", "Low", "Fast responses with lighter reasoning"),
            ("medium", "Medium", "Balances speed and reasoning depth for everyday tasks"),
            ("high", "High", "Greater reasoning depth for complex problems"),
        ]),
    ]


def say(session, kind, text):
    for word in text.split(" "):
        if cancelled.is_set():
            return
        update(session, {"sessionUpdate": kind, "content": {"type": "text", "text": word + " "}})
        time.sleep(0.03)


def tool(session, call, title, kind, pause=0.4, **extra):
    update(session, {"sessionUpdate": "tool_call", "toolCallId": call, "title": title, "kind": kind, "status": "in_progress"})
    time.sleep(pause)
    if not cancelled.is_set():
        update(session, {"sessionUpdate": "tool_call_update", "toolCallId": call, "status": "completed", **extra})


ESSAY = """## How the tab strip works

The chat tab is **pinned**: it always sits first and has no close button. Everything else opens *beside* it.

It has three kinds of tab:

- **Files**, opened from the tree, read once when the tab opens.
- **Diffs**, built from the edits a thread's agent has finished.
- A `Terminal` placeholder, which does nothing yet.

Opening something that is already open focuses its tab instead of adding another. The whole rule is one function:

```rust
fn open(&mut self, kind: TabKind, cx: &mut Context<Self>) {
    self.active = match self.tabs.iter().position(|t| t.kind == kind) {
        Some(i) => i,
        None => {
            self.tabs.push(Tab { id: self.next_tab, kind });
            self.tabs.len() - 1
        }
    };
    cx.notify();
}
```

### Shortcuts

| Keys | Action |
| --- | --- |
| `⌘ W` | Close the tab, unless it is the chat |
| `⌃ Tab` | Next tab |
| `⌘ 1` to `⌘ 3` | Context, Changes, Files |

1. Tabs belong to the project that is showing.
2. Switching project closes them, because their paths would no longer make sense.
3. Switching thread keeps them and returns to the chat.

> Diffs come from what the agent reports, not from git, so edits made by shell commands are not listed.

A long closing paragraph follows so that wrapping can be checked too: the strip scrolls sideways when there are more tabs than fit, the active tab shares the content's background so the two read as one surface, and inactive tabs keep a bottom border that the active one drops. See [the protocol site](https://agentclientprotocol.com) for how tool calls are described.

That is everything the strip does today.
"""


def turn(rid, session, prompt, cwd):
    cancelled.clear()
    turns[0] += 1
    n = f"turn-{turns[0]}"
    plan = ["Read the code", "Make the edit", "Check it builds"]

    def show_plan(done):
        update(session, {"sessionUpdate": "plan", "entries": [
            {"content": p, "priority": "medium", "status": "completed" if i < done else "in_progress" if i == done else "pending"}
            for i, p in enumerate(plan)
        ]})

    if "question" in prompt.lower() and client["forms"]:
        # Asked the way the Claude adapter forwards AskUserQuestion: one pick-one
        # question and its free-text "Other" companion.
        answer = ask("elicitation/create", {"sessionId": session, "mode": "form", "message": "Which database?",
                                            "requestedSchema": {"type": "object", "properties": {
                                                "question_0": {"type": "string", "title": "Database", "oneOf": [
                                                    {"const": "Postgres", "title": "Postgres", "description": "Relational"},
                                                    {"const": "Redis", "title": "Redis"}]},
                                                "question_0_custom": {"type": "string", "title": "Other",
                                                                      "description": "Type your own answer (optional)."}}}})
        say(session, "agent_message_chunk", "Answer: " + json.dumps(answer, sort_keys=True))
        return finish(rid, session)

    if "explain" in prompt.lower():
        # A long written answer, to exercise Markdown and scrolling.
        for line in ESSAY.split("\n"):
            if cancelled.is_set():
                break
            update(session, {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": line + "\n"}})
            time.sleep(0.02)
        return finish(rid, session)

    show_plan(0)
    say(session, "agent_thought_chunk", "The user wants a change. I should look before I edit.")
    say(session, "agent_message_chunk", f"Running on `{state['model']}` at {state['effort']} effort. You said: {prompt.strip()[:200]}\n\nI'll read the code first.")
    tool(session, n + "-read", "src/main.rs", "read", locations=[{"path": cwd + "/src/main.rs"}])
    tool(session, n + "-search", "fn main", "search", locations=[{"path": cwd + "/Cargo.toml"}, {"path": cwd + "/src/main.rs"}])
    show_plan(1)

    path = cwd + "/src/main.rs"
    edit = {"toolCallId": n + "-edit", "title": "Edit src/main.rs", "kind": "edit", "content": [{
        "type": "diff", "path": path,
        "oldText": "fn main() {\n    println!(\"hello\");\n}\n",
        "newText": "fn main() {\n    // Greet whoever is running this.\n    let name = std::env::var(\"USER\").unwrap_or_default();\n    println!(\"hello, {name}\");\n}\n",
    }]}
    update(session, {"sessionUpdate": "tool_call", "status": "pending", **edit})
    if state["mode"] == "ask":
        answer = ask("session/request_permission", {"sessionId": session, "toolCall": edit, "options": [
            {"optionId": "yes", "name": "Allow", "kind": "allow_once"},
            {"optionId": "always", "name": "Always allow", "kind": "allow_always"},
            {"optionId": "no", "name": "Reject", "kind": "reject_once"},
        ]})
        chosen = (answer or {}).get("outcome", {}).get("optionId")
        if chosen not in ("yes", "always"):
            update(session, {"sessionUpdate": "tool_call_update", "toolCallId": n + "-edit", "status": "failed"})
            if not cancelled.is_set():
                say(session, "agent_message_chunk", "Understood, I left the file alone.")
            return finish(rid, session)
    update(session, {"sessionUpdate": "tool_call_update", "toolCallId": n + "-edit", "status": "completed"})
    show_plan(2)
    # A long command with several lines of output, like real agents run.
    tool(session, n + "-run", "cargo check --workspace --all-targets --message-format short 2>&1; cargo test --workspace --quiet -- --test-threads 4 2>&1 | tail -n 20", "execute", pause=0.8,
         content=[{"type": "content", "content": {"type": "text", "text": "    Checking demo v0.1.0 (" + cwd + ")\n    Finished `dev` profile [unoptimized + debuginfo] target(s) in 3.21s\n\nrunning 4 tests\n....\ntest result: ok. 4 passed; 0 failed; 0 ignored; finished in 0.01s"}}])
    show_plan(3)
    say(session, "agent_message_chunk", "Done. `main` now greets the current user, and the build passes.")
    finish(rid, session)


def finish(rid, session):
    update(session, {"sessionUpdate": "usage_update", "used": 42000 + turns[0] * 3100, "size": 200000,
                      "cost": {"amount": 0.21 * turns[0], "currency": "USD"}})
    with turn_lock:
        del active_turns[rid]
        send({"id": rid, "result": {"stopReason": "cancelled" if cancelled.is_set() else "end_turn"}})


def main():
    cwds = {}
    for line in sys.stdin:
        try:
            message = json.loads(line)
        except ValueError:
            continue
        method, rid, params = message.get("method"), message.get("id"), message.get("params") or {}
        if method is None:
            if rid in replies:
                done, holder = replies.pop(rid)
                holder["result"] = message.get("result")
                done.set()
        elif method == "initialize":
            client["forms"] = "form" in ((params.get("clientCapabilities") or {}).get("elicitation") or {})
            send({"id": rid, "result": {"protocolVersion": 1, "agentCapabilities": {"loadSession": True}, "authMethods": [],
                                        "_meta": {"steering": {"supported": True}},
                                        "agentInfo": {"name": "mock", "title": "Mock Agent", "version": "0.1.0"}}})
        elif method == "session/new":
            session = f"mock-{len(cwds) + 1}"
            cwds[session] = params.get("cwd", ".")
            send({"id": rid, "result": {"sessionId": session, "configOptions": options()}})
            update(session, {"sessionUpdate": "available_commands_update", "availableCommands": [
                {"name": "init", "description": "Write an AGENTS.md for this project"},
                {"name": "review", "description": "Review the current changes"},
                {"name": "compact", "description": "Summarise the conversation to free context"},
            ]})
        elif method == "session/load":
            session = params["sessionId"]
            cwds[session] = params.get("cwd", ".")
            # A real agent replays the conversation; the client should drop it.
            update(session, {"sessionUpdate": "user_message_chunk", "content": {"type": "text", "text": "REPLAYED"}})
            update(session, {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "REPLAYED"}})
            send({"id": rid, "result": {"configOptions": options()}})
        elif method == "session/prompt":
            session = params["sessionId"]
            prompt = "".join(b.get("text", "") for b in params.get("prompt", []))
            # What a signed-out agent answers, to see how the thread explains it.
            if "sign in" in prompt.lower():
                send({"id": rid, "error": {"code": -32000, "message": "Authentication required"}})
                continue
            with turn_lock:
                active_turns[rid] = session
            threading.Thread(target=turn, args=(rid, session, prompt, cwds.get(session, ".")), daemon=True).start()
        elif method == "_session/steering":
            session = params["sessionId"]
            prompt = "".join(b["text"] for b in params["prompt"] if b["type"] == "text")
            if not prompt or params.get("_meta", {}).get("steering", {}).get("idleBehavior") != "promptRequired":
                send({"id": rid, "error": {"code": -32602, "message": "Expected text and promptRequired idle behavior"}})
                continue
            with turn_lock:
                if session in active_turns.values():
                    send({"id": rid, "result": {"outcome": "injected"}})
                    update(session, {"sessionUpdate": "agent_message_chunk",
                                     "content": {"type": "text", "text": "Steered: " + prompt + "\n"}})
                else:
                    send({"id": rid, "result": {"outcome": "promptRequired", "reason": "noRunningTurn"}})
        elif method == "session/cancel":
            cancelled.set()
        elif method == "session/set_config_option":
            if params.get("configId") in state:
                state[params["configId"]] = params.get("value")
            send({"id": rid, "result": {"configOptions": options()}})
        elif rid is not None:
            send({"id": rid, "error": {"code": -32601, "message": f"Method not found: {method}"}})


if __name__ == "__main__":
    main()
