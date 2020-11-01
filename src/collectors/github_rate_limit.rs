use prometheus::{core::Collector, IntGauge, Opts};

use reqwest::header::{ACCEPT, AUTHORIZATION, USER_AGENT};
use reqwest::Method;
use std::collections::HashMap;
use tokio::time::Duration;

#[derive(Clone)]
struct User {
    token: String,
    name: String,
    limit: IntGauge,
    remaining: IntGauge,
    reset: IntGauge,
}

const GH_API_TOKENS_ENV_KEY: &str = "GITHUB_API_TOKENS";
const DATA_CACHE_UPDATE_SECONDS: u64 = 120;

#[derive(Clone)]
pub struct GitHubRateLimit {
    descriptions: Vec<prometheus::core::Desc>,
    users: Vec<User>,
}

impl GitHubRateLimit {
    pub async fn new() -> Self {
        let tokens: Vec<String> = match std::env::var(GH_API_TOKENS_ENV_KEY) {
            Ok(t) => t.split(',').map(|v| v.trim().to_string()).collect(),
            _ => panic!("{} env var not set.", GH_API_TOKENS_ENV_KEY),
        };

        let users = Self::get_users_for_tokens(tokens).await;
        let descriptions = Vec::new();

        let rv = Self {
            users,
            descriptions,
        };

        let mut rv2 = rv.clone();
        tokio::spawn(async move {
            loop {
                rv2.update_stats().await;
                tokio::time::delay_for(Duration::from_secs(DATA_CACHE_UPDATE_SECONDS)).await;
            }
        });

        rv
    }

    async fn get_users_for_tokens(tokens: Vec<String>) -> Vec<User> {
        let ns = String::from("rustinfra_github_rate_limit");
        let mut rv: Vec<User> = Vec::new();
        for token in tokens.into_iter() {
            let ns2 = ns.clone();
            let username = GitHubRateLimit::get_github_api_username(&token).await;
            let user_future = tokio::task::spawn_blocking(move || {
                let rate_limit = IntGauge::with_opts(
                    Opts::new("limit", "Rate limit.")
                        .namespace(ns2.clone())
                        .const_label("username", username.clone()),
                )
                .unwrap();

                let rate_remaining = IntGauge::with_opts(
                    Opts::new("remaining", "Rate remaining.")
                        .namespace(ns2.clone())
                        .const_label("username", username.clone()),
                )
                .unwrap();

                let rate_reset = IntGauge::with_opts(
                    Opts::new("reset", "Rate reset.")
                        .namespace(ns2.clone())
                        .const_label("username", username.clone()),
                )
                .unwrap();

                User {
                    token: token.to_owned(),
                    name: username,
                    limit: rate_limit,
                    remaining: rate_remaining,
                    reset: rate_reset,
                }
            });

            let user = match user_future.await {
                Ok(u) => u,
                _ => panic!("We need to decide if we wanna panic or keep going"),
            };

            rv.push(user);
        }

        rv
    }

    async fn get_github_api_username(token: &str) -> String {
        #[derive(serde::Deserialize)]
        struct GithubUser {
            pub login: String,
        }

        let client = reqwest::Client::new();
        let req = client
            .request(Method::GET, "https://api.github.com/user")
            .header(USER_AGENT, "monitorbot-rust-lang")
            .header(AUTHORIZATION, format!("{} {}", "token", token))
            .header(ACCEPT, "application/vnd.github.v3+json")
            .build()
            .unwrap();

        let u = client
            .execute(req)
            .await
            .unwrap()
            .json::<GithubUser>()
            .await
            .unwrap();

        u.login
    }

    pub async fn update_stats(&mut self) {
        #[derive(Debug, serde::Deserialize)]
        struct GithubRateLimit {
            pub rate: HashMap<String, usize>,
        }

        let client = reqwest::Client::new();

        //FIXME: we will (might?) need a RWLock on users structure
        for u in self.users.iter_mut() {
            let req = client
                .request(Method::GET, "https://api.github.com/rate_limit")
                .header(USER_AGENT, "monitorbot-rust-lang")
                .header(AUTHORIZATION, format!("{} {}", "token", u.token))
                .header(ACCEPT, "application/vnd.github.v3+json")
                .build()
                .unwrap();

            let mut data = client
                .execute(req).await.unwrap()
                .json::<GithubRateLimit>().await.unwrap();

            let remaining = data.rate.remove("remaining").unwrap_or(0);
            let limit = data.rate.remove("limit").unwrap_or(0);
            let reset = data.rate.remove("reset").unwrap_or(0);

            u.remaining.set(remaining as i64);
            u.reset.set(reset as i64);
            u.limit.set(limit as i64);
        }
    }
}

impl Collector for GitHubRateLimit {
    fn desc(&self) -> std::vec::Vec<&prometheus::core::Desc> {
        self.descriptions.iter().collect()
    }

    fn collect(&self) -> std::vec::Vec<prometheus::proto::MetricFamily> {
        // collect MetricFamilys.
        let mut mfs = Vec::new();
        for user in self.users.iter() {
            mfs.extend(user.limit.collect());
            mfs.extend(user.remaining.collect());
            mfs.extend(user.reset.collect());
        }

        mfs
    }
}
