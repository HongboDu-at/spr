/*
 * Copyright (c) Radical HQ Limited
 *
 * This source code is licensed under the MIT license found in the
 * LICENSE file in the root directory of this source tree.
 */

use crate::{
    error::{Error, Result, ResultExt},
    git::CommitOption,
    message::MessageSection,
    output::output,
    utils::run_command,
};
use inquire::MultiSelect;

#[derive(Debug, clap::Parser)]
pub struct MergeOptions {
    /// Open an interactive selection to select all or some commits to
    /// merge pull requests, not just the HEAD commit
    #[clap(long, short = 'a')]
    all: bool,
}

pub async fn merge(
    opts: MergeOptions,
    git: &crate::git::Git,
    config: &crate::config::Config,
) -> Result<()> {
    let mut result = Ok(());

    // Look up the commits on the local branch
    let prepared_commits = git.get_prepared_commits(config, None)?;
    let length = prepared_commits.len();

    if prepared_commits.get(0).is_none() {
        output("👋", "Branch is empty - nothing to do. Good bye!")?;
        return result;
    };

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
                            .unwrap_or_else(|| "?????".to_string()),
                        title
                    ),
                    index: i as isize,
                }
            })
            .rev()
            .collect::<Vec<CommitOption>>();

        let ans =
            MultiSelect::new("Select commits to merge:", options).prompt()?;

        ans.iter().map(|x| x.index as usize).rev().collect()
    } else {
        vec![length - 1]
    };

    // selected_indexes is sorted from lower commits to higher commits
    for index in selected_indexes {
        if result.is_err() {
            break;
        }

        let pull_request_number = prepared_commits[index].pull_request_number;

        if let Some(pull_request_number) = pull_request_number {
            // This could be refactored to use the GitHub API directly
            // but this is a quick and easy way to get the job done
            // `spr label` and git config frequent labels could be added
            run_command(
                tokio::process::Command::new("gh")
                    .arg("pr")
                    .arg("edit")
                    .arg(pull_request_number.to_string())
                    .arg("--add-label")
                    .arg("mergeme"),
            )
            .await
            .reword("adding 'mergeme' label failed".to_string())?;

            let pull_request_url = config.pull_request_url(pull_request_number);

            output(
                "✅",
                &format!(
                    "Added 'mergeme' label on Pull Request #{}: {}",
                    pull_request_number, &pull_request_url,
                ),
            )?;
        } else {
            result = Err(Error::new(
                "Your selection obviously has no PR created yet".to_string(),
            ));
        }
    }

    result
}
