use actix_web::web::{Data, ServiceConfig};
use octocrab::models::Repository as CrabRepository;
use serde::Serialize;
use utoipa::{OpenApi, ToSchema};

use crate::{
    db::{BuildResult, Db, DeploymentWithProject, InsertProject, UpdateProject},
    deployments::{deployment::Deployment, manager::Manager},
    github::Github,
    logging::{Level, Log},
};

mod apps;
mod deployments;
mod security;
pub(crate) mod server;
mod system;
mod utils;

pub(crate) const API_PORT: u16 = 5045;

// TODO: move this to routes.rs so I don't forget updating them
#[derive(OpenApi)]
#[openapi(
    paths(
        system::health,
        system::get_repos,
        system::get_system_logs,
        apps::get_projects,
        apps::get_project,
        apps::create_project,
        apps::update_project,
        apps::delete_project,
        deployments::redeploy,
        deployments::delete_deployment,
        deployments::sync,
        deployments::get_deployment_logs,
        deployments::get_deployment_build_logs
    ),
    components(schemas(ProjectInfo, FullProjectInfo, ErrorResponse, UpdateProject, Repository, ApiDeployment, Log, Level, Status, InsertProject)),
    tags(
        (name = "prezel", description = "Prezel management endpoints.")
    ),
)]
struct ApiDoc;

fn configure_service(store: Data<AppState>) -> impl FnOnce(&mut ServiceConfig) {
    |config: &mut ServiceConfig| {
        config
            .app_data(store)
            .service(system::health)
            .service(system::get_repos)
            .service(system::get_system_logs)
            .service(apps::get_projects)
            .service(apps::get_project)
            .service(apps::create_project)
            .service(apps::update_project)
            .service(apps::delete_project)
            .service(deployments::redeploy)
            .service(deployments::delete_deployment)
            .service(deployments::sync)
            .service(deployments::get_deployment_logs)
            .service(deployments::get_deployment_build_logs);
        // If I add anything here also need to add it in api/mod.rs
    }
}

// TODO: there is some duplication here, because manager holds db and github as well
#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) db: Db,
    pub(crate) manager: Manager,
    pub(crate) github: Github,
}

#[derive(Serialize, ToSchema)]
enum ErrorResponse {
    /// When Todo is not found by search term.
    NotFound(String),
    /// When there is a conflict storing a new todo.
    Conflict(String),
    /// When todo endpoint was called without correct credentials
    Unauthorized(String),
}

// #[derive(Serialize, ToSchema)]
// struct LogLine {
//     log_type: String,
//     content: String,
// }

// impl From<Line> for LogLine {
//     fn from(value: Line) -> Self {
//         match value {
//             Line::Stdout(line) => Self {
//                 log_type: "ok".to_owned(),
//                 content: line,
//             },
//             Line::Stderr(line) => Self {
//                 log_type: "error".to_owned(),
//                 content: line,
//             },
//         }
//     }
// }

#[derive(Debug, PartialEq, Clone, Copy, ToSchema, Serialize)]
pub(crate) enum Status {
    Built,
    StandBy,
    Queued,
    Building,
    Ready,
    Failed,
}

impl ToString for Status {
    fn to_string(&self) -> String {
        let string = match self {
            Self::Built => "built",
            Self::Queued => "queued",
            Self::Building => "building",
            Self::StandBy => "stand by",
            Self::Ready => "ready",
            Self::Failed => "failed",
        };
        string.to_owned()
    }
}

#[derive(Serialize, ToSchema)]
#[schema(title = "Deployment")]
struct ApiDeployment {
    id: i64,
    url_id: String,
    // project: Project, // TODO: review why I needed this
    sha: String,
    gitref: String,
    // port: u16,
    url: Option<String>,
    target_url: Option<String>,
    db_url: Option<String>,
    status: Status,
    app_container: Option<String>,
    // execution_logs: Vec<DockerLog>,
    created: i64,
    build_started: Option<i64>,
    build_finished: Option<i64>,
}

// TODO: move this somewhere else
impl ApiDeployment {
    // TODO: make info an option so deployments can show up in the API before the manager reads them
    async fn from(
        deployment: Option<&Deployment>,
        db_deployment: &DeploymentWithProject,
        is_prod: bool,
        box_domain: &str,
        github: &Github,
    ) -> Self {
        let (status, url, prod_url, db_url, app_container) = if let Some(deployment) = deployment {
            let status = deployment.app_container.status.read().await.to_status();

            let project_name = &db_deployment.project.name;
            let url = Some(deployment.get_app_hostname(box_domain, project_name)).plus_https();
            let db_url = Some(deployment.get_db_hostname(box_domain, project_name)).plus_https();
            let prod_url = is_prod
                .then_some(deployment.get_prod_hostname(box_domain, project_name))
                .plus_https();

            let app_container = deployment.app_container.get_container_id().await;
            (status, url, prod_url, db_url, app_container)
        } else {
            let status = match db_deployment.result {
                Some(BuildResult::Failed) => Status::Failed,
                Some(BuildResult::Built) => Status::Built,
                None => Status::Queued,
            };
            (status, None, None, None, None)
        };

        let repo_id = db_deployment.project.repo_id.clone();
        let gitref = match &db_deployment.branch {
            Some(branch) => branch.clone(),
            None => github.get_default_branch(&repo_id).await.unwrap(),
        };

        // TODO: I should have a nested struct for the container related
        // info so it can be an option as a whole
        Self {
            id: db_deployment.id,
            url_id: db_deployment.url_id.clone(),
            // project: value.deployment.project.clone(),// TODO: review why I needed this
            sha: db_deployment.sha.clone(),
            gitref,
            url, // TODO: add method to get the http version from the same object !!!
            target_url: prod_url,
            db_url,
            status,
            app_container,
            created: db_deployment.created,
            build_started: db_deployment.build_started,
            build_finished: db_deployment.build_finished,
        }
    }
}

trait PlusHttps {
    fn plus_https(&self) -> Option<String>;
}

impl PlusHttps for Option<String> {
    fn plus_https(&self) -> Option<String> {
        self.as_ref().map(|hostname| format!("https://{hostname}"))
    }
}

#[derive(Serialize, ToSchema)]
struct Repository {
    id: String,
    name: String,
    owner: Option<String>,
    default_branch: Option<String>,
    pushed_at: Option<i64>,
}

impl From<CrabRepository> for Repository {
    fn from(value: CrabRepository) -> Self {
        Self {
            id: value.id.to_string(),
            name: value.name,
            owner: value.owner.map(|owner| owner.login),
            default_branch: value.default_branch,
            pushed_at: value.pushed_at.map(|at| at.timestamp_millis()),
        }
    }
}

#[derive(Serialize, ToSchema)]
struct ProjectInfo {
    name: String,
    id: i64,
    repo: Repository,
    created: i64,
    env: String,
    custom_domains: Vec<String>,
    prod_deployment_id: Option<i64>,
    prod_deployment: Option<ApiDeployment>,
}

#[derive(Serialize, ToSchema)]
struct FullProjectInfo {
    name: String,
    id: i64,
    repo: Repository,
    created: i64,
    env: String,
    custom_domains: Vec<String>,
    prod_deployment_id: Option<i64>,
    prod_deployment: Option<ApiDeployment>,
    /// All project deployments sorted by created datetime descending
    deployments: Vec<ApiDeployment>,
}
