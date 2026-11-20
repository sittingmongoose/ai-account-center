use super::policy::{Connection, Transport};
use super::storage::{
    DAY, Files, StoreError, hex, is_hex, iso, random, same, sha, timestamp, token,
};
use serde_json::{Value, json};
use std::sync::LazyLock;

const CAP: usize = 20;

pub(super) fn id_valid(value: &str) -> bool {
    value
        .strip_prefix("dev_")
        .is_some_and(|id| is_hex(id, 16) && id == id.to_ascii_lowercase())
}

pub(super) fn token_valid(value: &str) -> bool {
    value.strip_prefix("aacd_").is_some_and(|token| {
        token.len() == 43
            && token
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    })
}

fn hash_valid(value: &Value) -> bool {
    value
        .as_str()
        .is_some_and(|hash| is_hex(hash, 64) && hash == hash.to_ascii_lowercase())
}

fn active(value: &Value) -> bool {
    value["revokedAt"].is_null()
}

fn idle(value: &Value) -> Option<i64> {
    timestamp(
        value["lastSeenAt"]
            .as_str()
            .or_else(|| value["pairedAt"].as_str())?,
    )
    .map(|time| time + 90 * DAY)
}

fn active_at(value: &Value, at: i64) -> bool {
    active(value) && idle(value).is_some_and(|expiry| at <= expiry)
}

fn rotate_at(value: &Value) -> Option<String> {
    let date = value["rotatedAt"]
        .as_str()
        .or_else(|| value["pairedAt"].as_str())?;
    iso(timestamp(date)? + 30 * DAY)
}

fn previous_live(value: &Value, at: i64) -> bool {
    value["prevTokenValidUntil"]
        .as_str()
        .and_then(timestamp)
        .is_some_and(|expiry| at < expiry)
}

fn clear_previous(value: &mut Value) {
    value["prevTokenSha256"] = Value::Null;
    value["prevTokenPlain"] = Value::Bool(true);
    value["prevTokenValidUntil"] = Value::Null;
}

fn revoked(value: &Value, time: i64, reason: &str) -> Value {
    json!({
        "id": value["id"], "revokedAt": iso(time), "revokedReason": reason,
        "tokenSha256": value["tokenSha256"], "prevTokenSha256": value["prevTokenSha256"]
    })
}

fn parse_record(mut value: Value) -> Option<Value> {
    if !id_valid(value["id"].as_str()?) {
        return None;
    }
    if value["revokedAt"].is_string() {
        for key in ["tokenSha256", "prevTokenSha256"] {
            if !hash_valid(&value[key]) {
                value[key] = Value::Null;
            }
        }
        if !value["revokedReason"].is_string() {
            value["revokedReason"] = json!("dashboard");
        }
        return Some(value);
    }
    if !value["name"].is_string()
        || !matches!(value["platform"].as_str(), Some("mac" | "windows"))
        || !hash_valid(&value["tokenSha256"])
        || !value["pairedAt"].is_string()
        || [
            "installId",
            "appVersion",
            "rotatedAt",
            "lastSeenAt",
            "lastSeenAddress",
            "prevTokenValidUntil",
        ]
        .iter()
        .any(|key| !value[key].is_null() && !value[key].is_string())
    {
        return None;
    }
    if !hash_valid(&value["prevTokenSha256"]) {
        value["prevTokenSha256"] = Value::Null;
    }
    value["revokedAt"] = Value::Null;
    value["revokedReason"] = Value::Null;
    Some(value)
}

pub(super) struct Pair<'a> {
    pub(super) name: &'a str,
    pub(super) platform: &'a str,
    pub(super) install_id: Option<String>,
    pub(super) app_version: Option<&'a str>,
}

impl<'a> Pair<'a> {
    pub(super) fn parse(value: &'a Value) -> Option<Self> {
        static OTHER: LazyLock<Option<regex::Regex>> =
            LazyLock::new(|| regex::Regex::new(r"\p{C}").ok());
        let name = value["deviceName"].as_str()?.trim();
        if !(1..=64).contains(&name.chars().count()) || OTHER.as_ref()?.is_match(name) {
            return None;
        }
        let platform = value["platform"]
            .as_str()
            .filter(|text| matches!(*text, "mac" | "windows"))?;
        let install_id = if let Some(raw) = value.get("installId") {
            let text = raw.as_str()?;
            let parts: Vec<_> = text.split('-').collect();
            if parts.len() != 5
                || !parts
                    .iter()
                    .zip([8, 4, 4, 4, 12])
                    .all(|(part, len)| is_hex(part, len))
            {
                return None;
            }
            Some(text.to_ascii_lowercase())
        } else {
            None
        };
        let app_version = if let Some(raw) = value.get("appVersion") {
            let text = raw.as_str()?;
            if !(1..=32).contains(&text.len())
                || !text.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
            {
                return None;
            }
            Some(text)
        } else {
            None
        };
        Some(Self {
            name,
            platform,
            install_id,
            app_version,
        })
    }
}

pub(super) struct Devices(Vec<Value>);

pub(super) struct DeviceAuth {
    pub(super) id: String,
    pub(super) platform: String,
    previous: bool,
    hash: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DeviceError {
    Invalid,
    Revoked,
    Expired,
    Plain,
    Store,
    Cap,
    Unknown,
}

impl DeviceError {
    pub(super) fn code(self) -> &'static str {
        match self {
            Self::Invalid => "invalid_token",
            Self::Revoked => "device_revoked",
            Self::Expired => "device_expired",
            Self::Plain => "plain_http_token",
            Self::Store => "auth_store_unavailable",
            Self::Cap => "too_many_devices",
            Self::Unknown => "unknown_device",
        }
    }
}

impl From<StoreError> for DeviceError {
    fn from(_: StoreError) -> Self {
        Self::Store
    }
}

impl Devices {
    pub(super) fn load(files: &Files) -> Result<Self, DeviceError> {
        let Some(doc) = files.devices()? else {
            return Ok(Self(Vec::new()));
        };
        let entries = doc["devices"]
            .as_array()
            .filter(|_| doc["version"] == 1)
            .ok_or(DeviceError::Store)?;
        entries
            .iter()
            .cloned()
            .map(parse_record)
            .collect::<Option<Vec<_>>>()
            .map(Self)
            .ok_or(DeviceError::Store)
    }

    fn save(mut self, files: &Files, at: i64) -> Result<(), DeviceError> {
        for value in &mut self.0 {
            if active(value) {
                if let Some(expiry) = idle(value).filter(|expiry| at > *expiry) {
                    *value = revoked(value, expiry, "expired");
                } else if !previous_live(value, at) {
                    clear_previous(value);
                }
            }
        }
        self.0.retain(|value| {
            value["revokedAt"]
                .as_str()
                .and_then(timestamp)
                .is_none_or(|revoked_at| at - revoked_at <= 30 * DAY)
        });
        files.save_devices(&json!({"version":1,"devices":self.0}))?;
        Ok(())
    }

    pub(super) fn count(&self, at: i64) -> usize {
        self.0.iter().filter(|row| active_at(row, at)).count()
    }

    pub(super) fn list(&self, at: i64) -> Value {
        Value::Array(self.0.iter().filter(|row| active_at(row, at)).map(|row| json!({
            "id":row["id"],"name":row["name"],"platform":row["platform"],"appVersion":row["appVersion"],
            "pairedAt":row["pairedAt"],"lastSeenAt":row["lastSeenAt"],"lastSeenAddress":row["lastSeenAddress"],
            "rotatedAt":row["rotatedAt"],"idleExpiresAt":idle(row).and_then(iso)
        })).collect())
    }

    pub(super) fn pair(
        mut self,
        files: &Files,
        request: Pair<'_>,
        connection: &Connection,
        at: i64,
    ) -> Result<Value, DeviceError> {
        let replacing = request.install_id.as_ref().and_then(|id| {
            self.0
                .iter()
                .position(|row| active_at(row, at) && row["installId"].as_str() == Some(id))
        });
        if self.count(at) - usize::from(replacing.is_some()) >= CAP {
            return Err(DeviceError::Cap);
        }
        let replaced = replacing.map(|index| {
            let id = self.0[index]["id"].clone();
            self.0[index] = revoked(&self.0[index], at, "replaced");
            id
        });
        let id = format!("dev_{}", hex(&random::<8>()?));
        let token = token()?;
        let paired_at = iso(at);
        self.0.push(json!({
            "id":id,"name":request.name,"platform":request.platform,"installId":request.install_id,
            "appVersion":request.app_version,"tokenSha256":sha(&token),"tokenPlain":connection.transport == Transport::Plain,
            "prevTokenSha256":null,"prevTokenPlain":true,"prevTokenValidUntil":null,"pairedAt":paired_at,
            "rotatedAt":null,"lastSeenAt":null,"lastSeenAddress":connection.client_address(),
            "revokedAt":null,"revokedReason":null
        }));
        self.save(files, at)?;
        if let Some(replaced) = replaced {
            tracing::info!(device_id = %replaced, by = "self", "auth.device.revoked");
        }
        tracing::info!(device_id = %id, platform = request.platform, "auth.device.paired");
        Ok(
            json!({"deviceId":id,"token":token,"name":request.name,"platform":request.platform,
            "pairedAt":paired_at,"rotateAfter":iso(at + 30 * DAY)}),
        )
    }

    pub(super) fn authenticate(
        mut self,
        files: &Files,
        token: &str,
        connection: &Connection,
        at: i64,
    ) -> Result<DeviceAuth, DeviceError> {
        let hash = sha(token);
        let mut matched = None;
        for (index, row) in self.0.iter().enumerate() {
            if row["tokenSha256"]
                .as_str()
                .is_some_and(|own| same(own, &hash))
            {
                matched = Some((index, false));
            } else if row["prevTokenSha256"]
                .as_str()
                .is_some_and(|own| same(own, &hash))
            {
                matched = Some((index, true));
            }
        }
        let (index, previous) = matched.ok_or(DeviceError::Invalid)?;
        let row = &mut self.0[index];
        if !active(row) {
            return Err(if row["revokedReason"] == "expired" {
                DeviceError::Expired
            } else {
                DeviceError::Revoked
            });
        }
        if !active_at(row, at) {
            return Err(DeviceError::Expired);
        }
        if previous && !previous_live(row, at) {
            return Err(DeviceError::Invalid);
        }

        let plain_key = if previous {
            "prevTokenPlain"
        } else {
            "tokenPlain"
        };
        let was_plain = row[plain_key] != false;
        let plain = connection.transport == Transport::Plain;
        let mut changed = plain && !was_plain;
        if changed {
            row[plain_key] = json!(true);
        }
        if connection.lan_proxy && (plain || was_plain) {
            if changed {
                self.save(files, at)?;
            }
            return Err(DeviceError::Plain);
        }
        if !previous && !row["prevTokenSha256"].is_null() {
            clear_previous(row);
            changed = true;
        }
        if row["lastSeenAt"]
            .as_str()
            .and_then(timestamp)
            .is_none_or(|last| at - last >= 60_000)
        {
            row["lastSeenAt"] = json!(iso(at));
            row["lastSeenAddress"] = json!(connection.client_address());
            changed = true;
        }
        let result = DeviceAuth {
            id: row["id"].as_str().unwrap_or_default().into(),
            platform: row["platform"].as_str().unwrap_or_default().into(),
            previous,
            hash,
        };
        // Plain provenance and the end of a rotation grace period must survive a restart.
        if changed {
            self.save(files, at)?;
        }
        Ok(result)
    }

    pub(super) fn me(&self, device: &DeviceAuth, at: i64) -> Result<Value, DeviceError> {
        let row = self
            .0
            .iter()
            .find(|row| row["id"] == device.id && active_at(row, at))
            .ok_or(DeviceError::Revoked)?;
        Ok(json!({
            "id":row["id"],"name":row["name"],"platform":row["platform"],
            "pairedAt":row["pairedAt"],"rotateAfter":if device.previous { iso(at) } else { rotate_at(row) },
            "idleExpiresAt":idle(row).and_then(iso)
        }))
    }

    pub(super) fn rotate(
        mut self,
        files: &Files,
        device: &DeviceAuth,
        connection: &Connection,
        at: i64,
    ) -> Result<Value, DeviceError> {
        let row = self
            .0
            .iter_mut()
            .find(|row| row["id"] == device.id && active_at(row, at))
            .ok_or(DeviceError::Revoked)?;
        let token = token()?;
        let plain = connection.transport == Transport::Plain;
        let present_plain = row[if device.previous {
            "prevTokenPlain"
        } else {
            "tokenPlain"
        }] != false;
        row["prevTokenSha256"] = json!(device.hash);
        row["prevTokenPlain"] = json!(plain || present_plain);
        row["prevTokenValidUntil"] = json!(iso(at + DAY));
        row["tokenSha256"] = json!(sha(&token));
        row["tokenPlain"] = json!(plain);
        row["rotatedAt"] = json!(iso(at));
        self.save(files, at)?;
        tracing::info!(device_id = %device.id, "auth.device.rotated");
        Ok(json!({"token":token,"rotateAfter":iso(at + 30 * DAY)}))
    }

    pub(super) fn revoke(
        mut self,
        files: &Files,
        target: &str,
        reason: &str,
        at: i64,
    ) -> Result<(), DeviceError> {
        let row = self
            .0
            .iter_mut()
            .find(|row| row["id"] == target && active_at(row, at))
            .ok_or(DeviceError::Unknown)?;
        *row = revoked(row, at, reason);
        self.save(files, at)?;
        tracing::info!(device_id = target, by = reason, "auth.device.revoked");
        Ok(())
    }

    pub(super) fn revoke_all(mut self, files: &Files, at: i64) -> Result<usize, DeviceError> {
        let mut ids = Vec::new();
        for row in self.0.iter_mut().filter(|row| active(row)) {
            ids.push(row["id"].clone());
            *row = revoked(row, at, "revoke-all");
        }
        self.save(files, at)?;
        for id in &ids {
            tracing::info!(device_id = %id, by = "revoke-all", "auth.device.revoked");
        }
        Ok(ids.len())
    }
}
