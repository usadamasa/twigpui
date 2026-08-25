//! background sync の loop が 1 回の起床で何をすべきか｡
//!
//! #163 の上に足した auto-sync のうち純粋な側の半分で､`ui::reload_policy`
//! が `ui` から切り出されているのと同じ線で [`super::auto`] から切り出して
//! ある: ここにあるのはすべて支出するかどうかの判断で､どれも request を
//! 投げない｡[`Step`] を受けて動く loop は隣の [`super::auto`] にあり､
//! そちらは [`super::api::ListSyncApi`] 越しに request を投げる｡

/// loop が今すべきこと｡
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    /// 両側を丸ごと読み､新しい plan を書く｡高くつく方だ: 両側のどの
    /// アカウントも課金 resource になる｡
    Diff,
    /// plan に残っている write を 1 batch 送る｡
    Apply,
    /// `until` まですることは無い｡呼び出し側が sleep する — 1 回でどれだけ
    /// sleep するかに上限を付けるのは呼び出し側の仕事で､この関数のでは
    /// ない｡
    Wait { until: i64 },
}

/// sync がどこまで進んだかについて [`next_step`] が知る必要のあるすべて｡
///
/// 引数 5 つではなく struct にしてある: `Option<i64>` が 2 つと件数が 2 つ
/// 並ぶ形は､呼び出し側が黙って取り違える形そのものだ｡
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Situation {
    /// 最後に diff を *試みた* 時刻｡state ファイル由来で､一度も無ければ
    /// `None`｡成功ではなく試行なのは — 失敗した read でもこれが動く理由は
    /// [`super::auto`] を見よ｡
    pub last_diff_at: Option<i64>,
    /// `config.sync_interval_seconds`｡
    pub interval_seconds: u32,
    /// ファイル上の plan のうち loop がまだ送ってよい entry — 未適用の
    /// entry すべてではなく [`sendable`] だ｡#176 が保留する removal は
    /// 除いてある｡数えてしまうと､終わらない plan を流し続ける `Apply` に
    /// [`next_step`] を縛りつけることになる｡
    pub pending: usize,
    /// いつまで何も送ってはならないか — [`super::state::SyncState::blocked_until`]､
    /// loop を止めるものが何も無ければ `None`｡
    pub blocked_until: Option<i64>,
    /// 次の write の batch までの自分に課した間 —
    /// [`super::state::SyncState::paused_until`]｡
    ///
    /// `blocked_until` と別なのは順位が別だから｡あちらは拒否なのであらゆる
    /// step の前に立つが､こちらはペース配分なので write だけを抑える｡
    pub paused_until: Option<i64>,
}

/// 1 回の起床が何をすべきかを決める｡
///
/// 優先順位が設計のすべてだ:
///
/// 1. **生きている rate limit が勝つ｡** 他の 2 つの step はどちらも
///    request を送るし､既に拒否した window へ送り込むのは､自ら課した
///    throttle を X のものに変える道だ｡
/// 2. **流し切りが diff のやり直しに優先する｡** plan の entry は､それを
///    生んだ diff によって支払われている｡流し切っていない plan の上で
///    diff し直せば同じ答えを 2 度買い､既に送ったものの記録を捨てる｡
///    ただし直前の batch が置いた間 (`paused_until`) はこの枝の *中* で
///    効く｡枝の中に置いたので順位は 1 つも動かず､送るものが無い loop は
///    この間をまったく見ない｡
/// 3. **そのうえで interval｡** 一度も走っていない diff は即座に期限を
///    迎える｡そうでなければ最後の試行から `interval_seconds` 後だ｡
///
/// 未来の `last_diff_at` は待たずに､今が期限として扱う｡このコードの
/// 持ち物ではない時計が打刻したファイル由来なので､そうしなければ時計が
/// 巻き戻ったとき (あるいは state ファイルを手で編集したとき) 追いつくまで
/// loop が止まる — 十分に先の値なら永久にだ｡
pub(crate) fn next_step(situation: &Situation, now: i64) -> Step {
    if let Some(until) = situation.blocked_until
        && until > now
    {
        return Step::Wait { until };
    }
    if situation.pending > 0 {
        if let Some(until) = situation.paused_until
            && until > now
        {
            return Step::Wait { until };
        }
        return Step::Apply;
    }
    let Some(last) = situation.last_diff_at else {
        return Step::Diff;
    };
    if last > now {
        return Step::Diff;
    }
    let due_at = last.saturating_add(i64::from(situation.interval_seconds));
    if due_at > now {
        Step::Wait { until: due_at }
    } else {
        Step::Diff
    }
}

/// 1 回の tick が何をしたか｡呼び出し側が log し､表示し､ペース配分の
/// 拠りどころにする｡
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// 期限を迎えたものは無かった｡何も送っていない｡
    ///
    /// `pending` はファイル上の plan がまだ負っているもので､常に 0 とは
    /// 限らない: [`next_step`] は `pending` を見る *前に* `blocked_until` を
    /// 見るので､catch-up の途中で生きている rate limit に当たると､数百の
    /// 未送信 write を抱えたまま idle な tick になる｡#174 の手動 sync は
    /// まさにこの区別で完了を判断する — 「idle」だけでは､丸ごと 1 回の
    /// diff を支払って得た plan を置き去りにしてしまう｡
    Idle { until: i64, pending: usize },
    /// 両側を読み､新しい plan を書いた｡
    ///
    /// `held` は removal についての #176 の判定だ: `true` なら list の
    /// `members_total` に対して `sync_prune_limit_percent` の許す割合を
    /// 超えているということで､background sync は addition を流し切り､
    /// removal は `--sync-list --apply --prune` のために plan ファイルへ
    /// 残す｡
    Diffed {
        adds: usize,
        removals: usize,
        members_total: usize,
        held: bool,
    },
    /// plan の write を 1 batch 送り出した｡`remaining` は loop がまだ
    /// 送ってよいもの — [`sendable`] なので保留された removal は数えない｡
    Applied { sent: usize, remaining: usize },
    /// write が拒否された — 送る前に追跡している window によってか､
    /// X から 429 で｡disk 上の plan は catch-up がどこまで進んだかを正確に
    /// 記録する: `sent` はこの batch のうち refusal の前に届いた件数､
    /// `remaining` はまだ負っている分だ｡
    ///
    /// `opaque` は [`crate::rate_limit::RateLimited::opaque`] のフラグだ:
    /// ヘッダが記述しない上限なら `true` で､[`super::state::settle`] が
    /// 毎回さらに後退していく (#197)｡`until` が答えになるのはもう一方の
    /// 種類のときだけで､opaque なものではクライアントの最初の当て推量に
    /// すぎず､state の連続回数が決める｡
    RateLimited {
        until: i64,
        opaque: bool,
        sent: usize,
        remaining: usize,
    },
}

/// write の batch が何になったか: 通った送信分の [`Outcome`] と､batch が
/// 途中で止まったならその理由｡
///
/// rate limit はどちらの経路で来ても — 送信前に追跡している window に
/// 拒否されても､X から 429 で来ても — エラーではなく outcome だ｡どちらの
/// 場合も disk 上の plan が届いたものをすべて記録しており､loop がそれに
/// ついてすべきことは待つことだけだからだ｡それ以外の失敗は伝播する:
/// 失効した scope や削除された list を越えて続ければ､受け付けられないと
/// 今しがた証明した credential や list に write を使い続けることになる｡
///
/// downcast を除けば純粋で､`auto` ではなくここにあるのはそのためだ:
/// 「クライアントが言ったこと」から「loop が覚えること」への写像こそ､
/// #198 が情報を落とした継ぎ目だ｡
pub(crate) fn apply_outcome(
    sent: usize,
    remaining: usize,
    result: anyhow::Result<()>,
) -> anyhow::Result<Outcome> {
    match result {
        Ok(()) => Ok(Outcome::Applied { sent, remaining }),
        Err(error) => match error.downcast_ref::<crate::rate_limit::RateLimited>() {
            Some(crate::rate_limit::RateLimited {
                reset_at: Some(until),
                opaque,
            }) => Ok(Outcome::RateLimited {
                until: *until,
                opaque: *opaque,
                sent,
                remaining,
            }),
            _ => Err(error),
        },
    }
}

/// `outcome` についてウィンドウが言うべきこと｡何も言わないなら `None`｡
///
/// 最も頻繁に起きる二つの場合では意図して黙る: 何も見つけなかった diff
/// (定常状態で 1 日に何度も) と､catch-up の途中の batch (何時間ものあいだ
/// 数秒ごとに 1 回)｡どちらかでも告知すれば､background の機能が自分に
/// ついての通知の流れに変わってしまう｡
///
/// rate limit を表示せず log に落とすのは三つ目の理由による: それが続く
/// あいだ status bar が連続回数とカウントダウンごと既に運んでいる —
/// バナーを出しても､何もしない dismiss ボタンの付いた同じ知らせになる｡
pub(crate) fn notice(outcome: &Outcome) -> Option<String> {
    match outcome {
        // apply の tick ごとではなく diff ごとに 1 回出す: loop は addition
        // の batch を流すたびに保留された plan へ再到達するので､そのたび
        // バナーが戻ってくれば下の rate limit の問題の再来になる｡
        Outcome::Diffed {
            adds,
            removals,
            members_total,
            held: true,
        } => Some(format!(
            "List sync: {adds} to add; {removals} of {members_total} members would be removed, \
             which is over the background limit, so they are held. Run \
             `twigpui --sync-list --apply --prune` to confirm them."
        )),
        Outcome::Diffed {
            adds,
            removals,
            held: false,
            ..
        } if *adds > 0 || *removals > 0 => {
            Some(format!("List sync: {adds} to add, {removals} to remove."))
        }
        Outcome::Applied { sent, remaining: 0 } if *sent > 0 => {
            Some(format!("List sync: {sent} change(s) applied."))
        }
        Outcome::Diffed { .. }
        | Outcome::Applied { .. }
        | Outcome::Idle { .. }
        | Outcome::RateLimited { .. } => None,
    }
}

/// 呼び出し側が強制したかどうか (#174) を踏まえ､この tick での
/// [`Situation::last_diff_at`] が何であるべきか｡
///
/// 手動トリガーの仕組みのすべてだ｡[`super::auto::tick`] の中の `if` では
/// なく名前の付いた関数にしてあるのは､あの関数が HTTP request を投げる
/// せいで自前のテストを持てないからだ — ここへ出てくるまではこれにも
/// テストが無く､両側を読み直すかどうかを決めるスイッチをそのまま置いて
/// おいてよい場所ではなかった｡
///
/// 強制は interval を短くするのではなく､記録された時刻を捨てる｡`None` は
/// [`next_step`] が既に「diff が一度も走っていない」と読む値なので､その
/// 下の判断には手が付かない: 生きている rate limit は今も tick を拒むし､
/// 流し切っていない plan は何かを買い直す前に流し切られる｡
pub(crate) fn last_diff_for(forced: bool, recorded: Option<i64>) -> Option<i64> {
    if forced { None } else { recorded }
}

/// 呼び出し側が強制したかどうか (#174) と block が何のためのものかを
/// 踏まえ､この tick での [`Situation::blocked_until`] が何であるべきか｡
///
/// block は #198 で永続化され､強制された tick が出会う種類が二つになった｡
/// 失敗した tick の block (`refusals == 0`) は､loop が毎分再試行しないよう
/// [`super::state::settle`] が失効した scope や削除された list に渡す
/// interval だ — そして status bar が「Failed」を押せるものとして出すのは､
/// 原因を直した人が今すぐ再試行できるようにするためにほかならない｡強制は
/// こちらをまたぐ｡refusal の block (`refusals > 0`) は否と言った上限から
/// 後退していく梯子であり､ボタンを押すことはそれが明けた証拠にならない:
/// 強制は他の tick と同じくこちらは待ち切る｡それがボタンを上限の迂回路に
/// しないための仕組みだ｡
///
/// 使い切った window 由来の block (`refusals` には触れない) もまたぐが､
/// 費用はかからない: `rate_limit::decision` が送信前に拒否し､tick は同じ
/// 期限を持って `RateLimited` で戻ってくる｡
pub(crate) fn blocked_for(forced: bool, state: &super::SyncState) -> Option<i64> {
    if forced && state.refusals == 0 {
        None
    } else {
        state.blocked_until
    }
}

/// 手動起動が [`Situation::paused_until`] に何を見るか｡
///
/// [`blocked_for`] と違い､強制はこれを常に落とす｡refusal の後退は X が
/// 言ったことだが､batch と batch の間はこちら側が自分に課したものにすぎず､
/// しかも課す目的が「機械らしく見えないこと」なので､人間がボタンを押した
/// 瞬間には守る相手がいない｡
pub(crate) fn paused_for(forced: bool, state: &super::SyncState) -> Option<i64> {
    if forced { None } else { state.paused_until }
}

/// 1 pass だけを求められた実行に､もうすることが残っていないかどうか
/// (#174)｡
///
/// 止まることを前提とした loop のためだけのものだ — 定期 sync は決して
/// 止まらず､これを訊きもしない｡守っているのは plan ファイルだ: 数千の
/// follow に対する diff はドルの話なので､entry を未送信のまま立ち去る
/// 手動実行は､書き換え終えていない list にその金を捨てたことになる｡
///
/// だから判定は「idle」ではなく､**残件ゼロの idle** だ｡この二つは
/// ちょうど効くところで分かれる: [`next_step`] は `pending` より先に
/// `blocked_until` を見るので､catch-up の途中で rate limit に当たると
/// 数百の write を負ったまま `Idle` になる｡その実行は完了を宣言せず
/// 待ち続けなければならない｡
///
/// 失敗した tick (`None`) も実行を終わらせない — [`super::state::settle`]
/// が戻ってくるための interval を丸ごと既に与えているし､流していた plan は
/// まだ disk にある｡
pub(crate) fn is_finished(outcome: Option<&Outcome>) -> bool {
    matches!(outcome, Some(Outcome::Idle { pending: 0, .. }))
}

/// loop が次に送るべき `limit` 件の entry｡addition と removal から交互に
/// 取る｡
///
/// addition を全部送ってから removal ではなく交互なのは､ひどく古びた
/// list の catch-up が何時間もかかるからだ: add を先に送れば､最初の
/// stale なアカウントが消えるはるか前に新しいアカウントがすべて見え､
/// 途中で中断した実行は list を正解に近づけるどころか､あるべき大きさより
/// 確実に大きいまま残す｡
///
/// `prune` が false なら removal は丸ごと落とす — それが CLI の既定で､
/// 二つの経路が分かれる唯一の場所がこの関数だ｡
///
/// `plan` を借りずに所有権のある id を返すのは､呼び出し側が進めながら
/// entry に適用済みの印を付けるからだ｡
pub(crate) fn next_batch(
    plan: &super::Plan,
    prune: bool,
    limit: usize,
) -> Vec<(super::Action, String)> {
    let mut adds = plan.pending(super::Action::Add);
    let mut removals = plan.pending(super::Action::Remove);
    let mut batch = Vec::new();
    while batch.len() < limit {
        let add = adds.next();
        let removal = if prune { removals.next() } else { None };
        if add.is_none() && removal.is_none() {
            break;
        }
        if let Some(entry) = add {
            batch.push((super::Action::Add, entry.user_id.clone()));
        }
        if batch.len() >= limit {
            break;
        }
        if let Some(entry) = removal {
            batch.push((super::Action::Remove, entry.user_id.clone()));
        }
    }
    batch
}

/// background sync が `plan` の removal を送ってよいかどうか (#176)｡
///
/// 上限は list に対する割合だ: plan が diff された相手の `members_total` の
/// うち削除するのが最大 `limit_percent` までなら removal は出ていく｡
/// `read_all` の all-or-nothing 規則では見えない失敗のためのものだ —
/// *200 を返しながら* 足りずに戻ってきた follow の read (障害､黙って
/// 落ちた scope､`plan` より上流の退行) は大量 unfollow と読まれ､prune が
/// 無条件ならそれは大量削除になる｡
///
/// 上限を超えたら最初の N 件を送るのではなく removal を *すべて* 保留する:
/// 疑っているのは read の方で､悪い read の最初の N 件は最後の N 件より
/// ましではない｡保留された removal は plan ファイルに残り — 支払い済み
/// だからだ — そこから `--sync-list --apply --prune` が人の目の下で送る｡
/// CLI に上限は無い: その dry-run の report が同じ数字を見せ､それを読んで
/// から `--prune` と打つことが確認になる｡
///
/// diff された時点の plan ではなく今なお残っているものを測るので､CLI が
/// 既に大半を prune した plan が､届いた分のせいで保留され続けることは
/// ない｡`members_total` が 0 で removal が残っていれば保留する: それは
/// この上限より前の plan ファイル (`#[serde(default)]`) であり､次の diff が
/// 置き換える｡
///
/// `limit_percent` は `config.sync_prune_limit_percent` で 0..=100｡100 なら
/// list を空にすることも許し､0 なら background sync は追加専用になる｡
pub(crate) fn prune_allowed(plan: &super::Plan, limit_percent: u8) -> bool {
    let removals = plan.pending_count(super::Action::Remove);
    if removals == 0 {
        return true;
    }
    // 割らずにたすき掛けにしてある｡そうすれば 15 件中 1 件を 10% で見た
    // とき (許容 1.5) 線上に丸められず､超過として扱われる｡
    removals.saturating_mul(100)
        <= plan
            .members_total
            .saturating_mul(usize::from(limit_percent))
}

/// background sync が `plan` の entry をまだ何件送ってよいか —
/// [`Situation::pending`] の値だ (#176)｡
///
/// addition は常に数える｡removal を数えるのは `prune` ([`prune_allowed`] の
/// 判定) が送ってよいと言うあいだだけだ｡保留されたものは loop にとって
/// 残務ではない｡loop が決してそれをしないからだ｡
pub(crate) fn sendable(plan: &super::Plan, prune: bool) -> usize {
    let adds = plan.pending_count(super::Action::Add);
    if prune {
        adds.saturating_add(plan.pending_count(super::Action::Remove))
    } else {
        adds
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::{Action, Plan, PlanEntry};

    fn plan_of(adds: &[&str], removals: &[&str]) -> Plan {
        let entry = |user_id: &str, action| PlanEntry {
            user_id: user_id.to_string(),
            username: format!("user{user_id}"),
            action,
            applied: false,
        };
        Plan {
            list_id: "7".to_string(),
            created_at: 0,
            members_total: 0,
            entries: adds
                .iter()
                .map(|id| entry(id, Action::Add))
                .chain(removals.iter().map(|id| entry(id, Action::Remove)))
                .collect(),
        }
    }

    /// batch を `+id` / `-id` の簡潔な文字列にしたもの｡交互になっている
    /// 様子が tuple の vec に埋もれず assertion 上で読めるようにだ｡
    fn batch(plan: &Plan, prune: bool, limit: usize) -> String {
        next_batch(plan, prune, limit)
            .iter()
            .map(|(action, user_id)| match action {
                Action::Add => format!("+{user_id}"),
                Action::Remove => format!("-{user_id}"),
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// 落ち着いた loop: diff は走り済み､残件も block も pause も無い｡
    /// 各テストは自分が扱う 1 フィールドだけを上書きする｡
    fn idle() -> Situation {
        Situation {
            last_diff_at: Some(1_000),
            interval_seconds: 21_600,
            pending: 0,
            blocked_until: None,
            paused_until: None,
        }
    }

    #[test]
    fn a_sync_that_has_never_run_diffs_immediately() {
        let situation = Situation {
            last_diff_at: None,
            ..idle()
        };
        assert_eq!(next_step(&situation, 0), Step::Diff);
    }

    #[test]
    fn nothing_happens_again_until_the_interval_has_elapsed() {
        // 永続化された `last_diff_at` が､再起動に両側の全 read をもう一度
        // 支払わせないための仕組みだ｡
        assert_eq!(next_step(&idle(), 1_001), Step::Wait { until: 22_600 });
    }

    #[test]
    fn the_diff_comes_due_exactly_one_interval_after_the_last_one() {
        assert_eq!(next_step(&idle(), 22_600), Step::Diff);
    }

    #[test]
    fn a_diff_that_is_long_overdue_is_still_just_one_diff() {
        // 1 週間眠っていたマシンが目覚めて負っているのは sync 1 回分で
        // あって､1 週間分ではない｡
        assert_eq!(next_step(&idle(), 1_000_000), Step::Diff);
    }

    #[test]
    fn outstanding_entries_are_drained_before_the_next_diff() {
        // entry はそれを見つけた diff によって支払われている｡その上で
        // diff し直せば同じ答えを 2 度買うことになる｡
        let situation = Situation {
            pending: 3,
            ..idle()
        };
        assert_eq!(next_step(&situation, 1_000_000), Step::Apply);
    }

    #[test]
    fn draining_happens_inside_the_interval_too() {
        // apply を少しずつ垂らすことが全体の要だ: 2,000 アカウント遅れた
        // list は次の diff の期限まで留め置かれず､1 batch ずつ追いつく｡
        let situation = Situation {
            pending: 2_000,
            ..idle()
        };
        assert_eq!(next_step(&situation, 1_001), Step::Apply);
    }

    #[test]
    fn a_live_rate_limit_outranks_the_work_waiting_behind_it() {
        // 他の 2 つの step はどちらも送る｡既に拒否した window へ送り込む
        // のが､自ら課した throttle を X のものに変える道だ｡
        let situation = Situation {
            pending: 5,
            blocked_until: Some(2_000),
            ..idle()
        };
        assert_eq!(next_step(&situation, 1_500), Step::Wait { until: 2_000 });
    }

    #[test]
    fn a_rate_limit_outranks_an_overdue_diff_as_well() {
        let situation = Situation {
            blocked_until: Some(2_000),
            ..idle()
        };
        assert_eq!(next_step(&situation, 1_500), Step::Wait { until: 2_000 });
    }

    #[test]
    fn an_elapsed_rate_limit_holds_nothing_back() {
        let situation = Situation {
            pending: 5,
            blocked_until: Some(2_000),
            ..idle()
        };
        assert_eq!(next_step(&situation, 2_000), Step::Apply);
    }

    #[test]
    fn a_last_diff_stamped_in_the_future_is_treated_as_due_now() {
        // 打刻はこのコードの持ち物ではない時計が書いたファイル由来だ｡
        // 待ち切れば時計が追いつくまで loop が止まる — 十分に先の値なら
        // 永久にだ｡
        let situation = Situation {
            last_diff_at: Some(i64::MAX),
            ..idle()
        };
        assert_eq!(next_step(&situation, 1_000), Step::Diff);
    }

    // --- apply_outcome ---

    #[test]
    fn a_batch_that_finished_is_applied() {
        assert_eq!(
            apply_outcome(20, 400, Ok(())).unwrap(),
            Outcome::Applied {
                sent: 20,
                remaining: 400
            }
        );
    }

    #[test]
    fn a_refusal_keeps_what_the_batch_sent_before_it() {
        // `sent` の件数があるから state は「3 件届いたあとの拒否」と
        // 「また拒否」を見分けられる: 前者は連続を数え直す｡
        let error: anyhow::Error = crate::rate_limit::RateLimited {
            reset_at: Some(5_000),
            opaque: true,
        }
        .into();
        assert_eq!(
            apply_outcome(3, 397, Err(error)).unwrap(),
            Outcome::RateLimited {
                until: 5_000,
                opaque: true,
                sent: 3,
                remaining: 397,
            }
        );
    }

    #[test]
    fn a_refusal_the_window_explains_carries_its_reset_and_is_not_opaque() {
        let error: anyhow::Error = crate::rate_limit::RateLimited {
            reset_at: Some(5_000),
            opaque: false,
        }
        .into();
        assert!(matches!(
            apply_outcome(0, 400, Err(error)).unwrap(),
            Outcome::RateLimited {
                until: 5_000,
                opaque: false,
                ..
            }
        ));
    }

    #[test]
    fn any_other_failure_propagates() {
        // 失効した scope､削除された list: 待ち切って済むものではない｡
        let error = apply_outcome(0, 400, Err(anyhow::anyhow!("403 Forbidden"))).unwrap_err();
        assert!(error.to_string().contains("403"), "{error}");
    }

    #[test]
    fn a_rate_limit_with_no_reset_time_at_all_propagates() {
        // 実運用では `reset_at` は常に埋まる (`Refusal::into_error`)｡これは
        // 将来クライアントを変えたときに生まれうる形で､何も無いところまで
        // 待つよりは log に残る失敗とする方が安全だ｡
        let error: anyhow::Error = crate::rate_limit::RateLimited {
            reset_at: None,
            opaque: false,
        }
        .into();
        assert!(apply_outcome(0, 1, Err(error)).is_err());
    }

    // --- #174: interval を越えて tick を強制する ---

    #[test]
    fn an_ordinary_tick_is_paced_by_the_recorded_diff_time() {
        assert_eq!(last_diff_for(false, Some(1_000)), Some(1_000));
    }

    #[test]
    fn a_forced_tick_discards_the_recorded_diff_time() {
        assert_eq!(last_diff_for(true, Some(1_000)), None);
    }

    // ボタンの端から端までの形: 期限まで 4 時間ある diff が今の期限に
    // なり､判断のそれ以外は何も動かない｡
    #[test]
    fn forcing_turns_a_tick_that_would_have_waited_into_a_diff() {
        let recorded = Some(1_000);
        let waiting = Situation {
            last_diff_at: last_diff_for(false, recorded),
            interval_seconds: 21_600,
            ..idle()
        };
        assert_eq!(
            next_step(&waiting, 1_100),
            Step::Wait { until: 22_600 },
            "unforced, this is hours away"
        );

        let forced = Situation {
            last_diff_at: last_diff_for(true, recorded),
            ..waiting
        };
        assert_eq!(next_step(&forced, 1_100), Step::Diff);
    }

    // 強制が落とすのは interval だけ､interval *のみ* だ｡`next_step` で
    // その上にある二つの検査は今も効いており､それがボタンをそれらの
    // 迂回路にしないための仕組みだ｡
    #[test]
    fn forcing_does_not_get_past_a_live_rate_limit() {
        let refused = crate::sync::SyncState {
            last_diff_at: Some(1_000),
            blocked_until: Some(5_000),
            paused_until: None,
            refusals: 1,
        };
        let situation = Situation {
            last_diff_at: last_diff_for(true, refused.last_diff_at),
            blocked_until: blocked_for(true, &refused),
            ..idle()
        };
        assert_eq!(next_step(&situation, 1_100), Step::Wait { until: 5_000 });
    }

    // 失敗した tick が得る block だけは強制がまたいでよい:「Failed」を
    // 押せるものとして出すのは､直した原因を再起動が守るはずの interval の
    // あとではなく今すぐ再試行できるようにするためだ (#198 が block を
    // 永続化した)｡
    #[test]
    fn forcing_steps_over_the_interval_a_failed_tick_earned() {
        let failed = crate::sync::SyncState {
            last_diff_at: Some(1_000),
            blocked_until: Some(22_600),
            paused_until: None,
            refusals: 0,
        };
        assert_eq!(blocked_for(true, &failed), None);
        let situation = Situation {
            last_diff_at: last_diff_for(true, failed.last_diff_at),
            blocked_until: blocked_for(true, &failed),
            ..idle()
        };
        assert_eq!(next_step(&situation, 1_100), Step::Diff);
    }

    #[test]
    fn an_unforced_tick_honours_every_block() {
        let failed = crate::sync::SyncState {
            last_diff_at: Some(1_000),
            blocked_until: Some(22_600),
            paused_until: None,
            refusals: 0,
        };
        assert_eq!(blocked_for(false, &failed), Some(22_600));
    }

    #[test]
    fn forcing_never_steps_over_a_refusal_streak() {
        // ボタンを押すことは上限が明けた証拠にならない｡
        let refused = crate::sync::SyncState {
            last_diff_at: Some(1_000),
            blocked_until: Some(22_600),
            paused_until: None,
            refusals: 4,
        };
        assert_eq!(blocked_for(true, &refused), Some(22_600));
    }

    // --- batch と batch のあいだの間 ---

    #[test]
    fn a_live_pause_holds_the_next_batch() {
        let situation = Situation {
            pending: 2_155,
            paused_until: Some(1_090),
            ..idle()
        };
        assert_eq!(next_step(&situation, 1_030), Step::Wait { until: 1_090 });
    }

    #[test]
    fn an_elapsed_pause_lets_the_batch_go() {
        let situation = Situation {
            pending: 2_155,
            paused_until: Some(1_090),
            ..idle()
        };
        assert_eq!(next_step(&situation, 1_090), Step::Apply);
    }

    #[test]
    fn a_live_refusal_still_outranks_a_pause() {
        // 順位は変わっていない｡pause は残件の枝の中にあるので､拒否は
        // 今までどおり先に勝つ｡
        let situation = Situation {
            pending: 2_155,
            blocked_until: Some(5_000),
            paused_until: Some(1_090),
            ..idle()
        };
        assert_eq!(next_step(&situation, 1_030), Step::Wait { until: 5_000 });
    }

    #[test]
    fn a_pause_left_over_from_a_drained_plan_does_not_hold_back_a_diff() {
        // pause が抑えるのは write だけ｡送るものが無ければ期限を迎えた
        // diff は今までどおり走る｡
        let situation = Situation {
            last_diff_at: Some(1_000),
            pending: 0,
            paused_until: Some(99_999),
            ..idle()
        };
        assert_eq!(next_step(&situation, 22_600), Step::Diff);
    }

    #[test]
    fn a_manual_run_cuts_through_the_pause() {
        // 揺らぎは機械らしく見えないためのもの｡ボタンを押したのは人間
        // なので､隠す相手がそもそも居ない｡
        let paced = crate::sync::SyncState {
            last_diff_at: Some(1_000),
            blocked_until: None,
            paused_until: Some(1_090),
            refusals: 0,
        };
        assert_eq!(paused_for(false, &paced), Some(1_090));
        assert_eq!(paused_for(true, &paced), None);

        let situation = Situation {
            pending: 2_155,
            paused_until: paused_for(true, &paced),
            ..idle()
        };
        assert_eq!(next_step(&situation, 1_030), Step::Apply);
    }

    #[test]
    fn forcing_drains_an_outstanding_plan_before_buying_a_new_one() {
        let situation = Situation {
            last_diff_at: last_diff_for(true, Some(1_000)),
            pending: 340,
            ..idle()
        };
        assert_eq!(next_step(&situation, 1_100), Step::Apply);
    }

    // --- #174: 1 pass の実行が止まってよいのはいつか ---

    #[test]
    fn a_run_is_finished_once_it_goes_idle_with_nothing_owed() {
        assert!(is_finished(Some(&Outcome::Idle {
            until: 9_000,
            pending: 0
        })));
    }

    // この関数が存在する理由そのものの場合｡`next_step` は `pending` より
    // `blocked_until` を先に量るので､catch-up の途中の rate limit は idle に
    // 見える — そこで止まれば､満額支払った diff が生んだ plan を捨てる
    // ことになる｡
    #[test]
    fn a_run_blocked_part_way_through_a_catch_up_is_not_finished() {
        assert!(!is_finished(Some(&Outcome::Idle {
            until: 9_000,
            pending: 340
        })));
    }

    #[test]
    fn a_run_that_just_sent_a_batch_is_not_finished() {
        assert!(!is_finished(Some(&Outcome::Applied {
            sent: 20,
            remaining: 0
        })));
    }

    #[test]
    fn a_run_that_was_refused_by_the_rate_limit_is_not_finished() {
        assert!(!is_finished(Some(&Outcome::RateLimited {
            until: 9_000,
            opaque: true,
            sent: 0,
            remaining: 40,
        })));
    }

    // `settle` は失敗した tick に戻ってくるための interval を丸ごと既に
    // 渡しているし､流していたものは何であれまだ disk にある｡
    #[test]
    fn a_run_whose_tick_failed_is_not_finished() {
        assert!(!is_finished(None));
    }

    // --- notice ---

    #[test]
    fn a_diff_that_found_nothing_says_nothing() {
        // 定常状態｡1 日に何度も起きる｡
        assert_eq!(
            notice(&Outcome::Diffed {
                adds: 0,
                removals: 0,
                members_total: 100,
                held: false,
            }),
            None
        );
    }

    #[test]
    fn a_diff_that_found_work_says_what_it_found() {
        let text = notice(&Outcome::Diffed {
            adds: 3,
            removals: 1,
            members_total: 100,
            held: false,
        })
        .unwrap();
        assert!(text.contains("3 to add"), "{text}");
        assert!(text.contains("1 to remove"), "{text}");
    }

    #[test]
    fn a_batch_part_way_through_a_catch_up_says_nothing() {
        // 出すとしたら何時間ものあいだ数秒ごとに 1 回になる｡
        assert_eq!(
            notice(&Outcome::Applied {
                sent: 20,
                remaining: 400
            }),
            None
        );
    }

    #[test]
    fn the_batch_that_finishes_the_catch_up_reports_it() {
        let text = notice(&Outcome::Applied {
            sent: 12,
            remaining: 0,
        })
        .unwrap();
        assert!(text.contains("12"), "{text}");
    }

    #[test]
    fn a_final_batch_that_sent_nothing_still_says_nothing() {
        assert_eq!(
            notice(&Outcome::Applied {
                sent: 0,
                remaining: 0
            }),
            None
        );
    }

    #[test]
    fn neither_idling_nor_a_rate_limit_reaches_the_banner() {
        // とくに rate limit は: それが続くあいだ status bar が運ぶので､
        // バナーを出しても､何もしない dismiss ボタンの付いた同じ知らせに
        // なる｡
        assert_eq!(
            notice(&Outcome::Idle {
                until: 9_000,
                pending: 0
            }),
            None
        );
        assert_eq!(
            notice(&Outcome::RateLimited {
                until: 9_000,
                opaque: true,
                sent: 0,
                remaining: 40,
            }),
            None
        );
    }

    // --- next_batch ---

    #[test]
    fn a_batch_alternates_additions_and_removals() {
        // 何時間もかけて追いつく list は､まず最終的な大きさまで膨らんで
        // から stale な member を落とすのではなく､その間ずっと正解に
        // 近づいていくべきだ｡
        let plan = plan_of(&["1", "2"], &["3", "4"]);
        assert_eq!(batch(&plan, true, 10), "+1 -3 +2 -4");
    }

    #[test]
    fn a_batch_stops_at_the_limit_mid_pair() {
        // limit が奇数のときが､交互送信が黙って request を 1 回余分に
        // 送りかねない場合だ｡
        let plan = plan_of(&["1", "2"], &["3", "4"]);
        assert_eq!(batch(&plan, true, 3), "+1 -3 +2");
    }

    #[test]
    fn a_batch_carries_on_with_whichever_side_still_has_entries() {
        let plan = plan_of(&["1", "2", "3"], &["9"]);
        assert_eq!(batch(&plan, true, 10), "+1 -9 +2 +3");
    }

    #[test]
    fn removals_alone_still_fill_a_batch() {
        let plan = plan_of(&[], &["7", "8"]);
        assert_eq!(batch(&plan, true, 10), "-7 -8");
    }

    #[test]
    fn without_prune_a_batch_is_additions_only() {
        // CLI の既定｡removal は plan に載ったまま送られない｡
        let plan = plan_of(&["1", "2"], &["3", "4"]);
        assert_eq!(batch(&plan, false, 10), "+1 +2");
    }

    #[test]
    fn without_prune_removals_do_not_eat_into_the_limit() {
        // 交互送信が招くバグ: 飛ばした removal を `limit` の 1 件として
        // 数えてしまい､上限付きの batch が求められた addition の半分しか
        // 送らなくなる｡
        let plan = plan_of(&["1", "2"], &["3", "4"]);
        assert_eq!(batch(&plan, false, 2), "+1 +2");
    }

    #[test]
    fn an_already_applied_entry_is_never_in_a_batch() {
        // 再開した apply が安く済む理由: plan ファイルが通ったものを
        // 覚えており､送り直せば何も変えないのに write を 1 回使うことに
        // なる｡
        let mut plan = plan_of(&["1", "2"], &["3"]);
        plan.mark_applied("1", Action::Add);
        assert_eq!(batch(&plan, true, 10), "+2 -3");
    }

    #[test]
    fn a_fully_applied_plan_yields_an_empty_batch() {
        let mut plan = plan_of(&["1"], &["3"]);
        plan.mark_applied("1", Action::Add);
        plan.mark_applied("3", Action::Remove);
        assert_eq!(batch(&plan, true, 10), "");
    }

    #[test]
    fn a_zero_limit_sends_nothing() {
        let plan = plan_of(&["1"], &["3"]);
        assert_eq!(batch(&plan, true, 0), "");
    }

    #[test]
    fn an_interval_that_would_overflow_the_clock_still_answers() {
        // `+` ではなく `saturating_add` なのは: `interval_seconds` は
        // config 由来の u32､`last_diff_at` はファイル由来なので､どちらの
        // 範囲もこの関数が前提にしてよいものではないからだ｡
        let situation = Situation {
            last_diff_at: Some(i64::MAX.saturating_sub(1)),
            interval_seconds: u32::MAX,
            ..idle()
        };
        assert_eq!(
            next_step(&situation, i64::MAX.saturating_sub(1)),
            Step::Wait { until: i64::MAX }
        );
    }

    // --- #176: prune の上限 ---

    /// diff された時点で list が `members` 件のアカウントを持っていた plan｡
    fn plan_against(members: usize, adds: &[&str], removals: &[&str]) -> Plan {
        Plan {
            members_total: members,
            ..plan_of(adds, removals)
        }
    }

    const TEN: [&str; 10] = ["1", "2", "3", "4", "5", "6", "7", "8", "9", "10"];
    const ELEVEN: [&str; 11] = ["1", "2", "3", "4", "5", "6", "7", "8", "9", "10", "11"];

    #[test]
    fn removals_within_the_limit_are_allowed() {
        // 100 のうち 10 はちょうど 10%: 上限ちょうどであって超過ではない｡
        assert!(prune_allowed(&plan_against(100, &[], &TEN), 10));
    }

    #[test]
    fn one_removal_over_the_limit_holds_them_all() {
        // 収まるよう削るのではなく保留する: 上限より多く削除しようとする
        // plan は follow の read が全体として疑わしい plan であり､悪い
        // diff の最初の 10 件を送ることもやはり悪い diff を送ることだ｡
        assert!(!prune_allowed(&plan_against(100, &[], &ELEVEN), 10));
    }

    #[test]
    fn a_plan_with_no_removals_has_nothing_to_hold() {
        assert!(prune_allowed(&plan_against(0, &["1"], &[]), 10));
    }

    #[test]
    fn a_plan_that_does_not_know_the_list_size_holds_every_removal() {
        // #176 より前に書かれた plan ファイルは `members_total` を持たず
        // 0 と読まれる｡不明な総数で割れば何であれ上限超過だ｡interval で
        // 来る次の diff がファイルを置き換える｡
        assert!(!prune_allowed(&plan_against(0, &[], &["1"]), 10));
    }

    #[test]
    fn a_limit_of_one_hundred_percent_turns_the_cap_off() {
        // list を空にすることは定義上 100% の上限に収まる｡
        assert!(prune_allowed(&plan_against(3, &[], &["1", "2", "3"]), 100));
    }

    #[test]
    fn a_limit_of_zero_never_prunes_in_the_background() {
        assert!(!prune_allowed(&plan_against(1_000, &[], &["1"]), 0));
    }

    #[test]
    fn already_applied_removals_do_not_count_against_the_limit() {
        // 測るのは今送られるものだ｡CLI が既に大半を prune した plan が､
        // 届いた分のせいで上限超過のままになることはない｡
        let mut plan = plan_against(100, &[], &ELEVEN);
        plan.mark_applied("11", Action::Remove);
        assert!(prune_allowed(&plan, 10));
    }

    #[test]
    fn sendable_counts_removals_only_when_they_may_be_sent() {
        let plan = plan_against(100, &["a", "b"], &["1", "2", "3"]);
        assert_eq!(sendable(&plan, true), 5);
        assert_eq!(sendable(&plan, false), 2);
    }

    #[test]
    fn sendable_skips_what_already_landed() {
        let mut plan = plan_against(100, &["a"], &["1"]);
        plan.mark_applied("a", Action::Add);
        assert_eq!(sendable(&plan, true), 1);
    }

    #[test]
    fn a_diff_whose_removals_are_held_says_so_and_names_the_way_through() {
        let text = notice(&Outcome::Diffed {
            adds: 2,
            removals: 30,
            members_total: 100,
            held: true,
        })
        .unwrap();
        assert!(text.contains("30 of 100"), "{text}");
        assert!(text.contains("--prune"), "{text}");
    }

    #[test]
    fn a_diff_whose_removals_will_be_sent_reads_as_before() {
        let text = notice(&Outcome::Diffed {
            adds: 2,
            removals: 3,
            members_total: 100,
            held: false,
        })
        .unwrap();
        assert_eq!(text, "List sync: 2 to add, 3 to remove.");
    }
}
