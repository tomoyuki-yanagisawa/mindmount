# 設計上の決定

## D-001: ローカル版の Web UI は閲覧専用（viewer）とする

- 日付: 2026-11-09
- 状態: Accepted

### 背景

- Mindmount の主な利用者は開発者であり、Markdown は vim などのエディタで直接編集できる。
- 編集 UI を提供すると、CSRF、更新競合、リッチエディタなど、編集機能に付随する複雑さをローカル版に持ち込むことになる。
- ローカル Web UI の価値は、AI と共有している知識をブラウザで閲覧・検索・参照できることにある。

### 決定

- ローカル版の `mindmount serve` は**読み取り専用のビューア**とする。
- ローカル版には Markdown の編集画面、`GET /page/:id/edit`、`POST /page/:id` を設けない。
- 人間による編集は vim などでファイルを直接変更し、`mindmount index rescan` で index に反映する。
- ローカル版では Web UI 編集を実装しない。
- MCP の `write` Tool はローカル版でも提供する（AI からの編集経路は維持する）。

### 影響

- ローカル Web のルートは GET のみになる。書き込み API がないため CSRF 対策は不要だが、DNS rebinding 対策として Host / Origin 検証は維持する。
- Markdown の HTML レンダリングとサニタイズは閲覧に必要なため維持する。
- Phase 4 の完了条件は「人間がブラウザで AI と同じ知識を閲覧できる」に変更する。
- 本プロジェクトの Web UI は閲覧専用の範囲に留める。

## D-002: CLI を `mount` / `index` / `daemon` のサブコマンドグループに分ける

- 日付: 2026-11-09
- 状態: Accepted

### 背景

機能追加のたびにトップレベルのサブコマンドが増えると、CLI の責務が分かりにくくなる。

### 決定

トップレベルを責務ごとのグループに分ける。

```text
mindmount daemon start|stop|restart|status|run
mindmount mount add|list|remove|sync
mindmount index rescan|watch|pages|search|references|backlinks
mindmount metadata get|set
mindmount link add|delete
mindmount serve
mindmount mcp
```

引数だけを指定する `mindmount -p ... -d ...` は、従来どおりマウント登録と index 構築を行う。

### 影響

- 旧コマンド（`storage mount` / `storage list` / `storage unmount`）は削除した。
- マウント操作は `mount`、index と読み取りは `index`、常駐処理は `daemon` に集約する。

## D-003: ローカル版の重点は MCP と人間用ビューア

- 日付: 2026-11-09
- 状態: Accepted

### 背景

- ローカル版は、個人が自分の Markdown を AI と共有し、人間もブラウザで読める「個人のホーム」としての役割が中心になる。
- 開発者にとっては vim などの既存エディタで編集できれば十分で、ローカル Web に編集機能は不要。

### 決定

- ローカル版では **MCP 連携**と**人間用の読み取り専用ビューア**を主要機能として優先する。
- Web UI での編集は本プロジェクトの対象外とする。
- Web と MCP が同じコアを共有できる構造を保つが、ローカル版の単純さを犠牲にしない。

### 影響

- Phase 2 の index 整備は、MCP とビューアを支える土台として位置づける。
- Phase 3（MCP）と Phase 4（ビューア）をローカル版の中心機能として仕上げる。
- Web 編集は対象外とし、閲覧体験の向上に集中する。
