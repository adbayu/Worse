use super::{
    ask, fail, sv, window, ApiError, BTreeMap, GitCall, GitLogPage, GitLogRow, GitUi, GraphRow,
    ModelRc, Rc, VecModel, BOTTOM, LANE_W, MID, PAGE, STATE, TOP,
};
use slint::ComponentHandle;
use std::fmt::Write as _;
pub(super) fn load_graph() {
    let skip = STATE.with(|s| s.borrow().skip);
    let limit = PAGE.min(harness_workspace::LOG_DEPTH_MAX.saturating_sub(skip));
    if limit == 0 {
        return;
    }
    if let Some(w) = window() {
        w.global::<GitUi>().set_busy(true);
    }
    ask(
        |reply| GitCall::Log { skip, limit, reply },
        move |w, result: Result<GitLogPage, ApiError>| {
            let page = match result {
                Ok(page) => page,
                Err(e) => return fail(w, &e),
            };
            let ui = w.global::<GitUi>();
            let (rows, more) = STATE.with(|s| {
                let mut s = s.borrow_mut();
                let fetched = u32::try_from(page.rows.len()).unwrap_or(u32::MAX);
                for row in &page.rows {
                    let graph_row = graph_row(&mut s.open_lanes, row);
                    s.rows.push(graph_row);
                }
                s.skip = s.skip.saturating_add(fetched);
                let more = page.has_more && s.skip < harness_workspace::LOG_DEPTH_MAX;
                (s.rows.clone(), more)
            });
            ui.set_graph(ModelRc::from(Rc::new(VecModel::from(rows))));
            ui.set_graph_more(more);
            ui.set_busy(false);
        },
    );
}

/// One graph row: lines from the lanes that reach this commit, pass-through lines, and lines to each parent's lane.
fn graph_row(open: &mut BTreeMap<u32, String>, row: &GitLogRow) -> GraphRow {
    let x = |lane: u32| lane * LANE_W + LANE_W / 2;
    let dot = x(row.lane);
    let mut path = String::new();
    let mut width = row.lanes.max(row.lane + 1);
    for (lane, hash) in open.iter() {
        width = width.max(lane + 1);
        let lx = x(*lane);
        if *hash == row.hash {
            let _ = write!(path, "M {lx} {TOP} L {dot} {MID} ");
        } else {
            let _ = write!(path, "M {lx} {TOP} L {lx} {BOTTOM} ");
        }
    }
    open.retain(|_, hash| *hash != row.hash);
    for (i, parent) in row.parents.iter().enumerate() {
        let lane = row.parent_lanes.get(i).copied().unwrap_or(row.lane);
        width = width.max(lane + 1);
        let _ = write!(path, "M {dot} {MID} L {} {BOTTOM} ", x(lane));
        open.insert(lane, parent.clone());
    }
    let short = |h: &str| h.chars().take(7).collect::<String>();
    GraphRow {
        hash: sv(&row.hash),
        short: sv(&short(&row.hash)),
        subject: sv(&row.subject),
        author: sv(&row.author),
        date: sv(&crate::settings::date_of(row.time.saturating_mul(1000))),
        refs: sv(&row.refs.join(", ")),
        parents: sv(&row
            .parents
            .iter()
            .map(|p| short(p))
            .collect::<Vec<_>>()
            .join(" ")),
        lane: i32::try_from(row.lane).unwrap_or(0),
        lanes: i32::try_from(width).unwrap_or(1),
        path: sv(path.trim_end()),
    }
}
