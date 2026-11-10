# Mindmount 設計ドキュメント

対象: Phase 2（CLI / Storage / index）までの実装を反映した設計。

設計上の決定は [DECISIONS.md](DECISIONS.md) を参照してください。

## 1. 目的

Mindmount は、自分のメモリを AI と共有するためのローカルサーバーです。
既存の Markdown を移動・変換せずにマウントし、AI から検索・参照できるようにします。

基本原則:

1. **Local First** — オフラインで動作する
2. **File First** — 既存ファイルを尊重し、正本は常にファイル
3. **MCP First** — AI 連携を主要機能とする
4. **Reference First** — 知識間の参照関係を重視する
5. **Zero Dependencies** — 利用時に DB サーバーや別ランタイムを要求しない
6. **Shared Core** — Web と MCP が同じコアを共有できる構造にする

## 2. 現在のスコープ

実装済み:

- CLI とマウント管理
- `config.toml` と SQLite の同期
- Snowflake ID
- Markdown の走査、タイトル・ID 抽出、変更検出
- index の差分更新と再構築
- ページ一覧
- 実ファイルへの grep 検索（本文 index なし）
- Markdown リンク・Wiki リンクの参照とバックリンク
- `index watch` によるファイル監視
- デーモン（Web / MCP HTTP / ファイル監視）
- MCP Streamable HTTP サーバー（Tools / Resources、stdio は任意）
- 閲覧専用 Web ビューア

未実装:

- なし（Phase 5 はリリース作業）

単一ユーザー・ローカル専用です。Web UI での編集は提供しません。

## 3. アーキテクチャ

```text
main.rs
  ├── cli/                CLI アダプタ
  │     ├── mod.rs        clap 定義、コマンド分岐、ロック
  │     └── output.rs     CLI 表示
  ├── daemon/             デーモン（Web / MCP HTTP / 監視）
  ├── mcp/                MCP サーバー（HTTP、任意の stdio アダプタ）
  ├── web/                閲覧専用 Web アダプタ
  ├── usecase/            CLI / MCP / Web が共有する操作（出力なし）
  │     ├── mount.rs      マウント登録・一覧・解除
  │     ├── index.rs      index・検索・参照
  │     └── document.rs   ページの読み書き
  ├── config/             設定・パス
  │     ├── mod.rs        config.toml、マウント検証
  │     └── paths.rs      XDG パス、ディレクトリ、ロック
  ├── core/               ドメイン型・識別子・解析
  │     ├── document.rs   Markdown メタデータ
  │     ├── id.rs         Snowflake ID
  │     └── reference.rs  リンク抽出・解決
  ├── db/                 SQLite 永続化（リポジトリ）
  ├── index/              走査・index 更新・監視
  └── storage/            マウント配下の安全なファイルアクセス

daemon/ は web/ と mcp/ を同じ Axum サーバーに載せ、index/watch を起動する。
CLI は共有 usecase を直接使い、Unix Socket や管理 API は設けない。
```

### モジュールの責務

| モジュール | 責務 |
|---|---|
| `main.rs` | ログ初期化、CLI パース、`cli::run` の呼び出し |
| `cli/mod.rs` | コマンド、引数、ヘルプの定義、分岐、ロック |
| `cli/output.rs` | CLI 向けの表示 |
| `daemon/mod.rs` | デーモン起動、Web と MCP HTTP の統合、監視 |
| `mcp/mod.rs` | MCP Tools / Resources、HTTP サービス、任意の stdio アダプタ |
| `web/mod.rs` | Axum / Askama による閲覧専用ビューア |
| `usecase/mount.rs` | マウント登録・一覧・解除（出力なし） |
| `usecase/index.rs` | 再 index・ページ・検索・参照（出力なし） |
| `usecase/document.rs` | ページの読み書きと楽観的ロック（出力なし） |
| `config/mod.rs` | `config.toml`、マウント検証、ノード ID |
| `config/paths.rs` | XDG パス解決、データディレクトリ、ロック |
| `core/document.rs` | frontmatter、タイトル、SHA-256、ドメイン型 `Document` |
| `core/id.rs` | Snowflake ID の発行と永続状態 |
| `core/reference.rs` | リンク抽出・解決、`ResolvedReference` |
| `storage/filesystem.rs` | `MountRoot`、パス検証、読み取り、atomic write、走査 |
| `db/mod.rs` | 接続、migration、`Database` と共通エラー |
| `db/mounts.rs` | マウント登録の同期 |
| `db/queries.rs` | ドキュメント、参照・バックリンクの読み取り |
| `db/sync.rs` | 変更ファイルと影響範囲の index を 1 トランザクションで適用 |
| `index/indexer.rs` | 走査、差分計算、参照解決、全マウント同期 |
| `index/watch.rs` | ファイル監視と再走査 |

依存方向は `cli` / `daemon` → `web` / `mcp`、`web` / `mcp` → `usecase` → `index` / `config` / `db` / `storage` / `core` です。
`db` は `core` のドメイン型だけを使い、Markdown 解析やリンク解決には依存しません。
SQLite 固有の型は `db` の外へ漏らさず、呼び出し側は `Mount` や `Document` を扱います。
`usecase` は出力を持たないため、Phase 3 の MCP と Phase 4 の Web から同じ処理を
再利用できます。

## 4. データフロー

### 4.1 マウント登録

```text
mount add
  ├── config.toml を読み込む（なければ作成し、node_id を永続化）
  ├── SQLite を開き migration を適用
  ├── project / directory を canonicalize
  ├── プロジェクト配下か検証
  ├── 既存マウントとの重複・ネストを検証
  ├── 新規なら Mount を作成（Snowflake ID）
  ├── config.toml を atomic write
  ├── DB の mounts テーブルを config に同期
  └── マウント先を走査して index を構築
```

同じディレクトリが登録済みの場合は、新規登録せずに index の再同期だけを行います。

### 4.2 index 更新

```text
index rescan / index watch / mount add
  ├── config.toml を読み込む
  ├── DB の mounts を config に同期（削除されたマウントも反映）
  └── 各マウントについて
        ├── ファイル一覧を走査
        ├── 各ファイルを読み、SHA-256 を計算
        ├── 前回の content_hash と比較
        ├── 追加 / 更新 / 削除 / 変更なしを分類
        ├── ドキュメント ID の重複を検証
        ├── 変更・移動・削除でリンク解決が変わるページを判定
        └── 変更ファイルと影響を受ける参照を 1 マウント分のトランザクションで更新
```

- 追加: DB に存在しない相対パス
- 更新: 相対パスは存在するが `content_hash` が違う
- missing: 走査結果に存在しない相対パス。行は削除せず `missing_at` を記録する
- 変更なし: `content_hash` が同じ
- Markdown 解析の変更に伴う migration は、既存のハッシュを無効化して次回 rescan でタイトルを更新する

通常は、変更されたファイルと、その変更によってリンク解決が変わるページだけを更新します。
取りこぼしが疑われる場合や、DB を作り直したい場合は `index rescan` で全体を再構築できます。

読み取りに失敗したファイル（権限エラーなど）は、既存の index を削除せずスキップします。
そのページへのリンク解決も維持し、他のページのバックリンクを失いません。
ファイルが存在しない場合は削除として扱います。監視中の一時的な不在も、再走査で
ファイルが戻れば次回の更新で復元されます。

### 4.3 ページ一覧

```text
index pages
  ├── config.toml を読み込む
  ├── DB の mounts を config に同期
  └── documents を query して表示
```

`index pages` は Markdown を走査しません。ファイル変更を反映するには
`index rescan` が必要です。

## 5. 永続データと派生データ

### 5.1 正本

| データ | 正本 |
|---|---|
| Markdown 本文、Markdown リンク、タグ、タイトル | マウント先のファイル |
| マウント設定 | `config.toml` |
| Mindmount 独自メタデータ、Knowledge Links、文書 ID とパスの対応 | SQLite（正本） |
| index、content_hash、Markdown リンクの解決状態、バックリンク | SQLite（派生） |

`config.toml` がマウント設定の正本です。SQLite の `mounts` テーブルは
コマンド実行時に `config.toml` から同期されます。設定から消えたマウントは
削除せず `is_active = 0` にし、文書・メタデータ・Knowledge Links を保持します。
同じディレクトリを再登録すると、同じマウント ID で再有効化できます。

DB 独自のメタデータと Knowledge Links は再生成できません。`index rescan` で作り直せるのは
派生 index だけです。ファイルが消えた文書も削除せず `missing_at` を記録し、孤立した文書を許容します。

### 5.2 SQLite スキーマ

```sql
CREATE TABLE mounts (
    id INTEGER PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,
    project_path TEXT NOT NULL,
    directory_path TEXT NOT NULL,
    is_active INTEGER NOT NULL DEFAULT 1,
    created_at INTEGER NOT NULL
);

CREATE UNIQUE INDEX mounts_directory_path_unique ON mounts (directory_path);

CREATE TABLE documents (
    id INTEGER PRIMARY KEY NOT NULL,
    mount_id INTEGER NOT NULL,
    relative_path TEXT NOT NULL,
    title TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    modified_at INTEGER NOT NULL,
    indexed_at INTEGER NOT NULL,
    missing_at INTEGER,
    UNIQUE (mount_id, relative_path),
    FOREIGN KEY (mount_id) REFERENCES mounts (id)
        ON DELETE CASCADE ON UPDATE CASCADE
);

CREATE TABLE document_metadata (
    document_id INTEGER NOT NULL,
    key TEXT NOT NULL,
    value_json TEXT NOT NULL,
    updated_at INTEGER NOT NULL,
    updated_by TEXT NOT NULL,
    PRIMARY KEY (document_id, key)
);

CREATE TABLE markdown_links (
    id INTEGER PRIMARY KEY NOT NULL,
    source_document_id INTEGER NOT NULL,
    target_document_id INTEGER,
    target_uri TEXT,
    target_anchor TEXT,
    raw_reference TEXT NOT NULL,
    source_heading TEXT,
    source_line INTEGER,
    resolution_status TEXT NOT NULL,
    source_kind TEXT NOT NULL
);

CREATE TABLE knowledge_links (
    id INTEGER PRIMARY KEY NOT NULL,
    source_document_id INTEGER NOT NULL,
    target_document_id INTEGER,
    target_uri TEXT,
    relation_type TEXT NOT NULL,
    description TEXT,
    provenance TEXT,
    created_by TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'proposed',
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
```

- `markdown_links` は Markdown / Wiki リンクの派生データ。`source_heading` と
  `source_line` に抽出位置、`resolution_status` に `resolved` / `unresolved` を保存する。
- `knowledge_links` は人間や AI が明示的に追加した関係。`status` は `proposed` / `confirmed`。
- `document_metadata` は Markdown に書かれていない DB 独自の情報。
- 日時は SQLite に native な timestamp 型がないため、Unix ミリ秒の `INTEGER` で保存する。
- `mounts.directory_path` は canonical path を保存する。
- `documents.relative_path` はマウント先からの `/` 区切り相対パス。
- `(mount_id, relative_path)` に一意制約を置く。
- `documents.id` は主キー。frontmatter の `id` または内部 Snowflake ID。
- 外部キーは有効化し、WAL と 5 秒の busy timeout を使う。
- 通常の同期では文書行を削除しない。削除されたファイルは `missing_at`、解除されたマウントは
  `is_active = 0` で保持する。

### 5.3 トランザクション

- マウント一覧の同期は 1 トランザクション。
- 1 マウント分の index 更新は 1 トランザクション。
- 複数マウントの `index rescan` はマウントごとに独立して適用する。

途中で失敗したマウントの index はロールバックされ、他のマウントは処理を続けます。

## 6. ID 設計

### 6.1 Snowflake 互換 ID

63bit の正の整数を使います。

| ビット | 内容 |
|---|---|
| 41bit | Unix エポックからのミリ秒 |
| 10bit | ノード ID |
| 12bit | 同一ミリ秒内の連番 |

エポック:

```text
1970-01-01T00:00:00Z = 0 ms
```

エポックは一度決めたら変更しません。

### 6.2 ノード ID

- `config.toml` の `node_id` に永続化する
- 範囲は 0〜1023
- 初回の設定作成時に生成する
- 通常は変更しない

### 6.3 重複防止

同一プロセス内では、最後に発行したタイムスタンプと連番を保持して単調増加を保証します。

- 時計が逆行した場合は、最後のタイムスタンプを下限として連番を進める
- 同一ミリ秒で連番が上限に達した場合は、次のミリ秒を待つ
- 時計が 5ms 以上進まない場合は、最後のタイムスタンプ + 1 にフォールバックする

異なるプロセスやオフライン端末で同じノード ID を使うと、同一ミリ秒に同じ ID が
生成される可能性は残ります。DB の主キー制約で衝突を検出し、将来的には
インポート時の衝突解決を実装します。

### 6.4 ドキュメント ID

- Mindmount が作成する Markdown には frontmatter の `id` を付与する想定
- 既存 Markdown に `id` がなければ内部 Snowflake ID を割り当てる
- 内部 ID は `index rescan` で内容が変わっても維持する
- DB 再構築後の永続性は保証しない
- 恒久的な参照には相対パスを使い、安定 ID が必要な場合は frontmatter に `id` を書く

### 6.5 URL

将来的な URL:

```text
/page/<document-id>
```

URL 生成は `PageUrl` 相当のモジュールに閉じ込め、日時を含む URL へ変更できる余地を残します。
Snowflake ID 自体は変更しません。

## 7. Markdown の扱い

### 7.1 走査対象

- `.md` / `.markdown`（大文字小文字を区別しない）
- 隠しファイル・隠しディレクトリは除外
- symlink は除外
- UTF-8 のみ

ファイルは読み取り専用で扱い、index 構築時に書き換えません。

### 7.2 タイトル

優先順:

1. frontmatter の `title`
2. コードフェンス外の最初の ATX H1
   - 見出し内の Markdown 装飾を除いたテキストを使用する
3. ファイル名（拡張子なし）

### 7.3 frontmatter

現在は簡易パーサです。トップレベルの `id` と `title` だけを読みます。

```yaml
---
id: 1234567890123456789
title: Architecture
---
```

- `id` は正の整数
- `title` はプレーンまたはクォート文字列
- 行末コメントを除去
- 閉じ `---` がない場合は frontmatter とみなさない
- ネストした YAML は未対応

本格的な YAML 対応は、必要になった時点で crate を追加して置き換えます。

### 7.4 変更検出

本文全体の SHA-256 を `content_hash` に保存します。
mtime は保存しますが、変更判定には使いません。これにより、mtime だけが変わった
ファイルを不要に再インデックスしません。

## 8. ファイルシステムの安全性

`MountRoot` がマウント先の canonical path を保持し、すべてのアクセスをその配下に限定します。

### 8.1 パス検証

- 絶対パスを拒否
- `..` を含むパスを拒否
- 空パスやファイル名を持たないパスを拒否
- 既存パスは canonicalize し、マウントルート配下か確認
- 書き込み時は親ディレクトリも canonicalize し、ルート配下か確認

### 8.2 symlink

- 走査時は symlink をスキップ
- 読み取り時は symlink 解決後のパスがルート配下か確認
- 書き込み時は symlink 自体への書き込みを拒否
- 親ディレクトリ経由の symlink 脱出も拒否

### 8.3 書き込み

内部 API として次を実装済みです。現在の CLI には公開していません。

- `write_atomic`: 同一ディレクトリの一時ファイル + fsync + rename
- `create_new`: 既存ファイルがある場合は失敗
- 既存ファイルの permission を引き継ぐ

MCP の `write` Tool（Phase 3）では、`content_hash` による楽観的ロックを
呼び出し側で行います。

## 9. CLI 設計

```text
mindmount daemon start|stop|restart|status|run
mindmount mount add|list|remove|sync
mindmount index rescan|watch|pages|search|references|backlinks
mindmount metadata get|set
mindmount link add|delete
mindmount serve
mindmount mcp
```

責務ごとにグループ化することで、トップレベルのコマンド増加を抑えます。
決定の背景は [DECISIONS.md](DECISIONS.md) の D-002 を参照してください。

`mindmount -p ... -d ...` のルート実行は、マウント登録と index 構築のショートカットです。
Phase 4 以降は Web ビューアも起動する予定です。

## 10. Web / MCP の設計方針

### 10.1 ローカル Web は閲覧専用

ローカル版の Web UI は **viewer** です。`mindmount serve` で起動します。

- `GET /`、`GET /mounts`、`GET /page/{id}`、`GET /search`、`GET /static/style.css`
- マウント一覧、Markdown 表示、検索、Markdown リンク、Knowledge Links、バックリンク、DB メタデータ
- Markdown 内の解決済みリンクと Wiki リンクは `/page/{id}` に変換
- 編集画面、`GET /page/{id}/edit`、`POST /page/{id}` は持たない
- 人間は vim などでファイルを直接編集し、`index rescan` で反映する
- 検索はサーバー描画の GET フォーム（htmx は使わない）

理由と影響は [DECISIONS.md](DECISIONS.md) の D-001 を参照してください。

閲覧専用でも次を維持します。

- Markdown → HTML のレンダリングと `ammonia` によるサニタイズ
- Host / Origin 検証（DNS rebinding 対策）
- CSS を含む静的ファイルの `include_str!` による埋め込み
- `127.0.0.1` / `::1` のみの bind

### 10.2 MCP

Rust 公式 SDK `rmcp` を使います。デーモンが Streamable HTTP を `/mcp` で提供し、
stdio は任意の薄いアダプタ（`mindmount mcp`）として残します。

Tools:

- `mounts`
- `list`
- `search`
- `read`
- `write`
- `links`
- `backlinks`
- `metadata_get`
- `metadata_set`
- `link_add`
- `link_delete`

Resources:

- `mindmount://<mount-id>/documents/<document-id>`

`read` は `document_id` または `mount_id` + `path` を受け付けます。`write` も同じ指定方法で、
既存ファイルの更新には `read` が返した `content_hash` を `expected_hash` として要求します。
新規作成では `expected_hash` を省略し、既存ファイルを上書きしません。

MCP サーバーは CLI や `index watch` と同じ `usecase` を使います。書き込み時だけアプリケーション
ロックを取得して index を同期します。stdio MCP では stdout にログを出さず、ログは stderr に
限定します。HTTP MCP も同じハンドラを利用します。

デーモンは `127.0.0.1` / `::1` にのみ bind し、`daemon start|stop|status|run` で管理します。
MCP HTTP は `credentials.toml` の Bearer Token を要求し、Web は Host / Origin を検証します。
CLI はデーモンを必須にせず、同じ usecase を直接使います。デーモンは設定ファイルの変更を
監視してマウントを追従します。

### 10.3 共通コア

Web と MCP は、`usecase` を通じて config / db / index / storage の同じコアを共有します。
UI とプロトコルの層を分離し、どちらからも同じドメインロジックを使えるようにします。

- Web: 閲覧・検索・参照表示
- MCP: 読み書き・検索・参照
- 共通: `usecase`（マウント管理、index、検索、参照）、ファイルアクセス、ID

## 11. エラー処理とログ

- アプリケーション境界では `anyhow` で文脈を付与する
- `id` と `db` は `thiserror` でドメインエラーを定義する
- index 構築では、1 ファイルの読み取り失敗を警告にして処理を続ける
- マウント単位の失敗は `index rescan` の最後にまとめて非ゼロ終了する
- ログは stderr。stdio MCP でも stdout を汚さない
- `RUST_LOG` でフィルタを変更できる。既定は `mindmount=warn,warn`

終了コード:

| コード | 内容 |
|---|---|
| `0` | 成功 |
| `1` | 実行時エラー |
| `2` | CLI 引数エラー |

## 12. テスト戦略

### 12.1 ユニットテスト

- Snowflake: 単調増加、同一ミリ秒の連番、時計逆行、連番枯渇、エポック前、ノード ID 範囲
- Config: 設定の作成・読み書き、ノード ID の永続化、ネスト・重複・不正値の拒否
- Document: frontmatter、タイトル優先順、コードフェンス、SHA-256
- Filesystem: 走査、隠しファイル、symlink、パストラバーサル、atomic write、上書き拒否
- DB: migration、マウント同期、カスケード削除、upsert/delete、参照の保持
- Indexer: 追加・更新・削除、内部 ID 維持、frontmatter ID、重複 ID、DB 再構築
- Reference: Markdown / Wiki リンクの抽出、マウント内の解決
- Document: ページの読み書き、`content_hash` による楽観的ロック、新規作成
- Search: 実ファイルの grep、日本語、文字列 / 正規表現、ファイル名絞り込み、行番号、前後文脈、件数上限
- MCP: initialize、tools/list、resources/list / read、read / write の stdio 結合テスト
- Daemon: バックグラウンド起動、Web、MCP HTTP の initialize、停止
- Web: マウント一覧、ページ表示、検索、Host / Origin 拒否、実サーバーの HTTP 応答

### 12.2 統合テスト

`tests/cli.rs` で実バイナリを起動し、次を確認します。

- マウント → 一覧 → ページ → 解除
- 既存 Markdown が変更されないこと
- 重複マウントの冪等性
- ネスト・プロジェクト外・存在しないディレクトリの拒否
- DB 削除後の `index rescan`
- ルート実行とヘルプ
- 未実装コマンドのフェーズ表示

### 12.3 実行

```bash
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
```

## 13. セキュリティ

- マウント外アクセス禁止（絶対パス、`..`、symlink）
- Markdown は外部入力として扱う
- HTML レンダリング時の XSS 対策（Phase 4）
- atomic file write と楽観的ロック（MCP write）
- MCP HTTP の Bearer Token 認証
- stdio MCP の stdout にログを出さない
- MCP から任意のシェルコマンドを実行させない
- MCP が返す文章を Mindmount 自身への指示として解釈しない
- ローカル Web は `127.0.0.1` のみに bind し、外部公開しない
- 閲覧専用でも Host / Origin 検証を行う

## 14. 現状の制約と未解決事項

### 制約

- `index watch` を起動していない場合、外部編集は `index rescan` が必要
- `index rescan` は毎回全ファイルを走査する（ローカル MVP では許容）
- index は Markdown より遅れて更新され得る。監視と 30 秒ごとの再走査で最終的に整合する
- frontmatter は簡易 YAML パーサ
- 検索は実ファイルへの grep。結果は行単位で、履歴 index は持たない
- Wiki リンクの解決はマウント内に限定し、同名候補が複数ある場合は未解決とする
- Web は閲覧専用で、検索はサーバー描画（部分更新なし）
- CLI はデーモン経由ではなく、同じ usecase をローカルで実行する
- ローカル Web UI は閲覧専用（編集機能なし）
- 複数プロセス間の index 更新は完全には直列化していない
- Snowflake のノード ID 衝突を跨端末で完全には防げない
- DB を削除すると、DB 独自メタデータ・Knowledge Links・内部 ID の対応は失われる
- ファイルが消えた文書は `missing_at` で保持し、孤立した文書を許容する
- 非 UTF-8 のファイル名・内容はスキップする
- symlink された Markdown は対象外

### 未解決事項

- Wiki リンクの同名ページ・曖昧性の解決規則
- frontmatter ID の移行と衝突解決
- 設定を `XDG_CONFIG_HOME` から `XDG_DATA_HOME` へ移す時期
- Web UI の閲覧体験（検索・参照表示）の改善
