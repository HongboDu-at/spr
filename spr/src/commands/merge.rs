/*
 * Copyright (c) Radical HQ Limited
 *
 * This source code is licensed under the MIT license found in the
 * LICENSE file in the root directory of this source tree.
 */

use crate::{
    error::{Error, Result, ResultExt},
    output::output,
    utils::run_command,
};

#[derive(Debug, clap::Parser)]
pub struct MergeOptions {
    #[clap(flatten)]
    selection: crate::commands::select::CommitSelection,
}

pub async fn merge(
    opts: MergeOptions,
    git: &crate::git::Git,
    config: &crate::config::Config,
) -> Result<()> {
    let mut result = Ok(());

    // Look up the commits on the local branch
    let prepared_commits = git.get_prepared_commits(
        config,
        None,
        crate::git::PullRequestFilter::All,
    )?;

    if prepared_commits.is_empty() {
        output("👋", "Branch is empty - nothing to do. Good bye!")?;
        return result;
    };

    let selected_indexes = opts.selection.resolve(
        git,
        config,
        &prepared_commits,
        "Select commits to merge:",
    )?;

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
                    .arg("merge")
                    .arg(pull_request_number.to_string()),
            )
            .await
            .reword("enabling auto-merge failed".to_string())?;

            let pull_request_url = config.pull_request_url(pull_request_number);

            output(
                "✅",
                &format!(
                    "Enabled auto-merge on Pull Request #{}: {}",
                    pull_request_number, pull_request_url,
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
