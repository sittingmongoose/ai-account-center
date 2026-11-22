//! Turns `config/tables/{providers,tools,hosts}.json` into typed constants (`$OUT_DIR/tables.rs`).
//! A table that breaks a rule below fails the build, so a typo never reaches a consumer. The
//! test-only `synthetic-table` feature appends one synthetic provider and tool to the real tables.
use serde_json::{Map, Value};
use std::{collections::BTreeSet, env, fmt::Write as _, fs, path::PathBuf};

const PROVIDER_KEYS: [&str; 9] = [
    "label",
    "longLabel",
    "dashboard",
    "card",
    "keyStore",
    "analytics",
    "page",
    "failover",
    "quota",
];

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let tables = manifest.join("../../config/tables");
    for name in ["providers.json", "tools.json", "hosts.json"] {
        println!("cargo:rerun-if-changed={}", tables.join(name).display());
    }
    let synthetic = env::var_os("CARGO_FEATURE_SYNTHETIC_TABLE").is_some();
    let mut providers = read(&tables.join("providers.json"));
    let mut tools = read(&tables.join("tools.json"));
    let hosts = read(&tables.join("hosts.json"));
    if synthetic {
        add_synthetic(&mut providers, &mut tools);
    }
    let mut out = String::new();
    write_providers(&mut out, &providers);
    write_provider_views(&mut out, &providers);
    write_tools(&mut out, &tools);
    write_tool_views(&mut out, &tools, &providers);
    write_hosts(&mut out, &hosts);
    writeln!(
        out,
        "/// True only in the test-only `synthetic-table` build (one synthetic provider and tool).\n\
         pub const SYNTHETIC: bool = {synthetic};"
    )
    .expect("write");
    let path = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR")).join("tables.rs");
    fs::write(path, out).expect("write tables.rs");
}

fn read(path: &std::path::Path) -> Map<String, Value> {
    let text = fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let value: Value =
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let Value::Object(map) = value else {
        panic!("{}: not an object", path.display());
    };
    assert_eq!(
        map.get("version"),
        Some(&Value::from(1)),
        "{}: version",
        path.display()
    );
    map
}

/// The synthetic rows: every flag on, so each consumer meets it.
fn add_synthetic(providers: &mut Map<String, Value>, tools: &mut Map<String, Value>) {
    let rows = providers["providers"].as_object_mut().expect("providers");
    let order = rows
        .values()
        .filter_map(|row| row["analytics"]["order"].as_u64())
        .max()
        .map_or(0, |max| max + 1);
    rows.insert(
        "zz-synthetic".into(),
        serde_json::json!({
            "label": "Synthetic", "longLabel": "Synthetic provider", "dashboard": true,
            "card": true, "keyStore": true, "analytics": {"order": order, "label": "Synthetic"},
            "page": {"signIn": "API key", "multi": true},
            "failover": {"label": "Synthetic", "kinds": ["zz-synthetic-credential"]},
            "quota": true
        }),
    );
    tools["analyticsTools"]
        .as_object_mut()
        .expect("analyticsTools")
        .insert(
            "zz-tool".into(),
            serde_json::json!({"label": "Synthetic tool", "experiment": true, "extraRoots": true}),
        );
    tools["usageLogTools"]
        .as_object_mut()
        .expect("usageLogTools")
        .insert(
            "zz-log".into(),
            serde_json::json!({"label": "Synthetic log"}),
        );
}

/// Lowercase words joined by single hyphens: never an integer key, so JS keeps file order.
fn id_ok(id: &str) -> bool {
    !id.is_empty()
        && id.split('-').all(|word| {
            word.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
                && word
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        })
}

fn variant(id: &str) -> String {
    id.split('-')
        .map(|word| {
            let mut chars = word.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_ascii_uppercase().to_string() + chars.as_str()
            })
        })
        .collect()
}

fn text<'a>(row: &'a Value, key: &str, at: &str) -> &'a str {
    row[key]
        .as_str()
        .unwrap_or_else(|| panic!("{at}.{key}: not a string"))
}

fn flag(row: &Value, key: &str, at: &str) -> bool {
    row[key]
        .as_bool()
        .unwrap_or_else(|| panic!("{at}.{key}: not a boolean"))
}

fn rows<'a>(table: &'a Map<String, Value>, key: &str) -> &'a Map<String, Value> {
    let rows = table[key]
        .as_object()
        .unwrap_or_else(|| panic!("{key}: not an object"));
    for (id, row) in rows {
        // Field-mapping keys are JSON member names (camelCase); every other key is an id.
        let ok = if key == "usageLogFieldMapping" {
            id.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
                && id.bytes().all(|b| b.is_ascii_alphanumeric())
        } else {
            id_ok(id)
        };
        assert!(ok, "{key}.{id}: ids are lowercase words joined by '-'");
        assert!(row.is_object(), "{key}.{id}: not an object");
    }
    rows
}

fn write_providers(out: &mut String, table: &Map<String, Value>) {
    let retry = &table["planRetryAfter"];
    let seconds = |key: &str| {
        retry[key]
            .as_u64()
            .unwrap_or_else(|| panic!("planRetryAfter.{key}: not a count"))
    };
    let (floor, ceiling, default) = (seconds("floorS"), seconds("ceilingS"), seconds("defaultS"));
    assert!(
        floor <= default && default <= ceiling,
        "planRetryAfter: floor <= default <= ceiling"
    );
    let rows = rows(table, "providers");
    let mut orders = BTreeSet::new();
    let mut entries = String::new();
    for (index, (id, row)) in rows.iter().enumerate() {
        let at = format!("providers.{id}");
        let keys: BTreeSet<&str> = row
            .as_object()
            .expect("row")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, PROVIDER_KEYS.into_iter().collect(), "{at}: keys");
        let dashboard = flag(row, "dashboard", &at);
        let (card, quota) = (flag(row, "card", &at), flag(row, "quota", &at));
        assert!(
            dashboard || !(card || quota),
            "{at}: card and quota rows are dashboard rows"
        );
        assert_eq!(
            dashboard,
            !row["page"].is_null(),
            "{at}: page iff dashboard"
        );
        assert_eq!(
            dashboard,
            !row["analytics"].is_null(),
            "{at}: analytics iff dashboard"
        );
        let analytics = if row["analytics"].is_null() {
            "None".to_owned()
        } else {
            let a = &row["analytics"];
            let order = a["order"]
                .as_u64()
                .unwrap_or_else(|| panic!("{at}.analytics.order"));
            assert!(
                orders.insert(order),
                "{at}: analytics.order {order} repeats"
            );
            format!(
                "Some(Analytics {{ order: {order}, label: {:?} }})",
                text(a, "label", &at)
            )
        };
        let page = if row["page"].is_null() {
            "None".to_owned()
        } else {
            let p = &row["page"];
            format!(
                "Some(Page {{ sign_in: {:?}, multi: {} }})",
                text(p, "signIn", &at),
                flag(p, "multi", &at)
            )
        };
        let failover = &row["failover"];
        let kinds: Vec<String> = failover["kinds"]
            .as_array()
            .unwrap_or_else(|| panic!("{at}.failover.kinds"))
            .iter()
            .map(|k| {
                format!(
                    "{:?}",
                    k.as_str().unwrap_or_else(|| panic!("{at}.failover.kinds"))
                )
            })
            .collect();
        assert!(!kinds.is_empty(), "{at}: failover.kinds is empty");
        writeln!(
            entries,
            "    Provider {{\n        id: ProviderId::{v},\n        index: {index},\n        label: {label:?},\n        \
             long_label: {long:?},\n        dashboard: {dashboard},\n        card: {card},\n        \
             key_store: {key},\n        analytics: {analytics},\n        page: {page},\n        \
             failover: Failover {{ label: {flabel:?}, kinds: &[{kinds}] }},\n        quota: {quota},\n    }},",
            v = variant(id),
            label = text(row, "label", &at),
            long = text(row, "longLabel", &at),
            key = flag(row, "keyStore", &at),
            flabel = text(failover, "label", &at),
            kinds = kinds.join(", "),
        )
        .expect("write");
    }
    let expected: BTreeSet<u64> = (0..orders.len() as u64).collect();
    assert_eq!(
        orders, expected,
        "providers: analytics.order runs 0, 1, 2, ... without gaps"
    );
    let count = rows.len();
    let variants: String = rows
        .keys()
        .map(|id| format!("    /// `{id}`.\n    {},\n", variant(id)))
        .collect();
    let all: Vec<String> = rows
        .keys()
        .map(|id| format!("ProviderId::{}", variant(id)))
        .collect();
    let names: String = rows
        .keys()
        .map(|id| format!("            ProviderId::{} => {id:?},\n", variant(id)))
        .collect();
    let parses: String = rows
        .keys()
        .map(|id| format!("            {id:?} => Some(ProviderId::{}),\n", variant(id)))
        .collect();
    write!(
        out,
        "/// A provider id in config/tables/providers.json, in table order.\n\
         #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]\n\
         pub enum ProviderId {{\n{variants}}}\n\n\
         impl ProviderId {{\n    \
         /// Every provider, in table order.\n    \
         pub const ALL: [ProviderId; {count}] = [{all}];\n    \
         /// The id as the table spells it.\n    \
         pub const fn as_str(self) -> &'static str {{\n        match self {{\n{names}        }}\n    }}\n    \
         /// The provider an id names, or `None` for an id the table does not list.\n    \
         pub fn parse(id: &str) -> Option<ProviderId> {{\n        match id {{\n{parses}            _ => None,\n        }}\n    }}\n}}\n\n\
         /// Every provider row, in table order (`PROVIDERS[i].index == i`).\n\
         pub static PROVIDERS: [Provider; {count}] = [\n{entries}];\n\n\
         /// Plan quota's Retry-After policy (`planRetryAfter`): Python `plan_common.retry_after`.\n\
         pub const PLAN_RETRY_AFTER: RetryAfter = RetryAfter {{ floor_s: {floor}, ceiling_s: {ceiling}, default_s: {default} }};\n\n",
        all = all.join(", "),
    )
    .expect("write");
}

// ------------------------------------------------------- consumer views

/// One ordered id list of a table: names rows it lists, never repeats one.
fn id_list(table: &Map<String, Value>, key: &str, known: &Map<String, Value>) -> Vec<String> {
    let list = table[key]
        .as_array()
        .unwrap_or_else(|| panic!("{key}: not an array"));
    assert!(!list.is_empty(), "{key}: empty");
    let mut seen = BTreeSet::new();
    let mut ids = Vec::new();
    for value in list {
        let id = value
            .as_str()
            .unwrap_or_else(|| panic!("{key}: not a string"));
        assert!(known.contains_key(id), "{key}: {id} is not a listed row");
        assert!(seen.insert(id), "{key}: {id} repeats");
        ids.push(id.to_owned());
    }
    ids
}

/// `[(&str, &str)]` pairs of a JSON object, in file order.
fn pair_literals(map: &Map<String, Value>) -> String {
    map.iter()
        .map(|(key, value)| {
            format!(
                "({key:?}, {:?})",
                value
                    .as_str()
                    .unwrap_or_else(|| panic!("{key}: not a string"))
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// `providers.json`'s consumer views: provider id orders and the route and pricing maps.
fn write_provider_views(out: &mut String, table: &Map<String, Value>) {
    let rows = table["providers"].as_object().expect("providers");
    for (key, name, doc) in [
        (
            "additionalProviders",
            "ADDITIONAL_PROVIDERS",
            "The providers whose accounts are standalone usage sources (`additionalProviders`), in their declared order.",
        ),
        (
            "snapshotOrder",
            "SNAPSHOT_PROVIDERS",
            "The provider order the agent usage answer publishes (`snapshotOrder`).",
        ),
        (
            "signInSessions",
            "SIGN_IN_SESSIONS",
            "The providers the interactive sign-in session verb admits (`signInSessions`).",
        ),
    ] {
        let ids = id_list(table, key, rows);
        let items: Vec<String> = ids
            .iter()
            .map(|id| format!("ProviderId::{}", variant(id)))
            .collect();
        writeln!(
            out,
            "/// {doc}\npub static {name}: [ProviderId; {}] = [{}];\n",
            ids.len(),
            items.join(", ")
        )
        .expect("write");
    }
    let routes = table["usageRoutes"]
        .as_object()
        .expect("usageRoutes: not an object");
    for (route, provider) in routes {
        assert!(id_ok(route), "usageRoutes.{route}: not a route word");
        let provider = provider
            .as_str()
            .unwrap_or_else(|| panic!("usageRoutes.{route}: not a string"));
        assert!(
            rows.contains_key(provider),
            "usageRoutes.{route}: {provider} is not a provider"
        );
    }
    writeln!(
        out,
        "/// `usageRoutes`: the dashboard provider a usage route word names, in table order.\n\
         pub static USAGE_ROUTES: [(&str, &str); {}] = [{}];\n",
        routes.len(),
        pair_literals(routes)
    )
    .expect("write");
    let aliases = table["pricingAliases"]
        .as_object()
        .expect("pricingAliases: not an object");
    for (alias, models_dev) in aliases {
        assert!(id_ok(alias), "pricingAliases.{alias}: not a provider word");
        let models_dev = models_dev
            .as_str()
            .unwrap_or_else(|| panic!("pricingAliases.{alias}: not a string"));
        assert!(
            id_ok(models_dev),
            "pricingAliases.{alias}: {models_dev} is not a models.dev id"
        );
    }
    writeln!(
        out,
        "/// `pricingAliases`: the models.dev provider a pricing provider word names, in table order.\n\
         pub static PRICING_ALIASES: [(&str, &str); {}] = [{}];\n",
        aliases.len(),
        pair_literals(aliases)
    )
    .expect("write");
}

/// `tools.json`'s consumer views: the activity order, CLI targets, tool providers, the app
/// inventory and each tool's legacy cache rebuild version.
fn write_tool_views(out: &mut String, table: &Map<String, Value>, providers: &Map<String, Value>) {
    let rows = providers["providers"].as_object().expect("providers");
    let analytics = table["analyticsTools"].as_object().expect("analyticsTools");
    let scanner = table["scannerOnlyKinds"]
        .as_object()
        .expect("scannerOnlyKinds");
    let logs = table["usageLogTools"].as_object().expect("usageLogTools");
    let generic = table["genericUsageLogTool"]
        .as_str()
        .expect("genericUsageLogTool: not a string");
    assert!(
        logs.contains_key(generic),
        "genericUsageLogTool: {generic} is not a usage-log tool"
    );
    let sources = word_list(table, "activitySources");
    for id in &sources {
        assert!(
            analytics.contains_key(id) || logs.contains_key(id),
            "activitySources.{id}: not an analytics or usage-log tool"
        );
    }
    let targets = word_list(table, "cliTargets");
    let tool_providers = table["toolProviders"]
        .as_object()
        .expect("toolProviders: not an object");
    for (tool, provider) in tool_providers {
        assert!(
            analytics.contains_key(tool) || scanner.contains_key(tool),
            "toolProviders.{tool}: not a scanner kind"
        );
        let provider = provider
            .as_str()
            .unwrap_or_else(|| panic!("toolProviders.{tool}: not a string"));
        assert!(
            rows.contains_key(provider),
            "toolProviders.{tool}: {provider} is not a provider"
        );
    }
    let apps = word_list(table, "appInventory");
    let rebuilds = table["legacyRebuildThrough"]
        .as_object()
        .expect("legacyRebuildThrough: not an object");
    for (tool, version) in rebuilds {
        assert!(
            analytics.contains_key(tool),
            "legacyRebuildThrough.{tool}: not an analytics tool"
        );
        let version = version
            .as_u64()
            .unwrap_or_else(|| panic!("legacyRebuildThrough.{tool}: not a count"));
        assert!(
            (3..=8).contains(&version),
            "legacyRebuildThrough.{tool}: {version} is not a legacy cache version"
        );
    }
    let strings = |ids: &[String]| -> String {
        ids.iter()
            .map(|id| format!("{id:?}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    writeln!(
        out,
        "/// `genericUsageLogTool`: the generic JSONL format's tool id, whose roots are its own.\n\
         pub static GENERIC_USAGE_LOG_TOOL: &str = {generic:?};\n\n\
         /// `activitySources`: the tools the activity answer's range reads, in its order.\n\
         pub static ACTIVITY_SOURCES: [&str; {}] = [{}];\n\n\
         /// `cliTargets`: the session targets the activity answer accepts, in its order.\n\
         pub static CLI_TARGETS: [&str; {}] = [{}];\n\n\
         /// `toolProviders`: the tools whose logs are one provider's usage by construction.\n\
         pub static TOOL_PROVIDERS: [(&str, &str); {}] = [{}];\n\n\
         /// `appInventory`: the applications native app discovery reports, in its order.\n\
         pub static APP_INVENTORY: [&str; {}] = [{}];\n\n\
         /// `legacyRebuildThrough`: the last legacy cache version whose rows for a tool must be\n\
         /// rebuilt (an unlisted tool has no legacy rows).\n\
         pub static LEGACY_REBUILD_THROUGH: [(&str, u32); {}] = [{}];\n",
        sources.len(),
        strings(&sources),
        targets.len(),
        strings(&targets),
        tool_providers.len(),
        pair_literals(tool_providers),
        apps.len(),
        strings(&apps),
        rebuilds.len(),
        rebuilds
            .iter()
            .map(|(tool, version)| format!("({tool:?}, {})", version.as_u64().expect("version")))
            .collect::<Vec<_>>()
            .join(", ")
    )
    .expect("write");
}

/// A list of lowercase word ids that names no table row of its own (`cliTargets`, `appInventory`).
fn word_list(table: &Map<String, Value>, key: &str) -> Vec<String> {
    let list = table[key]
        .as_array()
        .unwrap_or_else(|| panic!("{key}: not an array"));
    assert!(!list.is_empty(), "{key}: empty");
    let mut seen = BTreeSet::new();
    let mut ids = Vec::new();
    for value in list {
        let id = value
            .as_str()
            .unwrap_or_else(|| panic!("{key}: not a string"));
        assert!(id_ok(id), "{key}.{id}: not lowercase words joined by '-'");
        assert!(seen.insert(id), "{key}.{id}: repeats");
        ids.push(id.to_owned());
    }
    ids
}

fn write_tools(out: &mut String, table: &Map<String, Value>) {
    let analytics = rows(table, "analyticsTools");
    let scanner = rows(table, "scannerOnlyKinds");
    let logs = rows(table, "usageLogTools");
    let mapping = rows(table, "usageLogFieldMapping");
    for id in scanner.keys() {
        assert!(
            !analytics.contains_key(id),
            "scannerOnlyKinds.{id}: also an analytics tool"
        );
    }
    let mut tools = String::new();
    for (id, row) in analytics {
        let at = format!("analyticsTools.{id}");
        writeln!(
            tools,
            "    AnalyticsTool {{ id: {id:?}, label: {:?}, experiment: {}, extra_roots: {} }},",
            text(row, "label", &at),
            flag(row, "experiment", &at),
            flag(row, "extraRoots", &at)
        )
        .expect("write");
    }
    let labelled = |rows: &Map<String, Value>, key: &str| -> String {
        rows.iter()
            .map(|(id, row)| {
                format!(
                    "    Labelled {{ id: {id:?}, label: {:?} }},\n",
                    text(row, "label", &format!("{key}.{id}"))
                )
            })
            .collect()
    };
    let keys: String = mapping
        .iter()
        .map(|(name, row)| {
            format!(
                "    MappingKey {{ name: {name:?}, required: {} }},\n",
                flag(row, "required", &format!("usageLogFieldMapping.{name}"))
            )
        })
        .collect();
    write!(
        out,
        "/// `analyticsTools`: the tools Analytics reads, in table order.\n\
         pub static ANALYTICS_TOOLS: [AnalyticsTool; {}] = [\n{tools}];\n\n\
         /// `scannerOnlyKinds`: kinds only the usage scanner reads, after the analytics tools.\n\
         pub static SCANNER_ONLY_KINDS: [Labelled; {}] = [\n{}];\n\n\
         /// `usageLogTools`: the tools an extra usage-log source may name, in table order.\n\
         pub static USAGE_LOG_TOOLS: [Labelled; {}] = [\n{}];\n\n\
         /// `usageLogFieldMapping`: the keys a generic JSONL field mapping may name.\n\
         pub static USAGE_LOG_FIELD_MAPPING: [MappingKey; {}] = [\n{keys}];\n\n",
        analytics.len(),
        scanner.len(),
        labelled(scanner, "scannerOnlyKinds"),
        logs.len(),
        labelled(logs, "usageLogTools"),
        mapping.len(),
    )
    .expect("write");
}

fn write_hosts(out: &mut String, table: &Map<String, Value>) {
    let hosts: Vec<String> = table["legacyStorageHosts"]
        .as_array()
        .expect("legacyStorageHosts")
        .iter()
        .map(|h| format!("{:?}", h.as_str().expect("legacyStorageHosts: strings")))
        .collect();
    write!(
        out,
        "/// `legacyStorageHosts`: the frozen storage allowlist (TS `LEGACY_STORAGE_HOSTS`).\n\
         pub static LEGACY_STORAGE_HOSTS: [&str; {}] = [{}];\n\n",
        hosts.len(),
        hosts.join(", ")
    )
    .expect("write");
}
