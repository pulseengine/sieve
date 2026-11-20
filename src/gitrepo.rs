//! In-process git access through gix: no `git` subprocess (REQ-024). Every
//! failure is an `Err`, which the callers turn into run_all.

use std::path::Path;

fn open(root: &Path) -> Result<gix::Repository, String> {
    gix::open(root).map_err(|e| format!("open repository {}: {e}", root.display()))
}

/// Paths changed between merge-base(base, head) and head, as
/// `git diff --name-only --no-renames base...head` reports them: a rename is
/// a deletion plus an addition, directories are not listed, a submodule
/// (gitlink) bump is listed as its path.
pub fn diff_names(root: &Path, base: &str, head: &str) -> Result<Vec<String>, String> {
    let repo = open(root)?;
    let commit = |spec: &str| -> Result<gix::Commit<'_>, String> {
        repo.rev_parse_single(spec)
            .map_err(|e| format!("resolve {spec}: {e}"))?
            .object()
            .map_err(|e| format!("read {spec}: {e}"))?
            .peel_to_commit()
            .map_err(|e| format!("{spec} is not a commit: {e}"))
    };
    let base_c = commit(base)?;
    let head_c = commit(head)?;
    let mb = repo
        .merge_base(base_c.id, head_c.id)
        .map_err(|e| format!("merge-base {base} {head}: {e}"))?
        .ok_or_else(|| format!("{base} and {head} have no merge base"))?;
    let old_tree = repo
        .find_commit(mb)
        .map_err(|e| format!("read merge base: {e}"))?
        .tree()
        .map_err(|e| format!("merge-base tree: {e}"))?;
    let new_tree = head_c.tree().map_err(|e| format!("{head} tree: {e}"))?;
    // Default options: full paths, no rewrite tracking.
    let changes = repo
        .diff_tree_to_tree(&old_tree, &new_tree, gix::diff::Options::default())
        .map_err(|e| format!("tree diff: {e}"))?;
    let mut out: Vec<String> = changes
        .iter()
        .filter(|c| !c.entry_mode().is_tree())
        .map(|c| c.location().to_string())
        .collect();
    out.sort();
    out.dedup();
    Ok(out)
}

/// The paths in the repository index, as `git ls-files` lists them.
pub fn index_paths(root: &Path) -> Result<Vec<String>, String> {
    let repo = open(root)?;
    let index = repo.open_index().map_err(|e| format!("read index: {e}"))?;
    let mut out: Vec<String> = index
        .entries()
        .iter()
        .map(|e| e.path(&index).to_string())
        .collect();
    out.sort();
    out.dedup();
    Ok(out)
}

/// The last `n` first-parent commits reachable from `refspec`, newest first,
/// each with its first parent. A root commit (no parent) is left out.
pub fn first_parent_commits(
    root: &Path,
    refspec: &str,
    n: usize,
) -> Result<Vec<(String, String)>, String> {
    let repo = open(root)?;
    let tip = repo
        .rev_parse_single(refspec)
        .map_err(|e| format!("resolve {refspec}: {e}"))?
        .object()
        .map_err(|e| format!("read {refspec}: {e}"))?
        .peel_to_commit()
        .map_err(|e| format!("{refspec} is not a commit: {e}"))?
        .id;
    let walk = repo
        .rev_walk([tip])
        .first_parent_only()
        .all()
        .map_err(|e| format!("walk {refspec}: {e}"))?;
    let mut out = Vec::new();
    for info in walk {
        let info = info.map_err(|e| format!("walk {refspec}: {e}"))?;
        if let Some(parent) = info.parent_ids().next() {
            out.push((info.id.to_string(), parent.to_string()));
        }
        if out.len() >= n {
            break;
        }
    }
    Ok(out)
}

/// The text of `path` at merge-base(base, head) and at head, `None` where the
/// path is absent or not a regular file there (REQ-030 reads a workflow's two
/// versions). A blob that is not UTF-8 is an error.
pub fn blob_pair(
    root: &Path,
    base: &str,
    head: &str,
    path: &str,
) -> Result<(Option<String>, Option<String>), String> {
    let repo = open(root)?;
    let commit = |spec: &str| -> Result<gix::Commit<'_>, String> {
        repo.rev_parse_single(spec)
            .map_err(|e| format!("resolve {spec}: {e}"))?
            .object()
            .map_err(|e| format!("read {spec}: {e}"))?
            .peel_to_commit()
            .map_err(|e| format!("{spec} is not a commit: {e}"))
    };
    let base_c = commit(base)?;
    let head_c = commit(head)?;
    let mb = repo
        .merge_base(base_c.id, head_c.id)
        .map_err(|e| format!("merge-base {base} {head}: {e}"))?
        .ok_or_else(|| format!("{base} and {head} have no merge base"))?;
    let old_tree = repo
        .find_commit(mb)
        .map_err(|e| format!("read merge base: {e}"))?
        .tree()
        .map_err(|e| format!("merge-base tree: {e}"))?;
    let new_tree = head_c.tree().map_err(|e| format!("{head} tree: {e}"))?;
    let read = |tree: &gix::Tree<'_>, which: &str| -> Result<Option<String>, String> {
        let Some(entry) = tree
            .lookup_entry_by_path(path)
            .map_err(|e| format!("{path} at {which}: {e}"))?
        else {
            return Ok(None);
        };
        if !entry.mode().is_blob() {
            return Ok(None);
        }
        let obj = entry
            .object()
            .map_err(|e| format!("{path} at {which}: {e}"))?;
        String::from_utf8(obj.data.clone())
            .map(Some)
            .map_err(|_| format!("{path} at {which} is not UTF-8"))
    };
    Ok((read(&old_tree, "merge base")?, read(&new_tree, head)?))
}
