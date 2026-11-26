# Benterm

[日本語](README.ja.md)

A personal macOS terminal for running Claude Code and Codex side by side.

- Panes, tabs and workspaces, saved and restored across restarts.
- Each pane shows whether its agent is working, waiting for you, or done, and the sidebar lists every agent.
- An agent can start other agents and hand them work with the `benterm` command.
- A task gets its own git branch, worktree and workspace.

## Build

You need macOS, Git, and Rust 1.99 or newer from [rustup](https://rustup.rs).

```sh
git clone https://github.com/ahirakawa/benterm.git
cd benterm
```

With full Xcode and its Metal Toolchain:

```sh
cargo xtask bundle
```

With only the Command Line Tools (`xcode-select --install`):

```sh
cargo xtask bundle --runtime-shaders
```

The app is built at `target/release/Benterm.app` and signed ad hoc. An app built on the Mac it runs on opens without a warning.

## Install

```sh
cp -R target/release/Benterm.app /Applications/
open /Applications/Benterm.app
/Applications/Benterm.app/Contents/MacOS/benterm setup install
```

`setup install` adds hooks to `~/.claude/settings.json` and `~/.codex/hooks.json` so that Benterm learns the agents' states sooner, links `~/.local/bin/benterm` to the app, and links the `benterm` skill for Claude Code and Codex. It shows the changes and asks before writing, and keeps a copy of each file it changes. Codex runs the new hooks only after you review them with `/hooks`. `benterm setup status` checks the setup, and `benterm setup uninstall` takes it out again.

## Update

Quit Benterm with ⌘Q, then:

```sh
git pull
cargo xtask bundle
rm -rf /Applications/Benterm.app
cp -R target/release/Benterm.app /Applications/
```

The setup points into the app, so it needs no new `setup install`.

## Keys

| Key | Action |
|---|---|
| ⌘D / ⌘⇧D | Split the pane right / down |
| ⌘⌥ + arrow | Move to the pane in that direction |
| ⌃⌘ + arrow | Resize the pane |
| ⌘⇧↩ | Zoom the pane |
| ⌘⇧B | Progress board |
| ⌘⇧N | New task |

## Files

- Settings: `~/.config/benterm/config.toml` (every key is optional; see `crates/benterm_config/examples/config.toml`).
- Session, scrollback, tasks and agent history: `~/Library/Application Support/benterm`.

Run `benterm --help` for the commands.

## Security

See [SECURITY.md](SECURITY.md) for what Benterm trusts and how to report a vulnerability.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT), at your option.
