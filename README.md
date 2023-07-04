![spr](./docs/spr.svg)

# spr &middot; [![GitHub](https://img.shields.io/github/license/getcord/spr)](https://img.shields.io/github/license/getcord/spr) [![GitHub release](https://img.shields.io/github/v/release/getcord/spr?include_prereleases)](https://github.com/getcord/spr/releases) [![crates.io](https://img.shields.io/crates/v/spr.svg)](https://crates.io/crates/spr) [![homebrew](https://img.shields.io/homebrew/v/spr.svg)](https://formulae.brew.sh/formula/spr) [![GitHub Repo stars](https://img.shields.io/github/stars/getcord/spr?style=social)](https://github.com/getcord/spr)

A command-line tool for submitting and updating GitHub Pull Requests from local
Git commits that may be amended and rebased. Pull Requests can be stacked to
allow for a series of code reviews of interdependent code.

spr is pronounced /ˈsuːpəɹ/, like the English word 'super'.

## Documentation

Comprehensive documentation is available here: https://getcord.github.io/spr/

## Fork Documentation

When creating stacked PRs, unlike the original spr creating an intermediate base branch, PR2 just uses the branch of PR1 as its bash branch. This allows PR2 to land automatically after PR1 is landed without changing base or rerunning CI.

An optional new `--base` option in the forked spr allows users to stack PR onto any remote branches or any local commits, eg, parent commit, grandparent commit, main, etc. Base branches are persisted so you only need to specify the base branch once and you can freely reorder the commit stack.

The forked spr also adds interactive base selections alternative to specifying `--base` and interactive commit selections for bulk creating/updating PRs or for updating a PR in the middle without using `exec spr diff` in an interactive rebase. It uses cherry-pick by default. `--no-cherry-pick` is added if you sometimes need to create an intermediate base branch.

### Directly use PR1 as Base Branch of PR2

When creating stacked PRs, unlike the original spr creating an intermediate base branch, PR2 just uses the branch of PR1 as its base branch. When the PR1 lands, the base branch in PR2 is automatically changed to main. This allows PR2 to land automatically after PR1 is landed without changing base or rerunning CI.

 - If the stacked PR shows no conflicts, we do not need to rebase or rerun CI in order to merge it.
 - If the stacked PR shows conflicts, we do not need to actually resolve any conflicts. `git pull --rebase && spr diff` will update the stacked PR. CI needs to rerun.

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

### Add `spr merge`, which adds 'mergeme' label

This is added specific to some workflows. When invoked, 'mergeme' label is added to the PR of the HEAD commit. Similar to `spr diff --all`, `spr merge --all` prompts users to select all or some commits to merge PRs.

### Cherry-pick by Default

Use cherry-pick by default. Add `--no-cherry-pick` to create an intermediate base branch.

### Disable Prompts for Title/Message differences

Almost all the time users update PR summary in GitHub directly. Some workflow has no point in keeping them in sync. So the fork  does not prompt when title/message differ.

### Default requireTestPlan to false
It does not work well with `#Test Plan` in markdown.

## Installation

### The only way to install the fork is from Source

spr is written in Rust. You need a Rust toolchain to build from source. See [rustup.rs](https://rustup.rs) for information on how to install Rust if you have not got a Rust toolchain on your system already.

With Rust all set up, clone this repository and run `cargo build --release`. The spr binary will be in the `target/release` directory.

## Quickstart

To use spr, run `spr init` inside a local checkout of a GitHub-backed git repository. You will be asked for a GitHub PAT (Personal Access Token), which spr will use to make calls to the GitHub API in order to create and merge pull requests.

To submit a commit for pull request, run `spr diff`.

If you want to make changes to the pull request, amend your local commit (and/or rebase it) and call `spr diff` again. When updating an existing pull request, spr will ask you for a short message to describe the update.

To squash-merge an open pull request, run `spr land`.

For more information on spr commands and options, run `spr help`. For more information on a specific spr command, run `spr help <COMMAND>` (e.g. `spr help diff`).

## Contributing

Feel free to submit an issue on [GitHub](https://github.com/getcord/spr) if you have found a problem. If you can even provide a fix, please raise a pull request!

If there are larger changes or features that you would like to work on, please raise an issue on GitHub first to discuss.

### License

spr is [MIT licensed](./LICENSE).
