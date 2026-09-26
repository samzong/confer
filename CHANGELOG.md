# Changelog


## [0.2.2](https://github.com/samzong/confer/compare/v0.2.1...v0.2.2) (2026-09-26)


### Features

* **mcp:** count execution seats independently of the host ([#6](https://github.com/samzong/confer/issues/6))
* **adapters:** add GitHub Copilot CLI support ([#7](https://github.com/samzong/confer/issues/7))
* **adapters:** add Kimi Code via native ACP ([#8](https://github.com/samzong/confer/issues/8))
* **cli:** show local MCP processes and running seats ([#12](https://github.com/samzong/confer/issues/12))
* **mcp:** require agent when seat sets model or reasoning effort
* **mcp:** return native resume commands for seats
* **adapters:** add Devin for Terminal via the headless CLI bridge ([#14](https://github.com/samzong/confer/issues/14))


### Fixes

* **adapters:** recognize Kimi provider env credentials ([#9](https://github.com/samzong/confer/issues/9))
* **adapters:** scrub inherited ACP_BACKEND from Devin seats ([#15](https://github.com/samzong/confer/issues/15))


### Refactors

* **core:** simplify adapters and delivery coordination ([#10](https://github.com/samzong/confer/issues/10))
* **mcp:** own activity snapshots without durable sync ([#13](https://github.com/samzong/confer/issues/13))


## [0.2.1](https://github.com/samzong/confer/compare/v0.2.0...v0.2.1) (2026-09-07)


### Features

* store room state under XDG_STATE_HOME ([#5](https://github.com/samzong/confer/issues/5))


## [0.2.0](https://github.com/samzong/confer/compare/v0.1.0...v0.2.0) (2026-09-06)


### Fixes

* tighten delivery, seat, and workspace contracts ([#4](https://github.com/samzong/confer/issues/4))


### Performance

* **mcp:** wake waiters on delivery updates ([#2](https://github.com/samzong/confer/issues/2))


### Refactors

* **mcp:** split server responsibilities ([#1](https://github.com/samzong/confer/issues/1))
* **agents:** unify host identity and remove dead handshake
* **adapters:** unify agent execution through acp ([#3](https://github.com/samzong/confer/issues/3))


### Documentation

* **README:** add install command
* **features:** add features


## [0.1.0](https://github.com/samzong/confer/releases/tag/v0.1.0) (2026-09-04)


### Features

* release Confer 0.1.0
* **agents:** add Antigravity and enable full permissions
* **mcp:** add per-seat queues and seat lifecycle


### Refactors

* **mcp:** replace room lifecycle with session-scoped reuse


### Documentation

* move specification under docs
* focus specification on design


