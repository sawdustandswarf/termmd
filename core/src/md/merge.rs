//! Three-way merging by lines: two editors' changes to one file, against
//! the text both started from (docs/MARKDOWN.md, "Several editors at once").

use std::ops::Range;

/// The markers around a conflict, as `git merge` writes them.
pub const YOURS: &str = "<<<<<<< yours";
pub const SPLIT: &str = "=======";
pub const THEIRS: &str = ">>>>>>> file";

/// Past this many pairs of lines, a change is taken whole rather than diffed.
const MAX_CELLS: usize = 1 << 24;

/// `base[base]` became `new[new]`.
#[derive(Clone, Debug)]
struct Hunk {
    base: Range<usize>,
    new: Range<usize>,
}

/// Whether two changes need the same lines. Adding lines beside lines the
/// other changed doesn't, but adding them at one place does.
fn overlap(a: &Range<usize>, b: &Range<usize>) -> bool {
    match (a.is_empty(), b.is_empty()) {
        (true, true) => a.start == b.start,
        (true, false) => b.start < a.start && a.start < b.end,
        (false, true) => a.start < b.start && b.start < a.end,
        (false, false) => a.start.max(b.start) < a.end.min(b.end),
    }
}

/// `ours` and `theirs`, both changed from `base`: the merged lines, and
/// whether both changed the same lines (which then stand between markers).
pub fn merge(base: &[String], ours: &[String], theirs: &[String]) -> (Vec<String>, bool) {
    let (a, c) = (hunks(base, ours), hunks(base, theirs));
    let (mut x, mut y) = (0, 0);
    let (mut out, mut conflict, mut at) = (Vec::new(), false, 0);
    loop {
        let first = match (a.get(x), c.get(y)) {
            (None, None) => break,
            (Some(h), None) | (None, Some(h)) => h,
            (Some(p), Some(q)) => {
                if (p.base.start, p.base.end) <= (q.base.start, q.base.end) {
                    p
                } else {
                    q
                }
            }
        };
        // Everything that needs the lines the first one does.
        let mut lines = first.base.clone();
        let (mut xe, mut ye) = (x, y);
        loop {
            if a.get(xe).is_some_and(|h| h.base == first.base || overlap(&h.base, &lines)) {
                lines.end = lines.end.max(a[xe].base.end);
                xe += 1;
            } else if c.get(ye).is_some_and(|h| h.base == first.base || overlap(&h.base, &lines)) {
                lines.end = lines.end.max(c[ye].base.end);
                ye += 1;
            } else {
                break;
            }
        }
        out.extend_from_slice(&base[at..lines.start]);
        match (side(&a[x..xe], &lines, ours), side(&c[y..ye], &lines, theirs)) {
            (Some(p), Some(q)) if p == q => out.extend_from_slice(p),
            (Some(p), Some(q)) => {
                conflict = true;
                out.push(YOURS.to_owned());
                out.extend_from_slice(p);
                out.push(SPLIT.to_owned());
                out.extend_from_slice(q);
                out.push(THEIRS.to_owned());
            }
            (Some(p), None) | (None, Some(p)) => out.extend_from_slice(p),
            (None, None) => {}
        }
        at = lines.end;
        (x, y) = (xe, ye);
    }
    out.extend_from_slice(&base[at..]);
    (out, conflict)
}

/// What one side has in place of base lines `lines`, which its `hunks` lie in.
fn side<'a>(hunks: &[Hunk], lines: &Range<usize>, new: &'a [String]) -> Option<&'a [String]> {
    let (first, last) = (hunks.first()?, hunks.last()?);
    Some(&new[first.new.start - (first.base.start - lines.start)..last.new.end + (lines.end - last.base.end)])
}

/// What changed from `base` to `new`, in order.
fn hunks(base: &[String], new: &[String]) -> Vec<Hunk> {
    let to_new = matches(base, new);
    let mut out = Vec::new();
    let (mut i, mut n) = (0, 0);
    // The end of both is one more place they meet.
    for (j, target) in to_new.into_iter().chain([Some(new.len())]).enumerate() {
        if let Some(target) = target {
            if i < j || n < target {
                out.push(Hunk { base: i..j, new: n..target });
            }
            (i, n) = (j + 1, target + 1);
        }
    }
    out
}

/// Whether `lines` hold a conflict not yet sorted out.
pub fn has_conflict(lines: &[String]) -> bool {
    lines.iter().any(|l| l == YOURS) && lines.iter().any(|l| l == THEIRS)
}

/// For each line of `a`, the line of `b` it is in a longest common
/// subsequence with.
pub(crate) fn matches<T: PartialEq>(a: &[T], b: &[T]) -> Vec<Option<usize>> {
    let mut out = vec![None; a.len()];
    let prefix = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    let (rest_a, rest_b) = (&a[prefix..], &b[prefix..]);
    let suffix = rest_a.iter().rev().zip(rest_b.iter().rev()).take_while(|(x, y)| x == y).count();
    for (i, slot) in out.iter_mut().enumerate().take(prefix) {
        *slot = Some(i);
    }
    for k in 0..suffix {
        out[a.len() - 1 - k] = Some(b.len() - 1 - k);
    }
    let (mid_a, mid_b) = (&rest_a[..rest_a.len() - suffix], &rest_b[..rest_b.len() - suffix]);
    let (n, m) = (mid_a.len(), mid_b.len());
    if n == 0 || m == 0 || n.saturating_mul(m) > MAX_CELLS {
        return out;
    }
    // How long a common subsequence each pair of tails has.
    let mut lengths = vec![0u32; (n + 1) * (m + 1)];
    let at = |i: usize, j: usize| i * (m + 1) + j;
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lengths[at(i, j)] = if mid_a[i] == mid_b[j] {
                lengths[at(i + 1, j + 1)] + 1
            } else {
                lengths[at(i + 1, j)].max(lengths[at(i, j + 1)])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if mid_a[i] == mid_b[j] {
            out[prefix + i] = Some(prefix + j);
            (i, j) = (i + 1, j + 1);
        } else if lengths[at(i + 1, j)] >= lengths[at(i, j + 1)] {
            i += 1;
        } else {
            j += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(t: &str) -> Vec<String> {
        t.split('\n').map(str::to_owned).collect()
    }

    fn run(base: &str, ours: &str, theirs: &str) -> (String, bool) {
        let (out, conflict) = merge(&lines(base), &lines(ours), &lines(theirs));
        (out.join("\n"), conflict)
    }

    #[test]
    fn changes_to_different_lines_both_stay() {
        assert_eq!(run("a\nb\nc", "A\nb\nc", "a\nb\nC"), ("A\nb\nC".into(), false));
        assert_eq!(run("a\nb\nc", "a\nx\nb\nc", "a\nb\ny\nc"), ("a\nx\nb\ny\nc".into(), false));
        assert_eq!(run("a\nb\nc", "b\nc", "a\nb"), ("b".into(), false));
        assert_eq!(run("a\nb", "a\nb", "z\na\nb"), ("z\na\nb".into(), false));
        assert_eq!(run("a\nb", "a\nb\nz", "a\nb"), ("a\nb\nz".into(), false));
        // Side by side, not on the same lines.
        assert_eq!(run("a\nb", "A\nb", "a\nB"), ("A\nB".into(), false));
        assert_eq!(run("a", "A", "top\na"), ("top\nA".into(), false));
        assert_eq!(run("a", "A", "a\nend"), ("A\nend".into(), false));
    }

    #[test]
    fn the_same_change_is_one_change() {
        assert_eq!(run("a\nb", "a\nX", "a\nX"), ("a\nX".into(), false));
        assert_eq!(run("a", "a", "a"), ("a".into(), false));
    }

    #[test]
    fn both_changing_a_line_is_a_conflict() {
        let (out, conflict) = run("a\nb\nc", "a\nmine\nc", "a\nother\nc");
        assert!(conflict);
        assert_eq!(out, "a\n<<<<<<< yours\nmine\n=======\nother\n>>>>>>> file\nc");
        assert!(has_conflict(&lines(&out)));
        assert!(!has_conflict(&lines("a\n=======\nb")), "a heading's underline, say");
        // Deleting what the other changed.
        let (out, conflict) = run("a\nb\nc", "a\nc", "a\nB\nc");
        assert!(conflict);
        assert_eq!(out, "a\n<<<<<<< yours\n=======\nB\n>>>>>>> file\nc");
    }

    #[test]
    fn adding_at_one_place_is_a_conflict_unless_the_same() {
        assert!(run("a\nb", "a\nx\nb", "a\ny\nb").1);
        assert_eq!(run("a\nb", "a\nx\nb", "a\nx\nb"), ("a\nx\nb".into(), false));
    }

    #[test]
    fn random_merges_keep_both_sides_changes() {
        // Each side changes its own lines of a shared text, so all of them stay.
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = move |n: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed as usize % n
        };
        for _ in 0..300 {
            let n = 2 + next(30);
            let base: Vec<String> = (0..n).map(|i| format!("line {i}")).collect();
            let (mut ours, mut theirs, mut want) = (Vec::new(), Vec::new(), Vec::new());
            for (i, line) in base.iter().enumerate() {
                let them = i % 2 == 1;
                let (mine, other) = if them { (&mut theirs, &mut ours) } else { (&mut ours, &mut theirs) };
                other.push(line.clone());
                match next(4) {
                    0 => {
                        let new = format!("new {i}");
                        mine.push(new.clone());
                        want.push(new);
                    }
                    1 => {}
                    2 => {
                        let new = format!("added {i}");
                        mine.extend([line.clone(), new.clone()]);
                        want.extend([line.clone(), new]);
                    }
                    _ => {
                        mine.push(line.clone());
                        want.push(line.clone());
                    }
                }
            }
            assert_eq!(merge(&base, &ours, &theirs), (want, false), "{base:?} {ours:?} {theirs:?}");
            assert_eq!(merge(&base, &base, &theirs), (theirs.clone(), false));
            assert_eq!(merge(&base, &ours, &base), (ours.clone(), false));
            assert_eq!(merge(&base, &ours, &ours), (ours.clone(), false));
        }
    }

    #[test]
    fn an_everything_changed_file_still_merges() {
        let (out, conflict) = run("", "ours", "theirs");
        assert!(conflict);
        assert!(out.contains("ours") && out.contains("theirs"));
    }
}
