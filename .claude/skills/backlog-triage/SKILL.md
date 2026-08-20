---
name: backlog-triage
description: >-
  twigpui の issue を新しく立てるとき、ラベルを振るとき、
  backlog issue (#4) からタスクを切り出すときに使う。
  ラベル体系と、立てる前の重複チェック手順を扱う。
---

# backlog-triage

## 立てる前に既存 issue を検索する

**必須。** 過去に重複を作った実績がある (#69 が #65 の重複だった)。

```sh
gh issue list --state all --search "<キーワード>"
```

タイトルの言い回しが違うだけで同じことを指している issue は見つけにくい。
機能名だけでなく、症状・エンドポイント名・ファイル名でも引く。

## ラベル体系

| 種別 | ラベル | 用途 |
| --- | --- | --- |
| 優先度 | `priority:high` / `priority:medium` / `priority:low` | 着手順 |
| 領域 | `area:auth` | 認証・OAuth |
| | `area:api` | X API クライアント、クォータ、レートリミット |
| | `area:timeline` | タイムラインの取得と描画 |
| | `area:ui` | gpui のウィンドウ、レイアウト、テーマ |
| | `area:cache` | ローカルキャッシュと永続化 |
| | `area:config` | 設定とファイル配置 |
| | `area:cost` | API 課金の可視化と抑制 |
| | `area:tui` | ターミナル UI モード |
| 状態 | `blocked` | 他 issue 待ち |
| 種類 | `enhancement` / `documentation` / `research` / `bug` | 既定のラベルに準じる |

**優先度ラベルを 1 つと、領域ラベルを最低 1 つ付ける。**

## backlog (#4) からの切り出し

- バックログに書かれたタスクは分解して個別の issue にする。
- issue 化したら、#4 の一覧からは削除してよい。
- **#4 の「指示」セクションは更新してはならない。**
- 優先度は適宜判断する。プロジェクトで管理してもよい。

## 優先度の付け方

優先順位の方針そのものは `CLAUDE.md` の「開発ポリシー」が定める。
ここでは触れず、そちらに従ってラベルを選ぶ。
