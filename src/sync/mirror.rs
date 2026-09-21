//! list members の全件取得を保存し､成功した write を追従する｡

use std::collections::HashSet;

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use super::{Action, Plan};
use crate::paths::Paths;
use crate::x_api::model::User;

// ponytail: x.com での手編集は最大 30 日見えない｡list.fields=member_count の
// probe へ拡張できる｡今すぐ全件を読むには sync_members.json を削除する｡
const MIRROR_MAX_AGE_SECONDS: i64 = 2_592_000;

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
    /// 時計の巻き戻りも期限切れと同じく全件取得へ戻す｡
    pub(super) fn usable(&self, list_id: &str, now: i64) -> bool {
        let reason = if self.list_id != list_id {
            Some("belongs to a different list")
        } else if self.read_at > now {
            Some("has a future read_at")
        // 外部の時刻同士なので､差が溢れたら期限切れとして扱う｡
        } else if now.saturating_sub(self.read_at) >= MIRROR_MAX_AGE_SECONDS {
            Some("has expired")
        } else {
            None
        };
        if let Some(reason) = reason {
            crate::log::info(&format!(
                "list sync: members mirror {reason}; a full member read is needed"
            ));
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

    fn log_drift(&self, fresh: &[User]) {
        let old: HashSet<&str> = self
            .members
            .iter()
            .map(|member| member.id.as_str())
            .collect();
        let new: HashSet<&str> = fresh.iter().map(|user| user.id.as_str()).collect();
        crate::log::info(&format!(
            "list sync: members mirror drift: {} only in mirror, {} only in fresh read",
            old.difference(&new).count(),
            new.difference(&old).count()
        ));
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
    let old = load(paths);
    if let Some(mirror) = &old
        && mirror.usable(list_id, now)
    {
        return Ok(mirror.users());
    }
    let users = read()?;
    if let Some(mirror) = old.filter(|mirror| mirror.list_id == list_id) {
        mirror.log_drift(&users);
    }
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
