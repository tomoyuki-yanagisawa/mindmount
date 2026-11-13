# Mindmount CLI 利用ガイド

対象: Phase 2（CLI / Storage / index）までの実装時点の挙動。

## 前提

- Rust 1.99 以上
- macOS または Linux
- ローカルに Markdown ディレクトリが存在すること

ビルド:

```bash
cargo build
```

以降の例ではインストール済みの `mindmount` を使います。開発中は
`cargo run --` に置き換えてください。

## クイックスタート

```bash
# 1. デーモンを起動
mindmount daemon start

# 2. プロジェクト内の notes をマウント
mindmount mount add -p ~/Project/app -d ./notes -n "Project notes"

# 3. マウントとページを確認
mindmount mount list
mindmount index pages

# 4. vim などで編集するとデーモンが自動で index を更新する
vim ~/Project/app/notes/index.md
mindmount index pages
```

CLI はデーモンの HTTP API を経由します。デーモンが停止している場合は
`Mindmount daemon is not running.` で失敗します。自動起動や SQLite への
直接アクセスは行いません。

## コマンド一覧

```text
mindmount daemon start|stop|restart|status|run [--host <HOST>] [--port <PORT>]

mindmount mount add -p <PATH> -d <PATH> [-n <NAME>]
mindmount mount list
mindmount mount remove <MOUNT_ID>
mindmount mount sync <MOUNT_ID> | --all

mindmount backup <PATH>
mindmount version

mindmount index pages [-m <MOUNT_ID>]
mindmount index search <QUERY> [-m <MOUNT_ID>] [--file <TEXT>] [--regex]
mindmount index references <DOCUMENT_ID>
mindmount index backlinks <DOCUMENT_ID>

mindmount -p <PATH> -d <PATH> [-n <NAME>] [--host <HOST>] [--port <PORT>]
```

| コマンド | 目的 |
|---|---|
| `daemon start` | Web・MCP HTTP・監視を OS サービスとして起動する |
| `daemon stop` | デーモンを停止する |
| `daemon restart` | デーモンを再起動する |
| `daemon status` | デーモンの稼働状態を表示する |
| `daemon run` | デーモンをフォアグラウンドで起動する |
| `mount add` | ディレクトリを登録し、index を構築する |
| `mount list` | 登録済みマウントを一覧表示する |
| `mount remove` | マウントを inactive にし、DB データを保持する |
| `mount sync` | 指定マウント、または `--all` で全マウントを再同期する |
| `backup` | SQLite の一貫したバックアップを作成する |
| `version` | バージョンを表示する |
| `index pages` | index 済みページを一覧表示する |
| `index search` | 実ファイルを grep 検索する |
| `index references` | ページからのリンクを一覧表示する |
| `index backlinks` | ページへのリンクを一覧表示する |
| `metadata get` | DB 独自メタデータを表示する |
| `metadata set` | DB 独自メタデータを設定する |
| `link add` | Knowledge Link を追加する |
| `link delete` | Knowledge Link を削除する |

引数だけの `mindmount -p ... -d ...` は、`mount add` と同じ登録・index 構築を行います。
`daemon` 以外のコマンドはデーモンの HTTP API を使うため、事前に `daemon start` が必要です。

## mount add

```bash
mindmount mount add -p ~/Project/app -d ./notes -n "Project notes"
```

### オプション

| Option | 説明 |
|---|---|
| `-p, --project <PATH>` | プロジェクトのルート。必須 |
| `-d, --directory <PATH>` | プロジェクト内の Markdown ディレクトリ。必須 |
| `-n, --name <NAME>` | 表示名。省略時はディレクトリ名 |

### マウントのルール

- `-p` と `-d` は実在するディレクトリでなければならない。自動作成はしない。
- `-d` が相対パスの場合は `-p` を基準に解決する。絶対パスも受け付けるが、`-p` の配下でなければならない。
- symlink は解決後の canonical path で保存する。canonical path がプロジェクト外になる場合は拒否する。
- 同じディレクトリを再指定した場合は重複登録せず、既存マウントの index を再同期する。表示名は既存のまま。
- ディレクトリのネスト登録は禁止する。既存マウントの祖先・子孫に当たるディレクトリは登録できない。
- 同じプロジェクト内の兄弟ディレクトリは登録できる。
- 表示名を省略した場合、または空白だけの場合はディレクトリ名を使う。

### 出力例

```text
Mounted "Project notes" (245510401681731584)
  project:   /Users/you/Project/app
  directory: /Users/you/Project/app/notes
  indexed:   3 files, 3 added, 0 updated, 0 missing, 0 unchanged, 0 errors
```

### 登録時に起きること

1. パスの正規化と検証
2. マウントの重複・ネスト確認
3. `config.toml` への保存
4. SQLite のマウント一覧を同期
5. Markdown の走査と index 構築

index 構築に失敗してもマウント登録は残ります。その場合は
`mindmount mount sync --all` で再試行してください。

## mount list

```bash
mindmount mount list
mindmount mount list --all
```

`--all` は inactive にしたマウントも表示します。

出力:

```text
ID	NAME	ACTIVE	DOCUMENTS	DIRECTORY
245510401681731584	Project notes	yes	3	/Users/you/Project/app/notes
```

| 列 | 内容 |
|---|---|
| `ID` | マウント ID。`mount remove` や `index pages --mount` で使う |
| `NAME` | 表示名 |
| `ACTIVE` | 現在有効かどうか |
| `DOCUMENTS` | index 済みページ数 |
| `DIRECTORY` | マウント先の canonical path |

マウントがない場合:

```text
No mounts registered.
```

## index pages

```bash
mindmount index pages
mindmount index pages --mount 245510401681731584
```

出力:

```text
ID	MOUNT	PATH	TITLE
245510401685925888	245510401681731584	architecture.md	Architecture
245510401685925889	245510401681731584	index.md	Index
```

| 列 | 内容 |
|---|---|
| `ID` | ドキュメント ID。`/page/:id` やリンク操作で使う |
| `MOUNT` | マウント ID |
| `PATH` | マウント先からの相対パス |
| `TITLE` | frontmatter の `title`、最初の H1、ファイル名の順で決定 |

`--mount` に未登録の ID を指定するとエラーになります。

### 重要: `index pages` はファイルを再走査しない

`index pages` は DB の index を読むだけです。デーモンが動いていればファイル変更は
自動で反映されます。反映されない場合は `mount sync --all` で再同期してください。

```bash
vim ~/Project/app/notes/index.md
mindmount index pages           # 監視が拾えば更新済み
mindmount mount sync --all      # 手動で再同期する場合
mindmount index pages
```

## ファイル監視

デーモンがすべてのマウントを監視します。起動時にすべてのマウントを走査し、その後は
ファイルの変更を監視して再走査します。イベントを 500ms まとめ、設定の変更も定期的に
確認します。通知漏れに備えて 30 秒ごとに全マウントを再走査します。
派生 index は Markdown より遅れて更新されることがあります。編集中に一時的に見えなくなった
ファイルは、その時点で index から外れる場合があります。

## index references / backlinks

```bash
mindmount index references <DOCUMENT_ID>
mindmount index backlinks <DOCUMENT_ID>
```

ドキュメント ID は `index pages` で確認できます。Markdown リンクと `[[Wiki リンク]]`、
DB に保存した Knowledge Links を表示します。リンク先は同じマウント内で解決します。Wiki リンクはページ名・タイトル・パスから
探し、候補が複数ある場合は未解決として記録します。外部 URL と未解決リンクの `TARGET_ID` は
`-` です。コードブロック・インラインコード内のリンクは対象外です。
Markdown リンクではパーセントエンコードされたパスも解決します。デーモンの監視により
編集・移動が自動で反映されます。

## metadata / link

```bash
mindmount metadata get <DOCUMENT_ID>
mindmount metadata set <DOCUMENT_ID> <KEY> <VALUE_JSON>

mindmount link add <SOURCE_ID> --target <TARGET_ID> --relation depends_on
mindmount link add <SOURCE_ID> --uri <TARGET_URI> --relation references
mindmount link delete <LINK_ID>
```

DB 独自メタデータと Knowledge Links を操作します。これらは Markdown から再生成できない
永続データです。再同期では削除されません。

## index search

```bash
mindmount index search '検索したい文字列'
mindmount index search '設計' --mount 245510401681731584
mindmount index search '設計.*メモ' --regex --file notes
```

マウント先の実ファイルを直接 grep します。索引を使わないため、`index` の更新状況に
関係なく常に現在のファイル内容を検索できます。

- 既定では検索語を文字列として扱い、大文字・小文字を区別しません
- `--regex` で正規表現として扱います
- `--file` で相対パスに含まれる文字列を絞り込めます
- 結果はファイルパス、行番号、行の内容で表示します
- 結果は最大 100 件です
- `--mount` でマウントを絞り込めます

## mount sync

```bash
mindmount mount sync <MOUNT_ID>
mindmount mount sync --all
```

全マウント、または指定マウントの Markdown を走査し、index を再構築します。通常は変更
ファイルと影響範囲だけが更新されるため、このコマンドは全体を作り直したいときの保険として
使えます。出力例:

```text
Reindexed "Project notes" (245510401681731584): 3 files, 1 added, 1 updated, 1 missing, 0 unchanged, 0 errors
```

- 新規ファイルは追加
- 内容が変わったファイルは更新
- 削除されたファイルは index から削除
- 内容が同じファイルはスキップ
- 既存の内部 ID は可能な限り維持

複数マウントのうち一部が読めない場合、そのマウントのエラーを表示して残りを続行し、
最後に非ゼロで終了します。

## mount remove

```bash
mindmount mount remove 245510401681731584
```

出力:

```text
Unmounted "Project notes" (245510401681731584)
Markdown files were left untouched.
Index metadata and knowledge links were kept inactive.
```

- `config.toml` からマウントを削除する
- SQLite のマウントは `is_active = 0` にして残す
- 文書、DB メタデータ、Knowledge Links は保持する
- 同じディレクトリを再登録すると、同じマウント ID で再有効化できる
- **Markdown ファイルは削除しない**

## 引数だけのルート実行

```bash
mindmount -p ~/Project/app -d ./notes -n "Project notes"
```

`mount add` と同じ登録・index 構築を行います。デーモンが動いていれば、そのまま
Web ビューアと MCP も利用できます。デーモンが停止している場合は次の注意書きを表示します。

```text
note: run `mindmount daemon start --host 127.0.0.1 --port 3000` to open the read-only web viewer
```

## daemon

```bash
mindmount daemon start
mindmount daemon status
mindmount daemon stop
mindmount daemon restart
mindmount daemon run
```

デーモンは Web ビューア、MCP Streamable HTTP、CLI HTTP API、ファイル監視を1プロセスで
提供します。`daemon start` は OS のサービス管理機構に登録して起動し、`daemon run` は
フォアグラウンドで起動します。

- Web: `http://127.0.0.1:3000/`
- MCP: `http://127.0.0.1:3000/mcp`
- API: `http://127.0.0.1:3000/api/*`
- bind は `127.0.0.1` / `::1` のループバックのみ
- macOS: launchd user agent、Linux: systemd user service
- MCP HTTP と CLI API は `credentials.toml` の `mcp_token` を
  `Authorization: Bearer <token>` で要求
- 二重起動はロックで拒否
- ログ: `$XDG_STATE_HOME/mindmount/mindmount.log`（未設定時 `~/.local/state/mindmount/`）
- `daemon status` はサービス状態、プロセス、API の応答を確認する
- サービス管理を使えない環境では `daemon run` を使う

### Web ビューア

表示できるもの:

- マウント一覧（`/`、`/mounts`）
- ページ本文と参照・バックリンク（`/page/<DOCUMENT_ID>`）
- grep 検索（`/search`）

Markdown 内の解決済みリンクと Wiki リンクはページ URL に変換されます。
HTML はレンダリング後にサニタイズされ、CSS はバイナリに埋め込まれています。
Host / Origin を検証するため、想定外のホスト名や Origin からのアクセスは拒否されます。

Web UI では Markdown を編集しません。人間は vim などでファイルを直接編集し、
デーモンの監視が自動で反映します。詳細は [DECISIONS.md](DECISIONS.md) の D-001 を参照してください。

## HTTP API

CLI はデーモンの `/api/*` を経由して操作します。Bearer Token は
`credentials.toml` の `mcp_token` を自動で読み込みます。

```text
GET    /api/admin/status
GET    /api/admin/mounts
POST   /api/admin/mounts
POST   /api/admin/mounts/sync
DELETE /api/admin/mounts/:id
POST   /api/admin/mounts/:id/sync
POST   /api/admin/backup

GET    /api/search
GET    /api/documents
GET    /api/documents/:id
GET    /api/documents/:id/links
GET    /api/documents/:id/backlinks
GET    /api/documents/:id/metadata
PUT    /api/documents/:id/metadata/:key
DELETE /api/documents/:id/metadata/:key
POST   /api/knowledge-links
DELETE /api/knowledge-links/:id
```

Markdown を編集・削除する API はありません。デーモンが停止している場合、CLI は
接続エラーとして失敗し、自動起動や SQLite へのフォールバックは行いません。

## MCP

MCP はデーモンの `/mcp` で Streamable HTTP として提供します。

Tools:

- `mounts` / `list` / `search`
- `read`
- `links` / `backlinks`
- `metadata_get` / `metadata_set`
- `link_add` / `link_delete`

`read` は `document_id` または `mount_id` + `path` を受け付けます。
Mindmount は Markdown を編集しないため、書き込み Tool は提供しません。

Resources として `mindmount://<mount-id>/documents/<document-id>` も公開します。

`credentials.toml` の `mcp_token` を `Authorization: Bearer <token>` で要求します。

```bash
cat ~/.config/mindmount/credentials.toml
```

MCP クライアントの設定例（形式はクライアントにより異なります）:

```json
{
  "mcpServers": {
    "mindmount": {
      "url": "http://127.0.0.1:3000/mcp",
      "headers": {
        "Authorization": "Bearer <mcp_token>"
      }
    }
  }
}
```

## ファイル編集と index 反映

デーモンが動いていれば、外部エディタでの変更は自動で index に反映されます。

```bash
# vim で編集
vim ~/Project/app/notes/index.md

# 確認
mindmount index pages
```

反映されない場合は `mindmount mount sync --all` で再同期してください。
同じディレクトリに `mount add` を再実行しても再同期されます。

## 対応する Markdown

- 拡張子 `.md` と `.markdown`（大文字小文字を区別しない）
- UTF-8 のテキストファイル
- 隠しファイル・隠しディレクトリ（`.` で始まるもの）は走査しない
- symlink は走査しない
- 読み取れないファイル、UTF-8 でないファイルはスキップし、エラー数に加算する。既存の index は削除しない

### タイトル

次の優先順で決定します。

1. frontmatter の `title`
2. コードフェンス外の最初の ATX H1（`# Title`）
   - 見出し内の Markdown 装飾を除いたテキストを使用する
3. ファイル名（拡張子なし）

### frontmatter

現在は簡易パーサで、トップレベルの `id` と `title` のみを読みます。

```yaml
---
id: 1234567890123456789
title: Architecture
---
```

- `id` は正の整数
- `title` はプレーンまたはクォートされた文字列
- 行末の `# comment` は除去
- 閉じ `---` がない場合は frontmatter として扱わない
- YAML のネストや複雑な構文は未対応

### ドキュメント ID

- frontmatter に `id` があればそれを使う
- なければ内部で Snowflake ID を発行する
- 内部 ID は再同期で内容が変わっても維持される
- frontmatter に `id` があれば、同一マウント内でファイルを移動しても ID を維持する。`id` がない場合、移動後は新しい ID になる
- DB を削除して再構築した場合、frontmatter に `id` がないドキュメントの内部 ID は変わり得る
- 同一マウント内で同じ ID が複数ドキュメントに使われている場合はエラーになり、そのマウントの index 更新は適用されない

### 変更検出

本文全体の SHA-256 を `content_hash` として保存し、前回と比較します。
mtime は保存しますが、変更判定には使いません。

## ローカルデータ

XDG 方式で config / data / state / cache を分離します。

```text
$XDG_CONFIG_HOME/mindmount/   # 未設定時: ~/.config/mindmount/
├── config.toml               # マウント設定の正本
└── credentials.toml          # CLI / MCP HTTP の Bearer Token

$XDG_DATA_HOME/mindmount/     # 未設定時: ~/.local/share/mindmount/
├── mindmount.sqlite          # 派生 index と DB 独自データ
└── id-state                  # Snowflake ID の発行状態

$XDG_STATE_HOME/mindmount/    # 未設定時: ~/.local/state/mindmount/
├── mindmount.daemon.lock     # デーモンの二重起動防止
├── daemon.pid                # デーモンの PID
├── daemon.endpoint           # CLI が接続する URL
└── mindmount.log             # サービス起動時のログ

$XDG_CACHE_HOME/mindmount/    # 未設定時: ~/.cache/mindmount/
```

`config.toml` の例:

```toml
node_id = 527

[[mounts]]
id = 245510401681731584
name = "Project notes"
project_path = "/Users/you/Project/app"
directory_path = "/Users/you/Project/app/notes"
created_at = "2026-11-09T11:20:04.405660Z"
```

- `config.toml` がマウント設定の正本
- SQLite は派生 index と、メタデータ・Knowledge Links などの永続データを含む。DB を削除すると後者は失われる
- 検索は実ファイルを直接読むため、SQLite に Markdown の本文は保存されない
- macOS / Linux では、起動時に各ディレクトリと DB ファイルからグループ・その他ユーザーのアクセス権を取り除く
- 旧レイアウトの `config/db/mindmount.sqlite` と `config/id-state` は初回起動時に data へ移動する
- `node_id` は 0〜1023。通常は変更しない
- `id-state` は ID の重複を防ぐため保持する。ID の範囲を先に予約するため、再起動後の ID の時刻部分が実時刻より進む場合がある
- デーモンが起動中は `config.toml` のマウント一覧が SQLite に同期される

### DB をリセットする

```bash
rm -f ~/.local/share/mindmount/mindmount.sqlite \
      ~/.local/share/mindmount/mindmount.sqlite-wal \
      ~/.local/share/mindmount/mindmount.sqlite-shm
mindmount daemon restart
mindmount mount sync --all
```

マウント設定は `config.toml` に残るため、index だけが再構築されます。frontmatter に
`id` がないドキュメントの内部 ID は再生成されます。

## 環境変数

| 変数 | 説明 |
|---|---|
| `XDG_CONFIG_HOME` | 設定ディレクトリの基準。絶対パスであること |
| `XDG_DATA_HOME` | DB と ID 状態の基準。未設定時は `~/.local/share` |
| `XDG_STATE_HOME` | PID・endpoint・ログの基準。未設定時は `~/.local/state` |
| `XDG_CACHE_HOME` | キャッシュの基準。未設定時は `~/.cache` |
| `HOME` | XDG 変数が未設定のときの基準 |
| `RUST_LOG` | ログフィルタ。既定は `mindmount=warn,warn` |
| `MINDMOUNT_DISABLE_SERVICE` | 設定すると `daemon start/stop` のサービス管理を無効化する（テスト用） |

ログは stderr に出力します。`daemon start` ではサービス管理機構がログを
`$XDG_STATE_HOME/mindmount/mindmount.log` に記録します。

`XDG_CONFIG_HOME` が相対パスの場合はエラーになります。

```bash
# 例: ログを詳細にする
RUST_LOG=mindmount=debug mindmount mount sync --all
```

## 終了コード

| コード | 内容 |
|---|---|
| `0` | 成功 |
| `1` | 実行時エラー（パス不正、DB エラー、未登録マウントなど） |
| `2` | CLI 引数のパースエラー |

`mount sync --all` は一部マウントで失敗しても残りを処理し、最後に `1` で終了します。

## トラブルシューティング

### `cannot access directory ...`

`-p` または `-d` のディレクトリが存在しません。Mindmount はディレクトリを自動作成しません。

### `must be inside the project`

`-d` がプロジェクト外を指しています。`-d` は `-p` の配下にしてください。

### `overlaps with existing mount`

既存マウントと祖先・子孫の関係にあります。MVP ではネスト登録できません。

### `No pages indexed.`

マウントが未登録、または index が空です。

```bash
mindmount mount list
mindmount mount sync --all
```

### 編集したのに `index pages` が変わらない

`index pages` は index を読むだけです。デーモンの監視が反映するまで待つか、
`mindmount mount sync --all` を実行してください。

### `document id N is used by both ...`

同じ `id` を複数の Markdown に書いています。frontmatter の `id` を一意にしてください。

### `mount N is not registered`

`mount list` でマウント ID を確認してください。

### `XDG_CONFIG_HOME must be an absolute path`

`XDG_CONFIG_HOME` には絶対パスを設定してください。

### `database is locked`

SQLite の WAL と 5 秒の busy timeout を使っています。別プロセスの処理が終わってから
再実行してください。

## セキュリティとデータ保護

- マウント外への `../` や絶対パスは拒否する
- symlink によるマウント外アクセスは拒否する
- index 構築は Markdown を変更しない
- `unmount` は Markdown を削除しない
- 設定ファイルは一時ファイル経由で atomic に書き換える
- ローカル Web ビューアは GET のみを提供する
- Markdown の HTML レンダリング時はサニタイズする

## 現時点の制約

- デーモンが停止していると CLI の通常操作は失敗する
- ローカル Web UI は閲覧専用（編集機能なし）
- frontmatter は簡易 YAML パーサ
- `mount sync --all` は毎回全ファイルを走査する
