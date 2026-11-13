# tab-gpui

tab's desktop app on [GPUI](https://www.gpui.rs/), with
[gpui-component](https://github.com/longbridge/gpui-component) for inputs,
menus, switches and markdown. It runs the same engine as the Tauri app
through `crates/tab-app`.

```sh
cargo run --manifest-path crates/tab-gpui/Cargo.toml --features sample-data
```

`TAB_IDENTIFIER` picks the data folder and ports. It defaults to
`dev.tab.gpui`, so the GPUI app never opens the installed app's data.
`TAB_APPEARANCE=light|dark` overrides the system appearance.

On Linux it needs `libxcb1-dev`, `libxkbcommon-dev`, `libxkbcommon-x11-dev`,
`libfontconfig-dev`, `libfreetype-dev`, `libwayland-dev`, `libx11-dev` and
`libegl-dev` to build, and Vulkan drivers to run.

## What's here

| Tauri module | GPUI | Not yet |
| --- | --- | --- |
| Work | Queue, search, filters, keyboard, item, done / snooze / reopen, item chat with streaming replies and approvals | New chat, slash commands, attachments, context drawer, PR review |
| Projects | Coordinator chat, threads, notes, repos, about and instructions | Resources and knowledge, vault, compaction history, coordinator settings |
| Workflows | List, on / off, flow, JSON definition checked as you type, dry run and run, runs, approve a blocked step | The step builder, insights, factory view, model families |
| Connections | Installed, marketplace, a connector's settings, kinds and rate budget | Remote MCP servers, GitHub and email sign-in cards, API usage, running in tab cloud |
| Settings | Account, agents, sandbox, chat titles, spend limits, ranking, storage, MCP server | Moving the hub to tab cloud, tab account sign-in, updates |
| Onboarding | Welcome, agent, work folder | Hub step, installing an agent |

The editors (PR review, diffs) are a follow-up: Zed's editor crates are
GPL-3.0, which tab's license doesn't allow.
