/*
 * Copyright (c) Radical HQ Limited
 *
 * This source code is licensed under the MIT license found in the
 * LICENSE file in the root directory of this source tree.
 */

use crate::{
    config::Config,
    error::{Error, Result},
    git::{CommitOption, Git, PreparedCommit, PullRequestFilter},
};
use inquire::MultiSelect;

/// Which of the commits on the local branch a command operates on. Commands
/// that work on more than the HEAD commit flatten this into their options.
#[derive(Debug, clap::Args)]
pub struct CommitSelection {
    /// Open an interactive selection to select all or some commits, not just
    /// the HEAD commit
    #[clap(long, short = 'a')]
    all: bool,

    /// Commit to operate on instead of the HEAD commit. This can be a commit
    /// id, a short commit id, or a name such as HEAD~2.
    #[clap(conflicts_with = "all")]
    commit: Option<String>,
}

impl CommitSelection {
    /// Which Pull Requests this run needs from GitHub. spr asks this before it
    /// prepares the commits, because a run that names one commit must not pay
    /// for a query and a `git fetch` for all the others.
    pub fn pull_request_filter(&self, git: &Git) -> Result<PullRequestFilter> {
        if let Some(commit) = &self.commit {
            return Ok(PullRequestFilter::Commit(resolve_named_commit(
                git, commit,
            )?));
        }

        if self.all {
            return Ok(PullRequestFilter::All);
        }

        Ok(PullRequestFilter::Head)
    }

    /// Work out which commits the command operates on. The result is sorted
    /// from the bottom commit to the top one.
    ///
    /// With neither a commit nor `--all` this is the HEAD commit. `--all`
    /// opens a picker. A commit names one commit directly.
    pub fn resolve(
        &self,
        git: &Git,
        config: &Config,
        prepared_commits: &[PreparedCommit],
        prompt: &str,
    ) -> Result<Vec<usize>> {
        if let Some(commit) = &self.commit {
            return Ok(vec![find_commit(
                git,
                config,
                prepared_commits,
                commit,
            )?]);
        }

        if !self.all {
            // The HEAD commit, or nothing at all if the branch is empty.
            return Ok(prepared_commits
                .len()
                .checked_sub(1)
                .into_iter()
                .collect());
        }

        let options = prepared_commits
            .iter()
            .enumerate()
            .map(|(index, commit)| CommitOption {
                message: format!(
                    "PR #{} - {}",
                    commit
                        .pull_request_number
                        .map(|number| number.to_string())
                        .unwrap_or_else(|| "??????".to_string()),
                    commit.title(),
                ),
                index: index as isize,
            })
            .rev()
            .collect::<Vec<CommitOption>>();

        let answer = MultiSelect::new(prompt, options).prompt()?;

        Ok(answer
            .iter()
            .map(|option| option.index as usize)
            .rev()
            .collect())
    }
}

/// Find the commit that the user named among the commits of the local branch.
fn find_commit(
    git: &Git,
    config: &Config,
    prepared_commits: &[PreparedCommit],
    commit: &str,
) -> Result<usize> {
    let oid = resolve_named_commit(git, commit)?;

    prepared_commits
        .iter()
        .position(|prepared_commit| prepared_commit.oid == oid)
        .ok_or_else(|| {
            Error::new(format!(
                "Commit '{}' is not on this branch. This command works on the \
                 commits between {} and HEAD.",
                commit,
                config.master_ref.branch_name()
            ))
        })
}

/// Resolve the revision that the user typed on the command line.
fn resolve_named_commit(git: &Git, commit: &str) -> Result<git2::Oid> {
    if commit.contains("..") {
        return Err(Error::new(format!(
            "'{commit}' names a range of commits. This command takes one \
             commit. Use --all to select several commits."
        )));
    }

    git.resolve_commit(commit)
}
