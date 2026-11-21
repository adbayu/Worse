#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Tag {
    Equal,
    Delete,
    Insert,
}

#[derive(Clone, Copy)]
pub(super) struct Op {
    tag: Tag,
    a: usize,
    b: usize,
}

/// Largest middle section (lines × lines) diffed with LCS; larger edits are shown as replace.
pub(super) const LCS_LIMIT: usize = 4_000_000;

/// Unified diff of two texts with 3 lines of context.
#[must_use]
pub fn unified_diff(label: &str, before: &str, after: &str) -> String {
    let a: Vec<&str> = before.lines().collect();
    let b: Vec<&str> = after.lines().collect();
    let ops = diff_ops(&a, &b);
    if ops.iter().all(|o| o.tag == Tag::Equal) {
        return String::new();
    }
    let mut out = format!("--- a/{label}\n+++ b/{label}\n");
    let ctx = 3;
    let mut i = 0;
    let mut prev_end = 0;
    while i < ops.len() {
        if ops[i].tag == Tag::Equal {
            i += 1;
            continue;
        }
        let start = i.saturating_sub(ctx).max(prev_end);
        let mut last_change = i;
        let mut j = i;
        while j < ops.len() {
            if ops[j].tag != Tag::Equal {
                last_change = j;
            } else if j - last_change > 2 * ctx {
                break;
            }
            j += 1;
        }
        let end = (last_change + ctx + 1).min(ops.len());
        let hunk = &ops[start..end];
        let old_len = hunk.iter().filter(|o| o.tag != Tag::Insert).count();
        let new_len = hunk.iter().filter(|o| o.tag != Tag::Delete).count();
        let old_start = hunk[0].a + usize::from(old_len > 0);
        let new_start = hunk[0].b + usize::from(new_len > 0);
        out.push_str(&format!(
            "@@ -{old_start},{old_len} +{new_start},{new_len} @@\n"
        ));
        for op in hunk {
            let (sign, line) = match op.tag {
                Tag::Equal => (' ', a[op.a]),
                Tag::Delete => ('-', a[op.a]),
                Tag::Insert => ('+', b[op.b]),
            };
            out.push(sign);
            out.push_str(line);
            out.push('\n');
        }
        prev_end = end;
        i = end;
    }
    out
}

pub(super) fn diff_ops(a: &[&str], b: &[&str]) -> Vec<Op> {
    let prefix = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    let max_suffix = a.len().min(b.len()) - prefix;
    let suffix = a
        .iter()
        .rev()
        .zip(b.iter().rev())
        .take(max_suffix)
        .take_while(|(x, y)| x == y)
        .count();
    let (am, bm) = (&a[prefix..a.len() - suffix], &b[prefix..b.len() - suffix]);
    let mut ops: Vec<Op> = (0..prefix)
        .map(|i| Op {
            tag: Tag::Equal,
            a: i,
            b: i,
        })
        .collect();
    let (mut ai, mut bi) = (prefix, prefix);
    if am.len().saturating_mul(bm.len()) <= LCS_LIMIT {
        // LCS table over the middle section.
        let w = bm.len() + 1;
        let mut t = vec![0u32; (am.len() + 1) * w];
        for i in (0..am.len()).rev() {
            for j in (0..bm.len()).rev() {
                t[i * w + j] = if am[i] == bm[j] {
                    t[(i + 1) * w + j + 1] + 1
                } else {
                    t[(i + 1) * w + j].max(t[i * w + j + 1])
                };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < am.len() || j < bm.len() {
            if i < am.len() && j < bm.len() && am[i] == bm[j] {
                ops.push(Op {
                    tag: Tag::Equal,
                    a: ai,
                    b: bi,
                });
                (i, j, ai, bi) = (i + 1, j + 1, ai + 1, bi + 1);
            } else if j < bm.len() && (i == am.len() || t[i * w + j + 1] > t[(i + 1) * w + j]) {
                ops.push(Op {
                    tag: Tag::Insert,
                    a: ai,
                    b: bi,
                });
                (j, bi) = (j + 1, bi + 1);
            } else {
                ops.push(Op {
                    tag: Tag::Delete,
                    a: ai,
                    b: bi,
                });
                (i, ai) = (i + 1, ai + 1);
            }
        }
    } else {
        for _ in am {
            ops.push(Op {
                tag: Tag::Delete,
                a: ai,
                b: bi,
            });
            ai += 1;
        }
        for _ in bm {
            ops.push(Op {
                tag: Tag::Insert,
                a: ai,
                b: bi,
            });
            bi += 1;
        }
    }
    for k in 0..suffix {
        ops.push(Op {
            tag: Tag::Equal,
            a: ai + k,
            b: bi + k,
        });
    }
    ops
}
