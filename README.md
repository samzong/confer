# Confer

Confer exposes local coding-agent execution through MCP. Coding agents and other applications can create rooms, send work to Claude Code, Codex, Cursor Agent, Grok Build, Antigravity CLI, and GitHub Copilot CLI, and collect their results.

The host manages tasks and relays; it is not an execution seat. Supply `target_size` for automatic seat selection, explicit `seats`, or both. There is no default count or fixed seat limit, and identical agent configurations can occupy independent seats.

![Confer architecture](docs/confer-architecture.svg)

Every supported agent uses a private per-seat FIFO queue. Idle seats start promptly, busy seats preserve message order, and different seats may run concurrently.

## Install

```bash
brew install samzong/tap/confer
```
