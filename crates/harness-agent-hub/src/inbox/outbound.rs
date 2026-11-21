use super::*;

impl Hub {
    pub(super) async fn close_thread(&self, source: &InboxSource, number: u64) {
        let source_id = source.id.clone();
        let result = self
            .ctx
            .store
            .run(move |c| {
                let n = i64::try_from(number).unwrap_or(i64::MAX);
                let mut keys = Vec::new();
                let mut changed = false;
                for mut stored in tickets(c, "WHERE source_id=?1 AND number=?2", &[&source_id, &n])?
                {
                    if !stored.ticket.status.is_active() {
                        continue;
                    }
                    stored.ticket.status = TicketStatus::Done;
                    stored.ticket.pending_post = false;
                    save_ticket(c, &mut stored)?;
                    changed = true;
                    if let Some(session) = &stored.ticket.session_id {
                        keys.extend(crate::board::move_session_cards(
                            c,
                            session,
                            BoardLane::Done,
                        )?);
                    }
                }
                Ok((changed, keys))
            })
            .await;
        match result {
            Ok((changed, keys)) => {
                for key in keys {
                    self.ctx
                        .events
                        .emit(None, EventKind::BoardChanged { workspace_key: key });
                }
                if changed {
                    self.inbox_changed();
                }
            }
            Err(error) => tracing::warn!(%error, "ticket not closed"),
        }
    }

    pub(super) async fn finish_turn(&self, id: String, session: SessionId, turn: TurnId) {
        let text = match HarnessCore::open_session(self, session).await {
            Ok(detail) => detail
                .turns
                .iter()
                .find(|t| t.id == turn)
                .map(|t| t.assistant_text.trim().to_owned())
                .unwrap_or_default(),
            Err(_) => String::new(),
        };
        let summary = clip(&text, SUMMARY_LIMIT);
        let Ok(stored) = self.ticket(&id).await else {
            return;
        };
        if !stored.ticket.status.is_active() {
            return;
        }
        let draft = stored.ticket.shadow
            || self
                .source(&stored.ticket.source_id)
                .await
                .is_ok_and(|s| s.draft_first);
        let _ = self
            .update_ticket(&id, move |s| {
                s.summary = summary;
                if draft {
                    s.ticket.status = TicketStatus::Review;
                    s.ticket.pending_post = true;
                }
                true
            })
            .await;
        if draft {
            return;
        }
        if let Err(error) = self.outbound(id).await {
            tracing::info!(%error, "ticket not posted");
        }
    }

    pub(super) async fn outbound(&self, id: String) -> Result<(), String> {
        let claimed = self
            .inbox
            .outbound
            .lock()
            .is_ok_and(|mut set| set.insert(id.clone()));
        if !claimed {
            return Err("the ticket is already being posted".into());
        }
        let result = self.post(&id).await;
        if let Ok(mut set) = self.inbox.outbound.lock() {
            set.remove(&id);
        }
        let outcome = result.clone();
        let _ = self
            .update_ticket(&id, move |s| {
                if matches!(
                    s.ticket.status,
                    TicketStatus::Done | TicketStatus::Discarded
                ) {
                    return false;
                }
                match outcome {
                    Ok(()) => {
                        s.ticket.status = TicketStatus::Review;
                        s.ticket.pending_post = false;
                        s.ticket.error = None;
                    }
                    Err(error) => {
                        s.ticket.status = TicketStatus::Failed;
                        s.ticket.error = Some(error);
                    }
                }
                true
            })
            .await;
        result
    }

    pub(super) async fn post(&self, id: &str) -> Result<(), String> {
        let stored = self.ticket(id).await.map_err(|e| e.to_string())?;
        let source = self
            .source(&stored.ticket.source_id)
            .await
            .map_err(|_| "the repository was removed from the inbox".to_owned())?;
        let tree = Path::new(&stored.worktree);
        let number = stored.ticket.number;
        let branch = stored.ticket.branch.clone().unwrap_or_default();
        if stored.worktree.is_empty() || branch.is_empty() {
            return Err("the ticket has no worktree".into());
        }
        let status = git(tree, &["status", "--porcelain"]).await?;
        if !status.trim().is_empty() {
            git(tree, &["add", "-A"]).await?;
            let subject = format!("{} (#{number})", stored.ticket.title);
            let trailer = format!("Harness ticket {}", stored.ticket.id);
            git(tree, &["commit", "-m", &subject, "-m", &trailer]).await?;
        }
        let range = format!("{}..HEAD", stored.base);
        let ahead = git(tree, &["rev-list", "--count", &range])
            .await?
            .trim()
            .parse::<u64>()
            .unwrap_or(0);
        let marker = format!("<!-- harness:ticket {} -->", stored.ticket.id);
        let summary = if stored.summary.trim().is_empty() {
            "Harness finished the work.".to_owned()
        } else {
            stored.summary.clone()
        };
        let mut pr_url = stored.ticket.pr_url.clone();
        if ahead > 0 {
            git(tree, &["push", "-u", "origin", &branch]).await?;
            if pr_url.is_none() {
                let url = self
                    .open_pull(&source.repo, &branch, &stored, &summary, &marker)
                    .await?;
                let saved = url.clone();
                let _ = self
                    .update_ticket(id, move |s| {
                        s.ticket.pr_url = Some(saved);
                        true
                    })
                    .await;
                pr_url = Some(url);
            }
        }
        let mut body = summary;
        if let Some(url) = &pr_url {
            let _ = write!(body, "\n\nPull request: {url}");
        } else {
            body.push_str("\n\nNo file changes were needed.");
        }
        let _ = write!(body, "\n\n{marker}");
        let path = format!("repos/{}/issues/{number}/comments", source.repo);
        let payload = serde_json::json!({ "body": body }).to_string();
        gh(&["api", "-X", "POST", &path, "--input", "-"], Some(payload)).await?;
        Ok(())
    }

    pub(super) async fn open_pull(
        &self,
        repo: &str,
        branch: &str,
        stored: &Stored,
        summary: &str,
        marker: &str,
    ) -> Result<String, String> {
        let base = gh(
            &["api", &format!("repos/{repo}"), "--jq", ".default_branch"],
            None,
        )
        .await?
        .trim()
        .to_owned();
        let payload = serde_json::json!({
            "title": stored.ticket.title,
            "head": branch,
            "base": if base.is_empty() { "main" } else { base.as_str() },
            "body": format!("Closes #{}\n\n{summary}\n\n{marker}", stored.ticket.number),
        })
        .to_string();
        let path = format!("repos/{repo}/pulls");
        match gh(&["api", "-X", "POST", &path, "--input", "-"], Some(payload)).await {
            Ok(out) => {
                let url = text_of(&parse_json(&out)?["html_url"]);
                if url.is_empty() {
                    Err("gh did not return the pull request link".into())
                } else {
                    Ok(url)
                }
            }
            Err(error) => {
                let owner = repo.split('/').next().unwrap_or_default();
                let head = format!("head={owner}:{branch}");
                let existing = gh(
                    &["api", "-X", "GET", &path, "-f", &head, "-f", "state=open"],
                    None,
                )
                .await
                .ok()
                .and_then(|out| parse_json(&out).ok())
                .and_then(|list| list[0]["html_url"].as_str().map(str::to_owned));
                existing.ok_or(error)
            }
        }
    }

    pub(crate) async fn ensure_inbox(&self) {
        let wanted = self.check_open().is_ok()
            && self.inbox_enabled().await.unwrap_or(false)
            && self
                .inbox_sources()
                .await
                .is_ok_and(|list| list.iter().any(|s| s.enabled));
        if !wanted {
            self.inbox.stop();
            return;
        }
        let Ok(mut slot) = self.inbox.task.lock() else {
            return;
        };
        if slot.as_ref().is_some_and(|t| !t.is_finished()) {
            self.inbox.wake.notify_one();
            return;
        }
        let weak = self.me.clone();
        let wake = Arc::clone(&self.inbox.wake);
        *slot = Some(tokio::spawn(run_loop(weak, wake)));
    }
}
