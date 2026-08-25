/*
 * Copyright (c) Radical HQ Limited
 *
 * This source code is licensed under the MIT license found in the
 * LICENSE file in the root directory of this source tree.
 */

use crate::{
    error::{add_error, Error, Result, ResultExt},
    git::{CommitOption, Git, PreparedCommit},
    github::{
        GitHub, PullRequestRequestReviewers, PullRequestState,
        PullRequestUpdate,
    },
    message::{validate_commit_message, MessageSection},
    output::{output, write_commit_title},
    utils::{parse_name_list, remove_all_parens, run_command_with_live_output},
};
use git2::Oid;
use indoc::formatdoc;
use inquire::Select;
/// A Pull Request that this run of `spr diff` created or updated, together
/// with the Pull Request its branch is based on. `base_pull_request_number` is
/// `None` when the Pull Request is based on the master branch, which means it
/// is the bottom of a stack (or not part of one at all).
///
/// `head_branch_name` lets a later commit in the same run recognise this Pull
/// Request as its base without asking GitHub.
#[derive(Debug, Clone)]
struct StackLink {
    pull_request_number: u64,
    head_branch_name: String,
    base_pull_request_number: Option<u64>,
}

const MAIN_SPECIAL_COMMIT_INDEX: isize = -1;
const UNKNOWN_PR_SPECIAL_COMMIT_INDEX: isize = -2;

#[derive(Debug, clap::Parser)]
pub struct DiffOptions {
    #[clap(flatten)]
    selection: crate::commands::select::CommitSelection,

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

    /// Add --no-verify for git push to GitHub. This is useful when you
    /// have a pre-push hook that you want to skip.
    /// For example: spr diff --no-verify
    #[clap(long, short = 'n')]
    no_verify: bool,
}

pub async fn diff(
    opts: DiffOptions,
    git: &crate::git::Git,
    gh: &mut crate::github::GitHub,
    config: &crate::config::Config,
) -> Result<()> {
    // Note that spr does not need a clean worktree here. It only reads commits
    // and writes new ones, and the commits it writes keep the tree of the
    // commit they replace, so the worktree stays valid. `spr sync` is
    // different: it checks out a new tree and does need a clean worktree.

    let mut result = Ok(());

    // Pre-fetch origin/main once so that concurrent PR fetch tasks
    // (spawned below) don't race on updating this shared ref.
    Git::fetch_from_remote(&[&config.master_ref], &config.remote_name).await?;

    // Look up the commits on the local branch. Only fetch the Pull Requests
    // that this run can look at: each one costs a query and a `git fetch`.
    let pull_request_filter = opts.selection.pull_request_filter(git)?;
    let mut prepared_commits =
        git.get_prepared_commits(config, Some(gh), pull_request_filter)?;

    // The parent of the first commit in the list is the commit on master that
    // the local branch is based on
    let master_base_oid = if let Some(first_commit) = prepared_commits.first() {
        first_commit.parent_oid
    } else {
        output("👋", "Branch is empty - nothing to do. Good bye!")?;
        return result;
    };

    let mut message_on_prompt = "".to_string();

    let selected_indexes = opts.selection.resolve(
        git,
        config,
        &prepared_commits,
        "Select commits to create/update PR:",
    )?;

    // Pull Requests created or updated below, ordered from the bottom commit
    // to the top one, so that we can tell GitHub about the stack they form.
    let mut stack_links: Vec<StackLink> = Vec::new();

    // selected_indexes is sorted from lower commits to higher commits
    for &index in &selected_indexes {
        if result.is_err() {
            break;
        }

        // The further implementation of the diff command is in a separate function.
        // This makes it easier to run the code to update the local commit message
        // with all the changes that the implementation makes at the end, even if
        // the implementation encounters an error or exits early.
        match diff_impl(
            &opts,
            &mut message_on_prompt,
            git,
            gh,
            config,
            &mut prepared_commits,
            master_base_oid,
            index,
            &selected_indexes,
            &stack_links,
        )
        .await
        {
            Ok(stack_link) => stack_links.extend(stack_link),
            Err(error) => result = Err(error),
        }
    }

    // This updates the commit message in the local Git repository (if it was
    // changed by the implementation)
    add_error(
        &mut result,
        git.rewrite_commit_messages(prepared_commits.as_mut_slice(), None),
    );

    // Now that all Pull Requests exist and have the right base branches, tell
    // GitHub about the stack they form.
    if result.is_ok() {
        update_github_stack(gh, &stack_links).await;
    }

    result
}

#[allow(clippy::too_many_arguments)]
async fn diff_impl(
    opts: &DiffOptions,
    message_on_prompt: &mut String,
    git: &crate::git::Git,
    gh: &mut crate::github::GitHub,
    config: &crate::config::Config,
    prepared_commits: &mut [PreparedCommit],
    master_base_oid: Oid,
    index: usize,
    selected_indexes: &[usize],
    stack_links: &[StackLink],
) -> Result<Option<StackLink>> {
    write_commit_title(prepared_commits.get_mut(index).unwrap())?;

    let pull_request = if let Some(task) =
        &mut prepared_commits.get_mut(index).unwrap().pull_request_task
    {
        Some(task.await??)
    } else {
        None
    };

    let (base_ref, base_pull_request_number) = if let Some(base) = &opts.base {
        let diff = parse_parent_or_zero(base);
        if diff == 0 {
            let base_pull_request_number_result =
                gh.get_open_pull_request_number_for_head(base.clone()).await;
            (
                config.new_github_branch(base),
                base_pull_request_number_result.ok(),
            )
        } else {
            let base_index = index as isize - diff;
            if base_index < 0 {
                (config.master_ref.clone(), None)
            } else if base_index >= index as isize {
                return Err(Error::new("Invalid base".to_string()));
            } else {
                let pull_request = get_pull_request_for_index(
                    gh,
                    prepared_commits,
                    base_index,
                )
                .await?;
                (pull_request.head, Some(pull_request.number))
            }
        }
    } else if let Some(pull_request) = &pull_request {
        // The Pull Request exists already, so GitHub knows its base branch and
        // we do not have to ask the user again. If that base is the branch of
        // another Pull Request, then this commit is part of a stack and we
        // need that Pull Request's number.
        let base_pull_request_number =
            base_pull_request_number(gh, &pull_request.base, stack_links).await;

        (pull_request.base.clone(), base_pull_request_number)
    } else if index == 0 {
        (config.master_ref.clone(), None)
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
                MAIN_SPECIAL_COMMIT_INDEX => (config.master_ref.clone(), None),
                UNKNOWN_PR_SPECIAL_COMMIT_INDEX => {
                    return Err(Error::new(
                        "Your selection obviously has no PR created yet"
                            .to_string(),
                    ));
                }
                choice_index => {
                    let pull_request = get_pull_request_for_index(
                        gh,
                        prepared_commits,
                        choice_index,
                    )
                    .await?;
                    (pull_request.head, Some(pull_request.number))
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

    // If the user has provided a base PR, add a "Depends On" line in PR body
    if let Some(base_pull_request_number) = base_pull_request_number {
        message.insert(
            MessageSection::BasePR,
            format!(
                "\n- #{} (powered by [spr-enhanced](https://go/spr-enhanced))",
                base_pull_request_number
            ),
        );
    }

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

            return Ok(Some(StackLink {
                pull_request_number: pull_request.number,
                head_branch_name: pull_request.head.branch_name().to_string(),
                base_pull_request_number,
            }));
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
        if pr_commit_parents.first() != Some(&oid) {
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

    // The Pull Request is part of a native GitHub stack only if its base is
    // the branch of the Pull Request below it. That is not the case when we
    // put an intermediate base branch in between, which happens with
    // --no-cherry-pick.
    let stack_base_pull_request_number =
        base_pull_request_number.filter(|_| base_branch.is_none());

    let mut cmd = tokio::process::Command::new("git");
    cmd.arg("push").arg("--atomic");

    if opts.no_verify {
        cmd.arg("--no-verify");
    }

    cmd.arg("--").arg(&config.remote_name).arg(format!(
        "{}:{}",
        pr_commit,
        pull_request_branch.on_github()
    ));

    // Set on both paths below, and returned at the end.
    let stack_link;

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
            run_command_with_live_output(&mut cmd)
                .await
                .reword("git push failed".to_string())?;

            // If the Pull Request's base is not set to the base branch yet,
            // change that now.
            if pull_request.base.branch_name() != base_branch.branch_name() {
                pull_request_updates.base =
                    Some(base_branch.branch_name().to_string());
            }
        } else {
            if opts.base.is_some()
                && pull_request.base.branch_name() != base_ref.branch_name()
            {
                pull_request_updates.base =
                    Some(base_ref.branch_name().to_string());
            }

            // The Pull Request is against the master branch. In that case we
            // only need to push the update to the Pull Request branch.
            run_command_with_live_output(&mut cmd)
                .await
                .reword("git push failed".to_string())?;
        }

        if !pull_request_updates.is_empty() {
            gh.update_pull_request(pull_request.number, pull_request_updates)
                .await?;
        }

        stack_link = Some(StackLink {
            pull_request_number: pull_request.number,
            head_branch_name: pull_request_branch.branch_name().to_string(),
            base_pull_request_number: stack_base_pull_request_number,
        });
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
        run_command_with_live_output(&mut cmd)
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
                pull_request_number, pull_request_url,
            ),
        )?;

        message.insert(MessageSection::PullRequest, pull_request_url);

        stack_link = Some(StackLink {
            pull_request_number,
            head_branch_name: pull_request_branch.branch_name().to_string(),
            base_pull_request_number: stack_base_pull_request_number,
        });

        // If current commit is not the last selected commit, update pull request number and task
        // so that it can be used as a base PR for the subsequent commits.
        if Some(&index) != selected_indexes.last() {
            local_commit.pull_request_number = Some(pull_request_number);
            local_commit.pull_request_task = Some(tokio::spawn(
                gh.clone().get_pull_request(pull_request_number),
            ));
        }

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

    Ok(stack_link)
}

async fn get_pull_request_for_index(
    gh: &crate::github::GitHub,
    prepared_commits: &mut [PreparedCommit],
    choice_index: isize,
) -> Result<crate::github::PullRequest> {
    let missing = || Error::new("Could not find a PR for the base".to_string());

    let commit = prepared_commits
        .get_mut(choice_index as usize)
        .ok_or_else(missing)?;

    // spr fetches a Pull Request in the background only for the commits that
    // the run knew about in advance. This commit is a base that the user chose
    // later, so fetch it now if that did not happen.
    if let Some(task) = &mut commit.pull_request_task {
        return task.await?;
    }

    match commit.pull_request_number {
        Some(number) => gh.clone().get_pull_request(number).await,
        None => Err(missing()),
    }
}

fn parse_parent_or_zero(s: &str) -> isize {
    if s == "HEAD~" || s == "HEAD^" {
        1
    } else if s.starts_with("HEAD^") || s.starts_with("HEAD^") {
        s[5..].parse::<isize>().unwrap_or_default()
    } else {
        0
    }
}

/// Find the number of the Pull Request whose branch is `base`, which is the
/// Pull Request below this one in a stack. Returns `None` when `base` is the
/// master branch, or an intermediate base branch, or any other branch that
/// does not belong to an open Pull Request.
///
/// `known` holds the Pull Requests that this run has handled already, so that
/// a stack submitted in one go needs no call to GitHub at all.
async fn base_pull_request_number(
    gh: &crate::github::GitHub,
    base: &crate::github::GitHubBranch,
    known: &[StackLink],
) -> Option<u64> {
    if base.is_master_branch() {
        return None;
    }

    if let Some(link) = known
        .iter()
        .find(|link| link.head_branch_name == base.branch_name())
    {
        return Some(link.pull_request_number);
    }

    gh.get_open_pull_request_number_for_head(base.branch_name().to_string())
        .await
        .ok()
}

const STACKS_NOT_ENABLED: &str =
    "This repository does not have stacked pull requests enabled";

/// GitHub answers "not found" on the stack endpoints when the repository does
/// not have stacked pull requests. We can only recognise this from the text of
/// the message, because our error type does not keep the status code.
fn is_not_found(error: &Error) -> bool {
    error
        .messages()
        .iter()
        .any(|message| message.to_lowercase().contains("not found"))
}

/// Tell GitHub about the stack that the Pull Requests of this run form, so
/// that GitHub shows them as a native stack.
///
/// This is best effort. The Pull Requests themselves are already created and
/// up to date at this point, so a problem here must never fail the command. We
/// report it and carry on.
async fn update_github_stack(
    gh: &crate::github::GitHub,
    stack_links: &[StackLink],
) {
    if let Err(error) = update_github_stack_impl(gh, stack_links).await {
        let _ = output("⚠️", "Could not update the stack on GitHub");
        for message in error.messages() {
            let _ = output("  ", message);
        }
    }
}

async fn update_github_stack_impl(
    gh: &crate::github::GitHub,
    stack_links: &[StackLink],
) -> Result<()> {
    let chain = build_stack_chain(gh, stack_links).await?;

    // GitHub needs at least two Pull Requests to form a stack. Anything
    // shorter means these commits go straight onto the master branch, which is
    // not a stack.
    if chain.len() < 2 {
        return Ok(());
    }

    let stack = match gh.find_stack_for_pull_request(chain[0]).await {
        Ok(stack) => stack,
        // There is no point in trying to create a stack in a repository that
        // does not have them.
        Err(error) if is_not_found(&error) => {
            return output("⚠️", STACKS_NOT_ENABLED);
        }
        // Any other problem: carry on to the create path, which reports its
        // own errors.
        Err(_) => None,
    };

    let Some(stack) = stack else {
        return create_github_stack(gh, &chain).await;
    };

    let current = stack.pull_request_numbers();

    if current == chain {
        return Ok(());
    }

    // GitHub's add endpoint can only append to the top of a stack. So we can
    // only add if what we have is what GitHub has, plus some new Pull Requests
    // on top.
    if chain.len() > current.len() && chain[..current.len()] == current[..] {
        gh.add_to_stack(stack.number, &chain[current.len()..])
            .await?;

        return output(
            "🥞",
            &format!(
                "Added {} pull request(s) to stack #{} on GitHub",
                chain.len() - current.len(),
                stack.number
            ),
        );
    }

    // The order changed, and GitHub has no endpoint for that. So we remove the
    // stack and make a new one. We only do this when the stack holds exactly
    // the Pull Requests that we have, so that we never drop a Pull Request
    // that somebody added to the stack somewhere else. Pull Request numbers
    // are unique, so equal length plus containment means equal sets.
    if chain.len() == current.len()
        && chain.iter().all(|number| current.contains(number))
    {
        output(
            "🥞",
            &format!(
                "The order of stack #{} changed - building it again on GitHub",
                stack.number
            ),
        )?;

        gh.unstack(stack.number).await?;

        return create_github_stack(gh, &chain).await;
    }

    output(
        "⚠️",
        &format!(
            "Stack #{} on GitHub does not match your commits and was not \
             updated. Your pull requests are up to date.",
            stack.number
        ),
    )
}

async fn create_github_stack(
    gh: &crate::github::GitHub,
    chain: &[u64],
) -> Result<()> {
    let error = match gh.create_stack(chain).await {
        Ok(stack) => {
            return output(
                "🥞",
                &format!(
                    "Created stack #{} on GitHub with {} pull requests",
                    stack.number,
                    chain.len()
                ),
            )
        }
        Err(error) => error,
    };

    // GitHub rejects a stack for a handful of reasons that are not faults of
    // ours. Recognise those and explain them, rather than show the raw error.
    let message = error.messages().join(" ");
    let lowercase_message = message.to_lowercase();

    let explanation = if lowercase_message.contains("already stacked")
        || lowercase_message.contains("already part of a stack")
    {
        // GitHub names the Pull Requests it rejected. If that list covers all
        // of ours, they are already stacked together and there is nothing to
        // do.
        if chain
            .iter()
            .all(|number| message.contains(&format!("#{}", number)))
        {
            return Ok(());
        }

        "Some of your pull requests are already part of a different stack on \
         GitHub"
    } else if lowercase_message.contains("must form a stack") {
        // This happens when a Pull Request in the middle of the stack has been
        // merged and GitHub has deleted its branch, which breaks the chain of
        // base branches.
        "GitHub did not accept the stack, because the base branch of each pull \
         request must be the branch of the pull request below it"
    } else if is_not_found(&error) {
        STACKS_NOT_ENABLED
    } else {
        return Err(error);
    };

    output("⚠️", explanation)
}

/// Walk from the top-most Pull Request of this run down to the bottom of the
/// stack, and return the Pull Request numbers ordered from the bottom to the
/// top. GitHub wants the whole stack, but the user may have submitted only
/// some of the commits, so Pull Requests that this run did not touch are
/// looked up on GitHub.
async fn build_stack_chain(
    gh: &crate::github::GitHub,
    stack_links: &[StackLink],
) -> Result<Vec<u64>> {
    let top = match stack_links.last() {
        Some(top) => top,
        None => return Ok(Vec::new()),
    };

    // Collected from the top down, and turned around at the end.
    let mut chain = vec![top.pull_request_number];
    let mut base = top.base_pull_request_number;

    while let Some(number) = base {
        // Guard against a cycle in the base branches, which would otherwise
        // keep us here forever.
        if chain.contains(&number) {
            break;
        }

        chain.push(number);

        base = match stack_links
            .iter()
            .find(|link| link.pull_request_number == number)
        {
            // We submitted this Pull Request in this run, so we know its base
            // already and do not have to ask GitHub.
            Some(link) => link.base_pull_request_number,
            None => {
                let base_branch = gh.get_pull_request_base(number).await?;

                base_pull_request_number(gh, &base_branch, stack_links).await
            }
        };
    }

    chain.reverse();

    Ok(chain)
}
