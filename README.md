# Confer

Confer gives the current coding agent a local room for consulting Claude Code, Codex, Cursor Agent, Grok Build, and Antigravity CLI through MCP.

![Confer architecture](docs/confer-architecture.svg)

Every supported agent uses a private per-seat FIFO queue. Idle seats start promptly, busy seats preserve message order, and different seats may run concurrently.

## Install

```bash
cargo install --path . --locked
confer mcp install
confer skill install --yes
```

## Development

```bash
make check
make install
```
