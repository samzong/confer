# Confer

Confer gives the current coding agent a local room for consulting Claude Code, Codex, Cursor Agent, and Grok Build through MCP.

## Install

```bash
cargo install --path . --locked
confer mcp install
confer skill install --yes
```

MCP registration and Skill installation are independent. Both commands detect only the four supported agents.

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
