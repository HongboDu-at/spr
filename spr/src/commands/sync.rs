/*
 * Copyright (c) Radical HQ Limited
 *
 * This source code is licensed under the MIT license found in the
 * LICENSE file in the root directory of this source tree.
 */

use crate::{
    error::{Error, Result, ResultExt},
    git::{Git, PreparedCommit},
    github::{PullRequest, PullRequestState},
    message::build_commit_message,
    output::{output, write_commit_title},
};
use git2::Oid;

#[derive(Debug, clap::Parser)]
pub struct SyncOptions {
    #[clap(flatten)]
    selection: crate::commands::select::CommitSelection,

    /// Show what would change without modifying the local stack
    #[clap(long)]
    dry_run: bool,
}

pub async fn sync(
    opts: SyncOptions,
    git: &crate::git::Git,
    gh: &mut crate::github::GitHub,
    config: &crate::config::Config,
) -> Result<()> {
    git.check_no_uncommitted_changes()?;

    // Pre-fetch origin/main once so that concurrent PR fetch tasks
    // (spawned below) don't race on updating this shared ref.
    Git::fetch_from_remote(&[&config.master_ref], &config.remote_name).await?;

    // Only fetch the Pull Requests that this run can look at: each one costs
    // a query and a `git fetch`.
    let pull_request_filter = opts.selection.pull_request_filter(git)?;
    let mut prepared_commits =
        git.get_prepared_commits(config, Some(gh), pull_request_filter)?;
    let length = prepared_commits.len();

    let master_base_oid = if let Some(first_commit) = prepared_commits.first() {
        first_commit.parent_oid
    } else {
        output("👋", "Branch is empty - nothing to do. Good bye!")?;
        return Ok(());
    };

    // Select which commits to consider BEFORE fetching PR info, so the
    // interactive prompt appears immediately.
    let selected_indexes = opts.selection.resolve(
        git,
        config,
        &prepared_commits,
        "Select commits to sync:",
    )?;

    // Await PR tasks only for selected commits
    let mut pull_requests: Vec<Option<PullRequest>> = vec![None; length];
    for &i in &selected_indexes {
        pull_requests[i] = prepared_commits[i].pull_request().await?;
    }

    // Check which selected commits have remote changes by comparing
    // cherry-picked trees. We cherry-pick the local commit onto the PR's
    // merge base (the master commit the PR was actually pushed from) so
    // the comparison is base-independent. Cache merge bases for reuse in
    // the reconstruction phase.
    let mut pr_merge_bases: Vec<Option<Oid>> = vec![None; length];
    let mut sync_indexes: Vec<usize> = Vec::new();
    for &i in &selected_indexes {
        if let Some(true) = check_remote_changes(
            git,
            &prepared_commits[i],
            &pull_requests[i],
            &mut pr_merge_bases[i],
        ) {
            sync_indexes.push(i);
        }
    }

    if sync_indexes.is_empty() {
        output("✅", "Already up to date")?;
        return Ok(());
    }

    // Print what will be synced
    for &i in &sync_indexes {
        write_commit_title(&prepared_commits[i])?;
        let pr = pull_requests[i].as_ref().unwrap();
        output("🔄", &format!("PR #{} has remote changes", pr.number))?;
    }

    if opts.dry_run {
        output("ℹ️ ", "Dry run - no changes made")?;
        return Ok(());
    }

    // Reconstruct the stack
    let mut sync_iter = sync_indexes.iter().peekable();
    let mut current_base = master_base_oid;
    let mut base_changed = false;

    for (i, commit) in prepared_commits.iter().enumerate() {
        if sync_iter.peek() == Some(&&i) {
            sync_iter.next();
            // Apply the remote PR's changes onto current_base using a
            // three-way merge. We use the PR's merge base as ancestor so
            // the diff is correct even if local master has moved. We can't
            // cherry-pick pr.head_oid because spr diff creates merge
            // commits on PR branches and cherrypick_commit fails on those.
            let pr = pull_requests[i].as_ref().unwrap();
            let index = {
                let repo = git.repo();
                let pr_merge_base = pr_merge_bases[i]
                    .expect("merge base cached by check_remote_changes");
                let ancestor_tree = repo.find_commit(pr_merge_base)?.tree()?;
                let our_tree = repo.find_commit(current_base)?.tree()?;
                let their_tree = repo.find_commit(pr.head_oid)?.tree()?;
                repo.merge_trees(&ancestor_tree, &our_tree, &their_tree, None)?
            };
            if index.has_conflicts() {
                return Err(Error::new(format!(
                    "Conflict while syncing PR #{} '{}'",
                    pr.number,
                    commit.title(),
                )));
            }
            let tree_oid = git.write_index(index)?;
            let message = build_commit_message(&commit.message);
            current_base = git.create_derived_commit(
                commit.oid,
                &message,
                tree_oid,
                &[current_base],
            )?;
            base_changed = true;
        } else if base_changed {
            // Cherry-pick this commit onto the new base
            let index = git.cherrypick(commit.oid, current_base)?;
            if index.has_conflicts() {
                return Err(Error::new(format!(
                    "Conflict while replaying commit '{}' after sync",
                    commit.title(),
                )));
            }
            let tree_oid = git.write_index(index)?;
            let raw_message = {
                let repo = git.repo();
                let c = repo.find_commit(commit.oid)?;
                String::from_utf8_lossy(c.message_bytes()).into_owned()
            };
            current_base = git.create_derived_commit(
                commit.oid,
                &raw_message,
                tree_oid,
                &[current_base],
            )?;
        } else {
            current_base = commit.oid;
        }
    }

    // Update HEAD and checkout
    {
        let repo = git.repo();
        let new_commit = repo.find_commit(current_base)?;

        let mut reference = repo.head()?.resolve()?;
        repo.checkout_tree(new_commit.as_object(), None)
            .map_err(Error::from)
            .reword(
                "Could not check out synced branch - please rebase manually"
                    .into(),
            )?;
        reference.set_target(current_base, "spr sync")?;
    }

    output("✅", "Sync complete")?;
    Ok(())
}

/// Check if a commit's PR has remote changes by comparing the local
/// cherry-pick tree against the remote PR head tree. Returns `Some(true)`
/// if changes exist, `Some(false)` if up to date, or `None` if the check
/// cannot be performed (no PR, closed PR, or git errors). Caches the PR
/// merge base OID for reuse in the reconstruction phase.
fn check_remote_changes(
    git: &Git,
    commit: &PreparedCommit,
    pr_opt: &Option<PullRequest>,
    merge_base_out: &mut Option<Oid>,
) -> Option<bool> {
    let pr = pr_opt.as_ref()?;
    if pr.state != PullRequestState::Open {
        return Some(false);
    }
    let pr_merge_base = git.repo().merge_base(pr.head_oid, pr.base_oid).ok()?;
    *merge_base_out = Some(pr_merge_base);
    let local_index = git.cherrypick(commit.oid, pr_merge_base).ok()?;
    if local_index.has_conflicts() {
        return None;
    }
    let local_cp_tree = git.write_index(local_index).ok()?;
    let remote_tree = git.get_tree_oid_for_commit(pr.head_oid).ok()?;
    Some(local_cp_tree != remote_tree)
}
