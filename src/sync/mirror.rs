//! このアプリが list に入れた相手の台帳｡最初の全件取得を種にし､成功した
//! write を追従する｡
//!
//! list の写しではない｡x.com で手で足した相手は載らないので diff に現れず､
//! prune も届かない｡手で外した相手は載ったままなので､足し直されない｡
//! どちらも手の編集を尊重する側に倒れる｡
//!
//! 古さでは読み直さない｡member の全件取得は following の 10 倍高く
//! (`x-api-budget` の pricing.md 実測ログ 5)､残高が足りなければ途中の 402 で
//! 何も残らない｡読み直したいときは `sync_members.json` を消す｡

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use super::{Action, Plan};
use crate::paths::Paths;
use crate::x_api::model::User;

#[derive(Debug, Serialize, Deserialize)]
struct Member {
    id: String,
    username: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct Mirror {
    version: u32,
    list_id: String,
    read_at: i64,
    members: Vec<Member>,
}

impl Mirror {
    /// 別の list の台帳だけを退ける｡`read_at` は記録で､判定には使わない｡
    pub(super) fn usable(&self, list_id: &str) -> bool {
        if self.list_id != list_id {
            crate::log::info(
                "list sync: members mirror belongs to a different list; a full member read is \
                 needed",
            );
            return false;
        }
        true
    }

    pub(super) fn members_total(&self, list_id: &str) -> Option<usize> {
        (self.list_id == list_id).then_some(self.members.len())
    }

    fn users(&self) -> Vec<User> {
        self.members
            .iter()
            .map(|member| User {
                id: member.id.clone(),
                name: member.username.clone(),
                username: member.username.clone(),
                profile_image_url: None,
            })
            .collect()
    }

    fn save(&self, paths: &Paths) -> Result<()> {
        let path = paths.sync_members_file();
        let json =
            serde_json::to_string_pretty(self).context("could not serialize the members mirror")?;
        std::fs::write(&path, json).with_context(|| format!("could not write {}", path.display()))
    }
}

/// 無いファイルは黙って扱い､壊れた記録は理由を残す｡
pub(super) fn load(paths: &Paths) -> Option<Mirror> {
    let contents = match std::fs::read_to_string(paths.sync_members_file()) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            crate::log::warn(&format!(
                "list sync: could not read members mirror: {error}; a full member read is needed"
            ));
            return None;
        }
    };
    match serde_json::from_str::<Mirror>(&contents) {
        Ok(mirror) if mirror.version == 1 => Some(mirror),
        Ok(mirror) => {
            crate::log::warn(&format!(
                "list sync: unsupported members mirror version {}; a full member read is needed",
                mirror.version
            ));
            None
        }
        Err(error) => {
            crate::log::warn(&format!(
                "list sync: corrupt members mirror: {error}; a full member read is needed"
            ));
            None
        }
    }
}

/// 全件取得が終わった時点で保存し､後続の following の失敗から切り離す｡
pub(super) fn members(
    paths: &Paths,
    list_id: &str,
    now: i64,
    read: impl FnOnce() -> Result<Vec<User>>,
) -> Result<Vec<User>> {
    if let Some(mirror) = load(paths)
        && mirror.usable(list_id)
    {
        return Ok(mirror.users());
    }
    let users = read()?;
    Mirror {
        version: 1,
        list_id: list_id.to_string(),
        read_at: now,
        members: users
            .iter()
            .map(|user| Member {
                id: user.id.clone(),
                username: user.username.clone(),
            })
            .collect(),
    }
    .save(paths)?;
    Ok(users)
}

/// 成功した write だけを追従する｡取得時刻は全件取得のものを維持する｡
pub(super) fn applied(paths: &Paths, plan: &Plan, id: &str, action: Action) -> Result<()> {
    let Some(mut mirror) = load(paths).filter(|mirror| mirror.list_id == plan.list_id) else {
        return Ok(());
    };
    match action {
        Action::Add => {
            if !mirror.members.iter().any(|member| member.id == id)
                && let Some(entry) = plan
                    .entries
                    .iter()
                    .find(|entry| entry.user_id == id && entry.action == action)
            {
                mirror.members.push(Member {
                    id: id.to_string(),
                    username: entry.username.clone(),
                });
            }
        }
        Action::Remove => mirror.members.retain(|member| member.id != id),
    }
    mirror.save(paths)
}
