use super::{answer, api, ids, summary, Adapter, Arc, ChatCall, FIND_LIMIT};

impl Adapter {
    /// Runs one chat call off the event loop; turn items are resolved here, before the call is spawned.
    #[allow(clippy::too_many_lines)]
    pub(super) fn chat(&self, call: ChatCall) {
        let core = Arc::clone(&self.core);
        match call {
            ChatCall::Rename {
                session,
                title,
                reply,
            } => answer(reply, async move {
                core.rename_session(api::SessionId::new(session), title)
                    .await
            }),
            ChatCall::SetPinned {
                sessions,
                pinned,
                reply,
            } => answer(reply, async move {
                core.set_pinned(ids(sessions), pinned).await
            }),
            ChatCall::SetUnread {
                session,
                unread,
                reply,
            } => answer(reply, async move {
                core.set_unread(api::SessionId::new(session), unread).await
            }),
            ChatCall::ListGroups(reply) => answer(reply, async move { core.list_groups().await }),
            ChatCall::CreateGroup { name, color, reply } => {
                answer(reply, async move { core.create_group(name, color).await });
            }
            ChatCall::UpdateGroup(request, reply) => {
                answer(reply, async move { core.update_group(request).await });
            }
            ChatCall::DeleteGroup(id, reply) => {
                answer(reply, async move { core.delete_group(id).await });
            }
            ChatCall::ReorderGroups(order, reply) => {
                answer(reply, async move { core.reorder_groups(order).await });
            }
            ChatCall::MoveSessions {
                sessions,
                group,
                reply,
            } => answer(reply, async move {
                core.move_sessions(ids(sessions), group).await
            }),
            ChatCall::Export { session, reply } => answer(reply, async move {
                core.open_session(api::SessionId::new(session)).await
            }),
            ChatCall::Fork {
                session,
                item,
                reply,
            } => {
                let up_to_turn = match item.map(|i| self.turn_of(&session, i)).transpose() {
                    Ok(turn) => turn,
                    Err(error) => return drop(reply.send(Err(error))),
                };
                answer(reply, async move {
                    let request = api::ForkSession {
                        session_id: api::SessionId::new(session),
                        up_to_turn,
                        title: None,
                    };
                    core.fork_session(request).await.map(|s| summary(&s))
                });
            }
            ChatCall::Rewind {
                session,
                item,
                restore_files,
                reply,
            } => {
                let turn_id = match self.turn_of(&session, item) {
                    Ok(turn) => turn,
                    Err(error) => return drop(reply.send(Err(error))),
                };
                answer(reply, async move {
                    let request = api::Rewind {
                        session_id: api::SessionId::new(session),
                        turn_id,
                        restore_files,
                    };
                    core.rewind_session(request).await
                });
            }
            ChatCall::EditResend {
                session,
                item,
                text,
                context,
                agent,
                restore_files,
                reply,
            } => {
                let turn_id = match self.turn_of(&session, item) {
                    Ok(turn) => turn,
                    Err(error) => return drop(reply.send(Err(error))),
                };
                answer(reply, async move {
                    let request = api::EditResend {
                        session_id: api::SessionId::new(session),
                        turn_id,
                        text,
                        context,
                        agent: agent.map(api::AgentId::new),
                        restore_files,
                    };
                    core.edit_and_resend(request).await.map(drop)
                });
            }
            ChatCall::Retry {
                session,
                item,
                agent,
                restore_files,
                reply,
            } => {
                let turn_id = match self.turn_of(&session, item) {
                    Ok(turn) => turn,
                    Err(error) => return drop(reply.send(Err(error))),
                };
                answer(reply, async move {
                    let request = api::Retry {
                        session_id: api::SessionId::new(session),
                        turn_id,
                        agent: agent.map(api::AgentId::new),
                        restore_files,
                    };
                    core.retry_turn(request).await.map(drop)
                });
            }
            ChatCall::PlanFiles {
                session,
                item,
                direction,
                reply,
            } => {
                let turn_id = match self.turn_of(&session, item) {
                    Ok(turn) => turn,
                    Err(error) => return drop(reply.send(Err(error))),
                };
                answer(reply, async move {
                    core.plan_file_changes(api::SessionId::new(session), turn_id, direction)
                        .await
                });
            }
            ChatCall::ApplyFiles {
                session,
                item,
                direction,
                paths,
                include_unsafe,
                reply,
            } => {
                let turn_id = match self.turn_of(&session, item) {
                    Ok(turn) => turn,
                    Err(error) => return drop(reply.send(Err(error))),
                };
                answer(reply, async move {
                    let request = api::ApplyFileChanges {
                        session_id: api::SessionId::new(session),
                        turn_id,
                        direction,
                        paths,
                        include_unsafe,
                    };
                    core.apply_file_changes(request).await
                });
            }
            ChatCall::Find {
                session,
                query,
                case_sensitive,
                reply,
            } => {
                let targets = self.find_targets(&session);
                answer(reply, async move {
                    let request = api::FindInSession {
                        session_id: api::SessionId::new(session),
                        query,
                        case_sensitive,
                        limit: FIND_LIMIT,
                    };
                    let hits = core.find_in_session(request).await?;
                    Ok(hits.into_iter().map(|h| targets.place(h)).collect())
                });
            }
            ChatCall::ResolveContext {
                session,
                items,
                reply,
            } => answer(reply, async move {
                core.resolve_context(api::SessionId::new(session), items)
                    .await
            }),
            ChatCall::Mentions(query, reply) => {
                answer(reply, async move { core.mention_candidates(query).await });
            }
            ChatCall::Commands { session, reply } => answer(reply, async move {
                core.list_commands(api::SessionId::new(session)).await
            }),
            ChatCall::Search(request, reply) => {
                answer(reply, async move { core.command_search(request).await });
            }
            ChatCall::ImportScan(request, reply) => {
                answer(reply, async move { core.import_scan(request).await });
            }
            ChatCall::Import(request, reply) => {
                answer(reply, async move { core.import_sessions(request).await });
            }
            ChatCall::SchedulesEnabled(reply) => {
                answer(reply, async move { core.schedules_enabled().await });
            }
            ChatCall::SetSchedulesEnabled(enabled, reply) => {
                answer(
                    reply,
                    async move { core.set_schedules_enabled(enabled).await },
                );
            }
            ChatCall::ListSchedules(reply) => {
                answer(reply, async move { core.list_schedules().await });
            }
            ChatCall::CreateSchedule(request, reply) => {
                answer(reply, async move { core.create_schedule(request).await });
            }
            ChatCall::UpdateSchedule(request, reply) => {
                answer(reply, async move { core.update_schedule(request).await });
            }
            ChatCall::DeleteSchedule(id, reply) => {
                answer(reply, async move { core.delete_schedule(id).await });
            }
            ChatCall::RunScheduleNow(id, reply) => {
                answer(reply, async move { core.run_schedule_now(id).await });
            }
            ChatCall::GetOrchestra(reply) => {
                answer(reply, async move { core.get_orchestra().await });
            }
            ChatCall::SetOrchestra(orchestra, reply) => {
                answer(reply, async move { core.set_orchestra(orchestra).await });
            }
            ChatCall::Board(key, reply) => answer(reply, async move { core.board(key).await }),
            ChatCall::CreateCard(request, reply) => {
                answer(reply, async move { core.create_card(request).await });
            }
            ChatCall::UpdateCard(request, reply) => {
                answer(reply, async move { core.update_card(request).await });
            }
            ChatCall::MoveCard {
                card,
                lane,
                index,
                reply,
            } => answer(
                reply,
                async move { core.move_card(card, lane, index).await },
            ),
            ChatCall::DeleteCard(card, reply) => {
                answer(reply, async move { core.delete_card(card).await });
            }
            ChatCall::StartCard(card, reply) => {
                answer(reply, async move { core.start_card(card).await });
            }
            ChatCall::LinkBoard(key, reply) => {
                answer(reply, async move { core.link_board(key).await });
            }
            ChatCall::UnlinkBoard(key, reply) => {
                answer(reply, async move { core.unlink_board(key).await });
            }
            ChatCall::SyncBoard(key, reply) => {
                answer(reply, async move { core.sync_board(key).await });
            }
            ChatCall::InboxStatus(reply) => answer(reply, async move { core.inbox_status().await }),
            ChatCall::SetInboxEnabled(on, reply) => {
                answer(reply, async move { core.set_inbox_enabled(on).await });
            }
            ChatCall::SaveInboxSource(source, reply) => {
                answer(reply, async move { core.save_inbox_source(source).await });
            }
            ChatCall::DeleteInboxSource(id, reply) => {
                answer(reply, async move { core.delete_inbox_source(id).await });
            }
            ChatCall::PollInboxNow(id, reply) => {
                answer(reply, async move { core.poll_inbox_now(id).await });
            }
            ChatCall::Tickets(reply) => answer(reply, async move { core.list_tickets().await }),
            ChatCall::Triggers(limit, reply) => {
                answer(reply, async move { core.list_triggers(limit).await });
            }
            ChatCall::PostTicket(id, reply) => {
                answer(reply, async move { core.post_ticket(id).await });
            }
            ChatCall::StopTicket(id, reply) => {
                answer(reply, async move { core.stop_ticket(id).await });
            }
            ChatCall::DiscardTicket(id, reply) => {
                answer(reply, async move { core.discard_ticket(id).await });
            }
            ChatCall::ResumableRun { session, reply } => answer(reply, async move {
                core.resumable_run(api::SessionId::new(session)).await
            }),
            ChatCall::ResumeRun { session, reply } => answer(reply, async move {
                core.resume_run(api::SessionId::new(session))
                    .await
                    .map(drop)
            }),
            ChatCall::DiscardRun { session, reply } => answer(reply, async move {
                core.discard_run(api::SessionId::new(session)).await
            }),
        }
    }

    /// Title of a (top-level) session for the subagent bar; falls back to the id.
    pub(super) async fn session_title(&self, id: &api::SessionId) -> String {
        let list = self
            .core
            .list_sessions(api::ListSessions::default())
            .await
            .unwrap_or_default();
        list.into_iter()
            .find(|s| &s.id == id)
            .map(|s| s.title)
            .filter(|t| !t.trim().is_empty())
            .unwrap_or_else(|| id.0.clone())
    }
}
