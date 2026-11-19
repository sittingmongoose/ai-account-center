use super::{AuthRuntime, Config, Reply, RequestInfo, json, response, set_header};
use std::collections::HashMap;

#[derive(Default)]
pub(in super::super) struct Limits(HashMap<String, Hit>);

struct Hit {
    count: u32,
    end: i64,
}

pub(super) struct Window {
    key: String,
    count: u32,
    max: u32,
    end: i64,
    now: i64,
    duration: i64,
}

impl Limits {
    pub(super) fn check(&mut self, key: String, at: i64, max: u32, duration: i64) -> Window {
        self.0.retain(|_, hit| hit.end > at);
        let existing = self.0.get(&key);
        let count = existing.map_or(if self.0.len() >= 65_536 { max } else { 0 }, |hit| {
            hit.count
        });
        Window {
            key,
            count,
            max,
            end: existing.map_or(at + duration, |hit| hit.end),
            now: at,
            duration,
        }
    }
    pub(super) fn spend(&mut self, window: &Window) {
        if !window.blocked() {
            self.0.insert(
                window.key.clone(),
                Hit {
                    count: window.count + 1,
                    end: window.end,
                },
            );
        }
    }
}

impl Window {
    pub(super) fn blocked(&self) -> bool {
        self.count >= self.max
    }
    pub(super) fn remaining(&self) -> u32 {
        self.max.saturating_sub(self.count.saturating_add(1))
    }
    fn seconds(&self) -> i64 {
        ((self.end - self.now + 999) / 1000).max(1)
    }
    pub(super) fn headers(&self, mut reply: Reply) -> Reply {
        set_header(
            &mut reply,
            "ratelimit-policy",
            &format!("{};w={}", self.max, self.duration / 1000),
        );
        set_header(&mut reply, "ratelimit-limit", &self.max.to_string());
        set_header(
            &mut reply,
            "ratelimit-remaining",
            &self.remaining().to_string(),
        );
        set_header(&mut reply, "ratelimit-reset", &self.seconds().to_string());
        reply
    }
}

pub(super) fn limited(client: &Window, applied: &Window, message: &str) -> Reply {
    let seconds = applied.seconds();
    let mut reply = response(
        429,
        json!({"error":message,"code":"rate_limited","retryAfterSeconds":seconds}),
    );
    set_header(&mut reply, "retry-after", &seconds.to_string());
    client.headers(reply)
}

#[derive(Default)]
pub(super) struct Budget {
    client: Option<Window>,
    server: Option<Window>,
}

impl Budget {
    pub(super) fn headers(&self, reply: Reply) -> Reply {
        if let Some(client) = &self.client {
            client.headers(reply)
        } else {
            reply
        }
    }
    pub(super) fn tries_left(&self) -> u32 {
        self.client
            .iter()
            .chain(self.server.iter())
            .map(Window::remaining)
            .min()
            .unwrap_or(0)
    }
}

impl AuthRuntime {
    pub(super) fn sign_in_budget(
        &mut self,
        config: &Config,
        req: &RequestInfo,
    ) -> Result<Budget, Reply> {
        const MESSAGE: &str = "Too many login attempts. Please try again later.";
        if !config.enabled {
            return Ok(Budget::default());
        }
        let client = self
            .login
            .check(req.connection.client_key(), req.at, 5, 900_000);
        if client.blocked() {
            return Err(limited(&client, &client, MESSAGE));
        }
        let server = if req.connection.via_proxy {
            let server = self
                .login_server
                .check("server".into(), req.at, 30, 3_600_000);
            if server.blocked() {
                return Err(limited(&client, &server, MESSAGE));
            }
            Some(server)
        } else {
            None
        };
        Ok(Budget {
            client: Some(client),
            server,
        })
    }

    pub(super) fn spend_login(&mut self, budget: &Budget, credential_refused: bool) {
        if let Some(client) = &budget.client {
            self.login.spend(client);
        }
        if credential_refused && let Some(server) = &budget.server {
            self.login_server.spend(server);
        }
    }
}
