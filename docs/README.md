# Mindmount ドキュメント

Mindmount のドキュメント一覧です。

Mindmount はローカルファーストの OSS プロジェクトです。自分のメモリを AI と
共有するための「個人のホーム」として設計しています。単一ユーザー・
ローカル専用です。

| ドキュメント | 内容 |
|---|---|
| [USAGE.md](USAGE.md) | CLI の使い方、設定、データ配置、トラブルシューティング |
| [DESIGN.md](DESIGN.md) | アーキテクチャ、データモデル、ID、索引、セキュリティ |
| [ROADMAP.md](ROADMAP.md) | Phase ごとの実装状況と次の作業 |
| [DECISIONS.md](DECISIONS.md) | 設計上の決定（ローカル Web は閲覧専用、CLI のグループ化など） |

## 読み方

- `USAGE.md` と `DESIGN.md` は、現在の実装に基づく挙動を説明します。
- 仕様と実装がずれる場合は、まず `DECISIONS.md` と `DESIGN.md` の「現状の制約」を確認してください。
- Phase 1〜4 は実装済みです。詳細は `ROADMAP.md` を参照してください。

## 現在の状態

- マウント登録、一覧、解除
- `config.toml` を正本としたマウント管理
- SQLite による派生インデックス
- Snowflake ID
- Markdown の走査、タイトル抽出、内容ハッシュ、差分反映
- `index rescan` による再インデックス
- `index pages` によるページ一覧
- `index search` による実ファイルの grep 検索
- `index references` / `index backlinks` による参照一覧
- `index watch` によるファイル監視と自動再走査
- デーモン（Web ビューア、MCP Streamable HTTP、ファイル監視）
- MCP Tools（`mounts` / `list` / `search` / `read` / `links` / `backlinks` / `metadata_get` / `metadata_set` / `link_add` / `link_delete`）
- MCP Resources（`mindmount://<mount-id>/documents/<document-id>`）
- 閲覧専用 Web ビューア（マウント一覧、ページ、検索、参照・バックリンク）

未実装の主な機能:

- リリース作業（Phase 5）
