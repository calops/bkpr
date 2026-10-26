## [0.4.2](https://github.com/calops/bkpr/compare/v0.4.1...v0.4.2) (2026-10-26)

## [0.4.1](https://github.com/calops/bkpr/compare/v0.4.0...v0.4.1) (2026-10-26)

### Bug Fixes

* **deps:** pin corrected native package version ([f65dcc0](https://github.com/calops/bkpr/commit/f65dcc0193a5b919500b8cff3779e063415723ee))

## [0.4.0](https://github.com/calops/bkpr/compare/v0.3.0...v0.4.0) (2026-10-26)

### Features

* share focused overlays for finder and creation forms ([90e7fcb](https://github.com/calops/bkpr/commit/90e7fcb9ac7c08a7f452b3be353fc9483a84e874))

## [0.3.0](https://github.com/calops/bkpr/compare/v0.2.2...v0.3.0) (2026-10-26)

### ⚠ BREAKING CHANGES

* **relay:** discover_agents returns compact records unless detail
is true, and title no longer defaults to "{harness} agent".

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
* **context:** Workspace scope now means the live Bkpr workspace, not repository/CWD grouping. Native 0.45.1-bkpr.3 and broker protocol 3 require fresh native sessions and explicit broker replacement; retained messages and receipts preserve their original scope.
* **bridge:** send_message requires an idempotency_key. Broker protocol
2 rejects older clients and brokers; restart the broker explicitly and
relaunch managed harnesses after preserving their pending work.

### Features

* add explicit harness launches and eager Codex messaging ([84c7621](https://github.com/calops/bkpr/commit/84c762199606eb18a5cb1598cbc474ceda6a99b0))
* **bridge:** make delivery state and retries explicit ([8f54543](https://github.com/calops/bkpr/commit/8f5454321926b9b4e2a3514d68a7b7fdd137ea94))
* **context:** scope agents to native workspaces ([2ee49d4](https://github.com/calops/bkpr/commit/2ee49d4d1a1f66bdb7ca1f004659ce4ab82c59c4))
* **control:** add typed CLI and MCP session control ([a508e67](https://github.com/calops/bkpr/commit/a508e67c2906d82bc1f49c523b2fc06b406183c5))
* **control:** use baseline APIs and add bottom-relative pane reads ([83d5893](https://github.com/calops/bkpr/commit/83d5893c09a6310672182fb37c17053b2bb37e93))
* merge live harness status discovery ([b056fe5](https://github.com/calops/bkpr/commit/b056fe53b50be9565816173642daefa3602e18cc))
* **omp:** bind native session ids for recovery ([b1e485e](https://github.com/calops/bkpr/commit/b1e485e8f9ec31d75e511bde5f81caad5e1c1e55))
* **recovery:** add bkpr resume and overwrite prompt ([bcbbc69](https://github.com/calops/bkpr/commit/bcbbc699a882294cd5352a28fcc1c16d3e1b892b))
* **recovery:** add restore-pane leaf launcher ([e56aef4](https://github.com/calops/bkpr/commit/e56aef49e9ce80beef77a5c1a8824aec07518120))
* **recovery:** add versioned recovery record and restore layout ([a31b8a0](https://github.com/calops/bkpr/commit/a31b8a0ef6db5cac7c4ba0e08c11462d0c106fd5))
* **recovery:** persist owner-claimed recovery records ([b62170e](https://github.com/calops/bkpr/commit/b62170ee0f68da42a35d121360fa703cae0dd145))
* **recovery:** resolve resumable harness sessions from relay state ([7d5e08f](https://github.com/calops/bkpr/commit/7d5e08f46a727de395c2b35c2cff0c46c0dff6f3))
* **recovery:** seed restored pane names and hold the restore baseline ([70b119c](https://github.com/calops/bkpr/commit/70b119c8b2f9dc638d9638aca198224facf37495))
* **recovery:** stream owner topology into recovery records ([4385dae](https://github.com/calops/bkpr/commit/4385dae7e39fbff472e11435fb555ee49feda94e))
* **recovery:** type harness resume commands into the restored shell ([6653bc7](https://github.com/calops/bkpr/commit/6653bc71efdf387f17b91afd4bc968fff6481ee2))
* **relay:** compact agent discovery with separate titles ([4af6d2c](https://github.com/calops/bkpr/commit/4af6d2cadf45225ba2503b25152eed9e25ce1c37))
* **relay:** expose live harness status in agent discovery ([a4a7e7b](https://github.com/calops/bkpr/commit/a4a7e7b7579b182e7e608f4878947ff5a9510eaa))
* **ui:** animate working harness clock hands clockwise ([d16e7e3](https://github.com/calops/bkpr/commit/d16e7e32fe35155d7b2019ab3babd64dff800c32))

### Bug Fixes

* allocate project colors across the whole session ([b8ecfb2](https://github.com/calops/bkpr/commit/b8ecfb24d7554df67f8564edad1f97695c3260f8))
* **bridge:** accept Codex native socket indirection ([fedef93](https://github.com/calops/bkpr/commit/fedef93fca8bc7966a52ea45ad3470a9a0690d46))
* **bridge:** keep the Codex MCP server name stable ([71d2752](https://github.com/calops/bkpr/commit/71d2752d54a2c84fc8b7a1a64539392a4c98877e))
* pin native runtime with terminal palette support ([1ece95b](https://github.com/calops/bkpr/commit/1ece95b9f55a2a9063e8f1c0a2fdc9e9ffc9d26a))
* preserve attachment to pre-context sessions ([3a6d9dd](https://github.com/calops/bkpr/commit/3a6d9ddcc365bc51f529c19141895a30fc252325))
* preserve sidebar controls and stop polling completed checks ([64f2535](https://github.com/calops/bkpr/commit/64f25350af398eae5a59211440debbd1c9162f73))
* **recovery:** detach the shared relay broker from the pane that starts it ([d4131b1](https://github.com/calops/bkpr/commit/d4131b1a4d8f21691ceb7a61dcc782a67d1295c3))
* **recovery:** harden restore layouts, owner sequencing and record ownership ([a8e5d9e](https://github.com/calops/bkpr/commit/a8e5d9e84ac25256adcbfefffaa70d58cd6a6549))
* **recovery:** hide the typed resume command and check flags by position ([a5c90e7](https://github.com/calops/bkpr/commit/a5c90e786fa02985ddf9a2e3eb8c1e83c0ea95ae))
* **recovery:** keep a refused harness resume's link for a later retry ([e43e08f](https://github.com/calops/bkpr/commit/e43e08fbc7a9684d8ced4e71544e689350b8ee5e))
* **recovery:** keep fresh launches working without recovery state ([e724309](https://github.com/calops/bkpr/commit/e72430910a75bfaf5441ed8e70420d6a932f7181))
* **recovery:** keep restored panes usable through job control and denial ([1b7c3c6](https://github.com/calops/bkpr/commit/1b7c3c6ef5aadf99e98e98be2fcfeed400265616))
* **recovery:** record hidden stack members the runtime parks as suppressed ([922e9f0](https://github.com/calops/bkpr/commit/922e9f0542c9d19fcfd2ac85c7e754cc8c7f87e5))
* **recovery:** refuse resume while the agent's own process is live ([26bf485](https://github.com/calops/bkpr/commit/26bf485fb60f8098dfe0206691dbed60690fe9ba))
* **recovery:** resume each native session from one restored pane only ([5f85912](https://github.com/calops/bkpr/commit/5f85912ceeb8fa64222a45f5999f6f6297e27a6a))
* **recovery:** retry unacknowledged recovery writes with backoff ([582a58e](https://github.com/calops/bkpr/commit/582a58ec6e4056018a3708abcb1b63f25323e575))
* **recovery:** start the relay broker before stopping a previous Codex server ([4d7909a](https://github.com/calops/bkpr/commit/4d7909a8ceed5bd9d1119039b5763d13dd0224c0))
* **relay:** prune exited agents and keep messaging on resume ([b0a4172](https://github.com/calops/bkpr/commit/b0a4172a408f412ae203d957e1c5b50184f4a5a8))
* **release:** keep automatic versions within 0.x ([31788d1](https://github.com/calops/bkpr/commit/31788d1b07fc7b024aa5bbaf11e413fa55b34f20))
* **sidebar:** preserve current titles across owner context updates ([34e7004](https://github.com/calops/bkpr/commit/34e7004f7125a4dac99b03922bbe959346a937e9))
* start attachments locked and highlight unlocked hints ([fb28ad9](https://github.com/calops/bkpr/commit/fb28ad900c1475b5786c58411d4fcaf24c5a4810))
* **ui:** apply native terminal colors across plugin roles ([7f9c5ae](https://github.com/calops/bkpr/commit/7f9c5ae4a3cb465d0da6c95e055a178b277f2e03))
* **ui:** soften accents and shade terminal-native surfaces ([4e2aeb4](https://github.com/calops/bkpr/commit/4e2aeb46b3b9df45135c4c5821df0dacb49051a2))
* **ui:** use the terminal palette for sidebar and popup styles ([a6bec51](https://github.com/calops/bkpr/commit/a6bec51bca6e0e79e9c5787d8df1eb7acebe06ff))
* use terminal ANSI colors for native frames ([dc57c95](https://github.com/calops/bkpr/commit/dc57c95299a27ec415672efc931192ce72873a4d))

## [0.2.2](https://github.com/calops/bkpr/compare/v0.2.1...v0.2.2) (2026-10-19)

### Bug Fixes

* **ci:** scope GitHub execution and sharing to item cwd ([a8d5744](https://github.com/calops/bkpr/commit/a8d5744349e81bee29d038f07bddd00549a40622))
* **nix:** keep Rust source paths out of the runtime closure ([ac76218](https://github.com/calops/bkpr/commit/ac76218bb4bf1ec7a5d9ccb35bc81c9f735e117f))
* **release:** publish tags and dispatch builds without channel notes ([f426e43](https://github.com/calops/bkpr/commit/f426e4319a399ce4426d2fc1ef9a7241f594f24a))

## [0.2.1](https://github.com/calops/bkpr/compare/v0.2.0...v0.2.1) (2026-10-19)

## [0.2.0](https://github.com/calops/bkpr/compare/v0.1.1...v0.2.0) (2026-10-19)

### Features

* **clipboard:** support image clipboard access through the native fork ([a4258e7](https://github.com/calops/bkpr/commit/a4258e70928c46780fc3325dd24a060b07a32d47))
* opt native sidebars into session-wide remembered widths ([81dca19](https://github.com/calops/bkpr/commit/81dca19b9f5d16ed99f06b2da5fd711177e0bd7d))

### Bug Fixes

* bound popup redraws and track remote CI heads ([901485e](https://github.com/calops/bkpr/commit/901485ea35f429177058b4420b3b647fb50e19f7))

## [0.1.1](https://github.com/calops/bkpr/compare/v0.1.0...v0.1.1) (2026-10-16)

### Bug Fixes

* **nix:** include bundled OMP extension in package source ([627e416](https://github.com/calops/bkpr/commit/627e41636d388bf30461cec488b134cf41493f2e))
