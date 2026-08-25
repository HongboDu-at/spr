![spr](./docs/spr.svg)

# spr &middot; [![GitHub](https://img.shields.io/github/license/HongboDu-at/spr)](https://github.com/HongboDu-at/spr/blob/main/LICENSE) [![GitHub release](https://img.shields.io/github/v/release/HongboDu-at/spr?include_prereleases)](https://github.com/HongboDu-at/spr/releases)

> A fork of [spacedentist/spr](https://github.com/spacedentist/spr). See
> [Fork Documentation](#fork-documentation) for the differences.

A command-line tool for submitting and updating GitHub Pull Requests from local
Git commits that may be amended and rebased. Pull Requests can be stacked to
allow for a series of code reviews of interdependent code.

spr is pronounced /ˈsuːpəɹ/, like the English word 'super'.

## Documentation

Comprehensive documentation of the original tool is available here:
https://getcord.github.io/spr/. It describes the original behaviour; see
[Fork Documentation](#fork-documentation) for where this fork differs.

## Fork Documentation

### Differences at a Glance

| Difference | Detail |
|---|---|
| No intermediate base branch | PR2 is based on PR1's branch. [More](#directly-use-pr1-as-base-branch-of-pr2) |
| Native GitHub stacks | The chain is registered with GitHub. [More](#native-github-stacks) |
| `Depends On` in the PR body | Links the stacked PR to its base. [More](#track-base-pr-in-the-stacked-pr-on-github) |
| `--base` | Base a PR on any branch or lower commit. [More](#override-base-branch-via-a-new---base-option) |
| Base branch persistence | Pass `--base` once, not on every update. [More](#base-branch-persistence) |
| Interactive base selection | Prompts when `--base` is omitted. [More](#interactive-base-selection-for-creating-new-pr) |
| Interactive commit selection | `--all` prompts for commits. [More](#interactively-select-some-commits-to-createupdate-pr) |
| `[COMMIT]` argument | Target one commit without a prompt. [More](#name-one-commit-with-the-commit-argument) |
| `spr sync` | New. Pulls remote PR changes into local commits. [More](#spr-sync) |
| `spr merge` | New. Enables auto-merge; requires `gh`. [More](#spr-merge) |
| `spr land` | Removed. Use `spr merge`. |
| Cherry-pick by default | `--no-cherry-pick` uses an intermediate base branch. [More](#cherry-pick-by-default) |
| Dirty worktree allowed | `spr diff` no longer refuses. [More](#a-dirty-worktree-is-allowed) |
| Pre-push hook runs | Skip it with `--no-verify`. [More](#the-pre-push-hook-runs) |
| Signatures preserved | Git signature and committer are kept. [More](#signatures-and-the-committer) |
| Live `git push` output | Shown as it runs. [More](#live-output-from-git-push) |
| No title/message prompt | Never asks when they differ. [More](#disable-prompts-for-titlemessage-differences) |
| `requireTestPlan` defaults to false | [More](#default-requiretestplan-to-false) |

### Directly use PR1 as Base Branch of PR2

When creating stacked PRs, unlike the original spr creating an intermediate base branch, PR2 just uses the branch of PR1 as its base branch. When the PR1 lands, the base branch in PR2 is automatically changed to main. This allows PR2 to land automatically after PR1 is landed without changing base or rerunning CI.

 - If the stacked PR shows no conflicts, we do not need to rebase or rerun CI in order to merge it.
 - If the stacked PR shows conflicts, we do not need to actually resolve any conflicts. `git pull --rebase && spr diff` will update the stacked PR. CI needs to rerun.

### Track Base PR in the Stacked PR on GitHub
A base PR link is automatically appended to the stacked PR’s body. GitHub automatically tracks the base PR's status.
Stacked PR is automatically mentioned in the base PR's conversation timeline.

For example, `Depends On: #<Base PR Number>`

### Interactive Base Selection for Creating New PR

If `--base` is not specified for new PR or you just do not like copy/paste, running `spr diff` for creating new PR automatically prompts users to select a base branch from lower local commits. This also means users just need to run the same command `spr diff` no matter creating or updating PRs. See the selection experience [here](https://github.com/mikaelmello/inquire#select).

### Interactively Select Some Commits to Create/Update PR

Running `spr diff --all` prompts users to select all or some commits to create/update PRs. Pressing → key easily selects all commits when needed. This allows users to have multiple stacks in one branch and users can select the commits of a stack to update. This also allows users to update any commit in the history without using `exec spr diff` in an interactive rebase. See the multi-selection experience [here](https://github.com/mikaelmello/inquire#multiselect).

### Override Base Branch via a New --base Option

Select base with parent HEAD references. This is useful when you have a series of new stacked commits and you can do `spr diff --all --base HEAD^` to stack all of them. BTW, when you have a series of new independent commits and you can do `spr diff --all --base main`.


`spr diff --base <any-branch-name>`  
`spr diff --base <branch-name-of-other-people-PR>`  
`spr diff --base HEAD^ --all`  
`spr diff --base HEAD~2`

### Base Branch Persistence

Users only need to specify a base branch when creating a PR. Updating an existing PR will continue to use the same base branch on GitHub. If needed, existing PR’s base branches can be changed with `--base`.

### spr merge

`spr merge` runs `gh pr merge <number>`, enabling auto-merge on the PR of the HEAD commit. It requires the [GitHub CLI](https://cli.github.com/) to be installed and authenticated. Like `spr diff`, `spr merge --all` prompts for commits and `spr merge <commit>` takes one.

`spr land` is removed. Use `spr merge`.

### Native GitHub Stacks

`spr diff` registers the chain as a [GitHub stack](https://docs.github.com/en/pull-requests/get-started/about-stacked-prs), so GitHub shows a stack map and re-targets the remaining PRs when the bottom one merges.

This happens automatically whenever each PR's base is the branch of the PR below it. Commits based on the master branch are not a stack, and neither are chains using an intermediate base branch (`--no-cherry-pick`). Reordering a stack needs `--base`. If GitHub rejects the stack, spr warns and carries on; the PRs are still updated.

### Name One Commit with the `[COMMIT]` Argument

`--all` needs a terminal, which scripts and agents do not have. `diff`, `merge` and `sync` also accept a single commit:

```shell
spr diff HEAD~2
spr diff 4b8f5ef
spr merge HEAD~1
spr sync HEAD~2
```

The commit must be in the local stack, cannot be combined with `--all`, and cannot be a range. Commit ids change when spr writes the `Pull Request:` line, so re-read them after each command that writes.

### spr sync

`spr sync` pulls changes made on GitHub back into the local commits, for example after someone pushed to a PR branch or applied a suggestion.

```shell
spr sync             # HEAD commit
spr sync --all       # prompt for commits
spr sync HEAD~2      # one commit
spr sync --dry-run   # report changes without writing
```

Unlike `spr diff`, `spr sync` checks out a new tree and so requires a clean worktree.

### A Dirty Worktree is Allowed

`spr diff` runs with uncommitted changes; the original spr refuses. It never checks out a tree, and the commits it writes keep the tree of the commits they replace, so the worktree is left untouched. `spr sync` still requires a clean worktree.

### The Pre-push Hook Runs

`spr diff` runs the `pre-push` hook, which the original spr does not. Use `spr diff --no-verify` to skip it.

### Signatures and the Committer

Commits written by spr keep your Git signature and follow Git's committer precedence rules.

### Live Output from `git push`

Output from `git push` and the `pre-push` hook is shown as it runs.

### Cherry-pick by Default

Use cherry-pick by default. Add `--no-cherry-pick` to create an intermediate base branch.

### Disable Prompts for Title/Message differences

Almost all the time users update PR summary in GitHub directly. Some workflow has no point in keeping them in sync. So the fork  does not prompt when title/message differ.

### Default requireTestPlan to false
It does not work well with `#Test Plan` in markdown.

## Installation

Homebrew, Nix and crates.io carry the original tool, not this fork.

### From a release

```shell
curl -fsSL https://raw.githubusercontent.com/HongboDu-at/spr/main/install.sh | sh
```

Installs the latest release into `$HOME/.local/bin`. Set `SPR_VERSION` (e.g. `1.3.4-enhanced.1`) or `SPR_INSTALL_DIR` to override. Binaries are built for Linux and macOS on x86_64 and arm64, and can also be downloaded from the [releases page](https://github.com/HongboDu-at/spr/releases).

### From source

spr is written in Rust. You need a Rust toolchain to build from source. See [rustup.rs](https://rustup.rs) for information on how to install Rust if you have not got a Rust toolchain on your system already.

With Rust all set up, clone this repository and run `cargo build --release`. The spr binary will be in the `target/release` directory.

## Quickstart

To use spr, run `spr init` inside a local checkout of a GitHub-backed git repository. You will be asked for a GitHub PAT (Personal Access Token), which spr will use to make calls to the GitHub API in order to create and merge pull requests.

To submit a commit for pull request, run `spr diff`.

If you want to make changes to the pull request, amend your local commit (and/or rebase it) and call `spr diff` again. When updating an existing pull request, spr will ask you for a short message to describe the update.

To enable auto-merge on an open pull request, run `spr merge`, which requires the [GitHub CLI](https://cli.github.com/). `spr land` is removed.

For more information on spr commands and options, run `spr help`. For more information on a specific spr command, run `spr help <COMMAND>` (e.g. `spr help diff`).

## Contributing

Feel free to submit an issue on [GitHub](https://github.com/HongboDu-at/spr/issues) if you have found a problem with this fork. If you can even provide a fix, please raise a pull request! For problems in the original tool, use [spacedentist/spr](https://github.com/spacedentist/spr/issues).

If there are larger changes or features that you would like to work on, please raise an issue on GitHub first to discuss.

### License

spr is [MIT licensed](./LICENSE).
