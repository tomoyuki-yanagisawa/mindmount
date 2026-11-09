# Mindmount ロードマップ

現在の実装は [DESIGN.md](DESIGN.md) を参照してください。

Mindmount は、自分のメモリを AI と共有するための個人向けローカルツールです。
Web UI での編集は提供しません。

## ステータス

| Phase | 内容 | 状態 |
|---|---|---|
| 1 | CLI / Storage / 基本 index | 実装済み |
| 2 | ファイル監視、全文検索、参照、バックリンク | 未着手 |
| 3 | MCP サーバー | 未着手 |
| 4 | Web ビューア（閲覧専用） | 未着手 |
| 5 | リリース | 未着手 |

ローカル版は、MCP 連携と人間用の読み取り専用ビューアを中心に据えます。

## Phase 1: CLI / Storage（実装済み）

### 完了した内容

- CLI 引数とサブコマンド
  - `storage mount` / `storage list` / `storage unmount`
  - `index rescan` / `index pages`
  - 引数だけのルート実行
- マウント管理
  - 存在確認、canonicalize、プロジェクト配下の検証
  - 重複登録の防止、ネスト登録の禁止
  - 表示名のデフォルト
- `config.toml`
  - XDG 設定ディレクトリの解決
  - ノード ID の永続化
  - atomic write
  - SQLite への同期
- SQLite
  - `mounts` / `documents` / `references` の migration
  - WAL、外部キー、busy timeout
- Snowflake ID
  - 41bit 時刻 + 10bit ノード + 12bit 連番
  - 時計逆行と連番枯渇の処理
- Markdown index
  - `.md` / `.markdown` の走査
  - frontmatter の `id` / `title`
  - 最初の H1 とファイル名によるタイトル
  - SHA-256 による変更検出
  - 追加・更新・削除の差分反映
  - 内部 ID の維持
  - 1 マウント単位のトランザクション
- 再構築
  - `index rescan`（Phase 2 予定の reindex を前倒しで実装）
- セキュリティ
  - パストラバーサル拒否
  - symlink 脱出拒否
  - atomic write
  - Markdown を変更しない

### 完了条件

既存ディレクトリをマウントし、CLI からページ一覧を取得できること。達成済み。

### 現時点の制約

- ファイル監視なし
- 全文検索なし
- 参照・バックリンクなし
- MCP なし
- Web ビューアなし
- 簡易 YAML パーサ
- 毎回の全ファイル走査

## Phase 2: Index / References

### 目的

外部エディタによる Markdown の編集を自動で index に反映し、参照関係を扱えるようにする。

### タスク

- `notify` によるファイル監視
- debounce
- atomic save と一時的なファイル不在の扱い
- 起動時の全マウント同期
- ファイル名変更・Git checkout への追従
- FTS5 による全文検索
  - 日本語のトークン分割を検証
  - trigram などの方式を比較
- Markdown リンクの抽出
- Wiki リンクの抽出
- 外部 URL の記録
- リンク解決
  - 同一マウント内を優先
  - 明示的な相対パスを優先
  - Wiki リンクはページ名・パスから候補検索
  - 同名ページは曖昧として扱う
  - 未解決リンクも記録
- バックリンク
- `references` テーブルの利用
- 必要に応じて `index search`、`index references`、`index backlinks` を追加

### 完了条件

外部エディタで Markdown を編集すると、自動で index に反映されること。

### 設計上の論点

- 日本語検索のトークナイザ
- Wiki リンクの曖昧性解決
- ページ削除時の参照の扱い
- DB 再構築の性能

## Phase 3: MCP

### 目的

AI が同じ Markdown を読み書きできるようにする。

### タスク

- `rmcp` の stdio transport
- Tools
  - `mounts`
  - `search`
  - `read`
  - `write`
  - `list`
  - `references`
  - `backlinks`
- Resources
  - `mindmount://<mount-id>/documents/<document-id>`
- `write` の楽観的ロック
  - `content_hash` の一致を要求
  - 新規作成時の存在確認
- パス指定での読み書き
- OpenCode / Claude Code との結合テスト
- stdout にログを出さない運用

### 完了条件

AI が同じ Markdown を読み書きできること。

### 設計上の論点

- 更新競合時のエラー形式
- 書き込み時の index 同期
- Resources 非対応クライアント向けの `read` Tool
- MCP 経由の内容を Mindmount への指示として解釈しないこと

## Phase 4: Web Viewer（閲覧専用）

### 目的

人間がブラウザで AI と同じ知識を閲覧できるようにする。

**ローカル版では編集機能を実装しない。**
編集は vim などのエディタで行い、`index rescan` で反映する。
Web UI での編集は提供しない。詳細は
[DECISIONS.md](DECISIONS.md) の D-001 を参照。

### タスク

- Axum
- Askama
- htmx（閲覧・検索の部分更新）
- Markdown 表示（閲覧専用）
- 参照先・バックリンク表示
- 検索
- HTML サニタイズ
- 静的ファイルのバイナリ埋め込み
- `127.0.0.1` のみの bind
- Host / Origin 検証
- ローカル版ではログインなし

### ルート案

```text
GET /
GET /mounts
GET /page/:id
GET /search
GET /static/*
```

`GET /page/:id/edit` と `POST /page/:id` は設けない。

### 完了条件

人間がブラウザで AI と同じ知識を閲覧できること。

## Phase 5: Release

### タスク

- macOS / Linux ビルド
- GitHub Releases
- Homebrew Tap
- README の整備
- サンプルプロジェクト
- インストール手順
- バックアップ・移行手順

### 完了条件

外部ランタイムや DB サーバーなしで利用できること。

## MVP の非対象

- PostgreSQL / MySQL
- ベクトル検索
- LLM による自動知識生成
- 高度な Knowledge Graph
- Git 自動コミット
- AI による自動リファレンス生成
- ローカル Web UI での編集

## 直近の優先順位

1. ファイル監視と自動 index 更新
2. 参照抽出とバックリンク
3. 全文検索（日本語対応の検証を含む）
4. MCP stdio サーバー
5. 読み取り専用 Web ビューア

## 未解決の設計課題

- 日本語検索のトークン分割方式
- 参照の正規化と曖昧性解決
- frontmatter ID の衝突・移行
- 設定ディレクトリを `XDG_DATA_HOME` へ移す時期
- 複数プロセスからの同時 index 更新
- Snowflake ノード ID の端末間衝突
- Web UI の閲覧体験（検索・参照表示）の改善
