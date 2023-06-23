/*
 * Copyright (c) Radical HQ Limited
 *
 * This source code is licensed under the MIT license found in the
 * LICENSE file in the root directory of this source tree.
 */

use crate::{
    error::{add_error, Error, Result, ResultExt},
    git::{CommitOption, PreparedCommit},
    github::{
        GitHub, PullRequestRequestReviewers, PullRequestState,
        PullRequestUpdate,
    },
    message::{validate_commit_message, MessageSection},
    output::{output, write_commit_title},
    utils::{parse_name_list, remove_all_parens, run_command},
};
use git2::Oid;
use indoc::formatdoc;
use inquire::{MultiSelect, Select};

const MAIN_SPECIAL_COMMIT_INDEX: isize = -1;
const UNKNOWN_PR_SPECIAL_COMMIT_INDEX: isize = -2;

#[derive(Debug, clap::Parser)]
pub struct DiffOptions {
    /// Open an interactive selection to select all or some commits to
    /// create/update pull requests, not just the HEAD commit
    #[clap(long, short = 'a')]
    all: bool,

    /// Update the pull request title and description on GitHub from the local
    /// commit message
    #[clap(long)]
    update_message: bool,

    /// Submit any new Pull Request as a draft
    #[clap(long)]
    draft: bool,

    /// Message to be used for commits updating existing pull requests (e.g.
    /// 'rebase' or 'review comments')
    #[clap(long, short = 'm')]
    message: Option<String>,

    /// Submit this commit and do not cherry-pick it onto any GitHub branch.
    /// An intermediate branch for the parent commit will be created as the
    /// base branch for the PR. Note: Once a PR is created with this option,
    /// this is also needed every time a PR is updated, otherwise the
    /// base branch on GitHub will not be updated with the parent commit
    #[clap(long)]
    no_cherry_pick: bool,

    /// PR base branch name. Use this to cherry-pick a PR on top of another
    /// PR branch instead of on top of the master branch. This avoids
    /// creating an intermediate base branch for stacked PRs.
    /// Example: spr diff --base <branch-name>. A special HEAD can be
    /// used to indicate that a parent commit should be used as the base.
    /// For example: spr diff --base HEAD^1
    #[clap(long, short = 'b')]
    base: Option<String>,
}

pub async fn diff(
    opts: DiffOptions,
    git: &crate::git::Git,
    gh: &mut crate::github::GitHub,
    config: &crate::config::Config,
) -> Result<()> {
    // Abort right here if the local Git repository is not clean
    git.check_no_uncommitted_changes()?;

    let mut result = Ok(());

    // Look up the commits on the local branch
    let mut prepared_commits = git.get_prepared_commits(config, Some(gh))?;
    let length = prepared_commits.len();

    // The parent of the first commit in the list is the commit on master that
    // the local branch is based on
    let master_base_oid = if let Some(first_commit) = prepared_commits.get(0) {
        first_commit.parent_oid
    } else {
        output("👋", "Branch is empty - nothing to do. Good bye!")?;
        return result;
    };

    let mut message_on_prompt = "".to_string();

    let selected_indexes = if opts.all {
        let options = prepared_commits
            .iter()
            .enumerate()
            .map(|(i, commit)| {
                let title = commit
                    .message
                    .get(&MessageSection::Title)
                    .map(|t| &t[..])
                    .unwrap_or("(untitled)");
                CommitOption {
                    message: format!(
                        "PR #{} - {}",
                        commit
                            .pull_request_number
                            .map(|n| n.to_string())
                            .unwrap_or("?????".to_string()),
                        title
                    ),
                    index: i as isize,
                }
            })
            .rev()
            .collect::<Vec<CommitOption>>();

        let ans =
            MultiSelect::new("Select commits to create/update PR:", options)
                .prompt()?;

        ans.iter().map(|x| x.index as usize).rev().collect()
    } else {
        vec![length - 1]
    };

    // selected_indexes is sorted from lower commits to higher commits
    for index in selected_indexes {
        if result.is_err() {
            break;
        }

        // The further implementation of the diff command is in a separate function.
        // This makes it easier to run the code to update the local commit message
        // with all the changes that the implementation makes at the end, even if
        // the implementation encounters an error or exits early.
        result = diff_impl(
            &opts,
            &mut message_on_prompt,
            git,
            gh,
            config,
            &mut prepared_commits,
            master_base_oid,
            index,
        )
        .await;
    }

    // This updates the commit message in the local Git repository (if it was
    // changed by the implementation)
    add_error(
        &mut result,
        git.rewrite_commit_messages(prepared_commits.as_mut_slice(), None),
    );

    result
}

#[allow(clippy::too_many_arguments)]
async fn diff_impl(
    opts: &DiffOptions,
    message_on_prompt: &mut String,
    git: &crate::git::Git,
    gh: &mut crate::github::GitHub,
    config: &crate::config::Config,
    prepared_commits: &mut Vec<PreparedCommit>,
    master_base_oid: Oid,
    index: usize,
) -> Result<()> {
    write_commit_title(&prepared_commits.get_mut(index).unwrap())?;

    let pull_request = if let Some(task) =
        &mut prepared_commits.get_mut(index).unwrap().pull_request_task
    {
        Some(task.await??)
    } else {
        None
    };

    let base_ref = if let Some(base) = &opts.base {
        let diff = parse_parent_or_zero(base);
        if diff == 0 {
            config.new_github_branch(base)
        } else {
            let base_index = index as isize - diff;
            if base_index < 0 {
                config.master_ref.clone()
            } else if base_index >= index as isize {
                return Err(Error::new("Invalid base".to_string()));
            } else {
                get_github_branch_for_index(prepared_commits, base_index)
                    .await?
            }
        }
    } else if let Some(pull_request) = &pull_request {
        pull_request.base.clone()
    } else if index == 0 {
        config.master_ref.clone()
    } else {
        let mut options: Vec<CommitOption> = Vec::new();

        for i in (0..index).rev() {
            let commit = prepared_commits.get(i).unwrap();
            let title = commit
                .message
                .get(&MessageSection::Title)
                .map(|t| &t[..])
                .unwrap_or("(untitled)");
            options.push(
                if let Some(pull_request_number) = commit.pull_request_number {
                    CommitOption {
                        message: format!(
                            "PR #{} - {}",
                            pull_request_number, title
                        ),
                        index: i as isize,
                    }
                } else {
                    CommitOption {
                        message: format!("PR #{} - {}", "?????", title),
                        index: UNKNOWN_PR_SPECIAL_COMMIT_INDEX,
                    }
                },
            );
        }
        options.push(CommitOption {
            message: config.master_ref.branch_name().to_string(),
            index: MAIN_SPECIAL_COMMIT_INDEX,
        });

        let ans = Select::new("Select a base:", options)
            .with_starting_cursor(index)
            .prompt();

        match ans {
            Ok(choice) => match choice.index {
                MAIN_SPECIAL_COMMIT_INDEX => config.master_ref.clone(),
                UNKNOWN_PR_SPECIAL_COMMIT_INDEX => {
                    return Err(Error::new(
                        "Your selection obviously has no PR created yet"
                            .to_string(),
                    ));
                }
                choice_index => {
                    get_github_branch_for_index(prepared_commits, choice_index)
                        .await?
                }
            },
            Err(_) => {
                return Err(Error::new(
                    "Aborted as per user request".to_string(),
                ));
            }
        }
    };

    let local_commit = prepared_commits.get_mut(index).unwrap();

    // Update master_base_oid if base if provided
    let master_base_oid = git
        .resolve_reference(base_ref.local())
        .unwrap_or(master_base_oid);

    // Parsed commit message of the local commit
    let message = &mut local_commit.message;

    // Determine the trees the Pull Request branch and the base branch should
    // have when we're done here.
    let (new_head_tree, new_base_tree) = if opts.no_cherry_pick {
        // If the user tells us not to cherry-pick, these should be the trees
        // of the current commit and its parent.
        let head_tree = git.get_tree_oid_for_commit(local_commit.oid)?;
        let base_tree = git.get_tree_oid_for_commit(local_commit.parent_oid)?;

        (head_tree, base_tree)
    } else {
        // Cherry-pick the current commit onto master
        let index = git.cherrypick(local_commit.oid, master_base_oid)?;

        if index.has_conflicts() {
            return Err(Error::new(formatdoc!(
                "This commit cannot be cherry-picked on {master}.",
                master = base_ref.branch_name(),
            )));
        }

        // This is the tree we are getting from cherrypicking the local commit
        // on master.
        let cherry_pick_tree = git.write_index(index)?;
        let master_tree = git.get_tree_oid_for_commit(master_base_oid)?;

        (cherry_pick_tree, master_tree)
    };

    if let Some(number) = local_commit.pull_request_number {
        output(
            "#️⃣ ",
            &format!(
                "Pull Request #{}: {}",
                number,
                config.pull_request_url(number)
            ),
        )?;
    }

    if local_commit.pull_request_number.is_none() || opts.update_message {
        validate_commit_message(message, config)?;
    }

    if let Some(ref pull_request) = pull_request {
        if pull_request.state == PullRequestState::Closed {
            return Err(Error::new(formatdoc!(
                "Pull request is closed. If you want to open a new one, \
                 remove the 'Pull Request' section from the commit message."
            )));
        }

        if !opts.update_message {
            let mut pull_request_updates: PullRequestUpdate =
                Default::default();
            pull_request_updates.update_message(pull_request, message);
        }
    }

    // Parse "Reviewers" section, if this is a new Pull Request
    let mut requested_reviewers = PullRequestRequestReviewers::default();

    if local_commit.pull_request_number.is_none() {
        if let Some(reviewers) = message.get(&MessageSection::Reviewers) {
            let reviewers = parse_name_list(reviewers);
            let mut checked_reviewers = Vec::new();

            for reviewer in reviewers {
                // Teams are indicated with a leading #
                if let Some(slug) = reviewer.strip_prefix('#') {
                    if let Ok(team) = GitHub::get_github_team(
                        (&config.owner).into(),
                        slug.into(),
                    )
                    .await
                    {
                        requested_reviewers
                            .team_reviewers
                            .push(team.slug.to_string());

                        checked_reviewers.push(reviewer);
                    } else {
                        return Err(Error::new(format!(
                            "Reviewers field contains unknown team '{}'",
                            reviewer
                        )));
                    }
                } else if let Ok(user) =
                    GitHub::get_github_user(reviewer.clone()).await
                {
                    requested_reviewers.reviewers.push(user.login);
                    if let Some(name) = user.name {
                        checked_reviewers.push(format!(
                            "{} ({})",
                            reviewer.clone(),
                            remove_all_parens(&name)
                        ));
                    } else {
                        checked_reviewers.push(reviewer);
                    }
                } else {
                    return Err(Error::new(format!(
                        "Reviewers field contains unknown user '{}'",
                        reviewer
                    )));
                }
            }

            message.insert(
                MessageSection::Reviewers,
                checked_reviewers.join(", "),
            );
        }
    }

    // Get the name of the existing Pull Request branch, or constuct one if
    // there is none yet.

    let title = message
        .get(&MessageSection::Title)
        .map(|t| &t[..])
        .unwrap_or("");

    let pull_request_branch = match &pull_request {
        Some(pr) => pr.head.clone(),
        None => config.new_github_branch(
            &config.get_new_branch_name(&git.get_all_ref_names()?, title),
        ),
    };

    // Get the tree ids of the current head of the Pull Request, as well as the
    // base, and the commit id of the master commit this PR is currently based
    // on.
    // If there is no pre-existing Pull Request, we fill in the equivalent
    // values.
    let (pr_head_oid, pr_head_tree, pr_base_oid, pr_base_tree, pr_master_base) =
        if let Some(pr) = &pull_request {
            let pr_head_tree = git.get_tree_oid_for_commit(pr.head_oid)?;

            let current_master_oid = git.resolve_reference(base_ref.local())?;
            let pr_base_oid =
                git.repo().merge_base(pr.head_oid, pr.base_oid)?;
            let pr_base_tree = git.get_tree_oid_for_commit(pr_base_oid)?;

            let pr_master_base =
                git.repo().merge_base(pr.head_oid, current_master_oid)?;

            (
                pr.head_oid,
                pr_head_tree,
                pr_base_oid,
                pr_base_tree,
                pr_master_base,
            )
        } else {
            let master_base_tree =
                git.get_tree_oid_for_commit(master_base_oid)?;
            (
                master_base_oid,
                master_base_tree,
                master_base_oid,
                master_base_tree,
                master_base_oid,
            )
        };
    let needs_merging_master = pr_master_base != master_base_oid;

    // At this point we can check if we can exit early because no update to the
    // existing Pull Request is necessary
    if let Some(ref pull_request) = pull_request {
        // So there is an existing Pull Request...
        if !needs_merging_master
            && pr_head_tree == new_head_tree
            && pr_base_tree == new_base_tree
        {
            // ...and it does not need a rebase, and the trees of both Pull
            // Request branch and base are all the right ones.
            output("✅", "No update necessary")?;

            if opts.update_message {
                // However, the user requested to update the commit message on
                // GitHub

                let mut pull_request_updates: PullRequestUpdate =
                    Default::default();
                pull_request_updates.update_message(pull_request, message);

                if !pull_request_updates.is_empty() {
                    // ...and there are actual changes to the message
                    gh.update_pull_request(
                        pull_request.number,
                        pull_request_updates,
                    )
                    .await?;
                    output("✍", "Updated commit message on GitHub")?;
                }
            }

            return Ok(());
        }
    }

    // Check if there is a intermediate base branch on GitHub already. That's the case when
    // there is an existing Pull Request, and its base is not the master branch or other PR's branch.
    let base_branch = if let Some(ref pr) = pull_request {
        if pr.base.is_master_branch()
            || opts.base.is_some()
            || !opts.no_cherry_pick
        {
            None
        } else {
            Some(pr.base.clone())
        }
    } else {
        None
    };

    // We are going to construct `pr_base_parent: Option<Oid>`.
    // The value will be the commit we have to merge into the new Pull Request
    // commit to reflect changes in the parent of the local commit (by rebasing
    // or changing commits between master and this one, although technically
    // that's also rebasing).
    // If it's `None`, then we will not merge anything into the new Pull Request
    // commit.
    // If we are updating an existing PR, then there are three cases here:
    // (1) the parent tree of this commit is unchanged and we do not need to
    //     merge in master, which means that the local commit was amended, but
    //     not rebased. We don't need to merge anything into the Pull Request
    //     branch.
    // (2) the parent tree has changed, but the parent of the local commit is on
    //     master (or we are cherry-picking) and we are not already using a base
    //     branch: in this case we can merge the master commit we are based on
    //     into the PR branch, without going via a base branch. Thus, we don't
    //     introduce a base branch here and the PR continues to target the
    //     master branch.
    // (3) the parent tree has changed, and we need to use a base branch (either
    //     because one was already created earlier, or we find that we are not
    //     directly based on master now): we need to construct a new commit for
    //     the base branch. That new commit's tree is always that of that local
    //     commit's parent (thus making sure that the difference between base
    //     branch and pull request branch are exactly the changes made by the
    //     local commit, thus the changes we want to have reviewed). The new
    //     commit may have one or two parents. The previous base is always a
    //     parent (that's either the current commit on an existing base branch,
    //     or the previous master commit the PR was based on if there isn't a
    //     base branch already). In addition, if the master commit this commit
    //     is based on has changed, (i.e. the local commit got rebased on newer
    //     master in the meantime) then we have to merge in that master commit,
    //     which will be the second parent.
    // If we are creating a new pull request then `pr_base_tree` (the current
    // base of the PR) was set above to be the tree of the master commit the
    // local commit is based one, whereas `new_base_tree` is the tree of the
    // parent of the local commit. So if the local commit for this new PR is on
    // master, those two are the same (and we want to apply case 1). If the
    // commit is not directly based on master, we have to create this new PR
    // with a base branch, so that is case 3.

    let (pr_base_parent, base_branch) = if pr_base_tree == new_base_tree
        && !needs_merging_master
    {
        // Case 1
        (None, base_branch)
    } else if base_branch.is_none() && !opts.no_cherry_pick {
        // Case 2
        (Some(master_base_oid), None)
    } else {
        // Case 3

        // We are constructing a base branch commit.
        // One parent of the new base branch commit will be the current base
        // commit, that could be either the top commit of an existing base
        // branch, or a commit on master.
        let mut parents = vec![pr_base_oid];

        // If we need to rebase on master, make the master commit also a
        // parent (except if the first parent is that same commit, we don't
        // want duplicates in `parents`).
        if needs_merging_master && pr_base_oid != master_base_oid {
            parents.push(master_base_oid);
        }

        let new_base_branch_commit = git.create_derived_commit(
            local_commit.parent_oid,
            &format!(
                "[𝘀𝗽𝗿] {}\n\nCreated using spr {}\n\n[skip ci]",
                if pull_request.is_some() {
                    "changes introduced through rebase".to_string()
                } else {
                    format!(
                        "changes to {} this commit is based on",
                        config.master_ref.branch_name()
                    )
                },
                env!("CARGO_PKG_VERSION"),
            ),
            new_base_tree,
            &parents[..],
        )?;

        // If `base_branch` is `None` (which means a base branch does not exist
        // yet), then make a `GitHubBranch` with a new name for a base branch
        let base_branch = if let Some(base_branch) = base_branch {
            base_branch
        } else {
            config.new_github_branch(
                &config.get_base_branch_name(&git.get_all_ref_names()?, title),
            )
        };

        (Some(new_base_branch_commit), Some(base_branch))
    };

    let mut github_commit_message = opts.message.clone();
    if pull_request.is_some() && github_commit_message.is_none() {
        let input = {
            let message_on_prompt = message_on_prompt.clone();

            tokio::task::spawn_blocking(move || {
                dialoguer::Input::<String>::new()
                    .with_prompt("Message (leave empty to abort)")
                    .with_initial_text(message_on_prompt)
                    .allow_empty(true)
                    .interact_text()
            })
            .await??
        };

        if input.is_empty() {
            return Err(Error::new("Aborted as per user request".to_string()));
        }

        *message_on_prompt = input.clone();
        github_commit_message = Some(input);
    }

    // Construct the new commit for the Pull Request branch. First parent is the
    // current head commit of the Pull Request (we set this to the master base
    // commit earlier if the Pull Request does not yet exist)
    let mut pr_commit_parents = vec![pr_head_oid];

    // If we prepared a commit earlier that needs merging into the Pull Request
    // branch, then that commit is a parent of the new Pull Request commit.
    if let Some(oid) = pr_base_parent {
        // ...unless if that's the same commit as the one we added to
        // pr_commit_parents first.
        if pr_commit_parents.get(0) != Some(&oid) {
            pr_commit_parents.push(oid);
        }
    }

    // Create the new commit
    let pr_commit = git.create_derived_commit(
        local_commit.oid,
        &format!(
            "{}\n\nCreated using spr {}",
            github_commit_message
                .as_ref()
                .map(|s| &s[..])
                .unwrap_or("[𝘀𝗽𝗿] initial version"),
            env!("CARGO_PKG_VERSION"),
        ),
        new_head_tree,
        &pr_commit_parents[..],
    )?;

    let mut cmd = tokio::process::Command::new("git");
    cmd.arg("push")
        .arg("--atomic")
        .arg("--no-verify")
        .arg("--")
        .arg(&config.remote_name)
        .arg(format!("{}:{}", pr_commit, pull_request_branch.on_github()));

    if let Some(pull_request) = pull_request {
        // We are updating an existing Pull Request

        if needs_merging_master {
            output(
                "⚾",
                &format!(
                    "Commit was rebased - updating Pull Request #{}",
                    pull_request.number
                ),
            )?;
        } else {
            output(
                "🔁",
                &format!(
                    "Commit was changed - updating Pull Request #{}",
                    pull_request.number
                ),
            )?;
        }

        // Things we want to update in the Pull Request on GitHub
        let mut pull_request_updates: PullRequestUpdate = Default::default();

        if opts.update_message {
            pull_request_updates.update_message(&pull_request, message);
        }

        if let Some(base_branch) = base_branch {
            // We are using a base branch.

            if let Some(base_branch_commit) = pr_base_parent {
                // ...and we prepared a new commit for it, so we need to push an
                // update of the base branch.
                cmd.arg(format!(
                    "{}:{}",
                    base_branch_commit,
                    base_branch.on_github()
                ));
            }

            // Push the new commit onto the Pull Request branch (and also the
            // new base commit, if we added that to cmd above).
            run_command(&mut cmd)
                .await
                .reword("git push failed".to_string())?;

            // If the Pull Request's base is not set to the base branch yet,
            // change that now.
            if pull_request.base.branch_name() != base_branch.branch_name() {
                pull_request_updates.base =
                    Some(base_branch.branch_name().to_string());
            }
        } else {
            if let Some(base) = &opts.base {
                if pull_request.base.branch_name() != base {
                    pull_request_updates.base = Some(base.clone());
                }
            }

            // The Pull Request is against the master branch. In that case we
            // only need to push the update to the Pull Request branch.
            run_command(&mut cmd)
                .await
                .reword("git push failed".to_string())?;
        }

        if !pull_request_updates.is_empty() {
            gh.update_pull_request(pull_request.number, pull_request_updates)
                .await?;
        }
    } else {
        // We are creating a new Pull Request.

        // If there's a base branch, add it to the push
        if let (Some(base_branch), Some(base_branch_commit)) =
            (&base_branch, pr_base_parent)
        {
            cmd.arg(format!(
                "{}:{}",
                base_branch_commit,
                base_branch.on_github()
            ));
        }
        // Push the pull request branch and the base branch if present
        run_command(&mut cmd)
            .await
            .reword("git push failed".to_string())?;

        // Then call GitHub to create the Pull Request.
        let pull_request_number = gh
            .create_pull_request(
                message,
                base_branch
                    .as_ref()
                    .unwrap_or(&base_ref)
                    .branch_name()
                    .to_string(),
                pull_request_branch.branch_name().to_string(),
                opts.draft,
            )
            .await?;

        let pull_request_url = config.pull_request_url(pull_request_number);

        output(
            "✨",
            &format!(
                "Created new Pull Request #{}: {}",
                pull_request_number, &pull_request_url,
            ),
        )?;

        message.insert(MessageSection::PullRequest, pull_request_url);

        let result = gh
            .request_reviewers(pull_request_number, requested_reviewers)
            .await;
        match result {
            Ok(()) => (),
            Err(error) => {
                output("⚠️", "Requesting reviewers failed")?;
                for message in error.messages() {
                    output("  ", message)?;
                }
            }
        }
    }

    Ok(())
}

async fn get_github_branch_for_index(
    prepared_commits: &mut Vec<PreparedCommit>,
    choice_index: isize,
) -> Result<crate::github::GitHubBranch> {
    let pull_request = if let Some(task) = &mut prepared_commits
        .get_mut(choice_index as usize)
        .unwrap()
        .pull_request_task
    {
        Some(task.await??)
    } else {
        None
    };
    Ok(match pull_request {
        Some(pull_request) => pull_request.head,
        None => {
            return Err(Error::new(
                "Could not find a PR for your selection".to_string(),
            ));
        }
    })
}

fn parse_parent_or_zero(s: &str) -> isize {
    if s == "HEAD^" || s == "HEAD^" {
        1
    } else if s.starts_with("HEAD^") || s.starts_with("HEAD^") {
        if let Ok(n) = s[5..].parse::<isize>() {
            n
        } else {
            0
        }
    } else {
        0
    }
}
