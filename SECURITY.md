# Security

## Reporting a vulnerability

Report it privately through GitHub: open the repository's Security tab and choose "Report a vulnerability". Please do not open a public issue for it.

Benterm is a personal project. Reports are read, but there is no promised response time.

## What Benterm trusts

Benterm runs on one Mac for one user. It protects the user's agents from each other, not the user's account from other programs the user runs.

- The app's socket and its state directory can be used only by the user who runs it.
- Every terminal gets its own credential for the socket. The panes the user opens may use the whole API. An agent started with `benterm agent run` gets a credential that lets it report its own state and report to the pane that started it; it may start and direct other agents only when started with `--control`.
- A credential works only from the processes of its own pane. A program outside Benterm, including one in tmux inside a pane, is refused.
- A report from a child agent is typed into its parent only when the parent is Claude Code or Codex. A parent shell does not get it, so a child cannot run commands there.
- Codex workers that Benterm starts in a `workspace-write` sandbox may write only to their workspace and their own mailbox, so they cannot pose as one another.

A program that runs as the same user outside a sandbox has the same access to the user's files and processes as Benterm, and can read a pane's credential from its environment. Benterm does not protect against such a program.

## Other precautions

- The git commands the app runs cannot start the filter programs a repository configures.
- The built-in browser opens only `http` and `https` pages. A link with a user name in it is not opened.
- The saved scrollback can hold whatever a program printed, such as a token. It is readable only by the user.
- `benterm setup install` changes `~/.claude/settings.json` and `~/.codex/hooks.json` only after showing the changes and asking, and keeps a copy of each file.
