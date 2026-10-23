//! The groups in the window: the sidebar list, and the open group's page.

use std::rc::Rc;

use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use crate::app::{initials, short_id, AppCtx};
use crate::engine::NodeService;
use crate::native::group::{GroupFile, Member, MemberState};
use crate::platform::toast;
use crate::{GroupFileRow, GroupRow, Logic, MemberRow, PeerRow, State};

/// Everything the window shows about one group, read off the UI thread.
struct GroupView {
    id: String,
    members: Vec<Member>,
    files: Vec<GroupFile>,
    my_state: MemberState,
}

/// The name to show for a member: what we call them if we are paired with
/// them, else the name the group's document carries.
fn member_name(paired: &[(String, String)], member: &Member) -> String {
    let name = paired
        .iter()
        .find(|(id, _)| *id == member.endpoint_id)
        .map(|(_, name)| name.as_str())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or(member.name.as_str());
    if name.trim().is_empty() {
        short_id(&member.endpoint_id)
    } else {
        name.to_string()
    }
}

fn human_size(bytes: u64) -> String {
    crate::format::fmt_bytes(bytes)
}

fn views(node: &NodeService) -> Vec<GroupView> {
    node.groups()
        .groups()
        .list()
        .into_iter()
        .filter_map(|group| {
            let my_state = group.my_state()?;
            matches!(my_state, MemberState::Pending | MemberState::Active).then(|| GroupView {
                id: group.id().to_string(),
                members: group.members(),
                files: group.files(),
                my_state,
            })
        })
        .collect()
}

/// Re-reads every group into the window.
pub(crate) fn refresh(ctx: &AppCtx) {
    let ctx_ui = ctx.clone();
    let _ = ctx.weak.upgrade_in_event_loop(move |ui| {
        let selected = ui.global::<State>().get_selected_group_id().to_string();
        load(&ctx_ui, selected);
    });
}

fn load(ctx: &AppCtx, selected_id: String) {
    let Some(node) = ctx.node() else {
        return;
    };
    let ctx = ctx.clone();
    ctx.rt.clone().spawn_blocking(move || {
        let me = node.device_info().endpoint_id.to_lowercase();
        let devices = node.list_paired().unwrap_or_default();
        let paired: Vec<(String, String)> = devices
            .iter()
            .map(|d| (d.endpoint_id.to_lowercase(), d.display_name.clone()))
            .collect();
        let groups = node.groups().clone();
        let views = views(&node);
        let online = |id: &str| groups.is_connected(id);

        let others = |view: &GroupView| -> Vec<(String, String)> {
            view.members
                .iter()
                .filter(|m| m.endpoint_id != me && m.state.is_connectable())
                .map(|m| (m.endpoint_id.clone(), member_name(&paired, m)))
                .collect()
        };

        let rows: Vec<(String, String, String, bool, bool)> = views
            .iter()
            .map(|view| {
                let others = others(view);
                let title = if others.is_empty() {
                    "Just you".to_string()
                } else {
                    others.iter().map(|(_, n)| n.as_str()).collect::<Vec<_>>().join(", ")
                };
                let invitation = view.my_state == MemberState::Pending;
                let detail = if invitation {
                    let inviter = view
                        .members
                        .iter()
                        .find(|m| m.endpoint_id == me)
                        .and_then(|mine| {
                            view.members.iter().find(|m| m.endpoint_id == mine.invited_by)
                        })
                        .map(|m| member_name(&paired, m))
                        .unwrap_or_else(|| "someone".to_string());
                    format!("Invitation from {inviter}")
                } else {
                    format!("{} members", others.len() + 1)
                };
                let any_online = others.iter().any(|(id, _)| online(id));
                (view.id.clone(), title, detail, invitation, any_online)
            })
            .collect();

        // The open group, if it still exists.
        struct Open {
            title: String,
            detail: String,
            invitation: bool,
            inviter: String,
            members: Vec<MemberRow>,
            files: Vec<(GroupFile, String, bool)>,
            invitable: Vec<PeerRowData>,
        }
        struct PeerRowData {
            id: String,
            name: String,
            online: bool,
        }
        let open = views.iter().find(|v| v.id == selected_id).map(|view| {
            let others = others(view);
            let invitation = view.my_state == MemberState::Pending;
            let title = rows
                .iter()
                .find(|r| r.0 == view.id)
                .map(|r| r.1.clone())
                .unwrap_or_default();
            let inviter = view
                .members
                .iter()
                .find(|m| m.endpoint_id == me)
                .and_then(|mine| view.members.iter().find(|m| m.endpoint_id == mine.invited_by))
                .map(|m| member_name(&paired, m))
                .unwrap_or_else(|| "Someone".to_string());
            let members = view
                .members
                .iter()
                .filter(|m| m.state.is_connectable())
                .map(|m| {
                    let name = member_name(&paired, m);
                    let detail = if m.endpoint_id == me {
                        "You".to_string()
                    } else if m.state == MemberState::Pending {
                        let by = view
                            .members
                            .iter()
                            .find(|x| x.endpoint_id == m.invited_by)
                            .map(|x| {
                                if x.endpoint_id == me {
                                    "you".to_string()
                                } else {
                                    member_name(&paired, x)
                                }
                            })
                            .unwrap_or_default();
                        format!("Invited by {by}")
                    } else if online(&m.endpoint_id) {
                        "Online".to_string()
                    } else {
                        "Offline".to_string()
                    };
                    let is_online = m.endpoint_id == me || online(&m.endpoint_id);
                    MemberRow {
                        initials: initials(&name).into(),
                        name: name.into(),
                        detail: detail.into(),
                        pending: m.state == MemberState::Pending,
                        online: is_online,
                        endpoint_id: m.endpoint_id.clone().into(),
                    }
                })
                .collect();
            let total = others.len() + 1;
            let files = view
                .files
                .iter()
                .map(|f| {
                    let owner = if f.owner == me {
                        "you".to_string()
                    } else {
                        view.members
                            .iter()
                            .find(|m| m.endpoint_id == f.owner)
                            .map(|m| member_name(&paired, m))
                            .unwrap_or_else(|| short_id(&f.owner))
                    };
                    let detail =
                        format!("Shared by {owner} · {} of {total} have it", f.holders.len());
                    let available = f.local_path.is_some()
                        || f.holders.iter().any(|h| *h != me && online(h));
                    (f.clone(), detail, available)
                })
                .collect();
            let in_group: Vec<&str> = view
                .members
                .iter()
                .filter(|m| m.state.is_connectable())
                .map(|m| m.endpoint_id.as_str())
                .collect();
            let invitable = devices
                .iter()
                .filter(|d| !in_group.contains(&d.endpoint_id.to_lowercase().as_str()))
                .map(|d| PeerRowData {
                    id: d.endpoint_id.to_lowercase(),
                    name: if d.display_name.trim().is_empty() {
                        short_id(&d.endpoint_id)
                    } else {
                        d.display_name.clone()
                    },
                    online: d.online,
                })
                .collect();
            Open {
                title,
                detail: if invitation {
                    "Invitation".to_string()
                } else {
                    format!("{total} members")
                },
                invitation,
                inviter,
                members,
                files,
                invitable,
            }
        });
        let invitations = rows.iter().filter(|r| r.3).count() as i32;
        let my_id = me.clone();
        let new_invitation = rows.iter().find(|r| r.3).map(|r| r.2.clone());

        let _ = ctx.weak.upgrade_in_event_loop(move |ui| {
            let state = ui.global::<State>();
            // Names of the files being downloaded in the open group.
            let group_key = crate::transfers::group_peer_key(&state.get_selected_group_id());
            let downloading: Vec<String> = state
                .get_transfers()
                .iter()
                .filter(|t| t.active && t.peer_id == group_key.as_str())
                .map(|t| t.title.to_string())
                .collect();
            if invitations > state.get_group_invitations() {
                let text = new_invitation
                    .as_deref()
                    .and_then(|d| d.strip_prefix("Invitation from "))
                    .map(|who| format!("{who} invited you to a group"))
                    .unwrap_or_else(|| "You were invited to a group".to_string());
                toast(&ui, &text, false);
            }
            state.set_group_invitations(invitations);
            let group_rows: Vec<GroupRow> = rows
                .into_iter()
                .map(|(id, title, detail, invitation, online)| GroupRow {
                    id: id.into(),
                    title: title.into(),
                    detail: detail.into(),
                    invitation,
                    online,
                })
                .collect();
            let selected_gone = open.is_none();
            state.set_groups(ModelRc::from(Rc::new(VecModel::from(group_rows))));
            match open {
                Some(open) => {
                    state.set_group_title(open.title.into());
                    state.set_group_detail(open.detail.into());
                    state.set_group_invitation(open.invitation);
                    state.set_group_inviter(open.inviter.into());
                    state.set_group_members(ModelRc::from(Rc::new(VecModel::from(open.members))));
                    let files: Vec<GroupFileRow> = open
                        .files
                        .into_iter()
                        .map(|(f, detail, available)| GroupFileRow {
                            downloading: downloading.contains(&f.name),
                            mine: f.owner == my_id,
                            have: f.local_path.is_some(),
                            available,
                            id: f.id.into(),
                            name: f.name.into(),
                            size: human_size(f.size).into(),
                            detail: detail.into(),
                        })
                        .collect();
                    state.set_group_files(ModelRc::from(Rc::new(VecModel::from(files))));
                    let invitable: Vec<PeerRow> = open
                        .invitable
                        .into_iter()
                        .map(|p| PeerRow {
                            initials: initials(&p.name).into(),
                            endpoint_id: p.id.into(),
                            name: p.name.into(),
                            online: p.online,
                            ..Default::default()
                        })
                        .collect();
                    state.set_group_invitable(ModelRc::from(Rc::new(VecModel::from(invitable))));
                }
                None => {
                    // Gone (left, declined): leave its page if it is showing.
                    if state.get_page() == "group" && selected_gone {
                        state.set_page(if state.get_compact() { "groups" } else { "peer" }.into());
                    }
                    state.set_selected_group_id("".into());
                    state.set_group_invitation(false);
                    state.set_group_invite_open(false);
                }
            }
        });
    });
}

/// Runs a group action in the background, then re-reads the groups.
fn act<F, Fut>(ctx: &AppCtx, what: &'static str, action: F)
where
    F: FnOnce(Arc<NodeService>) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = anyhow::Result<Option<String>>> + Send + 'static,
{
    let Some(node) = ctx.node() else {
        return;
    };
    let ctx = ctx.clone();
    ctx.rt.clone().spawn(async move {
        let result = action(node).await;
        let ctx_ui = ctx.clone();
        let _ = ctx.weak.upgrade_in_event_loop(move |ui| {
            match result {
                Ok(open) => {
                    if let Some(id) = open {
                        let state = ui.global::<State>();
                        state.set_selected_group_id(id.into());
                        state.set_group_invite_open(false);
                        state.set_page("group".into());
                    }
                }
                Err(e) => {
                    tracing::warn!("{what}: {e:#}");
                    toast(&ui, &format!("Could not {what}: {e}"), true);
                }
            }
            refresh(&ctx_ui);
        });
    });
}

use std::sync::Arc;

pub(crate) fn register(ctx: &AppCtx) {
    let Some(ui) = ctx.weak.upgrade() else {
        return;
    };
    let logic = ui.global::<Logic>();

    {
        let ctx = ctx.clone();
        logic.on_select_group(move |id: SharedString| {
            let Some(ui) = ctx.weak.upgrade() else {
                return;
            };
            let state = ui.global::<State>();
            if state.get_selected_group_id() != id {
                state.set_group_invite_open(false);
            }
            state.set_selected_group_id(id);
            refresh(&ctx);
        });
    }
    {
        let ctx = ctx.clone();
        logic.on_create_group(move |peer: SharedString| {
            let peer = peer.to_string();
            act(&ctx, "start the group", move |node| async move {
                node.groups().create_group(&peer).await.map(Some)
            });
        });
    }
    {
        let ctx = ctx.clone();
        logic.on_invite_to_group(move |peer: SharedString| {
            let (Some(ui), peer) = (ctx.weak.upgrade(), peer.to_string()) else {
                return;
            };
            let group = ui.global::<State>().get_selected_group_id().to_string();
            act(&ctx, "invite them", move |node| async move {
                node.groups().invite(&group, &peer).await.map(|()| None)
            });
        });
    }
    {
        let ctx = ctx.clone();
        logic.on_answer_group_invite(move |group: SharedString, accept: bool| {
            let group = group.to_string();
            act(&ctx, "answer the invitation", move |node| async move {
                node.groups().answer_invite(&group, accept).await.map(|()| None)
            });
        });
    }
    {
        let ctx = ctx.clone();
        logic.on_group_add_file(move || add_files(&ctx));
    }
    {
        let ctx = ctx.clone();
        logic.on_group_download_file(move |file: SharedString| download_file(&ctx, file.as_str()));
    }
    {
        let ctx = ctx.clone();
        logic.on_group_open_file(move |file: SharedString| {
            let Some(node) = ctx.node() else {
                return;
            };
            let Some(ui) = ctx.weak.upgrade() else {
                return;
            };
            let group = ui.global::<State>().get_selected_group_id().to_string();
            let path = node
                .groups()
                .groups()
                .get(&group)
                .and_then(|g| g.local_path(&file));
            match path {
                Some(path) => crate::platform::open_path(&path.to_string_lossy()),
                None => toast(&ui, "That file is not on this device any more", true),
            }
        });
    }
    {
        let ctx = ctx.clone();
        logic.on_group_remove_file(move |file: SharedString| {
            let Some(ui) = ctx.weak.upgrade() else {
                return;
            };
            let group = ui.global::<State>().get_selected_group_id().to_string();
            let file = file.to_string();
            act(&ctx, "stop sharing the file", move |node| async move {
                node.groups().remove_file(&group, &file).await.map(|()| None)
            });
        });
    }
    {
        let ctx = ctx.clone();
        logic.on_leave_group(move || {
            let Some(ui) = ctx.weak.upgrade() else {
                return;
            };
            let group = ui.global::<State>().get_selected_group_id().to_string();
            act(&ctx, "leave the group", move |node| async move {
                node.groups().leave(&group).await.map(|()| None)
            });
        });
    }
}

/// Asks the user for files and lists them in the open group.
fn add_files(ctx: &AppCtx) {
    let Some(ui) = ctx.weak.upgrade() else {
        return;
    };
    let group = ui.global::<State>().get_selected_group_id().to_string();
    if group.is_empty() {
        return;
    }
    #[cfg(target_os = "android")]
    {
        // The picker copies what was picked into the outbox and reports it
        // through `adopt_picked`.
        *PICKING_FOR.lock().unwrap() = Some((group, crate::android::outbox_files()));
        crate::android::pick_files();
    }
    #[cfg(not(target_os = "android"))]
    {
        let ctx = ctx.clone();
        ctx.rt.clone().spawn_blocking(move || {
            let picked = crate::platform::pick_group_files();
            if !picked.is_empty() {
                share_paths(&ctx, group, picked);
            }
        });
    }
}

/// Lists `paths` in the group, one at a time, off the UI thread.
fn share_paths(ctx: &AppCtx, group: String, paths: Vec<std::path::PathBuf>) {
    let Some(node) = ctx.node() else {
        return;
    };
    let ctx = ctx.clone();
    ctx.rt.clone().spawn(async move {
        for path in paths {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            if let Err(e) = node.groups().share_file(&group, path).await {
                tracing::warn!("share {name} in group: {e:#}");
                crate::platform::toast_later(
                    &ctx.weak,
                    format!("Could not share {name}: {e}"),
                    true,
                );
            }
        }
    });
}

/// The group a picker on Android is filling, and what the outbox held before
/// it opened (so only what was just picked is taken from it).
#[cfg(target_os = "android")]
static PICKING_FOR: std::sync::Mutex<Option<(String, Vec<std::path::PathBuf>)>> =
    std::sync::Mutex::new(None);

/// Android: files the picker just staged. True when they were for a group
/// and have been taken care of here.
#[cfg(target_os = "android")]
pub(crate) fn adopt_picked(ctx: &AppCtx, staged: &[std::path::PathBuf]) -> bool {
    let Some((group, before)) = PICKING_FOR.lock().unwrap().take() else {
        return false;
    };
    // The outbox is a queue to send from; a group's files have to stay put,
    // so they move to a folder of their own.
    let dir = crate::platform::default_data_dir().join("group-files");
    let mut kept = Vec::new();
    for path in staged.iter().filter(|p| !before.contains(p)) {
        let Some(name) = path.file_name() else {
            continue;
        };
        let target_dir = dir.join(uuid::Uuid::new_v4().simple().to_string());
        let target = target_dir.join(name);
        let moved = std::fs::create_dir_all(&target_dir)
            .and_then(|()| std::fs::rename(path, &target).or_else(|_| {
                std::fs::copy(path, &target).map(|_| ()).and_then(|()| std::fs::remove_file(path))
            }));
        match moved {
            Ok(()) => kept.push(target),
            Err(e) => tracing::warn!("keep picked file {}: {e}", path.display()),
        }
    }
    crate::transfers::refresh_outbox(ctx);
    if !kept.is_empty() {
        share_paths(ctx, group, kept);
    }
    true
}

/// Downloads a file of the open group from whoever online has it.
fn download_file(ctx: &AppCtx, file_id: &str) {
    let (Some(ui), Some(node)) = (ctx.weak.upgrade(), ctx.node()) else {
        return;
    };
    let group_id = ui.global::<State>().get_selected_group_id().to_string();
    let Some(file) = node
        .groups()
        .groups()
        .get(&group_id)
        .and_then(|g| g.files().into_iter().find(|f| f.id == file_id))
    else {
        return;
    };
    // Already on its way.
    let key = crate::transfers::group_peer_key(&group_id);
    let running = ui
        .global::<State>()
        .get_transfers()
        .iter()
        .any(|t| t.active && t.peer_id == key.as_str() && t.title == file.name.as_str());
    if running {
        return;
    }
    let ctx = ctx.clone();
    ctx.rt.clone().spawn(async move {
        let asked = node.groups().request_file(&group_id, &file.id).await;
        let ctx_ui = ctx.clone();
        let _ = ctx.weak.upgrade_in_event_loop(move |ui| match asked {
            Ok((holder, ticket)) => {
                crate::transfers::receive_group_file(
                    &ctx_ui,
                    &holder,
                    ticket,
                    crate::transfers::GroupDownload {
                        group_id,
                        file_id: file.id,
                        name: file.name,
                        size: file.size,
                        hash: file.hash,
                    },
                );
                refresh(&ctx_ui);
            }
            Err(e) => toast(&ui, &format!("Could not get {}: {e}", file.name), true),
        });
    });
}
