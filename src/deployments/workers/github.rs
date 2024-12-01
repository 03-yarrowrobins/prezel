use std::sync::Arc;

use tracing::error;

use crate::{
    db::{Db, InsertDeployment, Project},
    deployments::worker::{Worker, WorkerHandle},
    github::{Commit, Github},
};

#[derive(Clone)]
pub(crate) struct GithubWorker {
    pub(crate) github: Github,
    pub(crate) db: Db,
}

impl Worker for GithubWorker {
    fn work(&self) -> impl std::future::Future<Output = ()> + Send {
        async {
            for Project {
                repo_id, env, id, ..
            } in self.db.get_projects().await
            {
                let commit = get_latest_commit_for_default_branch(&self.github, &repo_id).await;
                match commit {
                    Err(error) => {
                        error!("Got error when trying to read from Github: {error}");
                        error!("Cancelling run of github worker");
                        break;
                    }
                    Ok(commit) => {
                        if let Some(commit) = commit {
                            // TODO: review, doesn't seem to make much sense that this is an Option
                            let deployment = InsertDeployment {
                                env: env.to_owned(),
                                sha: commit.sha,
                                timestamp: commit.timestamp,
                                branch: None,
                                project: id,
                            };
                            add_deployment_to_db_if_missing(&self.db, deployment).await;
                        }
                    }
                }

                let pulls = self.github.get_open_pulls(&repo_id).await.unwrap();
                for pull in pulls {
                    let branch = pull.head.ref_field;
                    // FIXME: some duplicated code in here as in above
                    let commit = self.github.get_latest_commit(&repo_id, &branch).await;
                    match commit {
                        Err(error) => {
                            error!("Got error when trying to read from Github: {error}");
                            error!("Cancelling run of github worker");
                            break;
                        }
                        Ok(commit) => {
                            if let Some(commit) = commit {
                                let deployment = InsertDeployment {
                                    env: env.to_owned(),
                                    sha: commit.sha,
                                    timestamp: commit.timestamp,
                                    branch: Some(branch),
                                    project: id,
                                };
                                add_deployment_to_db_if_missing(&self.db, deployment).await;
                            }
                        }
                    }
                }
            }
        }
    }
}

async fn get_latest_commit_for_default_branch(
    github: &Github,
    repo_id: &str,
) -> anyhow::Result<Option<Commit>> {
    let default_branch = github.get_default_branch(repo_id).await?;
    let commit = github.get_latest_commit(repo_id, &default_branch).await?;
    Ok(commit)
}

async fn add_deployment_to_db_if_missing(db: &Db, deployment: InsertDeployment) {
    if !db.hash_exists(&deployment.sha).await {
        db.insert_deployment(deployment).await
    }
}