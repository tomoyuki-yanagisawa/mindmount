# Mindmount CLI 利用ガイド

対象: Phase 1（CLI / Storage）実装時点の挙動。

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
# 1. プロジェクト内の notes をマウント
mindmount storage mount -p ~/Project/app -d ./notes -n "Project notes"

# 2. マウントとページを確認
mindmount storage list
mindmount index pages

# 3. vim などで Markdown を編集したら index を更新
mindmount index rescan
mindmount index pages
```

## コマンド一覧

```text
mindmount storage mount -p <PATH> -d <PATH> [-n <NAME>]
mindmount storage list
mindmount storage unmount <MOUNT_ID>

mindmount index rescan
mindmount index pages [-m <MOUNT_ID>]
mindmount index search <QUERY> [-m <MOUNT_ID>]
mindmount index references <DOCUMENT_ID>
mindmount index backlinks <DOCUMENT_ID>

mindmount serve [--host <HOST>] [--port <PORT>]
mindmount mcp

mindmount -p <PATH> -d <PATH> [-n <NAME>] [--host <HOST>] [--port <PORT>]
```

| コマンド | 目的 |
|---|---|
| `storage mount` | ディレクトリを登録し、index を構築する |
| `storage list` | 登録済みマウントを一覧表示する |
| `storage unmount` | マウント登録と派生 index を削除する |
| `index rescan` | 全マウントの index を再構築する |
| `index pages` | index 済みページを一覧表示する |
| `index search` | index 済み Markdown を文字列で検索する |
| `index references` | ページからのリンクを一覧表示する |
| `index backlinks` | ページへのリンクを一覧表示する |
| `serve` | 読み取り専用 Web ビューアを起動する（Phase 4、未実装） |
| `mcp` | MCP stdio サーバーを起動する（Phase 3、未実装） |

引数だけの `mindmount -p ... -d ...` は、`storage mount` と同じ登録・index 構築を行います。

## storage mount

```bash
mindmount storage mount -p ~/Project/app -d ./notes -n "Project notes"
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
  indexed:   3 files, 3 added, 0 updated, 0 removed, 0 unchanged, 0 errors
```

### 登録時に起きること

1. パスの正規化と検証
2. マウントの重複・ネスト確認
3. `config.toml` への保存
4. SQLite のマウント一覧を同期
5. Markdown の走査と index 構築

index 構築に失敗してもマウント登録は残ります。その場合は
`mindmount index rescan` で再試行してください。

## storage list

```bash
mindmount storage list
```

出力:

```text
ID	NAME	DOCUMENTS	DIRECTORY
245510401681731584	Project notes	3	/Users/you/Project/app/notes
```

| 列 | 内容 |
|---|---|
| `ID` | マウント ID。`unmount` や `index pages --mount` で使う |
| `NAME` | 表示名 |
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
245510401685925888	Project notes	architecture.md	Architecture
245510401685925889	Project notes	index.md	Index
```

| 列 | 内容 |
|---|---|
| `ID` | ドキュメント ID。将来の `/page/:id` で使う |
| `MOUNT` | マウントの表示名 |
| `PATH` | マウント先からの相対パス |
| `TITLE` | frontmatter の `title`、最初の H1、ファイル名の順で決定 |

`--mount` に未登録の ID を指定するとエラーになります。

### 重要: `index pages` はファイルを再走査しない

`index pages` は SQLite の index を読むだけです。ファイルを直接編集しても、
`index rescan`（または同じディレクトリへの `storage mount` 再実行）をするまで
表示は更新されません。

```bash
vim ~/Project/app/notes/index.md
mindmount index pages      # まだ古い
mindmount index rescan     # ここで反映
mindmount index pages      # 新しい内容
```

## index references / backlinks

```bash
mindmount index references <DOCUMENT_ID>
mindmount index backlinks <DOCUMENT_ID>
```

ドキュメント ID は `index pages` で確認できます。Markdown リンクと `[[Wiki リンク]]` を
対象とし、リンク先は同じマウント内で解決します。Wiki リンクはページ名・タイトル・パスから
探し、候補が複数ある場合は未解決として記録します。外部 URL と未解決リンクの `TARGET_ID` は
`-` です。コードブロック・インラインコード内のリンクは対象外です。
パーセントエンコードされたパスの解決は未対応です。編集や移動後、またアップグレード後は
`index rescan` が必要です。

## index search

```bash
mindmount index search '検索したい文字列'
mindmount index search '設計' --mount 245510401681731584
```

タイトルと Markdown 全文を部分一致で検索します。検索語は構文ではなく文字列として扱い、
ASCII 英字の大文字・小文字は区別しません。その他の文字の大小文字の扱いは検索語の長さにより異なります。
1〜2 文字の検索は index 全体を走査するため、データ量が多いと遅くなります。
外部編集後は `index rescan` で検索結果を更新してください。既存の index を
アップグレードした場合も、初回検索前に `index rescan` が必要です。

## index rescan

```bash
mindmount index rescan
```

全マウントの Markdown を走査し、index を再構築します。出力例:

```text
Reindexed "Project notes" (245510401681731584): 3 files, 1 added, 1 updated, 1 removed, 0 unchanged, 0 errors
```

- 新規ファイルは追加
- 内容が変わったファイルは更新
- 削除されたファイルは index から削除
- 内容が同じファイルはスキップ
- 既存の内部 ID は可能な限り維持

複数マウントのうち一部が読めない場合、そのマウントのエラーを表示して残りを続行し、
最後に非ゼロで終了します。

## storage unmount

```bash
mindmount storage unmount 245510401681731584
```

出力:

```text
Unmounted "Project notes" (245510401681731584)
Markdown files were left untouched.
```

- `config.toml` からマウントを削除する
- SQLite のマウント、ドキュメント、参照データを削除する
- **Markdown ファイルは削除しない**

## 引数だけのルート実行

```bash
mindmount -p ~/Project/app -d ./notes -n "Project notes"
```

`storage mount` と同じ登録・index 構築を行います。Phase 4 以降は、同じコマンドで
読み取り専用 Web ビューアも起動する予定です。現在は次の注意書きを表示します。

```text
note: the read-only web viewer will listen on 127.0.0.1:3000 starting in Phase 4
```

## serve と mcp

```bash
mindmount serve
mindmount mcp
```

Phase 1 ではどちらも未実装です。

- `serve` は Phase 4 で**読み取り専用の Web ビューア**として実装する。
- `mcp` は Phase 3 で stdio MCP サーバーとして実装する。

ローカル版の Web UI では Markdown を編集しません。人間は vim などでファイルを
直接編集し、`index rescan` で反映します。Web UI での編集は提供しません。
詳細は [DECISIONS.md](DECISIONS.md) の D-001 を参照してください。

## ファイル編集と index 反映

Phase 1 にはファイル監視がありません。外部エディタでの変更は手動で再同期します。

```bash
# vim で編集
vim ~/Project/app/notes/index.md

# index に反映
mindmount index rescan

# 確認
mindmount index pages
```

同じディレクトリに `storage mount` を再実行しても再同期されます。

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

Phase 1 は簡易パーサで、トップレベルの `id` と `title` のみを読みます。

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
- 内部 ID は `index rescan` で内容が変わっても維持される
- frontmatter に `id` があれば、同一マウント内でファイルを移動しても ID を維持する。`id` がない場合、移動後は新しい ID になる
- DB を削除して再構築した場合、frontmatter に `id` がないドキュメントの内部 ID は変わり得る
- 同一マウント内で同じ ID が複数ドキュメントに使われている場合はエラーになり、そのマウントの index 更新は適用されない

### 変更検出

本文全体の SHA-256 を `content_hash` として保存し、前回と比較します。
mtime は保存しますが、変更判定には使いません。

## ローカルデータ

```text
$XDG_CONFIG_HOME/mindmount/   # 未設定時: ~/.config/mindmount/
├── config.toml               # マウント設定の正本
├── id-state                  # Snowflake ID の発行状態
├── mindmount.lock            # プロセス間の排他制御
└── db/
    └── mindmount.sqlite      # 派生 index
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
- SQLite は派生データ。削除しても `index rescan` で再構築できる
- `node_id` は 0〜1023。通常は変更しない
- `id-state` は ID の重複を防ぐため保持する。ID の範囲を先に予約するため、再起動後の ID の時刻部分が実時刻より進む場合がある
- ロック中の別プロセスがある場合、コマンドはエラーで終了する
- 手で設定を編集した場合も、次のコマンド実行時に SQLite のマウント一覧へ同期される

### DB をリセットする

```bash
rm -f ~/.config/mindmount/db/mindmount.sqlite \
      ~/.config/mindmount/db/mindmount.sqlite-wal \
      ~/.config/mindmount/db/mindmount.sqlite-shm
mindmount index rescan
```

マウント設定は `config.toml` に残るため、index だけが再構築されます。frontmatter に
`id` がないドキュメントの内部 ID は再生成されます。

## 環境変数

| 変数 | 説明 |
|---|---|
| `XDG_CONFIG_HOME` | 設定ディレクトリの基準。絶対パスであること |
| `HOME` | `XDG_CONFIG_HOME` 未設定時の `~/.config` 解決に使う |
| `RUST_LOG` | ログフィルタ。既定は `mindmount=warn,warn` |

ログは stderr に出力します。

`XDG_CONFIG_HOME` が相対パスの場合はエラーになります。

```bash
# 例: ログを詳細にする
RUST_LOG=mindmount=debug mindmount index rescan
```

## 終了コード

| コード | 内容 |
|---|---|
| `0` | 成功 |
| `1` | 実行時エラー（パス不正、DB エラー、未登録マウントなど） |
| `2` | CLI 引数のパースエラー |

`index rescan` は一部マウントで失敗しても残りを処理し、最後に `1` で終了します。

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
mindmount storage list
mindmount index rescan
```

### 編集したのに `index pages` が変わらない

`index pages` は index を読むだけです。`mindmount index rescan` を実行してください。

### `document id N is used by both ...`

同じ `id` を複数の Markdown に書いています。frontmatter の `id` を一意にしてください。

### `mount N is not registered`

`storage list` でマウント ID を確認してください。

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
- 内部の書き込み API は一時ファイル + fsync + rename を使う
- ローカル Web ビューアは Phase 4 で GET のみを提供する予定
- Markdown の HTML レンダリング時はサニタイズする予定

## 現時点の制約

- ファイル監視なし（Phase 2）
- MCP なし（Phase 3）
- Web ビューアなし（Phase 4）
- ローカル Web UI は閲覧専用（編集機能なし）
- frontmatter は簡易 YAML パーサ
- `index rescan` は毎回全ファイルを走査する
