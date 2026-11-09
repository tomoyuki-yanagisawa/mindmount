# Mindmount

Mindmount は、自分のメモリを AI と共有するためのローカルサーバーです。
既存の Markdown ディレクトリを移動・変換せずにマウントし、AI から検索・参照できるようにします。

Mindmount はローカルファーストの OSS プロジェクトです。

このリポジトリは **Phase 1（CLI / Storage）** までを実装しています。

## ドキュメント

| ドキュメント | 内容 |
|---|---|
| [docs/README.md](docs/README.md) | ドキュメント一覧 |
| [docs/USAGE.md](docs/USAGE.md) | CLI の使い方、設定、トラブルシューティング |
| [docs/DESIGN.md](docs/DESIGN.md) | アーキテクチャ、データモデル、ID、セキュリティ |
| [docs/ROADMAP.md](docs/ROADMAP.md) | Phase ごとの実装状況と次の作業 |
| [docs/DECISIONS.md](docs/DECISIONS.md) | 設計上の決定 |

## ステータス

| Phase | 内容 | 状態 |
|---|---|---|
| 1 | CLI / Storage / 基本 index | 実装済み |
| 2 | ファイル監視、全文検索、参照、バックリンク | 未着手 |
| 3 | MCP サーバー | 未着手 |
| 4 | Web ビューア（閲覧専用） | 未着手 |
| 5 | リリース | 未着手 |

ローカル版の Web UI は**閲覧専用**です。人間による編集は vim などで行い、
`index rescan` で反映します。Web UI での編集は提供しません。
詳細は [docs/DECISIONS.md](docs/DECISIONS.md) の D-001 を参照してください。

## スコープ

Mindmount は、個人のローカル環境で動く「個人のホーム」です。

- ローカルの Markdown を移動・変換せずに扱う
- MCP 経由で AI から自分のメモリを検索・参照できるようにする
- 人間は読み取り専用の Web ビューアで閲覧する
- 編集は vim などで行い、`index rescan` で反映する

単一ユーザー・ローカル専用です。Web UI での編集は提供しません。

## 必要環境

- Rust 1.99 以上
- macOS または Linux

## ビルドとテスト

```bash
cargo build
cargo test
```

## クイックスタート

```bash
# マウント登録と index 構築
cargo run -- storage mount -p ~/Project/app -d ./notes -n "Project notes"

# マウント一覧
cargo run -- storage list

# index 済みページ一覧
cargo run -- index pages

# vim などで編集したら index を再構築
cargo run -- index rescan

# マウント解除（Markdown ファイルは削除しない）
cargo run -- storage unmount <mount-id>
```

`mindmount -p ... -d ...` の引数だけの実行でも、マウント登録と index 構築ができます。

CLI は責務ごとにグループ化されています。

```text
mindmount storage mount|list|unmount
mindmount index rescan|pages
mindmount serve
mindmount mcp
```

`serve` は Phase 4、`mcp` は Phase 3 で実装予定です。

## ローカルデータ

```text
$XDG_CONFIG_HOME/mindmount/   # 未設定時: ~/.config/mindmount/
├── config.toml               # マウント設定の正本
├── id-state                  # ID 発行の予約状態
├── mindmount.lock            # プロセス間ロック
└── db/mindmount.sqlite       # 派生 index
```

Markdown ファイルはマウント先にそのまま残ります。マウントや index 構築で
既存ファイルを書き換えることはありません。`storage unmount` も Markdown を
削除せず、設定と派生 index だけを削除します。

## ID

マウント ID とドキュメント ID には Snowflake 互換の 63bit 整数を使います。

- 41bit: 2025-01-01T00:00:00Z からのミリ秒
- 10bit: `config.toml` に永続化するノード ID
- 12bit: 同一ミリ秒内の連番

frontmatter に `id` がない Markdown には内部 ID を割り当てます。内部 ID は
`index rescan` では維持されますが、DB 再構築後の永続性は保証されません。
安定した ID が必要な場合は frontmatter に `id` を付与してください。

```yaml
---
id: 1234567890123456789
title: Architecture
---
```

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT license](LICENSE-MIT)

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this project by you, as defined in the Apache-2.0 license,
shall be dual licensed as above, without any additional terms or conditions.
