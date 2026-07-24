# headless-use

[English](README.md) | [한국어](README.ko.md) | [日本語](README.ja.md) | [中文](README.zh.md)

> ウェブ開発エージェントのためのコンピュータ使用、ヘッドレスLinuxとCI向けに構築。

`headless-use`は、AIコーディングエージェントが構築したウェブアプリを**見て、操作し、デバッグ**できるようにする軽量ブラウザランタイムです。Chrome DevTools Protocol（CDP）でChromeを制御し、JavaScriptの`element.click()`ではなく**実際の入力イベント**を使用します。エージェントにスクリーンショットベースの「コンピュータ使用」と、トークンを節約するセマンティック参照（`@e1`、`@e2`、…）の両方を提供します。

Node.jsランタイム不要の単一Rustバイナリで動作し、XvfbなしでGUIのないサーバー、Docker、CI環境で実行されるよう設計されています。

---

## なぜ必要か

既存のブラウザ自動化ツールは**テストスクリプトの作成**向けです。エージェントには**リアルタイムの相互作用**が必要です：ページを観察し、次のアクションを決定し、結果を検証します。`headless-use`はエージェント中心です：

| 一般的なブラウザ自動化            | headless-use                              |
| ------------------------------------- | ----------------------------------------- |
| テストコードの作成                     | リアルタイムエージェント操作                 |
| CSSセレクタ                         | 座標**と**セマンティック参照  |
| DOMのみ                              | スクリーンショット + AX/DOM + コンソール + ネットワーク   |
| Node.js依存が一般的             | 単一Rustバイナリ                        |
| ローカルデスクトップ中心                 | ヘッドレスLinux、Docker、CI優先          |
| 結果のみ                           | セッショントレース、リプレイ、診断レポート |

## 30秒クイックスタート

```bash
# ビルド（単一バイナリ、ランタイム依存なし）
cargo build --release

# 環境を診断
./target/release/headless-use doctor

# 長期間実行するセッションを開始（stdio経由JSON-RPC）
./target/release/headless-use serve --no-sandbox
```

別のターミナルで、実行中のセッションにJSON-RPCリクエストを送信します:

```bash
printf '%s\n' \
  '{"id":1,"method":"browser.open","params":{"url":"http://localhost:3000"},"jsonrpc":"2.0"}' \
  '{"id":2,"method":"observe","params":{},"jsonrpc":"2.0"}' \
  '{"id":3,"method":"click","params":{"ref":"@e1"},"jsonrpc":"2.0"}' \
  '{"id":4,"method":"type","params":{"text":"user@example.com"},"jsonrpc":"2.0"}' \
  '{"id":5,"method":"browser.close","params":{},"jsonrpc":"2.0"}' \
  | ./target/release/headless-use serve --no-sandbox
```

`observe`はインタラクティブな要素のリストを安定した参照と共に返します:

```
[@e1] textbox "メール"
[@e2] textbox "パスワード"
[@e3] button "ログイン"
[@e4] link "新規登録"
[@e5] checkbox "" [unchecked]
```

## ワンショットモード

```bash
# URLを開きスクリーンショットを保存して終了
./target/release/headless-use run --url https://example.com --screenshot out.png
```

## インストール

### ソースから

```bash
cargo install --path .
# PATHにChrome/Chromiumが必要、またはHEADLESS_USE_BROWSER_PATHを設定
```

### ブラウザの検出

`headless-use`は以下の順序でブラウザを探します:

1. `HEADLESS_USE_BROWSER_PATH`環境変数
2. `PATH`上の`chrome-headless-shell`、`chromium`、`google-chrome`、`google-chrome-stable`

明示的に指定:

```bash
headless-use launch --browser-path /opt/chrome-headless-shell
```

### Docker

```bash
docker build -t headless-use .
# イメージにChromiumが同梱。ワンショット実行:
docker run --rm --network host headless-use \
  run --url http://127.0.0.1:3000 --screenshot /output/page.png
```

> **サンドボックスに関する注意:** Chromiumはrootでは`--no-sandbox`なしで実行できません。Docker CIでは分離されたビルドなので許容されます。信頼できる本番環境ではnon-rootユーザーで実行（同梱のDockerfileがそうしています）し、サンドボックスを有効にしてください。[docs/security.md](docs/security.md)を参照。

## サポート機能

- **実際の入力**: `Input.dispatchMouseEvent`、`dispatchKeyEvent`、`insertText`
- **マウス**: 移動、クリック（左/右/中/戻る/進む）、ダブル/トリプル、down/up、hold、hover、ホイールスクロール、ドラッグ（補間）、drag-path
- **キーボード**: down/up/press、コード（`Control+Shift+P`）、type、insert-text（CJK/絵文字対応）、hold、repeat
- **観察**: アクセシビリティ/DOM抽出、セマンティック`@eN`参照、バウンディングボックス、stale参照検出
- **診断**: コンソール + 未捕捉エラー、ネットワーク（fetch/XHR）シークレットマスキング、wait-until-stable
- **スクリーンショット**: ビューポート、全ページ、要素
- **セッション**: 長期間`serve`（JSON-RPC stdio）、ワンショット`run`、トレース + リプレイ
- **トレース**: `actions.jsonl`、スクリーンショット、`report.html`（自己完結型）、シークレット自動マスキング
- **MCPサーバー**: stdio経由の仕様準拠`initialize`/`tools/list`/`tools/call`

## MCPサーバー

`headless-use mcp`はstdio経由で仕様準拠のMCPサーバー（protocolVersion `2024-11-05`）を実行します。AIエージェントはJSON-RPCのラッピングなしで直接接続します:

```bash
headless-use mcp --no-sandbox
```

サーバーは型付き`inputSchema`を持つ18個の`browser_*`ツールを提供します。スクリーンショット結果はMCP画像ブロックとして、それ以外は簡潔なJSONテキストブロックとして返されます。エラーは`isError: true`と復旧ヒントを返します。

### Claude Desktop / Cursor設定

MCPクライアント設定に追加:

```json
{
  "mcpServers": {
    "headless-use": {
      "command": "/usr/local/bin/headless-use",
      "args": ["mcp", "--no-sandbox"]
    }
  }
}
```

## CLIリファレンス

```
headless-use
├── launch      ブラウザを起動して維持
├── serve        stdio経由の長期JSON-RPCセッションを開始
├── run          ワンショットアクションを実行して終了
├── doctor       環境を診断
├── install-browser   ブラウザインストールガイダンスを表示
└── mcp          stdio経由のMCPサーバーを開始
```

## エラーモデル

エラーはエージェントが復旧を決定できるよう構造化されています:

```json
{
  "id": 14,
  "error": {
    "code": "STALE_REFERENCE",
    "message": "stale reference @e3",
    "recovery": "Reference @e3 is stale. Run `headless-use observe` again and use the new reference."
  }
}
```

エラーコード: `BROWSER_NOT_FOUND`、`LAUNCH_FAILED`、`CONNECTION_FAILED`、`PROTOCOL_ERROR`、`TIMEOUT`、`TARGET_CLOSED`、`ELEMENT_NOT_FOUND`、`ELEMENT_NOT_INTERACTABLE`、`STALE_REFERENCE`、`INVALID_INPUT`。

## 制限事項

- クロスオリジンiframeの操作には制限があり、ドキュメント化されたエラーを返します。
- OSレベルのIME入力は完全にはエミュレートされません。CJK/絵文字は`Input.insertText`を使用します。
- Firefox/WebKitは初期スコープ外です（Chromiumのみ）。
- 自動ブラウザダウンロード（`install-browser`）はダウンロードではなくガイダンスを表示します。
- タッチ入力（`touch tap`、`touch swipe`）は構造的にサポートされていますがMVP CLIには含まれません。

## コミュニティ

- [コントリビューションガイド](CONTRIBUTING.md) — 開発セットアップ、コード標準、PRプロセス
- [行動規範](CODE_OF_CONDUCT.md) — コミュニティ標準
- [セキュリティポリシー](SECURITY.md) — 脆弱性報告
- [変更履歴](CHANGELOG.md) — リリース履歴
- [ディスカッション](https://github.com/headless-use/headless-use/discussions) — 質問 & アイデア

## ライセンス

Apache License, Version 2.0の下でライセンスされています。[LICENSE](LICENSE)を参照。
