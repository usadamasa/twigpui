# 実行時のメモリと CPU の記録

`--perf` の測定記録｡測り方と閾値は `runtime-profiling` スキル
(`.claude/skills/runtime-profiling/SKILL.md`)｡**測ったら 1 行足す｡**
数字だけを足し､閾値はここでは決めない｡

idle は 11 秒目以降の区間 (起動直後の画像取得と､5 秒後の保留分到着を除く)｡
rss は最初の sample → 最後の sample､括弧は最大｡

| 日付 | commit | build | screen | 秒 | idle cpu avg / max | rss first → last (peak) | 備考 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 2026-08-26 | 12f4043 | debug | locked | 60 | 1.0% / 1.9% | 114,896 → 122,272 (122,544) kB | 6 秒目の保留分到着で 760 ms CPU､+1.7 MB｡以降は平ら |
| 2026-08-26 | 12f4043 | release | locked | 60 | 0.7% / 0.9% | 101,072 → 107,296 (107,472) kB | 保留分到着で 270 ms CPU､+2.4 MB (#226)｡以降 rss は −176 kB |

## 読み

- 2026-08-26 (初回): 閾値を超えたものは無い｡idle の 0.9% は `ps -o time` の
  1 刻み (10 ms) が 1 秒に 1 回乗っている形で､sampler 自身の `ps` 起動と､ロック中も
  描き続ける display link のどちらかが見分けられていない (#224)｡unlocked で測り直す
  まで､idle の下限は 1% と読む｡
