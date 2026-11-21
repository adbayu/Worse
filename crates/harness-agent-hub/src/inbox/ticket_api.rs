use super::{
    invalid, load_ticket, params, save_ticket, tickets, ApiError, ApiResult, HarnessCore, Hub,
    Stored, Ticket, TicketStatus, TriggerRecord,
};

impl Hub {
    pub(crate) async fn do_list_tickets(&self) -> ApiResult<Vec<Ticket>> {
        self.check_open()?;
        let list = self.ctx.store.run(|c| tickets(c, "", &[])).await?;
        Ok(list.into_iter().map(|s| s.ticket).collect())
    }

    pub(crate) async fn do_list_triggers(&self, limit: u32) -> ApiResult<Vec<TriggerRecord>> {
        self.check_open()?;
        let limit = i64::from(limit.clamp(1, 500));
        Ok(self
            .ctx
            .store
            .run(move |c| {
                let mut statement = c.prepare(
                    "SELECT json FROM inbox_triggers ORDER BY received_at DESC LIMIT ?1",
                )?;
                let rows = statement
                    .query_map(params![limit], |r| r.get::<_, String>(0))?
                    .filter_map(Result::ok)
                    .filter_map(|json| serde_json::from_str(&json).ok())
                    .collect();
                Ok(rows)
            })
            .await?)
    }

    pub(super) async fn ticket(&self, id: &str) -> ApiResult<Stored> {
        let lookup = id.to_owned();
        self.ctx
            .store
            .run(move |c| load_ticket(c, &lookup))
            .await?
            .ok_or_else(|| ApiError::NotFound(format!("ticket {id}")))
    }

    pub(super) async fn update_ticket(
        &self,
        id: &str,
        change: impl FnOnce(&mut Stored) -> bool + Send + 'static,
    ) -> ApiResult<Option<Stored>> {
        let lookup = id.to_owned();
        let updated = self
            .ctx
            .store
            .run(move |c| {
                let Some(mut stored) = load_ticket(c, &lookup)? else {
                    return Ok(None);
                };
                if !change(&mut stored) {
                    return Ok(None);
                }
                save_ticket(c, &mut stored)?;
                Ok(Some(stored))
            })
            .await?;
        if updated.is_some() {
            self.inbox_changed();
        }
        Ok(updated)
    }

    pub(crate) async fn do_post_ticket(&self, id: String) -> ApiResult<()> {
        self.check_open()?;
        let stored = self.ticket(&id).await?;
        if stored.ticket.shadow {
            if !stored.ticket.status.is_active() && stored.ticket.status != TicketStatus::Failed {
                return Err(invalid("this ticket is closed"));
            }
            let promoted = self.promote(&id).await.map_err(ApiError::Internal)?;
            if !matches!(
                promoted.ticket.status,
                TicketStatus::Review | TicketStatus::Failed
            ) {
                return Ok(());
            }
        } else if !matches!(
            stored.ticket.status,
            TicketStatus::Review | TicketStatus::Failed
        ) || stored.ticket.session_id.is_none()
        {
            return Err(invalid("only a finished ticket can be posted"));
        }
        self.outbound(id).await.map_err(ApiError::Internal)
    }

    pub(crate) async fn do_stop_ticket(&self, id: String) -> ApiResult<()> {
        self.check_open()?;
        let stored = self.ticket(&id).await?;
        if let Some(session) = stored.ticket.session_id.clone() {
            let _ = HarnessCore::stop(self, session).await;
        }
        self.update_ticket(&id, |s| {
            if !s.ticket.status.is_active() {
                return false;
            }
            s.ticket.status = TicketStatus::Failed;
            s.ticket.error = Some("stopped".into());
            true
        })
        .await?;
        Ok(())
    }

    pub(crate) async fn do_discard_ticket(&self, id: String) -> ApiResult<()> {
        self.check_open()?;
        let stored = self.ticket(&id).await?;
        if let Some(session) = stored.ticket.session_id.clone() {
            let _ = HarnessCore::stop(self, session).await;
        }
        self.update_ticket(&id, |s| {
            s.ticket.status = TicketStatus::Discarded;
            s.ticket.pending_post = false;
            true
        })
        .await?;
        Ok(())
    }
}
