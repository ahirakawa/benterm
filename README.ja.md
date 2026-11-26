# Benterm

[English](README.md)

Claude Code と Codex を並べて動かすための、個人用の macOS のターミナルです。

- ペイン、タブ、ワークスペースを使えます。再起動しても元に戻ります。
- 各ペインに、エージェントが作業中か、入力待ちか、終わったかを表示します。サイドバーには全エージェントの一覧が出ます。
- エージェントは `benterm` コマンドで別のエージェントを起動し、作業を任せられます。
- タスクごとに、git のブランチ、worktree、ワークスペースを作れます。

## ビルド

macOS、Git、[rustup](https://rustup.rs) で入れた Rust 1.99 以上が要ります。

```sh
git clone https://github.com/ahirakawa/benterm.git
cd benterm
```

Xcode と Metal Toolchain を入れている場合は、次を実行します。

```sh
cargo xtask bundle
```

Command Line Tools（`xcode-select --install`）だけの場合は、次を実行します。

```sh
cargo xtask bundle --runtime-shaders
```

アプリは `target/release/Benterm.app` にでき、アドホック署名されます。ビルドした Mac で開くときは、警告は出ません。

## 入れ方

```sh
cp -R target/release/Benterm.app /Applications/
open /Applications/Benterm.app
/Applications/Benterm.app/Contents/MacOS/benterm setup install
```

`setup install` は、`~/.claude/settings.json` と `~/.codex/hooks.json` に hook を書き足します。hook があると、Benterm はエージェントの状態を早く知れます。あわせて、`~/.local/bin/benterm` からアプリへのリンクと、Claude Code と Codex 用の `benterm` スキルへのリンクを作ります。書く前に変更点を見せて確認を取り、変えるファイルの写しも残します。Codex は、`/hooks` で確認するまで新しい hook を動かしません。`benterm setup status` で設定を確かめられ、`benterm setup uninstall` で取り除けます。

## 更新

⌘Q で Benterm を終了してから、次を実行します。

```sh
git pull
cargo xtask bundle
rm -rf /Applications/Benterm.app
cp -R target/release/Benterm.app /Applications/
```

設定はアプリの中を指しているので、`setup install` をやり直す必要はありません。

## キー操作

| キー | 動き |
|---|---|
| ⌘D / ⌘⇧D | ペインを右 / 下に分割する |
| ⌘⌥ + 矢印 | その向きのペインに移る |
| ⌃⌘ + 矢印 | ペインの大きさを変える |
| ⌘⇧↩ | ペインを広げる |
| ⌘⇧B | 進捗ボード |
| ⌘⇧N | 新しいタスク |

## ファイル

- 設定は `~/.config/benterm/config.toml` にあります。どの項目も省略でき、全項目は `crates/benterm_config/examples/config.toml` にあります。
- セッション、スクロールバック、タスク、エージェントの履歴は `~/Library/Application Support/benterm` にあります。

コマンドの一覧は `benterm --help` で見られます。

## セキュリティ

Benterm が何を信頼しているかと、脆弱性の報告の仕方は [SECURITY.md](SECURITY.md)（英語）にあります。

## ライセンス

[Apache License, Version 2.0](LICENSE-APACHE) と [MIT license](LICENSE-MIT) のどちらか、好きなほうで使えます。
