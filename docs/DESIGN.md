# Mindmount 設計ドキュメント

対象: Phase 1（CLI / Storage）実装時点の設計。

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

未実装:

- ファイル監視
- 全文検索
- 参照・バックリンク
- MCP
- Web ビューア

単一ユーザー・ローカル専用です。Web UI での編集は提供しません。

## 3. アーキテクチャ

```text
main.rs
  ├── cli.rs        clap の定義
  └── app.rs        コマンドのオーケストレーション
        ├── config.rs        設定・マウント管理
        ├── db/sqlite.rs     SQLite の永続化
        ├── id.rs            Snowflake ID
        ├── index/indexer.rs ファイル走査と index 更新
        └── storage/filesystem.rs  マウント配下の安全なファイルアクセス
              └── core/document.rs  Markdown メタデータ
```

### モジュールの責務

| モジュール | 責務 |
|---|---|
| `main.rs` | ログ初期化、CLI パース、`app::run` の呼び出し |
| `cli.rs` | コマンド、引数、ヘルプの定義 |
| `app.rs` | コマンドの分岐、マウント登録、出力、エラー文脈の付与 |
| `config.rs` | XDG パス解決、`config.toml`、マウント検証、ノード ID |
| `id.rs` | Snowflake ID の発行 |
| `core/document.rs` | frontmatter、タイトル、SHA-256、ドメイン型 `Document` |
| `storage/filesystem.rs` | `MountRoot`、パス検証、読み取り、atomic write、走査 |
| `db/sqlite.rs` | migration、マウント同期、ドキュメントの upsert/delete/query |
| `index/indexer.rs` | 走査結果と DB の差分計算、トランザクション適用 |

依存方向は `app` が各機能を組み立て、`index` が `core` / `db` / `storage` / `id` を使う形です。
SQLite 固有の型は `db/sqlite.rs` の外へ漏らさず、呼び出し側は `Mount` や `Document` を扱います。

## 4. データフロー

### 4.1 マウント登録

```text
storage mount
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

### 4.2 index 再構築

```text
index rescan
  ├── config.toml を読み込む
  ├── DB の mounts を config に同期（削除されたマウントも反映）
  └── 各マウントについて
        ├── ファイル一覧を走査
        ├── 各ファイルを読み、SHA-256 を計算
        ├── 前回の content_hash と比較
        ├── 追加 / 更新 / 削除 / 変更なしを分類
        ├── ドキュメント ID の重複を検証
        └── 1 マウント分を SQLite トランザクションで適用
```

- 追加: DB に存在しない相対パス
- 更新: 相対パスは存在するが `content_hash` が違う
- 削除: 走査結果に存在しない相対パス
- 変更なし: `content_hash` が同じ
- Markdown 解析の変更に伴う migration は、既存のハッシュを無効化して次回 rescan でタイトルを更新する

読み取りに失敗したファイルは、既存の index を削除せずスキップします。
一時的なファイル不在や権限エラーで index が消えないようにするためです。

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
| Markdown 本文、リンク、タグ、タイトル | マウント先のファイル |
| マウント設定 | `config.toml` |
| index、content_hash、バックリンク | SQLite（派生） |

`config.toml` がマウント設定の正本です。SQLite の `mounts` テーブルは
コマンド実行時に `config.toml` から同期され、設定から消えたマウントは削除されます。

SQLite を削除しても、`index rescan` で index を再構築できます。
ただし frontmatter に `id` がないドキュメントの内部 ID は再生成されます。

### 5.2 SQLite スキーマ

```sql
CREATE TABLE mounts (
    id INTEGER PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,
    project_path TEXT NOT NULL,
    directory_path TEXT NOT NULL,
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
    UNIQUE (mount_id, relative_path),
    FOREIGN KEY (mount_id) REFERENCES mounts (id)
        ON DELETE CASCADE ON UPDATE CASCADE
);
```

- `references` テーブルは Phase 2 用に作成済みだが、まだ使用していない。
- 日時は SQLite に native な timestamp 型がないため、Unix ミリ秒の `INTEGER` で保存する。
- `mounts.directory_path` は canonical path を保存する。
- `documents.relative_path` はマウント先からの `/` 区切り相対パス。
- `(mount_id, relative_path)` に一意制約を置く。
- `documents.id` は主キー。frontmatter の `id` または内部 Snowflake ID。
- 外部キーは有効化し、WAL と 5 秒の busy timeout を使う。

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
| 41bit | カスタムエポックからのミリ秒 |
| 10bit | ノード ID |
| 12bit | 同一ミリ秒内の連番 |

カスタムエポック:

```text
2025-01-01T00:00:00Z = 1735689600000 ms
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

Phase 1 は簡易パーサです。トップレベルの `id` と `title` だけを読みます。

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

内部 API として次を実装済みです。Phase 1 の CLI には公開していません。

- `write_atomic`: 同一ディレクトリの一時ファイル + fsync + rename
- `create_new`: 既存ファイルがある場合は失敗
- 既存ファイルの permission を引き継ぐ

MCP の `write` Tool（Phase 3）では、`content_hash` による楽観的ロックを
呼び出し側で行います。

## 9. CLI 設計

```text
mindmount storage mount|list|unmount
mindmount index rescan|pages
mindmount serve
mindmount mcp
```

責務ごとにグループ化することで、トップレベルのコマンド増加を抑えます。
決定の背景は [DECISIONS.md](DECISIONS.md) の D-002 を参照してください。

`mindmount -p ... -d ...` のルート実行は、マウント登録と index 構築のショートカットです。
Phase 4 以降は Web ビューアも起動する予定です。

## 10. Web / MCP の設計方針

### 10.1 ローカル Web は閲覧専用

ローカル版の Web UI は **viewer** です。

- `GET /`、`GET /mounts`、`GET /page/:id`、`GET /search`、`GET /static/*`
- Markdown 表示、参照先、バックリンク、検索
- 編集画面、`GET /page/:id/edit`、`POST /page/:id` は持たない
- 人間は vim などでファイルを直接編集し、`index rescan` で反映する
- Web UI での編集は提供しない

理由と影響は [DECISIONS.md](DECISIONS.md) の D-001 を参照してください。

閲覧専用でも次は維持します。

- Markdown → HTML のレンダリングとサニタイズ
- Host / Origin 検証（DNS rebinding 対策）
- 静的ファイルのバイナリ埋め込み
- `127.0.0.1` のみの bind

### 10.2 MCP

Phase 3 で Rust 公式 SDK `rmcp` の stdio transport を使います。

予定している Tools:

- `mounts`
- `search`
- `read`
- `write`
- `list`
- `references`
- `backlinks`

`write` は既存ファイル更新時に `content_hash` による楽観的ロックを要求します。
stdio MCP では stdout にログを出さず、ログは stderr に限定します。

### 10.3 共通コア

Web と MCP は、config / db / index / storage の同じコアを共有します。
UI とプロトコルの層を分離し、どちらからも同じドメインロジックを使えるようにします。

- Web: 閲覧・検索・参照表示
- MCP: 読み書き・検索・参照
- 共通: マウント管理、index、ファイルアクセス、ID

## 11. エラー処理とログ

- アプリケーション境界では `anyhow` で文脈を付与する
- `id` と `db` は `thiserror` でドメインエラーを定義する
- index 構築では、1 ファイルの読み取り失敗を警告にして処理を続ける
- マウント単位の失敗は `index rescan` の最後にまとめて非ゼロ終了する
- ログは stderr。将来の stdio MCP でも stdout を汚さない
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
- DB: migration、マウント同期、カスケード削除、upsert/delete
- Indexer: 追加・更新・削除、内部 ID 維持、frontmatter ID、重複 ID、DB 再構築

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
- stdio MCP の stdout にログを出さない
- MCP から任意のシェルコマンドを実行させない
- MCP が返す文章を Mindmount 自身への指示として解釈しない
- ローカル Web は `127.0.0.1` のみに bind し、外部公開しない
- 閲覧専用でも Host / Origin 検証を行う

## 14. 現状の制約と未解決事項

### 制約

- ファイル監視がないため、外部編集は `index rescan` が必要
- `index rescan` は毎回全ファイルを走査する（ローカル MVP では許容）
- frontmatter は簡易 YAML パーサ
- 全文検索と日本語トークン分割は未実装
- 参照・バックリンクは未実装
- Web ビューアと MCP は未実装
- ローカル Web UI は閲覧専用（編集機能なし）
- 複数プロセス間の index 更新は完全には直列化していない
- Snowflake のノード ID 衝突を跨端末で完全には防げない
- DB 再構築時の内部 ID 永続性は保証しない
- 非 UTF-8 のファイル名・内容はスキップする
- symlink された Markdown は対象外

### 未解決事項

- 日本語検索に trigram と unicode61 のどちらを使うか
- Wiki リンクの同名ページ・曖昧性の解決規則
- frontmatter ID の移行と衝突解決
- 設定を `XDG_CONFIG_HOME` から `XDG_DATA_HOME` へ移す時期
- ファイル監視の debounce と atomic save の扱い
- Web UI の閲覧体験（検索・参照表示）の改善
