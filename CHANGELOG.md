# Changelog


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


