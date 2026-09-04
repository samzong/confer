# Confer

Confer gives the current coding agent a local room for consulting Claude Code, Codex, Cursor Agent, Grok Build, and Antigravity CLI through MCP.

## Install

```bash
cargo install --path . --locked
confer mcp install
confer skill install --yes
```

MCP registration and Skill installation are independent. MCP registration detects Claude Code, Codex, Cursor, Grok, and Antigravity CLI. Skill installation remains limited to Claude Code, Codex, Cursor, and Grok.

## Commands

```bash
confer mcp
confer mcp capabilities
confer mcp install --dry-run
confer mcp uninstall --dry-run
confer skill install --dry-run --yes
```

Room operations are exposed only as MCP tools. See [SPEC.md](SPEC.md) for the contract.

## Development

```bash
make check
make install
```
