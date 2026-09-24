//! Selected nft rule transaction. Shared table/chains and every peer rule are
//! retained; a private write-ahead record supports failure/lost-ACK recovery.
use super::*;
use crate::operations::committed_uci::{read_file, Directory, Identity};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::Write;

const MAGIC: &[u8] = b"cake-autorate selected classifier v1\n";
const MAX_JOURNAL: u64 = 4 * MAX_COMMAND_OUTPUT as u64 + MAX_MANIFEST_BYTES as u64;
const MAX_KERNEL_OBJECTS: usize = 8192;

#[derive(Clone)]
struct KernelSnapshot {
    objects: Vec<Value>,
    digest: String,
    raw: Vec<u8>,
}
impl KernelSnapshot {
    fn parse(raw: &[u8]) -> Result<Self, String> {
        if raw.len() > MAX_COMMAND_OUTPUT {
            return Err("selected classifier snapshot exceeds bound".into());
        }
        let value: Value =
            serde_json::from_slice(raw).map_err(|_| "selected classifier snapshot is not JSON")?;
        let objects = value
            .get("nftables")
            .and_then(Value::as_array)
            .filter(|objects| objects.len() <= MAX_KERNEL_OBJECTS)
            .ok_or("selected classifier snapshot is invalid")?;
        let mut kept = Vec::with_capacity(objects.len());
        let mut table = false;
        let mut chains = BTreeSet::new();
        let mut handles = BTreeSet::new();
        for object in objects {
            let entry = object
                .as_object()
                .filter(|entry| entry.len() == 1)
                .ok_or("selected classifier object is invalid")?;
            let (kind, item) = entry
                .iter()
                .next()
                .ok_or("selected classifier object is empty")?;
            if kind == "metainfo" {
                continue;
            }
            if item.get("family").and_then(Value::as_str) != Some(TABLE_FAMILY) {
                return Err("selected classifier family changed".into());
            }
            match kind.as_str() {
                "table" => {
                    if table || item.get("name").and_then(Value::as_str) != Some(TABLE_NAME) {
                        return Err("selected classifier table changed".into());
                    }
                    table = true;
                }
                "chain" => {
                    let name = item
                        .get("name")
                        .and_then(Value::as_str)
                        .ok_or("selected classifier chain missing")?;
                    if item.get("table").and_then(Value::as_str) != Some(TABLE_NAME)
                        || !matches!(name, "forward" | "output")
                        || !chains.insert(name.to_string())
                        || item.get("hook").and_then(Value::as_str) != Some(name)
                        || item.get("type").and_then(Value::as_str)
                            != Some(if name == "forward" { "filter" } else { "route" })
                        || item.get("prio").and_then(Value::as_i64) != Some(-140)
                        || item.get("policy").and_then(Value::as_str) != Some("accept")
                    {
                        return Err("selected classifier base chain changed".into());
                    }
                }
                "rule" => {
                    rule_target(item)?;
                    let handle = rule_handle(item)?;
                    let chain = item
                        .get("chain")
                        .and_then(Value::as_str)
                        .ok_or("selected classifier rule chain missing")?;
                    if !handles.insert((chain.to_string(), handle)) {
                        return Err("selected classifier duplicate rule handle".into());
                    }
                }
                _ => return Err("selected classifier contains unsupported objects".into()),
            }
            kept.push(object.clone());
        }
        if !table || chains.len() != 2 {
            return Err("selected classifier container is incomplete".into());
        }
        let mut canonical = raw.to_vec();
        while canonical.last() == Some(&b'\n') {
            canonical.pop();
        }
        canonical.push(b'\n');
        Ok(Self {
            objects: kept,
            digest: hex_digest(&canonical),
            raw: canonical,
        })
    }
    fn rules(&self, target: &str) -> Result<Vec<Value>, String> {
        let mut rules = Vec::new();
        for object in &self.objects {
            if let Some(rule) = object.get("rule") {
                if rule_target(rule)? == target {
                    rules.push(rule.clone());
                }
            }
        }
        Ok(rules)
    }
    fn peers(&self, target: &str) -> Result<String, String> {
        let mut peers = Vec::new();
        for object in &self.objects {
            if let Some(rule) = object.get("rule") {
                if rule_target(rule)? == target {
                    continue;
                }
            }
            let mut peer = object.clone();
            if let Some(rule) = peer.get_mut("rule").and_then(Value::as_object_mut) {
                rule.remove("index");
            }
            peers.push(peer);
        }
        Ok(hex_digest(
            &serde_json::to_vec(&peers).map_err(|_| "selected classifier peer encoding failed")?,
        ))
    }
    fn attest_manifest(&self, manifest: &StateManifest) -> Result<(), String> {
        if self.digest != manifest.ruleset_sha256 {
            return Err("selected classifier table is not its recorded generation".into());
        }
        let mut targets = BTreeSet::new();
        for instance in &manifest.instances {
            if !targets.insert(instance.target.as_str()) {
                return Err("selected classifier target ownership is ambiguous".into());
            }
        }
        for object in &self.objects {
            if let Some(rule) = object.get("rule") {
                if !targets.contains(rule_target(rule)?) {
                    return Err("selected classifier has an untracked target".into());
                }
            }
        }
        Ok(())
    }
}

fn rule_handle(rule: &Value) -> Result<u64, String> {
    rule.get("handle")
        .and_then(Value::as_u64)
        .filter(|handle| *handle > 0)
        .ok_or_else(|| "selected classifier rule handle is invalid".into())
}
fn rule_target(rule: &Value) -> Result<&str, String> {
    if rule.get("family").and_then(Value::as_str) != Some(TABLE_FAMILY)
        || rule.get("table").and_then(Value::as_str) != Some(TABLE_NAME)
        || !matches!(
            rule.get("chain").and_then(Value::as_str),
            Some("forward" | "output")
        )
    {
        return Err("selected classifier rule is outside owned chains".into());
    }
    let expressions = rule
        .get("expr")
        .and_then(Value::as_array)
        .filter(|expr| !expr.is_empty() && expr.len() <= 64)
        .ok_or("selected classifier rule expression is invalid")?;
    let mut target = None;
    let mut action = false;
    for expression in expressions {
        if let Some(test) = expression.get("match") {
            if test.pointer("/left/meta/key").and_then(Value::as_str) == Some("oifname") {
                if action
                    || target.is_some()
                    || test.get("op").and_then(Value::as_str) != Some("==")
                {
                    return Err("selected classifier rule has ambiguous target scope".into());
                }
                target = test
                    .get("right")
                    .and_then(Value::as_str)
                    .filter(|name| safe_interface(name));
                if target.is_none() {
                    return Err("selected classifier rule target is invalid".into());
                }
            }
        } else {
            action = true;
        }
    }
    target.ok_or_else(|| "selected classifier rule has no exact target scope".into())
}
fn snapshot(environment: &Environment) -> Result<Option<KernelSnapshot>, String> {
    if !table_present(environment)? {
        return Ok(None);
    }
    let result = command(
        &environment.nft,
        &["-j", "list", "table", TABLE_FAMILY, TABLE_NAME],
        None,
    )?;
    if !result.status.success() {
        return Err("selected classifier snapshot failed".into());
    }
    KernelSnapshot::parse(&result.stdout).map(Some)
}
fn manifest(environment: &Environment) -> Result<Option<StateManifest>, String> {
    match fs::symlink_metadata(&environment.state_file) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err("selected classifier manifest unavailable".into()),
        Ok(_) => read_manifest(&environment.state_file).map(Some),
    }
}
fn pending_path(environment: &Environment) -> PathBuf {
    environment.state_file.with_extension("selected.pending")
}
fn temporary_path(environment: &Environment) -> PathBuf {
    environment
        .state_file
        .with_extension("selected.pending.tmp")
}
pub(super) fn require_no_pending(environment: &Environment) -> Result<(), String> {
    for path in [pending_path(environment), temporary_path(environment)] {
        match fs::symlink_metadata(path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) => return Err("selected classifier recovery metadata unavailable".into()),
            Ok(_) => return Err("selected classifier recovery is pending".into()),
        }
    }
    Ok(())
}

pub(crate) struct SelectedClassifier {
    environment: Environment,
    instance: String,
    target: String,
    job: String,
    before: Option<KernelSnapshot>,
    before_manifest: Option<StateManifest>,
    desired: RenderedRuleset,
}

/// Opaque observation held only across phases that must not change classifier
/// state. Drop and recapture around the authorized classifier phase itself.
pub(crate) struct ClassifierWitness {
    environment: Environment,
    state: Option<StateManifest>,
    kernel: Option<String>,
}
impl ClassifierWitness {
    pub(crate) fn capture() -> Result<Self, String> {
        let environment = Environment::live();
        let state = manifest(&environment)?;
        let kernel = snapshot(&environment)?;
        match (&state, &kernel) {
            (Some(state), Some(kernel)) => kernel.attest_manifest(state)?,
            (None, None) => {}
            _ => return Err("reload classifier witness ownership mismatch".into()),
        }
        let witness = Self {
            environment,
            state,
            kernel: kernel.map(|kernel| kernel.digest),
        };
        witness.attest()?;
        Ok(witness)
    }
    pub(crate) fn attest(&self) -> Result<(), String> {
        if manifest(&self.environment)? != self.state
            || snapshot(&self.environment)?.map(|kernel| kernel.digest) != self.kernel
        {
            return Err("reload preserved classifier changed".into());
        }
        Ok(())
    }
}

/// Ordinary reload may change several instances, but every nft transaction is
/// still selected and preserves its peers. Old target ownership is cleared as
/// a separate phase before moves/swaps. The parent owns the reload batch/lock.
pub(crate) fn reconcile_reload(
    package: &UciPackage,
    old_targets: &BTreeMap<String, BTreeSet<String>>,
    job: &str,
    attest: impl FnMut() -> Result<(), String>,
) -> Result<(), String> {
    reconcile_reload_with(&Environment::live(), package, old_targets, job, attest)
}

fn reconcile_reload_with(
    environment: &Environment,
    package: &UciPackage,
    old_targets: &BTreeMap<String, BTreeSet<String>>,
    job: &str,
    mut attest: impl FnMut() -> Result<(), String>,
) -> Result<(), String> {
    if job.len() != 32
        || !job
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err("reload classifier operation identity invalid".into());
    }
    attest()?;
    let desired: BTreeMap<_, _> = render_ruleset(environment, package)?
        .instances
        .into_iter()
        .map(|entry| (entry.instance, entry.target))
        .collect();
    let allowed = |instance: &str, target: &str| {
        old_targets
            .get(instance)
            .is_some_and(|targets| targets.contains(target))
            || desired.get(instance).is_some_and(|next| next == target)
    };
    let old = {
        let _guard = RuntimeGuard::lock(environment)?;
        attest()?;
        let temporary = temporary_path(environment);
        match fs::symlink_metadata(&temporary) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) => return Err("reload classifier draft unavailable".into()),
            Ok(_) => {
                if fs::symlink_metadata(pending_path(environment)).is_ok() {
                    return Err("reload classifier conflicting recovery records".into());
                }
                let (bytes, _, identity) = read_file(&temporary, MAX_JOURNAL, true)?;
                if !MAGIC.starts_with(&bytes) && !bytes.starts_with(MAGIC) {
                    return Err("reload classifier draft owner invalid".into());
                }
                if let Some(payload) = bytes.strip_prefix(MAGIC) {
                    match serde_json::from_slice::<Value>(payload) {
                        Ok(value)
                            if value["job"].as_str() == Some(job)
                                && value["instance"]
                                    .as_str()
                                    .zip(value["target"].as_str())
                                    .is_some_and(|(name, target)| allowed(name, target)) => {}
                        Err(error) if error.is_eof() => {}
                        _ => {
                            return Err(
                                "reload classifier draft belongs to another operation".into()
                            )
                        }
                    }
                }
                attest()?;
                let (current, _, actual) = read_file(&temporary, MAX_JOURNAL, true)?;
                if bytes != current || identity != actual {
                    return Err("reload classifier draft changed".into());
                }
                fs::remove_file(&temporary)
                    .map_err(|_| "reload classifier draft cleanup failed")?;
                sync_parent(&temporary)?;
            }
        }
        if let Some(pending) = Pending::load(environment)? {
            let instance = pending.value["instance"]
                .as_str()
                .ok_or("reload classifier pending instance invalid")?;
            let target = pending.value["target"]
                .as_str()
                .ok_or("reload classifier pending target invalid")?;
            if pending.value["job"].as_str() != Some(job) || !allowed(instance, target) {
                return Err("reload classifier recovery belongs to another operation".into());
            }
            attest()?;
            pending.restore(environment)?;
            attest()?;
        }
        let kernel = snapshot(environment)?;
        let old = manifest(environment)?;
        match (&kernel, &old) {
            (Some(kernel), Some(old)) => kernel.attest_manifest(old)?,
            (None, None) => {}
            _ => return Err("reload classifier ownership is unproven".into()),
        }
        if old.as_ref().is_some_and(|old| {
            old.instances
                .iter()
                .any(|entry| !allowed(&entry.instance, &entry.target))
        }) {
            return Err("reload classifier has an unrelated owner".into());
        }
        old
    };
    if let Some(old) = old {
        for entry in old.instances {
            if desired.get(&entry.instance) != Some(&entry.target) {
                SelectedClassifier::prepare_with(
                    environment.clone(),
                    &entry.instance,
                    &entry.target,
                    job,
                    &UciPackage::default(),
                    &mut attest,
                )?
                .apply(&mut attest)?;
            }
        }
    }
    for (instance, target) in &desired {
        SelectedClassifier::prepare_with(
            environment.clone(),
            instance,
            target,
            job,
            package,
            &mut attest,
        )?
        .apply(&mut attest)?;
    }
    attest()?;
    if !frozen_rules_unchanged_with(environment, package)? {
        return Err("reload classifier final projection is not ready".into());
    }
    attest()
}

impl SelectedClassifier {
    pub(crate) fn prepare(
        instance: &str,
        target: &str,
        job: &str,
        package: &UciPackage,
        attest: impl FnMut() -> Result<(), String>,
    ) -> Result<Self, String> {
        Self::prepare_with(Environment::live(), instance, target, job, package, attest)
    }
    fn prepare_with(
        environment: Environment,
        instance: &str,
        target: &str,
        job: &str,
        package: &UciPackage,
        mut attest: impl FnMut() -> Result<(), String>,
    ) -> Result<Self, String> {
        if !safe_name(instance)
            || !safe_interface(target)
            || job.len() != 32
            || !job
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err("selected classifier request binding is invalid".into());
        }
        if !environment.nft.is_file() {
            return Err("selected classifier nft is unavailable".into());
        }
        let _guard = RuntimeGuard::lock(&environment)?;
        attest()?;
        recover_pending(&environment, instance, target, job, &mut attest)?;
        let before = snapshot(&environment)?;
        let before_manifest = manifest(&environment)?;
        match (&before, &before_manifest) {
            (Some(kernel), Some(manifest)) => kernel.attest_manifest(manifest)?,
            (None, None) => {}
            _ => return Err("selected classifier state requires service recovery".into()),
        }
        if before_manifest.as_ref().is_some_and(|manifest| {
            manifest.instances.iter().any(|entry| {
                (entry.instance == instance && entry.target != target)
                    || (entry.instance != instance && entry.target == target)
            })
        }) {
            return Err("selected classifier target ownership changed".into());
        }
        let package = UciPackage {
            sections: package
                .sections
                .iter()
                .filter(|(name, section)| {
                    (section.section_type == "cake_autorate" && name.as_str() == instance)
                        || (section.section_type == "traffic_rule"
                            && option(section, "instance") == instance)
                })
                .map(|(name, section)| (name.clone(), section.clone()))
                .collect(),
        };
        let desired = render_ruleset(&environment, &package)?;
        if desired.instances.len() > 1
            || desired
                .instances
                .first()
                .is_some_and(|entry| entry.instance != instance || entry.target != target)
        {
            return Err("selected classifier projection escaped its target".into());
        }
        attest()?;
        let result = Self {
            environment,
            instance: instance.into(),
            target: target.into(),
            job: job.into(),
            before,
            before_manifest,
            desired,
        };
        result.attest_before()?;
        Ok(result)
    }
    fn attest_before(&self) -> Result<(), String> {
        let current = snapshot(&self.environment)?;
        if current.as_ref().map(|value| &value.digest)
            != self.before.as_ref().map(|value| &value.digest)
            || manifest(&self.environment)? != self.before_manifest
        {
            return Err("selected classifier changed after preflight".into());
        }
        Ok(())
    }
    pub(crate) fn apply(
        &self,
        mut attest: impl FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        let _guard = RuntimeGuard::lock(&self.environment)?;
        require_no_pending(&self.environment)?;
        attest()?;
        self.attest_before()?;
        let old_entry = self.before_manifest.as_ref().and_then(|state| {
            state
                .instances
                .iter()
                .find(|entry| entry.instance == self.instance)
        });
        if old_entry == self.desired.instances.first() {
            return Ok(());
        }
        let old_rules = self
            .before
            .as_ref()
            .map(|before| before.rules(&self.target))
            .transpose()?
            .unwrap_or_default();
        let mut batch = String::new();
        if self.before.is_none() {
            batch.push_str(&format!("create table inet {TABLE_NAME} {{ comment \"cake-selected-{}\"; }}\nadd chain inet {TABLE_NAME} forward {{ type filter hook forward priority -140; policy accept; }}\nadd chain inet {TABLE_NAME} output {{ type route hook output priority -140; policy accept; }}\n", self.job));
        }
        for rule in &old_rules {
            batch.push_str(&format!(
                "delete rule inet {TABLE_NAME} {} handle {}\n",
                rule["chain"]
                    .as_str()
                    .ok_or("selected classifier chain missing")?,
                rule_handle(rule)?
            ));
        }
        for (chain, rules) in [
            ("forward", &self.desired.forward),
            ("output", &self.desired.output),
        ] {
            for line in rules.lines() {
                batch.push_str(&format!(
                    "add rule inet {TABLE_NAME} {chain} {}\n",
                    line.trim()
                ));
            }
        }
        if batch.len() > MAX_COMMAND_OUTPUT {
            return Err("selected classifier batch exceeds bound".into());
        }
        let checked = command(
            &self.environment.nft,
            &["-c", "-f", "-"],
            Some(batch.as_bytes()),
        )?;
        if !checked.status.success() {
            return Err("selected classifier batch failed validation".into());
        }
        attest()?;
        self.attest_before()?;
        let pending = Pending::write(self, old_rules)?;
        let result = (|| {
            pending.attest()?;
            attest()?;
            self.attest_before()?;
            let applied = command(&self.environment.nft, &["-f", "-"], Some(batch.as_bytes()))?;
            if !applied.status.success() {
                return Err("selected classifier apply failed".into());
            }
            let current =
                snapshot(&self.environment)?.ok_or("selected classifier table disappeared")?;
            pending.verify_peers(Some(&current))?;
            if current.rules(&self.target)?.len()
                != self.desired.forward.lines().count() + self.desired.output.lines().count()
            {
                return Err("selected classifier rule count is not its requested result".into());
            }
            let mut state = self.before_manifest.clone().unwrap_or(StateManifest {
                ruleset_sha256: String::new(),
                projection_sha256: None,
                instances: Vec::new(),
            });
            state
                .instances
                .retain(|entry| entry.instance != self.instance);
            state
                .instances
                .extend(self.desired.instances.iter().cloned());
            state.instances.sort_by(|a, b| a.instance.cmp(&b.instance));
            state.ruleset_sha256 = current.digest.clone();
            state.projection_sha256 = None;
            attest()?;
            pending.attest()?;
            pending.verify_peer_manifest(&self.environment)?;
            write_manifest(&self.environment.state_file, &state)?;
            attest()?;
            let after =
                snapshot(&self.environment)?.ok_or("selected classifier table disappeared")?;
            if after.digest != current.digest
                || read_manifest(&self.environment.state_file)? != state
            {
                return Err("selected classifier changed before acceptance".into());
            }
            pending.retire()
        })();
        if let Err(error) = result {
            return match pending.restore(&self.environment) {
                Ok(()) => Err(error),
                Err(recovery) => Err(format!(
                    "{error}; selected classifier recovery required: {recovery}"
                )),
            };
        }
        Ok(())
    }
}

struct Pending {
    path: PathBuf,
    bytes: Vec<u8>,
    identity: Identity,
    value: Value,
}
impl Pending {
    fn write(plan: &SelectedClassifier, rules: Vec<Value>) -> Result<Self, String> {
        let value = json!({"schema":1,"instance":plan.instance,"target":plan.target,"job":plan.job,
            "before_manifest":plan.before_manifest.as_ref().map(StateManifest::encode).transpose()?,
            "before_raw":plan.before.as_ref().map(|before| std::str::from_utf8(&before.raw)).transpose().map_err(|_| "selected classifier snapshot encoding failed")?,
            "peers":plan.before.as_ref().map(|before| before.peers(&plan.target)).transpose()?,"rules":rules});
        let mut bytes = MAGIC.to_vec();
        bytes.extend(
            serde_json::to_vec(&value)
                .map_err(|_| "selected classifier journal encoding failed")?,
        );
        if bytes.len() as u64 > MAX_JOURNAL {
            return Err("selected classifier journal exceeds bound".into());
        }
        let path = pending_path(&plan.environment);
        let temporary = temporary_path(&plan.environment);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&temporary)
            .map_err(|_| "selected classifier journal creation failed")?;
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(|_| "selected classifier journal persistence failed")?;
        let directory = Directory::open(
            path.parent()
                .ok_or("selected classifier journal parent missing")?,
            false,
            false,
        )?;
        let temp_name = temporary
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("selected classifier journal name invalid")?;
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("selected classifier journal name invalid")?;
        crate::operations::uci_transaction::rename(&directory, temp_name, &directory, name, false)?;
        let (actual, _, identity) = read_file(&path, MAX_JOURNAL, true)?;
        if actual != bytes {
            return Err("selected classifier journal changed".into());
        }
        Ok(Self {
            path,
            bytes,
            identity,
            value,
        })
    }
    fn load(environment: &Environment) -> Result<Option<Self>, String> {
        let path = pending_path(environment);
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err("selected classifier journal unavailable".into()),
            Ok(_) => {}
        }
        let (bytes, _, identity) = read_file(&path, MAX_JOURNAL, true)?;
        let raw = bytes
            .strip_prefix(MAGIC)
            .ok_or("selected classifier journal owner is invalid")?;
        let value: Value =
            serde_json::from_slice(raw).map_err(|_| "selected classifier journal is invalid")?;
        if value.get("schema").and_then(Value::as_u64) != Some(1)
            || value
                .get("rules")
                .and_then(Value::as_array)
                .is_none_or(|rules| rules.len() > MAX_KERNEL_OBJECTS)
        {
            return Err("selected classifier journal schema is invalid".into());
        }
        let result = Self {
            path,
            bytes,
            identity,
            value,
        };
        result.validate_record()?;
        Ok(Some(result))
    }
    fn validate_record(&self) -> Result<(), String> {
        let instance = self.value["instance"]
            .as_str()
            .filter(|name| safe_name(name))
            .ok_or("selected classifier journal instance invalid")?;
        let target = self.value["target"]
            .as_str()
            .filter(|name| safe_interface(name))
            .ok_or("selected classifier journal target invalid")?;
        let job = self.value["job"]
            .as_str()
            .filter(|job| {
                job.len() == 32
                    && job
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
            .ok_or("selected classifier journal job invalid")?;
        let _ = (instance, job);
        let rules = self.value["rules"]
            .as_array()
            .ok_or("selected classifier journal rules invalid")?;
        if let Some(raw) = self.value["before_raw"].as_str() {
            let before = KernelSnapshot::parse(raw.as_bytes())?;
            let state = StateManifest::decode(
                self.value["before_manifest"]
                    .as_str()
                    .ok_or("selected classifier journal manifest invalid")?,
            )?;
            before.attest_manifest(&state)?;
            if before.rules(target)? != *rules
                || self.value["peers"].as_str() != Some(before.peers(target)?.as_str())
            {
                return Err("selected classifier journal scope mismatch".into());
            }
        } else if !self.value["before_raw"].is_null()
            || !self.value["before_manifest"].is_null()
            || !self.value["peers"].is_null()
            || !rules.is_empty()
        {
            return Err("selected classifier journal absence mismatch".into());
        }
        Ok(())
    }
    fn attest(&self) -> Result<(), String> {
        let (bytes, _, identity) = read_file(&self.path, MAX_JOURNAL, true)?;
        if bytes != self.bytes || identity != self.identity {
            return Err("selected classifier journal changed".into());
        }
        Ok(())
    }
    fn verify_peers(&self, current: Option<&KernelSnapshot>) -> Result<(), String> {
        let target = self.value["target"]
            .as_str()
            .filter(|target| safe_interface(target))
            .ok_or("selected classifier journal target invalid")?;
        if let Some(peers) = self.value["peers"].as_str() {
            if current
                .ok_or("selected classifier shared table disappeared")?
                .peers(target)?
                != peers
            {
                return Err("selected classifier peer state changed".into());
            }
        } else if let Some(current) = current {
            let expected = format!(
                "cake-selected-{}",
                self.value["job"]
                    .as_str()
                    .ok_or("selected classifier journal job invalid")?
            );
            if !current.objects.iter().any(|object| {
                object
                    .get("table")
                    .and_then(|table| table.get("comment"))
                    .and_then(Value::as_str)
                    == Some(expected.as_str())
            }) {
                return Err("selected classifier new table ownership is unproven".into());
            }
            for object in &current.objects {
                if let Some(rule) = object.get("rule") {
                    if rule_target(rule)? != target {
                        return Err("selected classifier new table acquired another target".into());
                    }
                }
            }
        }
        Ok(())
    }
    fn restore(&self, environment: &Environment) -> Result<(), String> {
        self.attest()?;
        self.verify_peer_manifest(environment)?;
        let current = snapshot(environment)?;
        self.verify_peers(current.as_ref())?;
        let target = self.value["target"]
            .as_str()
            .ok_or("selected classifier journal target invalid")?;
        if self.value["before_manifest"].is_null() {
            if current.is_some() {
                let result = command(
                    &environment.nft,
                    &["delete", "table", TABLE_FAMILY, TABLE_NAME],
                    None,
                )?;
                if !result.status.success() || table_present(environment)? {
                    return Err("selected classifier new table removal failed".into());
                }
            }
            remove_manifest(&environment.state_file)?;
        } else {
            let mut state = StateManifest::decode(
                self.value["before_manifest"]
                    .as_str()
                    .ok_or("selected classifier original manifest invalid")?,
            )?;
            let current = current.ok_or("selected classifier original table missing")?;
            let old_rules = self.value["rules"]
                .as_array()
                .ok_or("selected classifier original rules invalid")?;
            let current_rules = current.rules(target)?;
            if rule_bodies(&current_rules)? == rule_bodies(old_rules)? {
                state.ruleset_sha256 = current.digest;
                if manifest(environment)?.as_ref() != Some(&state) {
                    write_manifest(&environment.state_file, &state)?;
                }
                return self.retire();
            }
            let mut commands = Vec::new();
            for rule in current_rules {
                commands.push(json!({"delete":{"rule":{"family":TABLE_FAMILY,"table":TABLE_NAME,"chain":rule["chain"],"handle":rule_handle(&rule)?}}}));
            }
            for old in self.value["rules"]
                .as_array()
                .ok_or("selected classifier original rules invalid")?
            {
                if rule_target(old)? != target {
                    return Err("selected classifier journal escaped its target".into());
                }
                let mut rule = old.clone();
                let fields = rule
                    .as_object_mut()
                    .ok_or("selected classifier original rule invalid")?;
                fields.remove("handle");
                fields.remove("index");
                commands.push(json!({"add":{"rule":rule}}));
            }
            let bytes = serde_json::to_vec(&json!({"nftables":commands}))
                .map_err(|_| "selected classifier restore encoding failed")?;
            if bytes.len() > 2 * MAX_COMMAND_OUTPUT {
                return Err("selected classifier restore batch exceeds bound".into());
            }
            self.attest()?;
            let result = command(&environment.nft, &["-j", "-f", "-"], Some(&bytes))?;
            if !result.status.success() {
                return Err("selected classifier restore command failed".into());
            }
            let restored =
                snapshot(environment)?.ok_or("selected classifier restored table missing")?;
            self.verify_peers(Some(&restored))?;
            if rule_bodies(&restored.rules(target)?)?
                != rule_bodies(
                    self.value["rules"]
                        .as_array()
                        .ok_or("selected classifier original rules invalid")?,
                )?
            {
                return Err("selected classifier restored rules mismatch".into());
            }
            state.ruleset_sha256 = restored.digest;
            write_manifest(&environment.state_file, &state)?;
        }
        self.retire()
    }
    fn verify_peer_manifest(&self, environment: &Environment) -> Result<(), String> {
        let instance = self.value["instance"]
            .as_str()
            .ok_or("selected classifier journal instance invalid")?;
        let before = self.value["before_manifest"]
            .as_str()
            .map(StateManifest::decode)
            .transpose()?;
        let current = manifest(environment)?;
        let peers = |state: &Option<StateManifest>| {
            state
                .as_ref()
                .map(|state| {
                    state
                        .instances
                        .iter()
                        .filter(|entry| entry.instance != instance)
                        .cloned()
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };
        if peers(&before) != peers(&current) || (before.is_some() && current.is_none()) {
            return Err("selected classifier peer manifest changed".into());
        }
        Ok(())
    }
    fn retire(&self) -> Result<(), String> {
        self.attest()?;
        fs::remove_file(&self.path).map_err(|_| "selected classifier journal retirement failed")?;
        sync_parent(&self.path)
    }
}
fn rule_bodies(rules: &[Value]) -> Result<Vec<Value>, String> {
    let mut bodies = Vec::with_capacity(rules.len());
    for rule in rules {
        let mut rule = rule.clone();
        let fields = rule
            .as_object_mut()
            .ok_or("selected classifier rule invalid")?;
        fields.remove("handle");
        fields.remove("index");
        bodies.push(rule);
    }
    Ok(bodies)
}
fn sync_parent(path: &Path) -> Result<(), String> {
    File::open(
        path.parent()
            .ok_or("selected classifier state parent missing")?,
    )
    .and_then(|directory| directory.sync_all())
    .map_err(|_| "selected classifier directory sync failed".into())
}
fn recover_pending(
    environment: &Environment,
    instance: &str,
    target: &str,
    job: &str,
    attest: &mut impl FnMut() -> Result<(), String>,
) -> Result<(), String> {
    let temporary = temporary_path(environment);
    match fs::symlink_metadata(&temporary) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return Err("selected classifier draft unavailable".into()),
        Ok(_) => {
            if fs::symlink_metadata(pending_path(environment)).is_ok() {
                return Err("selected classifier has conflicting recovery records".into());
            }
            let (bytes, _, identity) = read_file(&temporary, MAX_JOURNAL, true)?;
            if !MAGIC.starts_with(&bytes) && !bytes.starts_with(MAGIC) {
                return Err("selected classifier draft owner invalid".into());
            }
            attest()?;
            let (current, _, actual) = read_file(&temporary, MAX_JOURNAL, true)?;
            if bytes != current || identity != actual {
                return Err("selected classifier draft changed".into());
            }
            fs::remove_file(&temporary).map_err(|_| "selected classifier draft cleanup failed")?;
            sync_parent(&temporary)?;
        }
    }
    if let Some(pending) = Pending::load(environment)? {
        if pending.value["instance"].as_str() != Some(instance)
            || pending.value["target"].as_str() != Some(target)
            || pending.value["job"].as_str() != Some(job)
        {
            return Err("selected classifier recovery belongs to another operation".into());
        }
        attest()?;
        pending.restore(environment)?;
        attest()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct Fixture {
        root: PathBuf,
        environment: Environment,
        package: UciPackage,
    }
    impl Fixture {
        fn new(present: bool) -> Self {
            let root = std::env::temp_dir().join(format!(
                "cake-selected-nft-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir(&root).unwrap();
            fs::write(
                root.join("fixture.owner"),
                b"cake-nft-selected-fixture-v1\n",
            )
            .unwrap();
            let sys = root.join("sys");
            for target in ["eth0", "eth1"] {
                fs::create_dir_all(sys.join(target)).unwrap();
            }
            let nft = root.join("nft");
            let script =
                Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/nft-selected-fixture.py");
            fs::write(
                &nft,
                format!(
                    "#!/bin/sh\nexec python3 '{}' '{}' \"$@\"\n",
                    script.display(),
                    root.display()
                ),
            )
            .unwrap();
            fs::set_permissions(&nft, fs::Permissions::from_mode(0o700)).unwrap();
            let environment = Environment {
                uci: root.join("forbidden-uci"),
                nft,
                ubus: root.join("ubus"),
                sys_class_net: sys,
                runtime_dir: root.clone(),
                state_file: root.join("classifier.state"),
            };
            let mut package = UciPackage::default();
            for (name, target) in [("selected", "eth0"), ("peer", "eth1")] {
                package.sections.insert(
                    name.into(),
                    UciSection {
                        section_type: "cake_autorate".into(),
                        options: [
                            ("enabled", "1"),
                            ("manage_sqm", "1"),
                            ("sqm_enabled", "1"),
                            ("traffic_rules_enabled", "1"),
                            ("wan_if", target),
                            ("sqm_interface", target),
                            ("ul_if", target),
                            ("sqm_script", "layer_cake.qos"),
                            ("sqm_eqdisc_opts", "diffserv4"),
                            ("autotune_profile", "gaming"),
                            ("traffic_profile", "auto"),
                        ]
                        .into_iter()
                        .map(|(key, value)| (key.into(), value.into()))
                        .collect(),
                    },
                );
            }
            if present {
                let desired = render_ruleset(&environment, &package).unwrap();
                let mut objects = vec![
                    json!({"metainfo":{"json_schema_version":1}}),
                    json!({"table":{"family":"inet","name":TABLE_NAME,"handle":1}}),
                ];
                let mut handle = 10u64;
                for (chain, rules, kind) in [
                    ("forward", &desired.forward, "filter"),
                    ("output", &desired.output, "route"),
                ] {
                    objects.push(json!({"chain":{"family":"inet","table":TABLE_NAME,"name":chain,"handle":handle,"type":kind,"hook":chain,"prio":-140,"policy":"accept"}}));
                    handle += 1;
                    for line in rules.lines() {
                        let body = line.trim();
                        let target = body.split('"').nth(1).unwrap();
                        objects.push(json!({"rule":{"family":"inet","table":TABLE_NAME,"chain":chain,"handle":handle,"expr":[{"match":{"op":"==","left":{"meta":{"key":"oifname"}},"right":target}},{"fixture_body":body}]}}));
                        handle += 1;
                    }
                }
                fs::write(
                    root.join("kernel.json"),
                    serde_json::to_vec(&json!({"nftables":objects})).unwrap(),
                )
                .unwrap();
                let state = StateManifest {
                    ruleset_sha256: ruleset_digest(&environment).unwrap(),
                    projection_sha256: Some(hex_digest(desired.text.as_bytes())),
                    instances: desired.instances,
                };
                write_manifest(&environment.state_file, &state).unwrap();
            }
            Self {
                root,
                environment,
                package,
            }
        }
        fn plan(&self, package: &UciPackage) -> SelectedClassifier {
            SelectedClassifier::prepare_with(
                self.environment.clone(),
                "selected",
                "eth0",
                &"a".repeat(32),
                package,
                || Ok(()),
            )
            .unwrap()
        }
        fn changed(&self) -> UciPackage {
            let mut package = self.package.clone();
            package
                .sections
                .get_mut("selected")
                .unwrap()
                .options
                .insert("traffic_profile".into(), "fair".into());
            package
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    #[ignore = "requires explicit nft binary and a freshly isolated user/network namespace"]
    fn native_nft_selected_classifier_in_isolated_namespace() {
        let parent = std::env::var_os("CAKE_TEST_PARENT_NETNS")
            .expect("explicit parent network namespace required");
        let current = fs::read_link("/proc/self/ns/net").unwrap();
        assert_ne!(
            current.as_os_str(),
            parent.as_os_str(),
            "refusing to mutate the parent network namespace"
        );
        let nft = PathBuf::from(
            std::env::var_os("CAKE_TEST_NATIVE_NFT").expect("explicit native nft required"),
        );
        assert!(nft.is_absolute() && nft.is_file());
        let initial = command(&nft, &["-j", "list", "ruleset"], None).unwrap();
        assert!(
            initial.status.success(),
            "isolated nft probe failed: {}",
            String::from_utf8_lossy(&initial.stderr)
        );
        let initial: Value = serde_json::from_slice(&initial.stdout).unwrap();
        assert!(
            initial["nftables"]
                .as_array()
                .unwrap()
                .iter()
                .all(|entry| entry.get("metainfo").is_some()),
            "native fixture requires an empty isolated ruleset"
        );
        let mut fixture = Fixture::new(false);
        fixture.environment.nft = nft;
        apply_with_package(
            &fixture.environment,
            || Ok(fixture.package.clone()),
            || Ok(()),
        )
        .unwrap();
        let original = snapshot(&fixture.environment).unwrap().unwrap();
        let mut desired = fixture.changed();
        desired
            .sections
            .get_mut("peer")
            .unwrap()
            .options
            .insert("traffic_profile".into(), "fair".into());
        fixture.plan(&desired).apply(|| Ok(())).unwrap();
        let changed = snapshot(&fixture.environment).unwrap().unwrap();
        assert_eq!(
            original.peers("eth0").unwrap(),
            changed.peers("eth0").unwrap()
        );
        assert_eq!(
            original.rules("eth1").unwrap(),
            changed.rules("eth1").unwrap()
        );
        assert!(frozen_rules_unchanged_with(&fixture.environment, &fixture.changed()).unwrap());
        assert!(!frozen_rules_unchanged_with(&fixture.environment, &desired).unwrap());
        let restore_plan = fixture.plan(&fixture.package);
        let mut calls = 0;
        assert!(restore_plan
            .apply(|| {
                calls += 1;
                if calls == 4 {
                    Err("injected after real nft apply".into())
                } else {
                    Ok(())
                }
            })
            .is_err());
        let restored = snapshot(&fixture.environment).unwrap().unwrap();
        assert_eq!(
            changed.peers("eth0").unwrap(),
            restored.peers("eth0").unwrap()
        );
        assert_eq!(
            rule_bodies(&changed.rules("eth0").unwrap()).unwrap(),
            rule_bodies(&restored.rules("eth0").unwrap()).unwrap()
        );
        assert!(!pending_path(&fixture.environment).exists());
        fixture
            .plan(&UciPackage::default())
            .apply(|| Ok(()))
            .unwrap();
        let removed = snapshot(&fixture.environment).unwrap().unwrap();
        assert!(removed.rules("eth0").unwrap().is_empty());
        assert_eq!(
            changed.rules("eth1").unwrap(),
            removed.rules("eth1").unwrap()
        );
        clear(&fixture.environment).unwrap();
        fixture.plan(&fixture.changed()).apply(|| Ok(())).unwrap();
        let created = snapshot(&fixture.environment).unwrap().unwrap();
        assert!(created.rules("eth1").unwrap().is_empty());
        assert_eq!(
            manifest(&fixture.environment)
                .unwrap()
                .unwrap()
                .instances
                .len(),
            1
        );
        clear(&fixture.environment).unwrap();
        assert!(!table_present(&fixture.environment).unwrap());
    }

    #[test]
    fn r4_reload_classifier_preserves_unchanged_peer_and_supports_target_swaps() {
        let fixture = Fixture::new(true);
        let old = BTreeMap::from([
            ("selected".into(), BTreeSet::from(["eth0".into()])),
            ("peer".into(), BTreeSet::from(["eth1".into()])),
        ]);
        let before = snapshot(&fixture.environment).unwrap().unwrap();
        reconcile_reload_with(
            &fixture.environment,
            &fixture.changed(),
            &old,
            &"b".repeat(32),
            || Ok(()),
        )
        .unwrap();
        let changed = snapshot(&fixture.environment).unwrap().unwrap();
        assert_eq!(
            before.rules("eth1").unwrap(),
            changed.rules("eth1").unwrap()
        );
        let mut swapped = fixture.changed();
        for (name, target) in [("selected", "eth1"), ("peer", "eth0")] {
            for option in ["wan_if", "sqm_interface", "ul_if"] {
                swapped
                    .sections
                    .get_mut(name)
                    .unwrap()
                    .options
                    .insert(option.into(), target.into());
            }
        }
        reconcile_reload_with(
            &fixture.environment,
            &swapped,
            &old,
            &"c".repeat(32),
            || Ok(()),
        )
        .unwrap();
        assert!(frozen_rules_unchanged_with(&fixture.environment, &swapped).unwrap());
        let after = snapshot(&fixture.environment).unwrap().unwrap();
        reconcile_reload_with(
            &fixture.environment,
            &swapped,
            &old,
            &"c".repeat(32),
            || Ok(()),
        )
        .unwrap();
        assert_eq!(
            snapshot(&fixture.environment).unwrap().unwrap().raw,
            after.raw
        );
        let swapped_targets = BTreeMap::from([
            ("selected".into(), BTreeSet::from(["eth1".into()])),
            ("peer".into(), BTreeSet::from(["eth0".into()])),
        ]);
        reconcile_reload_with(
            &fixture.environment,
            &UciPackage::default(),
            &swapped_targets,
            &"d".repeat(32),
            || Ok(()),
        )
        .unwrap();
        assert!(frozen_rules_unchanged_with(&fixture.environment, &UciPackage::default()).unwrap());
    }

    #[test]
    fn r4_reload_classifier_recovers_its_record_first_and_refuses_other_owners() {
        let fixture = Fixture::new(true);
        let old = BTreeMap::from([
            ("selected".into(), BTreeSet::from(["eth0".into()])),
            ("peer".into(), BTreeSet::from(["eth1".into()])),
        ]);
        let before = snapshot(&fixture.environment).unwrap().unwrap();
        let plan = fixture.plan(&fixture.changed());
        let pending = Pending::write(&plan, before.rules("eth0").unwrap()).unwrap();
        let bytes = fs::read(&pending.path).unwrap();
        assert!(reconcile_reload_with(
            &fixture.environment,
            &fixture.changed(),
            &old,
            &"b".repeat(32),
            || Ok(())
        )
        .is_err());
        assert_eq!(fs::read(&pending.path).unwrap(), bytes);
        assert_eq!(
            snapshot(&fixture.environment).unwrap().unwrap().raw,
            before.raw
        );
        drop(pending);
        drop(plan);
        reconcile_reload_with(
            &fixture.environment,
            &fixture.changed(),
            &old,
            &"a".repeat(32),
            || Ok(()),
        )
        .unwrap();
        assert!(Pending::load(&fixture.environment).unwrap().is_none());
        let ready = snapshot(&fixture.environment).unwrap().unwrap();
        let mut incomplete = old;
        incomplete.remove("peer");
        assert!(reconcile_reload_with(
            &fixture.environment,
            &UciPackage::default(),
            &incomplete,
            &"c".repeat(32),
            || Ok(())
        )
        .is_err());
        assert_eq!(
            snapshot(&fixture.environment).unwrap().unwrap().raw,
            ready.raw
        );
        assert!(reconcile_reload_with(
            &fixture.environment,
            &fixture.package,
            &incomplete,
            &"c".repeat(32),
            || Err("source-changed".into())
        )
        .is_err());
        assert_eq!(
            snapshot(&fixture.environment).unwrap().unwrap().raw,
            ready.raw
        );
    }

    #[test]
    fn r4_selected_classifier_preserves_peer_rules_handles_and_unapplied_configuration() {
        let fixture = Fixture::new(true);
        let before = snapshot(&fixture.environment).unwrap().unwrap();
        let old_peer = manifest(&fixture.environment)
            .unwrap()
            .unwrap()
            .instances
            .into_iter()
            .find(|entry| entry.instance == "peer")
            .unwrap();
        let mut changed = fixture.changed();
        changed
            .sections
            .get_mut("peer")
            .unwrap()
            .options
            .insert("traffic_profile".into(), "fair".into());
        let plan = fixture.plan(&changed);
        plan.apply(|| Ok(())).unwrap();
        let after = snapshot(&fixture.environment).unwrap().unwrap();
        assert_eq!(before.peers("eth0").unwrap(), after.peers("eth0").unwrap());
        assert_eq!(before.rules("eth1").unwrap(), after.rules("eth1").unwrap());
        let state = manifest(&fixture.environment).unwrap().unwrap();
        assert_eq!(
            state
                .instances
                .iter()
                .find(|entry| entry.instance == "peer"),
            Some(&old_peer)
        );
        assert!(state.projection_sha256.is_none());
        assert!(!pending_path(&fixture.environment).exists());
        assert!(!frozen_rules_unchanged_with(&fixture.environment, &changed).unwrap());
        assert!(frozen_rules_unchanged_with(&fixture.environment, &fixture.changed()).unwrap());
        let actions = fs::read(fixture.root.join("actions.jsonl")).unwrap();
        fixture.plan(&fixture.changed()).apply(|| Ok(())).unwrap();
        assert_eq!(
            fs::read(fixture.root.join("actions.jsonl")).unwrap(),
            actions
        );
    }

    #[test]
    fn r4_selected_classifier_restores_only_selected_rules_after_lost_apply_ack() {
        let fixture = Fixture::new(true);
        let before = snapshot(&fixture.environment).unwrap().unwrap();
        let plan = fixture.plan(&fixture.changed());
        fs::write(fixture.root.join("fail-after-apply"), b"1").unwrap();
        assert!(plan.apply(|| Ok(())).is_err());
        let after = snapshot(&fixture.environment).unwrap().unwrap();
        assert_eq!(before.peers("eth0").unwrap(), after.peers("eth0").unwrap());
        assert_eq!(
            rule_bodies(&before.rules("eth0").unwrap()).unwrap(),
            rule_bodies(&after.rules("eth0").unwrap()).unwrap()
        );
        assert_eq!(
            manifest(&fixture.environment)
                .unwrap()
                .unwrap()
                .ruleset_sha256,
            after.digest
        );
        assert!(!pending_path(&fixture.environment).exists());
    }

    #[test]
    fn r4_selected_classifier_refuses_peer_drift_and_keeps_recovery_evidence() {
        let fixture = Fixture::new(true);
        let before = snapshot(&fixture.environment).unwrap().unwrap();
        let plan = fixture.plan(&fixture.changed());
        fs::write(fixture.root.join("peer-drift-after-apply"), b"1").unwrap();
        assert!(plan
            .apply(|| Ok(()))
            .unwrap_err()
            .contains("peer state changed"));
        let after = snapshot(&fixture.environment).unwrap().unwrap();
        assert_ne!(before.peers("eth0").unwrap(), after.peers("eth0").unwrap());
        assert!(pending_path(&fixture.environment).exists());
        assert!(require_no_pending(&fixture.environment).is_err());
        assert!(SelectedClassifier::prepare_with(
            fixture.environment.clone(),
            "selected",
            "eth0",
            &"b".repeat(32),
            &fixture.package,
            || Ok(())
        )
        .is_err());
    }

    #[test]
    fn r4_selected_classifier_resumes_its_durable_record_without_the_original_plan() {
        let fixture = Fixture::new(true);
        let before = snapshot(&fixture.environment).unwrap().unwrap();
        let plan = fixture.plan(&fixture.changed());
        let rules = before.rules("eth0").unwrap();
        let pending = Pending::write(&plan, rules.clone()).unwrap();
        let mut commands = Vec::new();
        for rule in &rules {
            commands.push(json!({"delete":{"rule":{"family":"inet","table":TABLE_NAME,"chain":rule["chain"],"handle":rule["handle"]}}}));
        }
        let mut changed = rules[0].clone();
        changed.as_object_mut().unwrap().remove("handle");
        changed["expr"][1] = json!({"fixture_body":"interrupted selected update"});
        commands.push(json!({"add":{"rule":changed}}));
        let bytes = serde_json::to_vec(&json!({"nftables":commands})).unwrap();
        assert!(
            command(&fixture.environment.nft, &["-j", "-f", "-"], Some(&bytes))
                .unwrap()
                .status
                .success()
        );
        drop(pending);
        drop(plan);
        let restored_plan = fixture.plan(&fixture.package);
        let after = snapshot(&fixture.environment).unwrap().unwrap();
        assert_eq!(before.peers("eth0").unwrap(), after.peers("eth0").unwrap());
        assert_eq!(
            rule_bodies(&rules).unwrap(),
            rule_bodies(&after.rules("eth0").unwrap()).unwrap()
        );
        assert!(!pending_path(&fixture.environment).exists());
        let actions = fs::read(fixture.root.join("actions.jsonl")).unwrap();
        restored_plan.apply(|| Ok(())).unwrap();
        assert_eq!(
            fs::read(fixture.root.join("actions.jsonl")).unwrap(),
            actions
        );
    }

    #[test]
    fn r4_selected_classifier_corrupt_restore_rules_do_not_override_the_saved_snapshot() {
        let fixture = Fixture::new(true);
        let before = snapshot(&fixture.environment).unwrap().unwrap();
        let plan = fixture.plan(&fixture.changed());
        let pending = Pending::write(&plan, before.rules("eth0").unwrap()).unwrap();
        let mut value = pending.value.clone();
        value["rules"][0]["expr"][1] = json!({"fixture_body":"tampered recovery bytes"});
        let mut bytes = MAGIC.to_vec();
        bytes.extend(serde_json::to_vec(&value).unwrap());
        fs::write(&pending.path, &bytes).unwrap();
        assert!(SelectedClassifier::prepare_with(
            fixture.environment.clone(),
            "selected",
            "eth0",
            &"a".repeat(32),
            &fixture.package,
            || Ok(())
        )
        .is_err());
        assert_eq!(
            snapshot(&fixture.environment).unwrap().unwrap().digest,
            before.digest
        );
        assert_eq!(fs::read(&pending.path).unwrap(), bytes);
        assert!(!fixture.root.join("actions.jsonl").exists());
    }

    #[test]
    fn r4_selected_classifier_source_failure_before_apply_preserves_rule_handles() {
        let fixture = Fixture::new(true);
        let plan = fixture.plan(&fixture.changed());
        let before = snapshot(&fixture.environment).unwrap().unwrap();
        let mut calls = 0;
        assert!(plan
            .apply(|| {
                calls += 1;
                if calls == 3 {
                    Err("source changed".into())
                } else {
                    Ok(())
                }
            })
            .is_err());
        assert_eq!(
            snapshot(&fixture.environment).unwrap().unwrap().digest,
            before.digest
        );
        assert!(!pending_path(&fixture.environment).exists());
        assert!(!fixture.root.join("actions.jsonl").exists());
    }

    #[test]
    fn r4_selected_classifier_draft_cleanup_is_owned_and_precedes_kernel_changes() {
        let fixture = Fixture::new(true);
        let before = snapshot(&fixture.environment).unwrap().unwrap().digest;
        let temporary = temporary_path(&fixture.environment);
        fs::write(&temporary, &MAGIC[..12]).unwrap();
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600)).unwrap();
        fixture.plan(&fixture.package);
        assert!(!temporary.exists());
        assert_eq!(
            snapshot(&fixture.environment).unwrap().unwrap().digest,
            before
        );
        fs::write(&temporary, b"foreign draft").unwrap();
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(SelectedClassifier::prepare_with(
            fixture.environment.clone(),
            "selected",
            "eth0",
            &"a".repeat(32),
            &fixture.package,
            || Ok(())
        )
        .is_err());
        assert_eq!(fs::read(&temporary).unwrap(), b"foreign draft");
        assert!(!fixture.root.join("actions.jsonl").exists());
    }

    #[test]
    fn r4_selected_classifier_rejects_nonexclusive_interface_rule_scope() {
        let fixture = Fixture::new(true);
        let bytes = fs::read(fixture.root.join("kernel.json")).unwrap();
        let mut value: Value = serde_json::from_slice(&bytes).unwrap();
        let rule = value["nftables"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find_map(|item| item.get_mut("rule"))
            .unwrap();
        rule["expr"][0]["match"]["op"] = json!("!=");
        assert!(KernelSnapshot::parse(&serde_json::to_vec(&value).unwrap()).is_err());
        assert!(!fixture.root.join("actions.jsonl").exists());
    }

    #[test]
    fn r4_selected_classifier_can_create_and_remove_only_its_first_table_on_failure() {
        let fixture = Fixture::new(false);
        let plan = fixture.plan(&fixture.changed());
        fs::write(fixture.root.join("fail-after-apply"), b"1").unwrap();
        assert!(plan.apply(|| Ok(())).is_err());
        assert!(snapshot(&fixture.environment).unwrap().is_none());
        assert!(manifest(&fixture.environment).unwrap().is_none());
        assert!(!pending_path(&fixture.environment).exists());
        fixture.plan(&fixture.changed()).apply(|| Ok(())).unwrap();
        let state = manifest(&fixture.environment).unwrap().unwrap();
        assert_eq!(state.instances.len(), 1);
        assert_eq!(state.instances[0].instance, "selected");
        let mut disabled = fixture.changed();
        disabled
            .sections
            .get_mut("selected")
            .unwrap()
            .options
            .insert("traffic_rules_enabled".into(), "0".into());
        fixture.plan(&disabled).apply(|| Ok(())).unwrap();
        assert!(manifest(&fixture.environment)
            .unwrap()
            .unwrap()
            .instances
            .is_empty());
        assert!(snapshot(&fixture.environment)
            .unwrap()
            .unwrap()
            .rules("eth0")
            .unwrap()
            .is_empty());
        assert!(frozen_rules_unchanged_with(&fixture.environment, &UciPackage::default()).unwrap());
    }
}
