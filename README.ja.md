# headless-use

[English](README.md) | [한국어](README.ko.md) | [日本語](README.ja.md) | [中文](README.zh.md)

> ウェブ開発エージェントのためのコンピュータ使用、ヘッドレスLinuxとCI向けに構築。

`headless-use`は、AIコーディングエージェントが構築したウェブアプリを**見て、操作し、デバッグ**できるようにする軽量ブラウザランタイムです。Chrome DevTools Protocol（CDP）でChromeを制御し、JavaScriptの`element.click()`ではなく**実際の入力イベント**を使用します。エージェントにスクリーンショットベースの「コンピュータ使用」と、トークンを節約するセマンティック参照（`@e1`、`@e2`、…）の両方を提供します。

Node.jsランタイム不要の単一Rustバイナリで動作し、XvfbなしでGUIのないサーバー、Docker、CI環境で実行されるよう設計されています。

---

## なぜ必要か

既存のブラウザ自動化ツールは**テストスクリプトの作成**向けです。エージェントには**リアルタイムの相互作用**が必要です：ページを観察し、次のアクションを決定し、結果を検証します。既存のツールはセレクタ中心・DOMのみで、各ステップごとの再スクリーンショットはエージェント任せです。`headless-use`はエージェント中心です：

| 一般的なブラウザ自動化            | headless-use                              |
| ------------------------------------- | ----------------------------------------- |
| テストコードの作成                     | リアルタイムエージェント操作                 |
| CSSセレクタ                         | 座標**と**セマンティック参照  |
| DOMのみ                              | スクリーンショット + DOM + コンソール + ネットワーク   |
| Node.js依存が一般的             | 単一Rustバイナリ                        |
| ローカルデスクトップ中心                 | ヘッドレスLinux、Docker、CI優先          |
| 結果のみ                           | セッショントレース、診断レポート |

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
# ページを開き、インタラクティブ要素を観察し、参照でクリックして入力する。
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

`--stealth`では順序が逆になります。ヘッドレスシェルはボット検知が読み取るブラウザAPIを欠いているため、最後に試されます（[ステルスモード](#ステルスモード)を参照）。

明示的に指定:

```bash
headless-use launch --browser-path /opt/chrome-headless-shell
```

### Docker

```bash
docker build -f docker/Dockerfile -t headless-use .
# イメージにChromiumが同梱。ワンショット実行:
docker run --rm --network host headless-use \
  run --url http://127.0.0.1:3000 --screenshot /output/page.png
```

> **サンドボックスに関する注意:** Chromiumはrootでは`--no-sandbox`なしで実行できません。Docker CIでは分離されたビルドなので許容されます。信頼できる本番環境ではnon-rootユーザーで実行（同梱のDockerfileがそうしています）し、サンドボックスを有効にしてください。[docs/security.md](docs/security.md)を参照。

## サポート機能

- **実際の入力**: `Input.dispatchMouseEvent`、`dispatchKeyEvent`、`insertText`
- **マウス**: 移動、クリック（左/右/中/戻る/進む）、ダブル/トリプル、down/up、hold、hover、ホイールスクロール、ドラッグ（補間）、drag-path
- **キーボード**: down/up/press、コード（`Control+Shift+P`）、type、insert-text（CJK/絵文字対応）、hold、repeat
- **観察**: DOMベースのインタラクティブ要素抽出、セマンティック`@g<gen>:eN`参照（世代に紐づきstale検出が可能）、バウンディングボックス、ナビゲーション発生時のstale参照検出
- **診断**: コンソール + 未捕捉エラー、ネットワーク（CDP `Network.*`イベント — JSのモンキーパッチではありません）とシークレットマスキング、wait-until-stable（アクティビティのタイムスタンプ基準なので、ポーリング間隔より短いリクエストも捕捉）
- **スクリーンショット**: ビューポート、全ページ、要素領域（`--element @eN`）
- **Dewiggle**: アニメーションするテキストCAPTCHAのグリフごとの上下の揺れを、**ピクセルのみ**で打ち消します — 解答配列もDOMのテキスト/プロパティも読みません。Nフレームを撮影し、各列を中立のベースラインへ再整列して平均化し、シャープ化した画像と（任意で）グリフごとの切り出しを出力します。`headless-use dewiggle --url ... --out out.png --chars 6`
- **ステルス**: `--stealth`は`--headless=new`のまま、ヘッドレスであることを名乗る信号を止めます — [ステルスモード](#ステルスモード)を参照
- **セッション**: 長期間`serve`（JSON-RPC stdio）、ワンショット`run`、トレース + レポート
- **トレース + リプレイ**: `actions.jsonl`、`report.html`（自己完結型、スクリーンショット埋め込み）、書き出し境界での強制的なシークレット秘匿、記録済みトレースを再実行する`replay`
- **MCPサーバー**: stdio経由の仕様準拠`initialize`/`tools/list`/`tools/call`

## MCPサーバー

`headless-use mcp`はstdio経由で仕様準拠のMCPサーバー（protocolVersion `2024-11-05`）を実行します。AIエージェントはJSON-RPCのラッピングなしで直接接続します:

```bash
headless-use mcp --no-sandbox
```

サーバーは型付き`inputSchema`を持つ19個の`browser_*`ツールを提供します。スクリーンショット結果はMCP画像ブロックとして、それ以外は簡潔なJSONテキストブロックとして返されます。エラーは`isError: true`と復旧ヒントを返します。

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

### プロトコルの流れ

```
client → initialize {protocolVersion, capabilities}
server ← {protocolVersion, capabilities, serverInfo}
client → notifications/initialized
client → tools/list
server ← {tools: [...19 browser_* tools...]}
client → tools/call {name: "browser_observe", arguments: {}}
server ← {content: [{type:"text", text:"{...elements...}"}], isError: false}
```

## CLIリファレンス

```
headless-use
├── launch       ブラウザを起動して維持
├── serve        stdio経由の長期JSON-RPCセッションを開始
├── run          ワンショットアクションを実行して終了
├── dewiggle     アニメーションするテキスト領域を撮影しグリフごとの揺れを打ち消す（ピクセルのみ）
├── view         ライブビューア + JSON-RPCセッションを提供（下記参照）
├── replay       実行ディレクトリに記録されたトレースを再実行
├── doctor       環境を診断
├── install-browser   ブラウザインストールガイダンスを表示
└── mcp          stdio経由のMCPサーバーを開始
```

```bash
# アニメーションするテキストCAPTCHAの揺れを打ち消し、グリフごとの切り出しを6枚保存。
headless-use dewiggle --url https://example.com/captcha --out out.png --chars 6 --frames 12
```

### ステルスモード

ボット検知（Cloudflare Turnstileなど）を備えたサイトは、ヘッドレスChromeにはチャレンジを出す一方、ヘッドフルChromeはそのまま通します。`--stealth`は実ディスプレイのコストを払わずにその差を埋めます。`launch`、`serve`、`run`、`view`、`mcp`で使えます:

```bash
headless-use run --url https://example.com/protected --screenshot out.png --stealth
headless-use serve --stealth --no-sandbox
```

重要な順に、何を変えるか:

| レイヤ | 消える信号 |
| --- | --- |
| 起動フラグ | `navigator.webdriver`（`--disable-blink-features=AutomationControlled`経由）、UA文字列中の`HeadlessChrome/…` |
| `Emulation.setUserAgentOverride` | `Sec-CH-UA: "HeadlessChrome"`クライアントヒントヘッダと`navigator.userAgentData` — UAフラグだけでは**直りません** |
| プリロードスクリプト | SwiftShaderのWebGLドライバ文字列、空の`navigator.plugins`、存在しない`window.chrome`、`outerHeight == innerHeight`、ウィンドウと同じ大きさの画面、`notifications: denied` |
| 自動attach | チャレンジウィジェットが実際に動くクロスオリジンiframeの内側にも同じ処理を適用 |

ユーザーエージェントはブラウザ自身のバージョンから生成するため、UA、クライアントヒントのブランド一覧、エンジンがすべて一致します。置き換えた関数はすべてネイティブソース（`function … () { [native code] }`）として報告します。本ツール自身のコンソール/ネットワーク収集器も含みます — ラップされた`fetch`はそれ自体が信号だからです。

注意:

- ステルスは`chrome-headless-shell`より**完全な**Chrome/Chromiumを優先します。シェルビルドには`window.chrome`もPDFプラグイン項目も独自コーデックもなく、これらは説得力を持って偽装できません。シェルしか無い場合は警告付きでそれを使います。
- `--stealth`では既定値が2つ変わります: スクロールバーを隠さなくなり（幅0のスクロールバーは知られた検知項目）、WebGLが存在するようGPUプロセスを維持します（WebGLが皆無であることはソフトウェアレンダラより目立ちます）。
- フィンガープリントの抑制はいたちごっこです。ヘッドレスブラウザを即座に識別させる信号は消しますが、あらゆる検知に対する保証ではありません。それでもチャレンジが出る場合、`--compat xvfb`はXvfb上で実際のヘッドフルブラウザを起動します（メモリはおよそ2倍）。

### ライブビューア

```bash
headless-use view --no-sandbox          # http://127.0.0.1:7780/
```

`view`は`serve`とまったく同じように動作し（stdio上のJSON-RPC）、加えてエージェントのカーソルオーバーレイ付きでページのMJPEGストリームを配信します。

**カーソルの動き。** `view`は既定で`--cursor-motion smooth`です: カーソルがクリック/ホバー先まで歩き、実際の中間`mouseMoved`イベントを発生させます。これがストリームを読みやすくし、実際の移動を必要とするホバーメニューも動かします。クリックごとに移動時間（約220ms）を要するため、`serve`/`run`/`mcp`の既定は`instant`です。どちら方向にも上書きできます:

```bash
headless-use view  --cursor-motion instant   # 最速、カーソルは瞬間移動
headless-use serve --cursor-motion smooth    # 低速、ホバーメニューに優しい
```

> **公開に関する注意:** ビューアは既定で`127.0.0.1`にバインドします。`--viewer-host 0.0.0.0`はネットワークに公開し、そのストリームは**認証されません** — そのアドレスに到達できる者は、ログイン済みの内容を含め、ページが表示している内容をすべて見られます。[docs/security.md](docs/security.md)を参照。

`serve`が受け付けるJSON-RPCメソッド（一部）: `browser.open`、`observe`、`screenshot`、`click`、`hover`、`mouse.move`、`mouse.down`、`mouse.up`、`scroll`、`mouse.drag`、`mouse.drag_path`、`type`、`insert-text`、`dewiggle`、`key.press`、`key.down`、`key.up`、`wait`、`console`、`network`、`browser.close`。launch/runには`--json`を付けると機械可読な出力になります。

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

## Dockerの例

```bash
docker build -f docker/Dockerfile -t headless-use .
docker run --rm --network host --shm-size=1g headless-use \
  serve --no-sandbox
```

## 制限事項

- クロスオリジンiframeの操作には制限があり、ドキュメント化されたエラーを返します。
- OSレベルのIME入力は完全にはエミュレートされません。CJK/絵文字は`Input.insertText`を使用します。
- Firefox/WebKitは初期スコープ外です（Chromiumのみ）。
- 自動ブラウザダウンロード（`install-browser`）はダウンロードではなくガイダンスを表示します。
- タッチ入力（`touch tap`、`touch swipe`）は構造的にサポートされていますがMVP CLIには含まれません。

## Playwright/Puppeteerとの違い

`headless-use`はPlaywrightをラップして**いません**。WebSocket経由でCDPを直接話し、プロセスのライフサイクル（一時プロファイルの後始末、ゾンビ防止、シグナル処理）を自ら所有し、エージェント優先のAPI（参照 + observe + 構造化エラー）を提供します。互換性チェックのためにPlaywrightと併用することはできますが、Playwrightのラッパーではありません。

## ロードマップ

- `report.html`のインタラクティブなタイムライン
- MIME処理を伴うHTML5ファイルドロップ
- チェックサム検証付きの`install-browser`

## セキュリティ

[docs/security.md](docs/security.md)を参照。要点: CDPは`127.0.0.1`にのみバインド、トレース中のシークレットはマスク（パスワードフィールドの自動検出を含む）、エージェントが指定するファイルパス（`trace.start`、`replay`）は作業ディレクトリ内に限定、Chromeのサイト分離は有効のまま、ナビゲーションにはホストの許可/拒否ポリシー（`--allow-host`/`--deny-host`）を適用します。このポリシーは同時にナビゲーションを`http`/`https`に限定するため、`file:`/`data:`/`javascript:`のURLでホストルールをすり抜けることはできません。ライブビューアは`--viewer-host`で広げない限りループバック限定で、広げた場合は設計上認証がありません。

## コミュニティ

- [コントリビューションガイド](CONTRIBUTING.md) — 開発セットアップ、コード標準、PRプロセス
- [行動規範](CODE_OF_CONDUCT.md) — コミュニティ標準
- [セキュリティポリシー](SECURITY.md) — 脆弱性報告
- [変更履歴](CHANGELOG.md) — リリース履歴
- [ディスカッション](https://github.com/flyingsquirrel0419/headless-use/discussions) — 質問 & アイデア

## ライセンス

Apache License, Version 2.0の下でライセンスされています。[LICENSE](LICENSE)を参照。
