use super::{
    AuthRuntime, Reply, RequestInfo,
    devices::{DeviceAuth, DeviceError, Devices, Pair, id_valid},
    error, invalid_body, plain,
    policy::{get, header, origin_allowed},
    response, service_error, set_header,
    storage::{Config, DAYS, NewSession, Session, StoreError, is_hex, iso, same},
};
use axum::http::Method;
use serde_json::{Value, json};
use std::sync::LazyLock;

mod limits;
pub(super) use limits::Limits;
use limits::{Budget, limited};

type Browser = Option<(String, Session)>;
type Outcome = Result<Reply, Reply>;

pub(super) fn dispatch(
    state: &mut AuthRuntime,
    config: &mut Config,
    req: &RequestInfo,
    session: &mut Browser,
    device: Option<&DeviceAuth>,
) -> Option<Reply> {
    Some(match req.path.as_str() {
        "/api/auth/check" if get(&req.method) => check(config, req, session),
        "/api/auth/setup" if get(&req.method) => setup_status(config, req, session),
        "/api/auth/setup" if req.method == Method::POST => setup(state, config, req, session),
        "/api/auth/login" if req.method == Method::POST => login(state, config, req, session),
        "/api/auth/logout" if req.method == Method::POST => logout(state, req, session),
        "/api/bar/auth" if get(&req.method) => bar(state, req),
        "/api/auth/session" if get(&req.method) => (|| {
            browser(config, session, device)?;
            no_query(req)?;
            Ok(response(200, json!({
                "username": username(config, session),
                "sessionTimeoutHours": config.days * 24, "sessionLifetimeDays": config.days,
                "expiresAt": session.as_ref().and_then(|(_, session)| session.expires_at()),
                "otherBrowsers": count_others(state, req, session), "passwordChangedAt": config.changed_at(),
                "pairedDevices": device_count(state, req.at), "managedBy": config.source(),
                "secureTransport": req.connection.secure, "secureOrigin": config.network.origin,
            })))
        })(),
        "/api/auth/password" if req.method == Method::POST => change_password(state, config, req, session, device),
        "/api/auth/sessions/revoke-others" if req.method == Method::POST => (|| {
            browser(config, session, device)?;
            body(req, true, &[], &[], false)?;
            let budget = state.rotation.check(account_key(req, config, session), req.at, 10, 900_000);
            if budget.blocked() {
                return Err(limited(&budget, &budget, "Too many sign-outs in a short time. Try again later."));
            }
            state.rotation.spend(&budget);
            let count = count_others(state, req, session);
            state.bump_epoch()?;
            let name = username(config, session).to_owned();
            rotate_session(state, config, req, session, &name, None)?;
            tracing::info!(count, "auth.sessions.revoked");
            Ok(budget.headers(response(200, json!({"signedOutBrowsers": count}))))
        })(),
        "/api/auth/session-lifetime" | "/api/auth/network"
            if get(&req.method) || req.method == Method::PUT => settings(state, config, req, session, device),
        "/api/auth/devices/pair" if req.method == Method::POST => pair(state, config, req),
        "/api/auth/devices" if get(&req.method) => (|| {
            browser(config, session, device)?;
            no_query(req)?;
            let devices = Devices::load(&state.files).map_err(device_write_error)?;
            Ok(response(200, json!({"devices":devices.list(req.at)})))
        })(),
        "/api/auth/devices/revoke-all" if req.method == Method::POST => (|| {
            browser(config, session, device)?;
            let fields = body(req, true, &[], &["signOutOtherBrowsers"], false)?;
            let sign_out = optional_bool(fields, "signOutOtherBrowsers")? != Some(false);
            let revoked = Devices::load(&state.files).and_then(|devices| devices.revoke_all(&state.files, req.at))
                .map_err(device_write_error)?;
            let count = if sign_out {
                let count = count_others(state, req, session);
                state.bump_epoch()?;
                tracing::info!(count, "auth.sessions.revoked");
                count
            } else { 0 };
            let name = username(config, session).to_owned();
            rotate_session(state, config, req, session, &name, None)?;
            Ok(response(200, json!({"revokedDevices":revoked,"signedOutBrowsers":count})))
        })(),
        "/api/auth/devices/me" if get(&req.method) => (|| {
            let device = require_device(config, device)?;
            no_query(req)?;
            let value = Devices::load(&state.files).and_then(|devices| devices.me(device, req.at))
                .map_err(device_write_error)?;
            Ok(response(200, value))
        })(),
        "/api/auth/devices/me/rotate" if req.method == Method::POST => (|| {
            secure(config, req)?;
            let device = require_device(config, device)?;
            body(req, false, &[], &[], true)?;
            let value = Devices::load(&state.files).and_then(|devices| devices.rotate(&state.files, device, &req.connection, req.at))
                .map_err(device_write_error)?;
            Ok(response(200, value))
        })(),
        "/api/auth/devices/me" if req.method == Method::DELETE => (|| {
            let device = require_device(config, device)?;
            body(req, false, &[], &[], true)?;
            Devices::load(&state.files).and_then(|devices| devices.revoke(&state.files, &device.id, "self", req.at))
                .map_err(device_write_error)?;
            Ok(super::empty(204))
        })(),
        path if req.method == Method::DELETE && path.strip_prefix("/api/auth/devices/")
            .is_some_and(|id| !id.is_empty() && !id.contains('/')) => (|| {
            browser(config, session, device)?;
            body(req, true, &[], &[], true)?;
            let raw = &path["/api/auth/devices/".len()..];
            let id = super::decode_component(raw).ok_or_else(|| plain(400, "The request could not be processed."))?;
            if !id_valid(&id) { return Err(device_write_error(DeviceError::Unknown)); }
            Devices::load(&state.files).and_then(|devices| devices.revoke(&state.files, &id, "dashboard", req.at))
                .map_err(device_write_error)?;
            Ok(super::empty(204))
        })(),
        _ => return None,
    }.unwrap_or_else(|reply| reply))
}

fn check(config: &Config, req: &RequestInfo, session: &Browser) -> Outcome {
    let configured = config.configured();
    let session = session.as_ref().map(|(_, session)| session);
    let authenticated = session.is_some_and(|session| session.authenticated);
    let mut value = json!({
        "authRequired":config.enabled,"authEnabled":config.enabled,"authConfigured":configured,
        "isLocalAccess":!req.connection.lan_proxy && req.connection.peer.is_loopback(),
        "accessMode":if !config.enabled {"open"} else if configured {"login"} else {"setup"},
        "authenticated":authenticated,"username":session.and_then(|session| session.username.as_deref()),
        "signedOutReason":session.filter(|session| session.revoked).map(|_| "revoked"),
        "setupCodeRequired":!configured && !req.connection.direct_loopback,
        "secureTransport":req.connection.secure,"secureOrigin":config.network.origin
    });
    if authenticated || !req.connection.lan_proxy {
        value["trustedLocalNetwork"] = json!(config.network.trust);
    }
    value["connection"] = req.connection.description();
    Ok(response(200, value))
}

fn setup_status(config: &Config, req: &RequestInfo, session: &Browser) -> Outcome {
    let configured = config.configured();
    let show = !req.connection.lan_proxy
        || session
            .as_ref()
            .is_some_and(|(_, session)| session.authenticated);
    let mut value = json!({"enabled":config.enabled,"configured":configured});
    if show {
        value["sessionTimeoutHours"] = json!(config.days * 24);
        value["sessionLifetimeDays"] = json!(config.days);
    }
    value["setupCodeRequired"] = json!(!configured && !req.connection.direct_loopback);
    if show {
        value["managedBy"] = json!(config.source());
    }
    value["secureTransport"] = json!(req.connection.secure);
    value["secureOrigin"] = json!(config.network.origin);
    if show {
        value["trustedLocalNetwork"] = json!(config.network.trust);
    }
    value["connection"] = req.connection.description();
    Ok(response(200, value))
}

fn login(
    state: &mut AuthRuntime,
    config: &Config,
    req: &RequestInfo,
    session: &mut Browser,
) -> Outcome {
    let budget = state.sign_in_budget(config, req)?;
    let run = (|| {
        let username = login_field(&req.body["username"], 256);
        let password = login_field(&req.body["password"], 1024);
        let remember = optional_bool(&req.body, "rememberMe");
        let (Some(username), Some(password), Ok(remember)) = (username, password, remember) else {
            // Login predates the strict auth request body contract.
            state.spend_login(&budget, false);
            return Err(plain(400, "Username and password required"));
        };
        if req.connection.lan_proxy
            && let Err(reply) = secure(config, req)
        {
            state.spend_login(&budget, false);
            return Err(reply);
        }
        if !config.enabled || !config.configured() {
            return Err(plain(400, "Authentication not configured"));
        }
        if !valid_hash(&config.hash) {
            state.spend_login(&budget, false);
            return Err(plain(500, "Invalid password hash format in config"));
        }
        verify_sign_in(state, config, req, &budget, username, password)?;
        rotate_session(
            state,
            config,
            req,
            session,
            username,
            Some(remember != Some(false)),
        )
        .map_err(|_| plain(500, "Session error"))?;
        Ok(response(200, json!({"success":true,"username":username})))
    })();
    Ok(budget.headers(run.unwrap_or_else(|reply| reply)))
}

fn pair(state: &mut AuthRuntime, config: &Config, req: &RequestInfo) -> Outcome {
    secure(config, req)?;
    configured(config)?;
    let fields = body(
        req,
        false,
        &["username", "password", "deviceName", "platform"],
        &["installId", "appVersion"],
        false,
    )?;
    let pair = Pair::parse(fields).ok_or_else(invalid_body)?;
    let username = login_field(&fields["username"], 256).ok_or_else(invalid_body)?;
    let password = login_field(&fields["password"], 1024).ok_or_else(invalid_body)?;
    let budget = state.sign_in_budget(config, req)?;
    let run = (|| {
        if !valid_hash(&config.hash) {
            state.spend_login(&budget, false);
            return Err(error(
                500,
                "invalid_hash",
                "The stored password hash is not valid.",
            ));
        }
        verify_sign_in(state, config, req, &budget, username, password)?;
        let value = Devices::load(&state.files)
            .and_then(|devices| devices.pair(&state.files, pair, &req.connection, req.at))
            .map_err(device_write_error)?;
        Ok(response(201, value))
    })();
    Ok(budget.headers(run.unwrap_or_else(|reply| reply)))
}

fn verify_sign_in(
    state: &mut AuthRuntime,
    config: &Config,
    req: &RequestInfo,
    budget: &Budget,
    username: &str,
    password: &str,
) -> Result<(), Reply> {
    let matches = verify(password, &config.hash);
    if same(username, &config.username) && matches && state.credential_unchanged(config) {
        return Ok(());
    }
    state.spend_login(budget, true);
    tracing::info!(remote_address = ?req.connection.client_address(), reason = "invalid_credentials", "auth.login.failed");
    Err(failure_with_tries(
        401,
        "invalid_credentials",
        "Invalid credentials",
        budget.tries_left(),
    ))
}

fn failure_with_tries(status: u16, code: &str, message: &str, left: u32) -> Reply {
    response(
        status,
        json!({"error":message,"code":code,"triesLeft":left}),
    )
}

fn rotate_session(
    state: &mut AuthRuntime,
    config: &Config,
    req: &RequestInfo,
    session: &mut Browser,
    name: &str,
    remember: Option<bool>,
) -> Result<(), StoreError> {
    *session = Some(state.sessions.start(
        &state.files,
        NewSession {
            prior: session.as_ref(),
            username: name,
            remember,
            connection: &req.connection,
            days: config.days,
            at: req.at,
        },
    )?);
    Ok(())
}

fn logout(state: &mut AuthRuntime, req: &RequestInfo, session: &mut Browser) -> Outcome {
    if let Some((id, _)) = session.as_ref() {
        state
            .sessions
            .destroy(&state.files, id, req.at)
            .map_err(|_| plain(500, "Failed to logout"))?;
    }
    *session = None;
    let mut reply = response(200, json!({"success":true}));
    set_header(
        &mut reply,
        "set-cookie",
        "connect.sid=; Path=/; Expires=Thu, 01 Jan 1970 00:00:00 GMT",
    );
    Ok(reply)
}

fn bar(state: &AuthRuntime, req: &RequestInfo) -> Outcome {
    let mut reply = response(200, json!({"status":"ok"}));
    if req.method == Method::GET
        && req.raw_path == "/api/bar/auth"
        && let Some(nonce) = header(&req.headers, "x-ccs-bar-nonce")
            .map(str::trim)
            .filter(|nonce| (16..=128).contains(&nonce.len()) && is_hex(nonce, nonce.len()))
    {
        set_header(
            &mut reply,
            "x-ccs-bar-token",
            &state.files.bar_proof(nonce)?,
        );
    }
    Ok(reply)
}

fn setup(
    state: &mut AuthRuntime,
    config: &mut Config,
    req: &RequestInfo,
    session: &mut Browser,
) -> Outcome {
    if config.managed_by_env {
        return Err(managed_by_env());
    }
    if config.configured() {
        return Err(already_configured());
    }
    let local = req.connection.direct_loopback;
    if !local {
        secure(config, req)?;
    }
    let fields = body(req, false, &["username", "password"], &["setupCode"], false)?;
    let username = fields["username"].as_str().ok_or_else(invalid_body)?;
    let password = fields["password"]
        .as_str()
        .filter(|password| password.encode_utf16().count() <= 1024)
        .ok_or_else(invalid_body)?;
    let setup_code = fields
        .get("setupCode")
        .map(|code| {
            code.as_str()
                .filter(|text| text.encode_utf16().count() <= 32)
                .ok_or_else(invalid_body)
        })
        .transpose()?;
    let mut chars = username.bytes();
    if !(3..=64).contains(&username.len())
        || !chars.next().is_some_and(|byte| byte.is_ascii_alphabetic())
        || !chars.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(error(
            400,
            "invalid_username",
            "Choose a username of 3 to 64 letters or digits.",
        ));
    }
    strength(password)?;
    let budget = if local {
        Budget::default()
    } else {
        let code = setup_code.filter(|code| !code.is_empty()).ok_or_else(|| {
            error(
                403,
                "setup_code_required",
                "Enter the setup code the server printed.",
            )
        })?;
        let budget = state.sign_in_budget(config, req)?;
        if !state
            .setup
            .as_ref()
            .is_some_and(|value| value.matches(code, req.at))
        {
            state.spend_login(&budget, true);
            return Err(budget.headers(failure_with_tries(
                403,
                "setup_code_invalid",
                "The setup code is not valid.",
                budget.tries_left(),
            )));
        }
        budget
    };
    let hash = bcrypt::hash(password, state.hash_cost).map_err(|_| StoreError)?;
    state.update_config(config, |latest| {
        if latest.configured() || latest.managed_by_env {
            return Err(already_configured());
        }
        credential_document(latest, username, &hash, req.at);
        Ok(())
    })?;
    state.setup = None;
    state.mark_credential(config);
    state.files.consume_setup()?;
    rotate_session(state, config, req, session, username, None)?;
    tracing::info!("auth.setup.completed");
    Ok(budget.headers(response(
        201,
        json!({"ok":true,"username":username,"session":session_view(session)}),
    )))
}

fn already_configured() -> Reply {
    error(
        409,
        "already_configured",
        "Dashboard sign-in is already set up.",
    )
}

fn change_password(
    state: &mut AuthRuntime,
    config: &mut Config,
    req: &RequestInfo,
    session: &mut Browser,
    device: Option<&DeviceAuth>,
) -> Outcome {
    const MESSAGE: &str = "Too many wrong passwords. Try again later.";
    secure(config, req)?;
    configured(config)?;
    if config.managed_by_env {
        return Err(managed_by_env());
    }
    browser(config, session, device)?;
    let fields = body(
        req,
        true,
        &["currentPassword", "newPassword"],
        &["signOutOtherBrowsers"],
        false,
    )?;
    let current = login_field(&fields["currentPassword"], 1024).ok_or_else(invalid_body)?;
    let new = fields["newPassword"]
        .as_str()
        .filter(|password| password.encode_utf16().count() <= 1024)
        .ok_or_else(invalid_body)?;
    let sign_out = optional_bool(fields, "signOutOtherBrowsers")? != Some(false);
    let client = state
        .password
        .check(account_key(req, config, session), req.at, 5, 900_000);
    if client.blocked() {
        return Err(limited(&client, &client, MESSAGE));
    }
    let server = state
        .password_server
        .check("server".into(), req.at, 10, 3_600_000);
    if server.blocked() {
        return Err(limited(&client, &server, MESSAGE));
    }
    let run = (|| {
        strength(new)?;
        if new == current {
            return Err(error(
                400,
                "same_password",
                "The new password must differ from the current one.",
            ));
        }
        if !valid_hash(&config.hash) {
            return Err(error(
                500,
                "invalid_hash",
                "The stored password hash is not valid.",
            ));
        }
        if !verify(current, &config.hash) {
            state.password.spend(&client);
            state.password_server.spend(&server);
            return Err(failure_with_tries(
                401,
                "wrong_password",
                "The current password is not correct.",
                client.remaining().min(server.remaining()),
            ));
        }
        let name = config.username.clone();
        let hash = bcrypt::hash(new, state.hash_cost).map_err(|_| StoreError)?;
        state.update_config(config, |latest| {
            credential_document(latest, &name, &hash, req.at);
            Ok(())
        })?;
        state.mark_credential(config);
        let mut count = 0;
        let mut rotation_failed = false;
        if sign_out {
            let others = count_others(state, req, session);
            if state.bump_epoch().is_ok() {
                count = others;
            } else {
                rotation_failed = true;
            }
        }
        let name = username(config, session).to_owned();
        let rotated = rotate_session(state, config, req, session, &name, None).is_ok();
        rotation_failed |= !rotated;
        let paired = device_count(state, req.at);
        tracing::info!(
            signed_out_browsers = count,
            paired_devices = paired,
            rotation_failed,
            "auth.password.changed"
        );
        let mut value = json!({"ok":true,"passwordChangedAt":iso(req.at),"signedOutBrowsers":count,
            "pairedDevices":paired,"session":if rotated {session_view(session)} else {Value::Null}});
        if rotation_failed {
            value["code"] = json!("session_rotation_failed");
        }
        Ok(response(200, value))
    })();
    Ok(client.headers(run.unwrap_or_else(|reply| reply)))
}

fn credential_document(config: &mut Config, username: &str, hash: &str, at: i64) {
    let old = &config.doc["dashboard_auth"];
    let mut auth = json!({
        "enabled":true,"username":username,"password_hash":hash,
        "session_timeout_hours":old.get("session_timeout_hours").and_then(Value::as_number)
            .map_or_else(|| json!(24), |value| json!(value))
    });
    if let Some(lifetime) = old
        .get("session_lifetime_days")
        .filter(|value| value.is_number())
    {
        auth["session_lifetime_days"] = lifetime.clone();
    }
    auth["password_changed_at"] = json!(iso(at));
    config.doc["dashboard_auth"] = auth;
}

fn settings(
    state: &mut AuthRuntime,
    config: &mut Config,
    req: &RequestInfo,
    session: &Browser,
    device: Option<&DeviceAuth>,
) -> Outcome {
    browser(config, session, device)?;
    if get(&req.method) {
        no_query(req)?;
    }
    if req.path == "/api/auth/session-lifetime" {
        if req.method == Method::PUT {
            let fields = body(req, true, &["days"], &[], false)?;
            let days = fields["days"]
                .as_f64()
                .and_then(|days| DAYS.into_iter().find(|valid| *valid as f64 == days))
                .ok_or_else(invalid_body)?;
            state
                .update_config(config, |latest| {
                    ensure_mapping(&mut latest.doc["dashboard_auth"]);
                    let auth = &mut latest.doc["dashboard_auth"];
                    for (key, value) in [
                        ("enabled", json!(true)),
                        ("username", json!("")),
                        ("password_hash", json!("")),
                    ] {
                        if auth[key].is_null() {
                            auth[key] = value;
                        }
                    }
                    auth["session_timeout_hours"] = json!(days * 24);
                    auth["session_lifetime_days"] = json!(days);
                    Ok(())
                })
                .map_err(|_| {
                    error(
                        500,
                        "write_failed",
                        "The session lifetime could not be saved.",
                    )
                })?;
            tracing::info!(days, "auth.session.lifetime_changed");
        }
        return Ok(response(
            200,
            json!({"days":config.days,"hours":config.days * 24,"options":DAYS}),
        ));
    }
    if req.method == Method::PUT {
        let fields = body(req, true, &["trustLocalNetwork"], &[], false)?;
        let on = fields["trustLocalNetwork"]
            .as_bool()
            .ok_or_else(invalid_body)?;
        if on && !req.connection.direct_loopback {
            return Err(error(
                403,
                "loopback_required",
                "Turn on local network trust on the dashboard computer itself, or in config.yaml.",
            ));
        }
        if on != config.network.trust {
            state
                .update_config(config, |latest| {
                    ensure_mapping(&mut latest.doc["dashboard_network"]);
                    latest.doc["dashboard_network"]["trust_local_network"] = json!(on);
                    Ok(())
                })
                .map_err(|_| error(500, "write_failed", "The setting could not be saved."))?;
            tracing::info!(trust_local_network = on, "auth.network.trust_changed");
        }
    }
    let connection = config
        .network
        .connection(&req.headers, req.connection.peer, req.tls);
    Ok(response(
        200,
        json!({"trustLocalNetwork":config.network.trust,
        "trustedNetworks":config.network.ranges.iter().map(ToString::to_string).collect::<Vec<_>>(),
        "connection":connection.description(),"canTurnOn":connection.direct_loopback}),
    ))
}

fn ensure_mapping(value: &mut Value) {
    if !value.is_object() {
        *value = json!({});
    }
}

impl From<StoreError> for Reply {
    fn from(_: StoreError) -> Self {
        service_error()
    }
}

fn count_others(state: &AuthRuntime, req: &RequestInfo, session: &Browser) -> usize {
    state.sessions.count_others(
        session.as_ref().map(|(id, _)| id.as_str()),
        state.files.epoch(),
        req.at,
    )
}

fn session_view(session: &Browser) -> Value {
    json!({"expiresAt":session.as_ref().and_then(|(_, session)| session.expires_at())})
}

fn device_count(state: &AuthRuntime, at: i64) -> usize {
    Devices::load(&state.files).map_or(0, |devices| devices.count(at))
}

fn username<'a>(config: &'a Config, session: &'a Browser) -> &'a str {
    session
        .as_ref()
        .and_then(|(_, session)| session.username.as_deref())
        .unwrap_or(&config.username)
}

fn account_key(req: &RequestInfo, config: &Config, session: &Browser) -> String {
    format!(
        "{}|{}",
        req.connection.client_key(),
        &super::storage::sha(username(config, session))[..32]
    )
}

fn no_query(req: &RequestInfo) -> Result<(), Reply> {
    if req.query.is_some() {
        Err(error(
            400,
            "unexpected_query",
            "This request does not take a query string.",
        ))
    } else {
        Ok(())
    }
}

fn body<'a>(
    req: &'a RequestInfo,
    origin_required: bool,
    required: &[&str],
    allowed: &[&str],
    optional_empty: bool,
) -> Result<&'a Value, Reply> {
    let origin = header(&req.headers, "origin");
    if (origin_required && origin.is_none_or(str::is_empty)) || !origin_allowed(&req.headers) {
        return Err(error(
            403,
            "origin_required",
            "This change requires the dashboard origin.",
        ));
    }
    no_query(req)?;
    if optional_empty && !req.has_body {
        return Ok(&req.body);
    }
    if !req.json_content || !req.has_body {
        return Err(error(
            415,
            "json_required",
            "This change requires application/json.",
        ));
    }
    let object = req.body.as_object().ok_or_else(invalid_body)?;
    if req.body_bytes > 4096
        || !required.iter().all(|key| object.contains_key(*key))
        || !object
            .keys()
            .all(|key| required.contains(&key.as_str()) || allowed.contains(&key.as_str()))
    {
        return Err(invalid_body());
    }
    Ok(&req.body)
}

fn optional_bool(fields: &Value, key: &str) -> Result<Option<bool>, Reply> {
    fields
        .get(key)
        .map(|value| value.as_bool().ok_or_else(invalid_body))
        .transpose()
}

fn login_field(value: &Value, max: usize) -> Option<&str> {
    value
        .as_str()
        .filter(|text| !text.is_empty() && text.encode_utf16().count() <= max)
}

fn valid_hash(hash: &str) -> bool {
    static FORM: LazyLock<Option<regex::Regex>> =
        LazyLock::new(|| regex::Regex::new(r"^\$2[aby]?\$[0-9]{2}\$.{53}$").ok());
    FORM.as_ref().is_some_and(|form| form.is_match(hash))
}

fn verify(password: &str, hash: &str) -> bool {
    // Native npm bcrypt accepts 2a/2b. The TS format preflight also admits 2y,
    // but bcrypt itself refuses it; Rust must never turn that refusal into a match.
    if !hash.starts_with("$2a$") && !hash.starts_with("$2b$") {
        return false;
    }
    let bytes = password.as_bytes();
    // Only modern 2b promises truncation. Oversized legacy inputs are refused.
    if hash.starts_with("$2a$") && bytes.len() > 72 {
        return false;
    }
    bcrypt::verify(&bytes[..bytes.len().min(72)], hash).unwrap_or(false)
}

fn strength(password: &str) -> Result<(), Reply> {
    let (reason, message) = if password.chars().count() < 8 {
        ("too_short", "The new password needs at least 8 characters.")
    } else if password.len() > 72 {
        ("too_long", "The new password is longer than 72 bytes.")
    } else {
        return Ok(());
    };
    Err(response(
        400,
        json!({"error":message,"code":"weak_password","reason":reason}),
    ))
}

fn managed_by_env() -> Reply {
    error(
        409,
        "managed_by_env",
        "The dashboard password is set by an environment variable on the server.",
    )
}

pub(super) fn secure(config: &Config, req: &RequestInfo) -> Result<(), Reply> {
    if req.connection.secure {
        return Ok(());
    }
    Err(response(
        403,
        json!({
            "error":"Use the secure dashboard address (HTTPS or a tunnel) for passwords, setup codes and device tokens.",
            "code":"secure_transport_required","secureOrigin":config.network.origin
        }),
    ))
}

fn configured(config: &Config) -> Result<(), Reply> {
    if config.enabled && config.configured() {
        Ok(())
    } else {
        Err(error(
            409,
            "auth_not_configured",
            "Dashboard sign-in is not set up on this server.",
        ))
    }
}

fn browser(config: &Config, session: &Browser, device: Option<&DeviceAuth>) -> Result<(), Reply> {
    configured(config)?;
    if device.is_some() {
        return Err(device_scope());
    }
    if session
        .as_ref()
        .is_some_and(|(_, session)| session.authenticated)
    {
        Ok(())
    } else {
        Err(error(401, "auth_required", "Authentication required"))
    }
}

pub(super) fn device_scope() -> Reply {
    error(
        403,
        "device_scope",
        "This action needs a signed-in dashboard session.",
    )
}

fn require_device<'a>(
    config: &Config,
    device: Option<&'a DeviceAuth>,
) -> Result<&'a DeviceAuth, Reply> {
    configured(config)?;
    device.ok_or_else(|| {
        error(
            403,
            "device_required",
            "This route is for a paired tray's own token.",
        )
    })
}

pub(super) fn device_read_error(kind: DeviceError) -> Reply {
    match kind {
        DeviceError::Invalid => error(401, "invalid_token", "The device token is not valid."),
        DeviceError::Revoked => error(
            401,
            "device_revoked",
            "This device was signed out from the dashboard.",
        ),
        DeviceError::Expired => error(
            401,
            "device_expired",
            "This device token expired after 90 days without use.",
        ),
        DeviceError::Plain => error(
            403,
            "plain_http_token",
            "This device key was used over plain HTTP on the local network, so it does not work through the HTTPS proxy. Pair the tray again through the secure address.",
        ),
        _ => error(
            503,
            "auth_store_unavailable",
            "Paired devices cannot be checked right now.",
        ),
    }
}

fn device_write_error(kind: DeviceError) -> Reply {
    match kind {
        DeviceError::Cap => error(
            409,
            "too_many_devices",
            "Twenty devices are paired. Revoke one first.",
        ),
        DeviceError::Unknown => error(404, "unknown_device", "That device is not paired."),
        DeviceError::Revoked => device_read_error(kind),
        _ => error(
            503,
            "auth_store_unavailable",
            "Paired devices cannot be saved right now.",
        ),
    }
}
