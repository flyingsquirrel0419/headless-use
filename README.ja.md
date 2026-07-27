# headless-use

[English](README.md) | [한국어](README.ko.md) | [日本語](README.ja.md) | [中文](README.zh.md)


> ウェブ開発エージェントのためのコンピュータ使用、ヘッドレスLinuxとCI向けに構築。

`headless-use`は、AIコーディングエージェントが構築したウェブアプリを**見て、
操作し、デバッグ**できるようにする軽量ブラウザランタイムです。Chrome DevTools
Protocol（CDP）でChromeを制御し、JavaScriptの`element.click()`ではなく**実際の
入力イベント**を使用します。エージェントにスクリーンショットベースの
「コンピュータ使用」と、トークンを節約するセマンティック参照（`@g1:e1`、
`@g1:e2`、…；短縮形`@eN`も可）の両方を提供します。

Node.jsランタイム不要の単一Rustバイナリで、XvfbなしでGUIのないサーバー、
Docker、CI環境で実行されるよう設計されています。

---

## なぜ必要か

ブラウザ自動化ツールは**テストスクリプト**のために作られています。エージェント
に必要なのは**リアルタイムの対話**です：ページを観察し、次の行動を決め、結果を
検証します。既存ツールはセレクタ中心・DOM専用で、エージェントに毎ステップの
再スクリーンショットを強います。`headless-use`はエージェント中心です：

| 一般的なブラウザ自動化                 | headless-use                              |
| ------------------------------------- | ----------------------------------------- |
| テストコードの作成                     | リアルタイムのエージェント操作             |
| CSSセレクタ                            | 座標**と**セマンティック参照               |
| DOMのみ                                | スクリーンショット + DOM + Console + Network |
| Node.js依存が一般的                    | 単一Rustバイナリ                           |
| ローカルデスクトップ中心               | ヘッドレスLinux、Docker、CI優先            |
| 結果のみ                               | セッショントレース、診断レポート           |

## 30秒クイックスタート

```bash
# ビルド（単一バイナリ、ランタイム依存なし）
cargo build --release

# 環境診断
./target/release/headless-use doctor
```

`serve`はstdin/stdout上でnewline-delimited JSON-RPCを話す長寿命セッションを
開始します（stdoutには**プロトコル応答のみ**が流れ、ログとバナーはstderrに
出ます）。リクエストは1つのプロセスにパイプで送ってください — 各`serve`が
自分のブラウザを所有します：

```bash
# ページを開き、インタラクティブ要素を観察し、参照でクリックし、入力。
printf '%s\n' \
  '{"id":1,"method":"browser.open","params":{"url":"http://localhost:3000"},"jsonrpc":"2.0"}' \
  '{"id":2,"method":"observe","params":{},"jsonrpc":"2.0"}' \
  '{"id":3,"method":"click","params":{"ref":"@g1:e1"},"jsonrpc":"2.0"}' \
  '{"id":4,"method":"type","params":{"text":"user@example.com"},"jsonrpc":"2.0"}' \
  '{"id":5,"method":"browser.close","params":{},"jsonrpc":"2.0"}' \
  | ./target/release/headless-use serve --no-sandbox
```

`observe`はページのメタデータとインタラクティブ要素の一覧を返します。各要素の
generation結合参照（`ref`）は`click`、`hover`、`screenshot`のターゲットとして
使えます：

```json
{
  "id": 2,
  "result": {
    "schemaVersion": 1,
    "page": { "url": "http://localhost:3000/", "title": "로그인", "viewport": { "width": 1280, "height": 720 } },
    "elements": [
      { "ref": "@g1:e1", "ref_id": 1, "role": "textbox", "name": "이메일" },
      { "ref": "@g1:e2", "ref_id": 2, "role": "textbox", "name": "비밀번호" },
      { "ref": "@g1:e3", "ref_id": 3, "role": "button",  "name": "로그인" }
    ],
    "generation": 1
  },
  "jsonrpc": "2.0",
  "schemaVersion": 1
}
```

（要素には他のフィールドもあります — bounding box、`visible`、`enabled`、
`checked`、`value`、`selectorHint` — ここでは省略。短縮形`@eN`も受け付けます
が、ナビゲーション後のstale参照を検知できるのは完全形式`@g<gen>:eN`だけです。）

デフォルトパスを軽く保つため、2つのコストはopt-inです：

- `observe`に`"listeners": true`を付けると、CDP経由でプログラム的に付加された
  クリックリスナーも検出します（候補要素あたり最大2回の追加CDPラウンドトリップ）。
- `click`に`"effects": true`を付けると、クリック後300msの効果
  （`dom_mutations`、`network_requests`、`navigated`、`focus_changed`）を
  サンプリングします。付けなければ`effects`は`null`で、安価なクリック前
  hit testのみ実行されます。

## ワンショットモード

```bash
# URLを開いてスクリーンショットを保存し、終了。
./target/release/headless-use run --url https://example.com --screenshot out.png
```

## インストール

### ソースから

```bash
cargo install --path .
# PATHにChrome/Chromiumが必要、またはHEADLESS_USE_BROWSER_PATHを設定。
```

### ブラウザ探索

`headless-use`は次の順でブラウザを探します：

1. `HEADLESS_USE_BROWSER_PATH`環境変数
2. `PATH`上の`chrome-headless-shell`、`chromium-headless-shell`、`chromium`、
   `chromium-browser`、`google-chrome`、`google-chrome-stable`

`--stealth`では順序が反転します：headless shellはボットチェックが読むブラウザ
APIを欠いているため、最後に試されます（[ステルスモード](#ステルスモード-experimental)
参照）。

明示的に指定：

```bash
headless-use launch --browser-path /opt/chrome-headless-shell
```

### Docker

```bash
docker build -f docker/Dockerfile -t headless-use .
# イメージはChromiumを同梱し、non-rootユーザーでWORKDIR /home/huで実行
# されます。出力用に書き込み可能なディレクトリをマウントしてください
# （先に mkdir -p output）：
docker run --rm --network host --shm-size=1g \
  --security-opt seccomp=unconfined \
  -v "$PWD/output:/home/hu/output" \
  headless-use \
  run --url http://127.0.0.1:3000 --screenshot output/page.png --no-sandbox
```

> **サンドボックス注記:** Chromiumはrootでは`--no-sandbox`なしに起動を拒否
> します。プロセスがrootの場合（CIコンテナで一般的）、`headless-use`が自動で
> 適用します。同梱のDockerfileはnon-rootユーザーで実行されますが、その場合も
> Dockerデフォルトのseccompプロファイルがサンドボックスを妨げることがあり
> ます — そのため上記例は同梱の`docker/docker-compose.yml`と同じく
> `--security-opt seccomp=unconfined` + `--no-sandbox`を使います。信頼できる
> 本番環境ではサンドボックスを有効のままseccompを調整する方を推奨します。
> [docs/security.md](docs/security.md)参照。

## 安定性：v1が保証するもの

**コアのブラウザ制御APIはv1.0.0から安定**で、semverに従います：

- CLIサブコマンド `serve`、`run`、`mcp`、`launch`、`doctor`、`install-browser`
- JSON-RPCメソッド `browser.open`/`page.goto`、`observe`、`click`、`hover`、
  `mouse.*`、`scroll`、`type`、`insert-text`、`key.*`、`wait`、`screenshot`、
  `console`、`network`、`evaluate`、`url`、`title`、`browser.close`
- 対応するMCP `browser_*`ツール
- エラーモデル（コードは安定した文字列）とレスポンスenvelope
  （`jsonrpc`、`schemaVersion` — 結果の形はフィールドが増えるのみ）

**Experimental** — 現在も使えますが、形とデフォルトはマイナーリリースで変わる
可能性があり、v1の安定性保証の対象外です：

- **ライブビューア**（`view`、`--viewer-*`フラグ、MJPEGストリーム）
- **Trace + Replay**（`trace.start`/`trace.stop`/`replay`、`actions.jsonl`、
  `report.html`）
- **ステルスモード**（`--stealth`）— フィンガープリント抑制は本質的に軍拡競争
- **Dewiggle**（`dewiggle`コマンド/メソッド/ツール）

## サポート機能

- **実入力**: `Input.dispatchMouseEvent`、`dispatchKeyEvent`、`insertText`
- **マウス**: 移動、クリック（left/right/middle/back/forward）、ダブル/トリプル、down/up、hold、hover、ホイールスクロール、ドラッグ（補間）、drag-path
- **キーボード**: down/up/press、コード（`Control+Shift+P`）、type、insert-text（CJK/絵文字対応）、hold、repeat
- **Observe**: DOMベースのインタラクティブ要素抽出、セマンティック参照`@g<gen>:eN`（stale検知のためのgeneration結合）、bounding box、あらゆるナビゲーションでのstale参照検知；opt-inのリスナースキャン（`"listeners": true`）はプログラム付加のクリックハンドラを持つ要素を昇格させ、`opaqueInteractive`面をフラグ
- **クリックレポート**: 全クリックがクリック前hit testを返す；opt-in（`"effects": true`）のクリック後効果サンプリングでdead clickを検知
- **診断**: console + 未捕捉エラー、network（CDP `Network.*`イベント — JSモンキーパッチではない）+ シークレットマスキング、wait-until-stable（アクティビティタイムスタンプベース、ポーリング間のリクエストも捕捉）
- **スクリーンショット**: viewport、full-page、要素領域（`--element @g1:e3`）
- **Dewiggle** *(experimental)*: アニメーションテキストCAPTCHAのグリフ別垂直揺れを**ピクセルのみ**で復元 — 解答配列やDOMテキスト/propsは読まない。Nフレームをキャプチャし、各列を基準線に再整列、平均化して鮮明な画像と任意のグリフ別クロップを生成。`headless-use dewiggle --url ... --out out.png --chars 6`
- **ステルス** *(experimental)*: `--stealth`は`--headless=new`を保ちながら自己申告をやめさせる — [ステルスモード](#ステルスモード-experimental)参照
- **セッション**: 長寿命`serve`（JSON-RPC stdio）、ワンショット`run`
- **Trace + Replay** *(experimental)*: `actions.jsonl`、`report.html`（自己完結、スクリーンショット埋め込み）、書き込み境界での強制シークレットマスキング、記録済みトレースを再実行する`replay`
- **MCPサーバー**: 仕様準拠の`initialize`/`tools/list`/`tools/call` over stdio

## MCPサーバー

`headless-use mcp`はstdio上で仕様準拠のMCPサーバー（protocolVersion
`2024-11-05`）を実行します。AIエージェントはJSON-RPCをラップせず直接接続：

```bash
headless-use mcp --no-sandbox
```

サーバーは型付き`inputSchema`を持つ19個の`browser_*`ツールを広告します。
スクリーンショット結果はMCP画像ブロックで、それ以外はコンパクトなJSONテキスト
ブロックで返ります。エラーは`isError: true`と復旧ヒントを返します。

### Claude Desktop / Cursor設定

MCPクライアント設定に追加：

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

### プロトコルフロー

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
├── launch       ブラウザを起動し維持
├── serve        stdio上の長寿命JSON-RPCセッションを開始
├── run          ワンショットアクションを実行して終了
├── dewiggle     (experimental) アニメーションテキスト領域をキャプチャしグリフ別揺れを復元（ピクセルのみ）
├── view         (experimental) ライブビューア + JSON-RPCセッション（下記参照）
├── replay       (experimental) runディレクトリの記録済みトレースを再実行
├── doctor       環境診断
├── install-browser   ブラウザインストール案内を表示
└── mcp          stdio上のMCPサーバーを開始
```

```bash
# アニメーションテキストCAPTCHAの揺れを復元し、グリフ別クロップを6個保存。
headless-use dewiggle --url https://example.com/captcha --out out.png --chars 6 --frames 12
```

### ステルスモード (experimental)

ボットチェック（Cloudflare Turnstileなど）の背後のサイトは、headful Chromeなら
素通りするチャレンジをheadless Chromeに突きつけます。`--stealth`は実ディスプレイ
のコストなしにその差を埋めます — `launch`、`serve`、`run`、`view`、`mcp`で
動作します：

```bash
headless-use run --url https://example.com/protected --screenshot out.png --stealth
headless-use serve --stealth --no-sandbox
```

重要な順に、変更点：

| レイヤー | 除去されるシグナル |
| --- | --- |
| 起動フラグ | `navigator.webdriver`（`--disable-blink-features=AutomationControlled`経由）、UA文字列の`HeadlessChrome/…` |
| `Emulation.setUserAgentOverride` | `Sec-CH-UA: "HeadlessChrome"` client-hintヘッダーと`navigator.userAgentData` — UAフラグだけでは直**らない** |
| プリロードスクリプト | SwiftShader WebGLドライバ文字列、空の`navigator.plugins`、欠けた`window.chrome`、`outerHeight == innerHeight`、ウィンドウと同サイズのscreen、`notifications: denied` |
| Auto-attach | チャレンジウィジェットが実際に動くcross-origin iframe内にも同じ処理 |

user agentはブラウザ自身のバージョンから導出されるため、UA・client-hintブランド
リスト・エンジンがすべて一致します。本ツール自身のconsole/networkコレクタを含む
すべての置換関数はnative source（`function … () { [native code] }`）を報告
します — ラップされた`fetch`はそれ自体がシグナルだからです。

補足：

- ステルスは`chrome-headless-shell`より**フル**のChrome/Chromiumを優先します。
  shellには`window.chrome`、PDFプラグイン項目、プロプライエタリコーデックが
  なく、説得力を持って偽装できません。shellしかなければ警告付きで使います。
- `--stealth`ではデフォルトが2つ変わります：スクロールバーを隠さなくなり
  （幅0のスクロールバーは既知のチェック）、WebGLが存在するようGPUプロセスを
  維持します（WebGLが全くないのはソフトウェアレンダラより大きなシグナル）。
- フィンガープリント抑制は軍拡競争です。headlessブラウザを即座に識別可能に
  するシグナルを除去するだけで、あらゆる検出器への保証ではありません。それでも
  チャレンジされる場合、`--compat xvfb`が約2倍のメモリでXvfb下の本物のheadful
  ブラウザを実行します。

### ライブビューア (experimental)

```bash
headless-use view --no-sandbox          # http://127.0.0.1:7780/?token=… を表示
```

`view`は`serve`と全く同じに動作しつつ（JSON-RPC on stdio）、追加でエージェント
カーソルオーバーレイ付きページのMJPEGストリームを提供します。

**アクセストークン。** 実行ごとにトークンが生成され、URLの一部としてstderrに
出力されます（stdoutはJSON-RPCチャネル）— 表示されたURLをそのまま開いて
ください。URLを固定したい場合は`--viewer-token <TOKEN>`で固定します。トークンが
*強制*されるかはバインドアドレス次第です：

| `--viewer-host` | 有効な`?token=`なしのリクエスト |
| --- | --- |
| loopback（`127.0.0.1`、デフォルト） | 提供される — トークンは受理されるが任意 |
| それ以外（`0.0.0.0`、LANアドレス） | `401 Unauthorized` |

```bash
headless-use view --viewer-host 0.0.0.0 --viewer-token "$(openssl rand -hex 16)"
```

**カーソルモーション。** `view`のデフォルトは`--cursor-motion smooth`：カーソル
がクリック/hover対象まで移動し、実際の中間`mouseMoved`イベントを発生させます。
ストリームを読みやすくし、実際の動きが必要なhoverメニューも開きます。クリック
ごとに移動時間（~220ms）がかかるため、`serve`/`run`/`mcp`のデフォルトは
`instant`です。どちらにも上書き可能：

```bash
headless-use view  --cursor-motion instant   # 最速、カーソルは瞬間移動
headless-use serve --cursor-motion smooth    # 遅いがhoverメニュー向き
```

> **露出注記:** ビューアはデフォルトで`127.0.0.1`にバインドします。
> `--viewer-host 0.0.0.0`はネットワークに開放し、その場合トークンが必須です。
> トークンはURL内のbearer資格情報なので、シェル/ブラウザ履歴や`Referer`
> ヘッダーに残り、ストリーム自体は平文HTTPです — そのURLを入手した者、
> あるいはトラフィックを見られる者は、ログイン済みコンテンツを含めページが
> 表示するすべてを見られます。信頼できないネットワークではトンネルするか
> TLSを前段に置いてください。[docs/security.md](docs/security.md)参照。

`serve`が受け付けるJSON-RPCメソッド：`browser.open`（別名`page.goto`）、
`observe`、`screenshot`、`click`、`hover`、`mouse.move`、`mouse.down`、
`mouse.up`、`scroll`、`mouse.drag`、`mouse.drag_path`、`type`、`insert-text`、
`key.press`、`key.down`、`key.up`、`wait`、`console`、`network`、`evaluate`、
`url`、`title`、`browser.close`、加えてexperimentalの`dewiggle`、
`trace.start`、`trace.stop`、`replay`。launch/runに`--json`を付けると機械可読
出力になります（`view`の`--json`ビューアバナーはstderrに出力 — stdoutは
プロトコル専用のまま）。

## エラーモデル

エージェントが復旧を判断できるよう、エラーは構造化されています。例：

```json
{
  "id": 14,
  "error": {
    "code": "STALE_REFERENCE",
    "message": "stale reference @e3",
    "recovery": "Reference @e3 is stale. Run the `observe` method again and use the new reference."
  },
  "jsonrpc": "2.0",
  "schemaVersion": 1
}
```

エラーコード：`BROWSER_NOT_FOUND`、`LAUNCH_FAILED`、`CONNECTION_FAILED`、
`PROTOCOL_ERROR`、`TIMEOUT`、`TARGET_CLOSED`、`ELEMENT_NOT_FOUND`、
`ELEMENT_NOT_INTERACTABLE`、`STALE_REFERENCE`、`INVALID_INPUT`、
`NAVIGATION_BLOCKED`、`NAVIGATION_FAILED`、`EVALUATION_FAILED`、
`UNEXPECTED_RESPONSE`、`TRACE_ERROR`、`DECODE_ERROR`、`IO_ERROR`、
`INTERNAL_ERROR`。[docs/protocol.md](docs/protocol.md)参照。

## Docker例

```bash
docker build -f docker/Dockerfile -t headless-use .
# -iはstdinを開いたままにします：serveはstdinからJSON-RPCを読み、EOFで終了。
docker run --rm -i --network host --shm-size=1g \
  --security-opt seccomp=unconfined \
  headless-use serve --no-sandbox
```

または同梱の[`docker/docker-compose.yml`](docker/docker-compose.yml)を
使ってください。

## 制限事項

- cross-origin iframeの操作は限定的で、文書化されたエラーが返ります。
- OSレベルのIME変換は完全にはエミュレートされません；CJK/絵文字は`Input.insertText`を使用。
- Firefox/WebKitは初期スコープ外（Chromium専用）。
- 自動ブラウザダウンロード（`install-browser`）は取得せず案内を表示。
- タッチ入力（`touch tap`、`touch swipe`）は構造的にサポートされますがMVP CLIにはありません。

## Playwright/Puppeteerとの違い

`headless-use`はPlaywrightをラップして**いません**。WebSocket上でCDPを直接
話し、プロセスライフサイクル（一時プロファイル掃除、ゾンビ防止、シグナル
処理）を自ら所有し、エージェントファーストAPI（参照 + observe + 構造化
エラー）を公開します。互換性確認のためPlaywrightと併用できますが、
Playwrightラッパーではありません。

## ロードマップ

- `report.html`インタラクティブタイムライン
- MIME処理付きHTML5ファイルドロップ
- チェックサム検証付き`install-browser`

## セキュリティ

[docs/security.md](docs/security.md)参照。要点：CDPは`127.0.0.1`のみに
バインド、トレースでのシークレットマスキング（パスワードフィールドの自動検出
含む）、エージェント提供のファイルパス（`trace.start`、`replay`）は作業
ディレクトリに制限、Chromeサイト分離は維持、ホストallow/denyポリシーが
ナビゲーションで強制されます（`--allow-host`/`--deny-host`）— その際
ナビゲーションは`http`/`https`にも制限され、`file:`/`data:`/`javascript:`
URLはホストルールをすり抜けられません。ライブビューアは`--viewer-host`で
広げない限りloopback専用で、non-loopbackバインドには`?token=`アクセス
トークン（`--viewer-token`）が必須です。loopbackではトークンは受理されますが
要求されません。

## コミュニティ

- [Contributing](CONTRIBUTING.md) — 開発環境、コード標準、PRプロセス
- [Code of Conduct](CODE_OF_CONDUCT.md) — コミュニティ標準
- [Security Policy](SECURITY.md) — 脆弱性報告
- [Changelog](CHANGELOG.md) — リリース履歴
- [Discussions](https://github.com/flyingsquirrel0419/headless-use/discussions) — 質問とアイデア

## ライセンス

Apache License, Version 2.0。[LICENSE](LICENSE)参照。
