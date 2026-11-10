# Mindmount ロードマップ

現在の実装は [DESIGN.md](DESIGN.md) を参照してください。

Mindmount は、自分のメモリを AI と共有するための個人向けローカルツールです。
Web UI での編集は提供しません。

## ステータス

| Phase | 内容 | 状態 |
|---|---|---|
| 1 | CLI / Storage / 基本 index | 実装済み |
| 2 | ファイル監視、grep 検索、参照、バックリンク | 実装済み |
| 3 | MCP サーバー | 実装済み |
| 4 | Web ビューア（閲覧専用） | 実装済み |
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

### Phase 1 完了時点の制約

- ファイル監視なし
- grep 検索なし
- 参照・バックリンクなし
- MCP なし
- Web ビューアなし
- 簡易 YAML パーサ
- 毎回の全ファイル走査

## Phase 2: Index / References

### 目的

外部エディタによる Markdown の編集を自動で index に反映し、参照関係を扱えるようにする。

### タスク

- `notify` によるファイル監視・debounce・起動時の全マウント同期（実装済み）
- atomic save やファイル名変更への追従（実装済み）
- Git checkout の追従（macOS と Linux コンテナで結合テスト済み）
- 監視によるベストエフォートの index 更新（実装済み）
- Linux コンテナでの監視検証（完了）
- 実ファイルへの grep 検索（実装済み）
  - 行番号・前後文脈・件数上限
- Markdown リンク・Wiki リンクの抽出と外部 URL の記録（実装済み）
- マウント内のリンク解決、曖昧・未解決リンクの記録（実装済み）
- `references` テーブルとバックリンク（実装済み）
- 変更ファイルと影響する参照だけを差分更新（実装済み。全体再構築は `index rescan`）
- `index search`、`index references`、`index backlinks`（実装済み）
- パーセントエンコードされた Markdown リンク先の解決（実装済み）

### 完了条件

外部エディタで Markdown を編集すると、自動で index に反映されること。
達成済み。検索・参照は多少古くてもよく、監視と定期再走査で最終的に整合する。

### 設計上の論点

- 日本語検索のトークナイザ
- Wiki リンクの曖昧性解決
- 大量ページ時の index 更新性能

## Phase 3: MCP

### 目的

AI が同じ Markdown を読み書きできるようにする。

### タスク

- `rmcp` の stdio transport（実装済み）
- Tools（実装済み）
  - `mounts`
  - `search`
  - `read`
  - `write`
  - `list`
  - `references`
  - `backlinks`
- Resources（実装済み）
  - `mindmount://<mount-id>/documents/<document-id>`
- `write` の楽観的ロック（実装済み）
  - `content_hash` の一致を要求
  - 新規作成時の存在確認
- パス指定での読み書き（実装済み）
- stdio の JSON-RPC 結合テスト（実装済み）
- stdout にログを出さない運用（実装済み）

### 完了条件

AI が同じ Markdown を読み書きできること。
stdio の initialize / tools / resources / read / write を結合テストで確認済み。
OpenCode / Claude Code など実クライアントでの確認は利用環境で行う。

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

- Axum（実装済み）
- Askama（実装済み）
- 検索はサーバー描画の GET フォーム（htmx は使わない）
- Markdown 表示（閲覧専用、実装済み）
- 参照先・バックリンク表示（実装済み）
- 検索（実装済み）
- HTML サニタイズ（実装済み）
- 静的ファイルの埋め込み（実装済み）
- `127.0.0.1` / `::1` のみの bind（実装済み）
- Host / Origin 検証（実装済み）
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
マウント一覧、ページ本文、検索、参照・バックリンクを表示でき、
Host / Origin 検証とループバック bind を備えています。

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

1. MCP stdio サーバー
2. 読み取り専用 Web ビューア

## 未解決の設計課題

- 日本語検索のトークン分割方式
- 参照の正規化と曖昧性解決
- frontmatter ID の衝突・移行
- 設定ディレクトリを `XDG_DATA_HOME` へ移す時期
- 複数プロセスからの同時 index 更新
- Snowflake ノード ID の端末間衝突
- Web UI の閲覧体験（検索・参照表示）の改善
