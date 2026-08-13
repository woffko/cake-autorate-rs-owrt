//! Read-only kernel-topology witness primitives used by native bootstrap.
//!
//! The module intentionally contains no socket and no mutation API. The
//! OpenWrt backend translates bounded `NETLINK_ROUTE` dumps into these records.
//! Volatile counters, timestamps and queue statistics cannot be represented by
//! the types below and are therefore excluded by construction.

use super::protocol::OperationRouteIdentity;
use ring::digest::{digest, SHA256};
use std::collections::BTreeSet;
use std::fmt;
use std::net::IpAddr;

pub const KERNEL_TOPOLOGY_SCHEMA: u16 = 3;
pub const TC_H_ROOT: u32 = u32::MAX;
/// The ingress/clsact qdisc attachment point (`TC_H_CLSACT`).
pub const TC_H_INGRESS: u32 = 0xffff_fff1;
/// The canonical ingress/clsact qdisc handle.
pub const TC_H_CLSACT_HANDLE: u32 = 0xffff_0000;
/// The ingress classifier hook below an ingress or clsact qdisc.
pub const TC_H_MIN_INGRESS: u32 = 0xffff_fff2;
/// The egress classifier hook, available only below a clsact qdisc.
pub const TC_H_MIN_EGRESS: u32 = 0xffff_fff3;
pub const MAX_OBSERVED_BYTES: usize = 256 * 1024;
pub const MAX_CANONICAL_BYTES: usize = 256 * 1024;
pub const MAX_QDISCS: usize = 128;
pub const MAX_FILTERS: usize = 512;
pub const MAX_ACTIONS_PER_FILTER: usize = 16;
pub const MAX_PRIVATE_LINKS: usize = 64;
pub const MAX_NAMESPACE_IDENTITIES: usize = 128;
pub const MAX_UNKNOWN_ATTRIBUTES: usize = 64;
pub const MAX_ATTRIBUTE_PATH: usize = 8;
pub const MAX_CONFIG_BYTES: usize = 16 * 1024;
pub const MAX_CONFIG_ATTRIBUTES: usize = 128;
/// Linux UAPI `TC_COOKIE_MAX_SIZE`.
pub const MAX_ACTION_COOKIE_BYTES: usize = 16;

const CANONICAL_DOMAIN: &[u8] = b"cake-autorate-kernel-topology";
const MAX_INTERFACE_NAME_BYTES: usize = 15;
const MAX_ALIAS_BYTES: usize = 256;
const MAX_KIND_BYTES: usize = 32;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KernelTopologyError {
    Backend(String),
    Invalid(String),
    Limit(String),
    UnknownOwnershipAttribute {
        object: TopologyObject,
        attribute_type: u16,
    },
}

impl fmt::Display for KernelTopologyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Backend(message) | Self::Invalid(message) | Self::Limit(message) => {
                formatter.write_str(message)
            }
            Self::UnknownOwnershipAttribute {
                object,
                attribute_type,
            } => write!(
                formatter,
                "unknown ownership-relevant {} attribute type {}",
                object.as_str(),
                attribute_type
            ),
        }
    }
}

impl std::error::Error for KernelTopologyError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum TopologyObject {
    Link,
    Qdisc,
    Filter,
    Action,
}

impl TopologyObject {
    fn as_str(self) -> &'static str {
        match self {
            Self::Link => "link",
            Self::Qdisc => "qdisc",
            Self::Filter => "filter",
            Self::Action => "action",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct UnknownOwnershipAttribute {
    pub object: TopologyObject,
    pub attribute_type: u16,
    pub nested_path: Vec<u16>,
}

/// Kernel link kinds are genuinely optional in RTM_NEWLINK. Physical devices
/// commonly omit IFLA_LINKINFO/IFLA_INFO_KIND, while private virtual links must
/// carry an exact named kind before they can be considered owned.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum KernelLinkKind {
    Absent,
    Named(String),
}

impl KernelLinkKind {
    fn validate(&self, label: &str) -> Result<(), KernelTopologyError> {
        if let Self::Named(kind) = self {
            validate_kind(label, kind)?;
        }
        Ok(())
    }
}

/// One configuration-only attribute after parsing and normalization.
///
/// `path` is relative to the object's TCA_OPTIONS payload. Attribute order,
/// alignment padding, counters, timestamps and statistics are deliberately not
/// representable. Parsers must reject rather than place volatile or unknown
/// ownership-relevant data in this structure.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct CanonicalConfigAttribute {
    pub path: Vec<u16>,
    pub network_byte_order: bool,
    pub value: Vec<u8>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct CanonicalConfig {
    pub attributes: Vec<CanonicalConfigAttribute>,
}

impl CanonicalConfig {
    fn validate(&self, label: &str) -> Result<(), KernelTopologyError> {
        if self.attributes.len() > MAX_CONFIG_ATTRIBUTES {
            return limit(format!("{label} attribute count exceeds its bound"));
        }
        let mut paths = BTreeSet::new();
        let mut bytes = 0usize;
        for attribute in &self.attributes {
            if attribute.path.is_empty()
                || attribute.path.len() > MAX_ATTRIBUTE_PATH
                || attribute.path.contains(&0)
            {
                return invalid(format!("{label} contains an invalid attribute path"));
            }
            if !paths.insert(attribute.path.clone()) {
                return invalid(format!("{label} contains a duplicate attribute path"));
            }
            let path_bytes = attribute.path.len().checked_mul(2).ok_or_else(|| {
                KernelTopologyError::Limit(format!("{label} path byte count overflows"))
            })?;
            bytes = bytes
                .checked_add(path_bytes)
                .and_then(|value| value.checked_add(attribute.value.len()))
                .ok_or_else(|| {
                    KernelTopologyError::Limit(format!("{label} byte count overflows"))
                })?;
            if bytes > MAX_CONFIG_BYTES {
                return limit(format!("{label} exceeds its byte bound"));
            }
        }
        Ok(())
    }

    fn normalized(&self) -> Self {
        let mut normalized = self.clone();
        normalized.attributes.sort();
        normalized
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct LinkRecord {
    pub ifindex: u32,
    pub name: String,
    pub alias: Option<String>,
    pub kind: KernelLinkKind,
    pub parent_ifindex: Option<u32>,
}

impl LinkRecord {
    fn validate(&self, label: &str) -> Result<(), KernelTopologyError> {
        if !valid_kernel_ifindex(self.ifindex)
            || self
                .parent_ifindex
                .is_some_and(|ifindex| !valid_kernel_ifindex(ifindex))
        {
            return invalid(format!("{label} has an invalid ifindex"));
        }
        validate_interface(&format!("{label} name"), &self.name)?;
        self.kind.validate(&format!("{label} kind"))?;
        if let Some(alias) = self.alias.as_deref() {
            validate_alias(&format!("{label} alias"), alias)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PrivateNamespaceRecord {
    pub link_names: Vec<String>,
    pub link_aliases: Vec<String>,
    pub qdisc_handles: Vec<u32>,
    pub filter_handles: Vec<u32>,
    pub filter_priorities: Vec<u16>,
    /// Exact action-table reservations. Linux action indices are scoped by
    /// action kind, so an index without its kind is never ownership authority.
    pub action_identities: Vec<PrivateActionIdentity>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PrivateActionIdentity {
    pub kind: String,
    pub index: u32,
    pub cookie: Option<Vec<u8>>,
}

impl PrivateActionIdentity {
    fn validate(&self, label: &str) -> Result<(), KernelTopologyError> {
        validate_kind(&format!("{label} kind"), &self.kind)?;
        if self.index == 0 {
            return invalid(format!("{label} index must be non-zero"));
        }
        if self
            .cookie
            .as_ref()
            .is_some_and(|cookie| cookie.is_empty() || cookie.len() > MAX_ACTION_COOKIE_BYTES)
        {
            return limit(format!("{label} cookie is empty or exceeds its bound"));
        }
        Ok(())
    }
}

impl PrivateNamespaceRecord {
    pub fn validate(&self) -> Result<(), KernelTopologyError> {
        for (label, count) in [
            ("private link names", self.link_names.len()),
            ("private link aliases", self.link_aliases.len()),
            ("private qdisc handles", self.qdisc_handles.len()),
            ("private filter handles", self.filter_handles.len()),
            ("private filter priorities", self.filter_priorities.len()),
            ("private action identities", self.action_identities.len()),
        ] {
            if count > MAX_NAMESPACE_IDENTITIES {
                return limit(format!("{label} exceed the bounded identity count"));
            }
        }
        for name in &self.link_names {
            validate_interface("private namespace link name", name)?;
        }
        for alias in &self.link_aliases {
            validate_alias("private namespace link alias", alias)?;
        }
        if self.qdisc_handles.contains(&0)
            || self.filter_handles.contains(&0)
            || self.filter_priorities.contains(&0)
        {
            return invalid("private namespace numeric identities must be non-zero");
        }
        for identity in &self.action_identities {
            identity.validate("private action identity")?;
        }
        ensure_unique(&self.link_names, "private link names")?;
        ensure_unique(&self.link_aliases, "private link aliases")?;
        ensure_unique(&self.qdisc_handles, "private qdisc handles")?;
        ensure_unique(&self.filter_handles, "private filter handles")?;
        ensure_unique(&self.filter_priorities, "private filter priorities")?;
        validate_action_identity_set(&self.action_identities, "private action identities", true)?;
        let mut link_identities = BTreeSet::new();
        if self
            .link_names
            .iter()
            .chain(self.link_aliases.iter())
            .any(|identity| !link_identities.insert(identity))
        {
            return invalid("private link names and aliases contain an ambiguous identity");
        }
        Ok(())
    }

    fn normalized(&self) -> Self {
        let mut normalized = self.clone();
        normalized.link_names.sort();
        normalized.link_aliases.sort();
        normalized.qdisc_handles.sort_unstable();
        normalized.filter_handles.sort_unstable();
        normalized.filter_priorities.sort_unstable();
        normalized.action_identities.sort();
        normalized
    }

    pub(super) fn matches_link_identity(&self, identity: &str) -> bool {
        self.link_names
            .iter()
            .chain(self.link_aliases.iter())
            .any(|reserved| reserved == identity)
    }

    pub(super) fn matches_action_identity(&self, observed: &PrivateActionIdentity) -> bool {
        action_identity_collides(observed, self)
    }

    fn matches_link(&self, link: &LinkRecord) -> bool {
        self.matches_link_identity(&link.name)
            || link
                .alias
                .as_deref()
                .is_some_and(|alias| self.matches_link_identity(alias))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct QdiscRecord {
    pub ifindex: u32,
    pub parent: u32,
    pub handle: u32,
    pub kind: String,
    /// Parsed, configuration-only TCA payload.
    pub options: CanonicalConfig,
}

impl QdiscRecord {
    fn validate(&self, known_ifindexes: &BTreeSet<u32>) -> Result<(), KernelTopologyError> {
        if !known_ifindexes.contains(&self.ifindex) {
            return invalid("qdisc references an unobserved interface");
        }
        validate_kind("qdisc kind", &self.kind)?;
        self.options.validate("qdisc options")?;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum MirredDirection {
    EgressRedirect,
    IngressRedirect,
    EgressMirror,
    IngressMirror,
}

impl MirredDirection {
    fn tag(self) -> u8 {
        match self {
            Self::EgressRedirect => 1,
            Self::IngressRedirect => 2,
            Self::EgressMirror => 3,
            Self::IngressMirror => 4,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ActionTarget {
    None,
    Mirred {
        direction: MirredDirection,
        target_ifindex: u32,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ActionRecord {
    pub order: u16,
    pub action_index: u32,
    pub kind: String,
    pub cookie: Option<Vec<u8>>,
    /// Parsed, configuration-only action payload.
    pub options: CanonicalConfig,
    pub target: ActionTarget,
}

impl ActionRecord {
    fn validate(&self, known_ifindexes: &BTreeSet<u32>) -> Result<(), KernelTopologyError> {
        if self.order == 0 || self.action_index == 0 {
            return invalid("action order and index must be non-zero");
        }
        validate_kind("action kind", &self.kind)?;
        self.options.validate("action options")?;
        if self
            .cookie
            .as_ref()
            .is_some_and(|cookie| cookie.is_empty() || cookie.len() > MAX_ACTION_COOKIE_BYTES)
        {
            return limit("action cookie is empty or exceeds its bound");
        }
        match (&self.kind[..], &self.target) {
            ("mirred", ActionTarget::Mirred { target_ifindex, .. }) => {
                if !known_ifindexes.contains(target_ifindex) {
                    return invalid("mirred action targets an unobserved interface");
                }
            }
            ("mirred", ActionTarget::None) => return invalid("mirred action has no typed target"),
            (_, ActionTarget::Mirred { .. }) => {
                return invalid("non-mirred action carries a mirred target")
            }
            (_, ActionTarget::None) => {}
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct FilterRecord {
    pub ifindex: u32,
    pub parent: u32,
    pub chain: u32,
    pub priority: u16,
    pub protocol: u16,
    pub handle: u32,
    pub kind: String,
    /// Parsed, configuration-only classifier payload.
    pub options: CanonicalConfig,
    pub actions: Vec<ActionRecord>,
}

impl FilterRecord {
    fn validate(&self, known_ifindexes: &BTreeSet<u32>) -> Result<(), KernelTopologyError> {
        if !known_ifindexes.contains(&self.ifindex) {
            return invalid("filter references an unobserved interface");
        }
        if self.priority == 0 || self.protocol == 0 || self.handle == 0 {
            return invalid("filter priority, protocol and handle must be non-zero");
        }
        if self.actions.len() > MAX_ACTIONS_PER_FILTER {
            return limit("filter action count exceeds its bound");
        }
        validate_kind("filter kind", &self.kind)?;
        self.options.validate("filter options")?;
        let mut orders = BTreeSet::new();
        let mut indexes = BTreeSet::new();
        for action in &self.actions {
            action.validate(known_ifindexes)?;
            if !orders.insert(action.order) || !indexes.insert(action.action_index) {
                return invalid("filter contains duplicate action order or index");
            }
        }
        if orders.iter().copied().ne(1..=orders.len() as u16) {
            return invalid("filter action orders must be contiguous from one");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KernelTopologyQuery {
    pub target_interface: String,
    pub route: OperationRouteIdentity,
    pub private_namespace: PrivateNamespaceRecord,
}

impl KernelTopologyQuery {
    pub fn validate(&self) -> Result<(), KernelTopologyError> {
        validate_interface("kernel topology target", &self.target_interface)?;
        self.route
            .validate()
            .map_err(KernelTopologyError::Invalid)?;
        self.private_namespace.validate()?;
        if self.route.l3_device != self.target_interface {
            return invalid("kernel topology target does not match route L3 device");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KernelTopologySnapshot {
    pub netns_cookie: u64,
    pub route: OperationRouteIdentity,
    pub target: LinkRecord,
    pub private_namespace: PrivateNamespaceRecord,
    /// Bounded non-target links needed for collision proof. Representation
    /// permits unrelated links; stage policy decides ownership.
    pub private_links: Vec<LinkRecord>,
    pub root_qdiscs: Vec<QdiscRecord>,
    pub ingress_qdiscs: Vec<QdiscRecord>,
    pub ingress_filters: Vec<FilterRecord>,
    pub egress_filters: Vec<FilterRecord>,
    /// Only action-table rows colliding with the reserved namespace are kept.
    /// Unrelated global actions are intentionally outside this witness.
    pub private_actions: Vec<PrivateActionIdentity>,
}

impl KernelTopologySnapshot {
    pub fn validate(&self) -> Result<(), KernelTopologyError> {
        if self.netns_cookie == 0 {
            return invalid("network namespace cookie must be non-zero");
        }
        self.route
            .validate()
            .map_err(KernelTopologyError::Invalid)?;
        self.target.validate("target link")?;
        self.private_namespace.validate()?;
        if self.route.l3_device != self.target.name {
            return invalid("snapshot target link does not match route L3 device");
        }
        if self.private_links.len() > MAX_PRIVATE_LINKS {
            return limit("private link count exceeds its bound");
        }
        if self
            .root_qdiscs
            .len()
            .saturating_add(self.ingress_qdiscs.len())
            > MAX_QDISCS
        {
            return limit("qdisc count exceeds its bound");
        }
        if self
            .ingress_filters
            .len()
            .saturating_add(self.egress_filters.len())
            > MAX_FILTERS
        {
            return limit("filter count exceeds its bound");
        }
        if self.private_actions.len() > MAX_NAMESPACE_IDENTITIES {
            return limit("private action count exceeds its bound");
        }
        for identity in &self.private_actions {
            identity.validate("observed private action")?;
        }
        validate_action_identity_set(&self.private_actions, "observed private actions", false)?;

        let mut known_ifindexes = BTreeSet::from([self.target.ifindex]);
        let mut link_identities = BTreeSet::new();
        insert_link_identities(&mut link_identities, &self.target)?;
        for link in &self.private_links {
            link.validate("private link")?;
            if !known_ifindexes.insert(link.ifindex) {
                return invalid("snapshot contains duplicate link identity");
            }
            insert_link_identities(&mut link_identities, link)?;
        }

        let mut qdisc_keys = BTreeSet::new();
        for qdisc in &self.root_qdiscs {
            qdisc.validate(&known_ifindexes)?;
            if qdisc.parent != TC_H_ROOT {
                return invalid("root qdisc record has a non-root parent");
            }
            if !qdisc_keys.insert((qdisc.ifindex, qdisc.parent)) {
                return invalid("snapshot contains duplicate qdisc slot");
            }
        }
        for qdisc in &self.ingress_qdiscs {
            qdisc.validate(&known_ifindexes)?;
            if qdisc.ifindex != self.target.ifindex
                || qdisc.parent != TC_H_INGRESS
                || qdisc.handle != TC_H_CLSACT_HANDLE
                || !matches!(qdisc.kind.as_str(), "ingress" | "clsact")
            {
                return invalid("ingress qdisc is not bound to the target ingress/clsact hook");
            }
            if !qdisc_keys.insert((qdisc.ifindex, qdisc.parent)) {
                return invalid("snapshot contains duplicate qdisc slot");
            }
        }

        let mut filter_keys = BTreeSet::new();
        for filter in &self.ingress_filters {
            filter.validate(&known_ifindexes)?;
            if filter.ifindex != self.target.ifindex || filter.parent != TC_H_MIN_INGRESS {
                return invalid("ingress filter is not bound to the target ingress hook");
            }
            if !filter_keys.insert(filter_identity(filter)) {
                return invalid("snapshot contains duplicate filter identity");
            }
        }
        for filter in &self.egress_filters {
            filter.validate(&known_ifindexes)?;
            if filter.ifindex != self.target.ifindex || filter.parent != TC_H_MIN_EGRESS {
                return invalid("egress filter is not bound to the target egress hook");
            }
            if !filter_keys.insert(filter_identity(filter)) {
                return invalid("snapshot contains duplicate filter identity");
            }
        }
        if !self.ingress_filters.is_empty() && self.ingress_qdiscs.is_empty() {
            return invalid("ingress filters are present without an ingress/clsact qdisc");
        }
        if !self.egress_filters.is_empty()
            && !self
                .ingress_qdiscs
                .iter()
                .any(|qdisc| qdisc.kind == "clsact")
        {
            return invalid("egress filters are present without a clsact qdisc");
        }
        Ok(())
    }

    pub fn ensure_query_binding(
        &self,
        query: &KernelTopologyQuery,
    ) -> Result<(), KernelTopologyError> {
        query.validate()?;
        if self.target.name != query.target_interface
            || self.route != query.route
            || self.private_namespace.normalized() != query.private_namespace.normalized()
        {
            return invalid("kernel topology snapshot does not match its read-only query");
        }
        Ok(())
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>, KernelTopologyError> {
        self.validate()?;
        let mut writer = CanonicalWriter::default();
        writer.bytes(CANONICAL_DOMAIN)?;
        writer.u16(KERNEL_TOPOLOGY_SCHEMA)?;
        writer.record(1, |record| encode_route(record, &self.route))?;
        writer.record(2, |record| {
            record.u64(self.netns_cookie)?;
            encode_link(record, &self.target)
        })?;
        writer.record(3, |record| {
            encode_namespace(record, &self.private_namespace.normalized())
        })?;
        encode_sorted_links(&mut writer, 4, &self.private_links)?;
        encode_sorted_qdiscs(&mut writer, 5, &self.root_qdiscs)?;
        encode_sorted_qdiscs(&mut writer, 6, &self.ingress_qdiscs)?;
        encode_sorted_filters(&mut writer, 7, &self.ingress_filters)?;
        encode_sorted_filters(&mut writer, 8, &self.egress_filters)?;
        encode_action_identities(&mut writer, 9, &self.private_actions)?;
        Ok(writer.finish())
    }
}

/// The complete topology that a future permit authorizes the operation to own.
///
/// This is deliberately separate from [`KernelTopologySnapshot`]: snapshots
/// describe facts, while this value describes planned authority and slots.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExpectedOwnedTopology {
    pub private_links: Vec<LinkRecord>,
    pub root_qdiscs: Vec<QdiscRecord>,
    pub ingress_qdiscs: Vec<QdiscRecord>,
    pub ingress_filters: Vec<FilterRecord>,
    pub egress_filters: Vec<FilterRecord>,
    pub private_actions: Vec<PrivateActionIdentity>,
}

/// Stage policy for positive absence checks and future exact temporary-owner
/// checks. Bootstrap admission currently uses `BarePreflight`; permanent SQM
/// candidates are attested through their managed Config/CAKE/ingress contract
/// rather than the calibration-private namespace represented here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KernelTopologyStagePolicy {
    /// No planned resource or slot may already be occupied.
    BarePreflight(ExpectedOwnedTopology),
    /// Every colliding resource must exactly equal the complete expected set.
    TemporaryOwned(ExpectedOwnedTopology),
}

impl KernelTopologySnapshot {
    pub fn validate_stage_policy(
        &self,
        policy: &KernelTopologyStagePolicy,
    ) -> Result<(), KernelTopologyError> {
        self.validate()?;
        let expected = match policy {
            KernelTopologyStagePolicy::BarePreflight(expected)
            | KernelTopologyStagePolicy::TemporaryOwned(expected) => expected,
        };
        expected.validate_for(self)?;

        let colliding = self.colliding_topology(expected);
        match policy {
            KernelTopologyStagePolicy::BarePreflight(_) => {
                if colliding.is_empty() {
                    Ok(())
                } else {
                    invalid("bare kernel topology preflight found an occupied ownership slot")
                }
            }
            KernelTopologyStagePolicy::TemporaryOwned(_) => {
                if colliding.normalized() == expected.normalized() {
                    Ok(())
                } else {
                    invalid(
                        "temporary kernel topology does not exactly match the expected owned set",
                    )
                }
            }
        }
    }

    fn colliding_topology(&self, expected: &ExpectedOwnedTopology) -> ExpectedOwnedTopology {
        let namespace = &self.private_namespace;
        let mut colliding_links = self
            .private_links
            .iter()
            .filter(|link| namespace.matches_link(link))
            .cloned()
            .collect::<Vec<_>>();
        if namespace.matches_link(&self.target) {
            colliding_links.push(self.target.clone());
        }
        ExpectedOwnedTopology {
            private_links: colliding_links,
            root_qdiscs: self
                .root_qdiscs
                .iter()
                .filter(|qdisc| qdisc_collides(qdisc, namespace, expected))
                .cloned()
                .collect(),
            ingress_qdiscs: self
                .ingress_qdiscs
                .iter()
                .filter(|qdisc| qdisc_collides(qdisc, namespace, expected))
                .cloned()
                .collect(),
            ingress_filters: self
                .ingress_filters
                .iter()
                .filter(|filter| filter_collides(filter, namespace, expected))
                .cloned()
                .collect(),
            egress_filters: self
                .egress_filters
                .iter()
                .filter(|filter| filter_collides(filter, namespace, expected))
                .cloned()
                .collect(),
            private_actions: self
                .private_actions
                .iter()
                .filter(|action| action_identity_collides(action, namespace))
                .cloned()
                .collect(),
        }
    }
}

impl ExpectedOwnedTopology {
    fn is_empty(&self) -> bool {
        self.private_links.is_empty()
            && self.root_qdiscs.is_empty()
            && self.ingress_qdiscs.is_empty()
            && self.ingress_filters.is_empty()
            && self.egress_filters.is_empty()
            && self.private_actions.is_empty()
    }

    fn normalized(&self) -> Self {
        let mut normalized = self.clone();
        normalized.private_links.sort();
        normalized.root_qdiscs.sort();
        normalized.ingress_qdiscs.sort();
        normalize_filters(&mut normalized.ingress_filters);
        normalize_filters(&mut normalized.egress_filters);
        normalized.private_actions.sort();
        normalized
    }

    fn validate_for(&self, observed: &KernelTopologySnapshot) -> Result<(), KernelTopologyError> {
        let expected = KernelTopologySnapshot {
            netns_cookie: observed.netns_cookie,
            route: observed.route.clone(),
            target: observed.target.clone(),
            private_namespace: observed.private_namespace.clone(),
            private_links: self.private_links.clone(),
            root_qdiscs: self.root_qdiscs.clone(),
            ingress_qdiscs: self.ingress_qdiscs.clone(),
            ingress_filters: self.ingress_filters.clone(),
            egress_filters: self.egress_filters.clone(),
            private_actions: self.private_actions.clone(),
        };
        expected.validate()?;

        let namespace = &observed.private_namespace;
        if self
            .private_links
            .iter()
            .any(|link| !matches!(&link.kind, KernelLinkKind::Named(_)))
        {
            return invalid("expected owned private link lacks an exact named kind");
        }
        if self.private_links.iter().any(|link| {
            !namespace.link_names.iter().any(|name| name == &link.name)
                || link.alias.as_ref().is_some_and(|alias| {
                    !namespace
                        .link_aliases
                        .iter()
                        .any(|reserved| reserved == alias)
                })
        }) {
            return invalid("expected owned link lacks its exact reserved name or alias");
        }
        if self
            .root_qdiscs
            .iter()
            .chain(self.ingress_qdiscs.iter())
            .any(|qdisc| !namespace.qdisc_handles.contains(&qdisc.handle))
        {
            return invalid("expected owned qdisc lacks a reserved handle");
        }
        if self
            .ingress_filters
            .iter()
            .chain(self.egress_filters.iter())
            .any(|filter| {
                !namespace.filter_handles.contains(&filter.handle)
                    || !namespace.filter_priorities.contains(&filter.priority)
            })
        {
            return invalid("expected owned filter lacks a reserved identity");
        }
        let expected_filter_actions = self
            .ingress_filters
            .iter()
            .chain(self.egress_filters.iter())
            .flat_map(|filter| &filter.actions)
            .map(action_identity)
            .collect::<BTreeSet<_>>();
        let expected_table_actions = self
            .private_actions
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        if expected_filter_actions != expected_table_actions {
            return invalid("expected owned action table does not match filter actions");
        }
        for action in &self.private_actions {
            if !namespace
                .action_identities
                .iter()
                .any(|reserved| reserved == action)
            {
                return invalid("expected owned action lacks a reserved identity");
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KernelTopologyRead {
    pub snapshot: KernelTopologySnapshot,
    pub observed_bytes: usize,
    pub unknown_ownership_attributes: Vec<UnknownOwnershipAttribute>,
}

impl KernelTopologyRead {
    fn validate_read(&self) -> Result<(), KernelTopologyError> {
        if self.observed_bytes == 0 {
            return invalid("kernel topology dump byte count must be non-zero");
        }
        if self.observed_bytes > MAX_OBSERVED_BYTES {
            return limit("kernel topology dump exceeds its byte bound");
        }
        if self.unknown_ownership_attributes.len() > MAX_UNKNOWN_ATTRIBUTES {
            return limit("unknown ownership attribute count exceeds its bound");
        }
        for unknown in &self.unknown_ownership_attributes {
            if unknown.attribute_type == 0 || unknown.nested_path.len() > MAX_ATTRIBUTE_PATH {
                return invalid("unknown ownership attribute metadata is malformed");
            }
        }
        if let Some(unknown) = self.unknown_ownership_attributes.first() {
            return Err(KernelTopologyError::UnknownOwnershipAttribute {
                object: unknown.object,
                attribute_type: unknown.attribute_type,
            });
        }
        Ok(())
    }
}

/// Read-only abstraction. It intentionally exposes no mutation method.
pub trait KernelTopologyReadBackend {
    fn read_topology(
        &mut self,
        query: &KernelTopologyQuery,
    ) -> Result<KernelTopologyRead, KernelTopologyError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KernelTopologyDigest([u8; 32]);

impl KernelTopologyDigest {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn to_hex(self) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut output = String::with_capacity(64);
        for byte in self.0 {
            output.push(HEX[(byte >> 4) as usize] as char);
            output.push(HEX[(byte & 0x0f) as usize] as char);
        }
        output
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KernelTopologyWitness {
    snapshot: KernelTopologySnapshot,
    canonical: Vec<u8>,
    digest: KernelTopologyDigest,
}

impl KernelTopologyWitness {
    pub fn snapshot(&self) -> &KernelTopologySnapshot {
        &self.snapshot
    }

    pub fn canonical_bytes(&self) -> &[u8] {
        &self.canonical
    }

    pub fn digest(&self) -> KernelTopologyDigest {
        self.digest
    }

    pub fn ensure_exact_reattestation(&self, current: &Self) -> Result<(), KernelTopologyError> {
        if self.canonical == current.canonical && self.digest == current.digest {
            Ok(())
        } else {
            invalid("kernel topology changed during exact re-attestation")
        }
    }
}

pub fn capture_read_only_witness(
    backend: &mut impl KernelTopologyReadBackend,
    query: &KernelTopologyQuery,
) -> Result<KernelTopologyWitness, KernelTopologyError> {
    query.validate()?;
    let read = backend.read_topology(query)?;
    read.validate_read()?;
    read.snapshot.ensure_query_binding(query)?;
    let canonical = read.snapshot.canonical_bytes()?;
    let raw_digest = digest(&SHA256, &canonical);
    let mut digest_bytes = [0u8; 32];
    digest_bytes.copy_from_slice(raw_digest.as_ref());
    Ok(KernelTopologyWitness {
        snapshot: read.snapshot,
        canonical,
        digest: KernelTopologyDigest(digest_bytes),
    })
}

fn encode_route(
    writer: &mut CanonicalWriter,
    route: &OperationRouteIdentity,
) -> Result<(), KernelTopologyError> {
    writer.string(route.mode.as_str())?;
    writer.optional_string(route.mwan3_member.as_deref())?;
    writer.string(&route.l3_device)?;
    match route.source_ip {
        None => writer.u8(0)?,
        Some(IpAddr::V4(address)) => {
            writer.u8(4)?;
            writer.bytes(&address.octets())?;
        }
        Some(IpAddr::V6(address)) => {
            writer.u8(6)?;
            writer.bytes(&address.octets())?;
        }
    }
    writer.optional_u32(route.fwmark)?;
    writer.optional_u32(route.routing_table)
}

fn encode_link(writer: &mut CanonicalWriter, link: &LinkRecord) -> Result<(), KernelTopologyError> {
    writer.u32(link.ifindex)?;
    writer.string(&link.name)?;
    writer.optional_string(link.alias.as_deref())?;
    match &link.kind {
        KernelLinkKind::Absent => writer.u8(0)?,
        KernelLinkKind::Named(kind) => {
            writer.u8(1)?;
            writer.string(kind)?;
        }
    }
    writer.optional_u32(link.parent_ifindex)
}

fn encode_config(
    writer: &mut CanonicalWriter,
    config: &CanonicalConfig,
) -> Result<(), KernelTopologyError> {
    config.validate("canonical configuration")?;
    let normalized = config.normalized();
    writer.collection(1, normalized.attributes.len(), |writer, index| {
        let attribute = &normalized.attributes[index];
        writer.u16s(&attribute.path)?;
        writer.u8(u8::from(attribute.network_byte_order))?;
        writer.bytes(&attribute.value)
    })
}

fn encode_namespace(
    writer: &mut CanonicalWriter,
    namespace: &PrivateNamespaceRecord,
) -> Result<(), KernelTopologyError> {
    writer.strings(&namespace.link_names)?;
    writer.strings(&namespace.link_aliases)?;
    writer.u32s(&namespace.qdisc_handles)?;
    writer.u32s(&namespace.filter_handles)?;
    writer.u16s(&namespace.filter_priorities)?;
    encode_action_identity_values(writer, &namespace.action_identities)
}

fn encode_action_identities(
    writer: &mut CanonicalWriter,
    tag: u16,
    identities: &[PrivateActionIdentity],
) -> Result<(), KernelTopologyError> {
    let mut ordered = identities.iter().collect::<Vec<_>>();
    ordered.sort();
    writer.collection(tag, ordered.len(), |writer, index| {
        encode_action_identity(writer, ordered[index])
    })
}

fn encode_action_identity_values(
    writer: &mut CanonicalWriter,
    identities: &[PrivateActionIdentity],
) -> Result<(), KernelTopologyError> {
    let mut ordered = identities.iter().collect::<Vec<_>>();
    ordered.sort();
    writer.u32(checked_u32(ordered.len(), "action identity count")?)?;
    for identity in ordered {
        writer.record(1, |record| encode_action_identity(record, identity))?;
    }
    Ok(())
}

fn encode_action_identity(
    writer: &mut CanonicalWriter,
    identity: &PrivateActionIdentity,
) -> Result<(), KernelTopologyError> {
    identity.validate("action identity")?;
    writer.string(&identity.kind)?;
    writer.u32(identity.index)?;
    writer.optional_bytes(identity.cookie.as_deref())
}

fn encode_sorted_links(
    writer: &mut CanonicalWriter,
    tag: u16,
    links: &[LinkRecord],
) -> Result<(), KernelTopologyError> {
    let mut ordered = links.iter().collect::<Vec<_>>();
    ordered.sort();
    writer.collection(tag, ordered.len(), |writer, index| {
        encode_link(writer, ordered[index])
    })
}

fn encode_sorted_qdiscs(
    writer: &mut CanonicalWriter,
    tag: u16,
    qdiscs: &[QdiscRecord],
) -> Result<(), KernelTopologyError> {
    let mut ordered = qdiscs.iter().collect::<Vec<_>>();
    ordered.sort();
    writer.collection(tag, ordered.len(), |writer, index| {
        let qdisc = ordered[index];
        writer.u32(qdisc.ifindex)?;
        writer.u32(qdisc.parent)?;
        writer.u32(qdisc.handle)?;
        writer.string(&qdisc.kind)?;
        encode_config(writer, &qdisc.options)
    })
}

fn encode_sorted_filters(
    writer: &mut CanonicalWriter,
    tag: u16,
    filters: &[FilterRecord],
) -> Result<(), KernelTopologyError> {
    let mut ordered = filters.iter().collect::<Vec<_>>();
    ordered.sort_by(|left, right| filter_key(left).cmp(&filter_key(right)));
    writer.collection(tag, ordered.len(), |writer, index| {
        let filter = ordered[index];
        writer.u32(filter.ifindex)?;
        writer.u32(filter.parent)?;
        writer.u32(filter.chain)?;
        writer.u16(filter.priority)?;
        writer.u16(filter.protocol)?;
        writer.u32(filter.handle)?;
        writer.string(&filter.kind)?;
        encode_config(writer, &filter.options)?;
        let mut actions = filter.actions.iter().collect::<Vec<_>>();
        actions.sort_by_key(|action| action.order);
        writer.u32(checked_u32(actions.len(), "action count")?)?;
        for action in actions {
            writer.record(1, |record| encode_action(record, action))?;
        }
        Ok(())
    })
}

fn filter_key(filter: &FilterRecord) -> (u32, u32, u32, u16, u16, u32, &str) {
    (
        filter.ifindex,
        filter.parent,
        filter.chain,
        filter.priority,
        filter.protocol,
        filter.handle,
        &filter.kind,
    )
}

fn filter_identity(filter: &FilterRecord) -> (u32, u32, u32, u16, u16, u32) {
    (
        filter.ifindex,
        filter.parent,
        filter.chain,
        filter.priority,
        filter.protocol,
        filter.handle,
    )
}

fn filter_slot(filter: &FilterRecord) -> (u32, u32, u32, u16, u16) {
    (
        filter.ifindex,
        filter.parent,
        filter.chain,
        filter.priority,
        filter.protocol,
    )
}

fn insert_link_identities(
    identities: &mut BTreeSet<String>,
    link: &LinkRecord,
) -> Result<(), KernelTopologyError> {
    if !identities.insert(link.name.clone())
        || link
            .alias
            .as_ref()
            .is_some_and(|alias| !identities.insert(alias.clone()))
    {
        return invalid("snapshot contains an ambiguous link name or alias collision");
    }
    Ok(())
}

fn qdisc_collides(
    qdisc: &QdiscRecord,
    namespace: &PrivateNamespaceRecord,
    expected: &ExpectedOwnedTopology,
) -> bool {
    namespace.qdisc_handles.contains(&qdisc.handle)
        || expected
            .root_qdiscs
            .iter()
            .chain(expected.ingress_qdiscs.iter())
            .any(|planned| (planned.ifindex, planned.parent) == (qdisc.ifindex, qdisc.parent))
}

fn action_has_reserved_identity(action: &ActionRecord, namespace: &PrivateNamespaceRecord) -> bool {
    action_identity_collides(&action_identity(action), namespace)
}

fn action_identity(action: &ActionRecord) -> PrivateActionIdentity {
    PrivateActionIdentity {
        kind: action.kind.clone(),
        index: action.action_index,
        cookie: action.cookie.clone(),
    }
}

fn action_identity_collides(
    observed: &PrivateActionIdentity,
    namespace: &PrivateNamespaceRecord,
) -> bool {
    namespace.action_identities.iter().any(|reserved| {
        (reserved.kind == observed.kind && reserved.index == observed.index)
            || reserved.cookie.as_ref().is_some_and(|cookie| {
                observed.kind == reserved.kind && observed.cookie.as_ref() == Some(cookie)
            })
    })
}

fn validate_action_identity_set(
    identities: &[PrivateActionIdentity],
    label: &str,
    require_unique_cookies: bool,
) -> Result<(), KernelTopologyError> {
    let mut keys = BTreeSet::new();
    let mut cookies = BTreeSet::new();
    for identity in identities {
        if !keys.insert((identity.kind.clone(), identity.index)) {
            return invalid(format!("{label} contain duplicate kind/index identities"));
        }
        if require_unique_cookies
            && identity
                .cookie
                .as_ref()
                .is_some_and(|cookie| !cookies.insert((identity.kind.clone(), cookie.clone())))
        {
            return invalid(format!("{label} contain duplicate kind/cookie identities"));
        }
    }
    Ok(())
}

fn filter_has_reserved_identity(filter: &FilterRecord, namespace: &PrivateNamespaceRecord) -> bool {
    namespace.filter_handles.contains(&filter.handle)
        || namespace.filter_priorities.contains(&filter.priority)
        || filter
            .actions
            .iter()
            .any(|action| action_has_reserved_identity(action, namespace))
}

fn filter_collides(
    filter: &FilterRecord,
    namespace: &PrivateNamespaceRecord,
    expected: &ExpectedOwnedTopology,
) -> bool {
    filter_has_reserved_identity(filter, namespace)
        || expected
            .ingress_filters
            .iter()
            .chain(expected.egress_filters.iter())
            .any(|planned| filter_slot(planned) == filter_slot(filter))
        || expected.ingress_qdiscs.iter().any(|planned_hook| {
            planned_hook.ifindex == filter.ifindex
                && matches!(filter.parent, TC_H_MIN_INGRESS | TC_H_MIN_EGRESS)
        })
}

fn normalize_filters(filters: &mut [FilterRecord]) {
    for filter in filters.iter_mut() {
        filter.actions.sort_by_key(|action| action.order);
    }
    filters.sort();
}

fn encode_action(
    writer: &mut CanonicalWriter,
    action: &ActionRecord,
) -> Result<(), KernelTopologyError> {
    writer.u16(action.order)?;
    writer.u32(action.action_index)?;
    writer.string(&action.kind)?;
    writer.optional_bytes(action.cookie.as_deref())?;
    encode_config(writer, &action.options)?;
    match action.target {
        ActionTarget::None => writer.u8(0)?,
        ActionTarget::Mirred {
            direction,
            target_ifindex,
        } => {
            writer.u8(1)?;
            writer.u8(direction.tag())?;
            writer.u32(target_ifindex)?;
        }
    }
    Ok(())
}

struct CanonicalWriter {
    bytes: Vec<u8>,
    limit: usize,
}

impl Default for CanonicalWriter {
    fn default() -> Self {
        Self {
            bytes: Vec::new(),
            limit: MAX_CANONICAL_BYTES,
        }
    }
}

impl CanonicalWriter {
    #[cfg(test)]
    fn with_limit(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
        }
    }

    fn finish(self) -> Vec<u8> {
        self.bytes
    }

    fn ensure_additional(&mut self, additional: usize) -> Result<(), KernelTopologyError> {
        let next = self.bytes.len().checked_add(additional).ok_or_else(|| {
            KernelTopologyError::Limit("canonical byte count overflow".to_string())
        })?;
        if next > self.limit {
            return limit("canonical kernel topology exceeds its byte bound");
        }
        if next <= self.bytes.capacity() {
            return Ok(());
        }
        const RESERVE_CHUNK: usize = 4 * 1024;
        let requested_capacity = next
            .checked_add(RESERVE_CHUNK - 1)
            .map(|rounded| (rounded / RESERVE_CHUNK) * RESERVE_CHUNK)
            .unwrap_or(self.limit)
            .min(self.limit);
        self.bytes
            .try_reserve_exact(requested_capacity - self.bytes.len())
            .map_err(|_| KernelTopologyError::Limit("canonical allocation failed".to_string()))
    }

    fn append(&mut self, value: &[u8]) -> Result<(), KernelTopologyError> {
        self.ensure_additional(value.len())?;
        self.bytes.extend_from_slice(value);
        Ok(())
    }

    fn u8(&mut self, value: u8) -> Result<(), KernelTopologyError> {
        self.append(&[value])
    }

    fn u16(&mut self, value: u16) -> Result<(), KernelTopologyError> {
        self.append(&value.to_be_bytes())
    }

    fn u32(&mut self, value: u32) -> Result<(), KernelTopologyError> {
        self.append(&value.to_be_bytes())
    }

    fn u64(&mut self, value: u64) -> Result<(), KernelTopologyError> {
        self.append(&value.to_be_bytes())
    }

    fn bytes(&mut self, value: &[u8]) -> Result<(), KernelTopologyError> {
        let length = checked_u32(value.len(), "canonical byte string")?;
        let additional = 4usize.checked_add(value.len()).ok_or_else(|| {
            KernelTopologyError::Limit("canonical byte count overflow".to_string())
        })?;
        self.ensure_additional(additional)?;
        self.bytes.extend_from_slice(&length.to_be_bytes());
        self.bytes.extend_from_slice(value);
        Ok(())
    }

    fn string(&mut self, value: &str) -> Result<(), KernelTopologyError> {
        self.bytes(value.as_bytes())
    }

    fn optional_bytes(&mut self, value: Option<&[u8]>) -> Result<(), KernelTopologyError> {
        match value {
            None => self.u8(0),
            Some(value) => {
                let additional = 5usize.checked_add(value.len()).ok_or_else(|| {
                    KernelTopologyError::Limit("canonical byte count overflow".to_string())
                })?;
                let length = checked_u32(value.len(), "canonical optional byte string")?;
                self.ensure_additional(additional)?;
                self.bytes.push(1);
                self.bytes.extend_from_slice(&length.to_be_bytes());
                self.bytes.extend_from_slice(value);
                Ok(())
            }
        }
    }

    fn optional_string(&mut self, value: Option<&str>) -> Result<(), KernelTopologyError> {
        self.optional_bytes(value.map(str::as_bytes))
    }

    fn optional_u32(&mut self, value: Option<u32>) -> Result<(), KernelTopologyError> {
        match value {
            None => self.u8(0),
            Some(value) => {
                self.ensure_additional(5)?;
                self.bytes.push(1);
                self.bytes.extend_from_slice(&value.to_be_bytes());
                Ok(())
            }
        }
    }

    fn strings(&mut self, values: &[String]) -> Result<(), KernelTopologyError> {
        self.u32(checked_u32(values.len(), "string count")?)?;
        for value in values {
            self.string(value)?;
        }
        Ok(())
    }

    fn u32s(&mut self, values: &[u32]) -> Result<(), KernelTopologyError> {
        self.u32(checked_u32(values.len(), "u32 identity count")?)?;
        for value in values {
            self.u32(*value)?;
        }
        Ok(())
    }

    fn u16s(&mut self, values: &[u16]) -> Result<(), KernelTopologyError> {
        self.u32(checked_u32(values.len(), "u16 identity count")?)?;
        for value in values {
            self.u16(*value)?;
        }
        Ok(())
    }

    fn byte_vectors(&mut self, values: &[Vec<u8>]) -> Result<(), KernelTopologyError> {
        self.u32(checked_u32(values.len(), "byte vector count")?)?;
        for value in values {
            self.bytes(value)?;
        }
        Ok(())
    }

    fn record(
        &mut self,
        tag: u16,
        encode: impl FnOnce(&mut CanonicalWriter) -> Result<(), KernelTopologyError>,
    ) -> Result<(), KernelTopologyError> {
        self.ensure_additional(6)?;
        self.bytes.extend_from_slice(&tag.to_be_bytes());
        let length_position = self.bytes.len();
        self.bytes.extend_from_slice(&0u32.to_be_bytes());
        let payload_start = self.bytes.len();
        encode(self)?;
        let payload_length = checked_u32(self.bytes.len() - payload_start, "canonical record")?;
        self.bytes[length_position..length_position + 4]
            .copy_from_slice(&payload_length.to_be_bytes());
        Ok(())
    }

    fn collection(
        &mut self,
        tag: u16,
        count: usize,
        mut encode: impl FnMut(&mut CanonicalWriter, usize) -> Result<(), KernelTopologyError>,
    ) -> Result<(), KernelTopologyError> {
        self.record(tag, |record| {
            record.u32(checked_u32(count, "canonical collection count")?)?;
            for index in 0..count {
                record.record(1, |item| encode(item, index))?;
            }
            Ok(())
        })
    }
}

fn checked_u32(value: usize, label: &str) -> Result<u32, KernelTopologyError> {
    u32::try_from(value).map_err(|_| KernelTopologyError::Limit(format!("{label} exceeds u32")))
}

fn validate_interface(label: &str, value: &str) -> Result<(), KernelTopologyError> {
    validate_identifier(label, value, MAX_INTERFACE_NAME_BYTES, b"._:@-")
}

fn valid_kernel_ifindex(ifindex: u32) -> bool {
    ifindex != 0 && ifindex <= i32::MAX as u32
}

fn validate_kind(label: &str, value: &str) -> Result<(), KernelTopologyError> {
    validate_identifier(label, value, MAX_KIND_BYTES, b"_-")
}

fn validate_alias(label: &str, value: &str) -> Result<(), KernelTopologyError> {
    if value.is_empty()
        || value.len() > MAX_ALIAS_BYTES
        || value
            .bytes()
            .any(|byte| !byte.is_ascii_graphic() && byte != b' ')
    {
        return invalid(format!("{label} is invalid"));
    }
    Ok(())
}

fn validate_identifier(
    label: &str,
    value: &str,
    maximum: usize,
    punctuation: &[u8],
) -> Result<(), KernelTopologyError> {
    if value.is_empty()
        || value.len() > maximum
        || value.bytes().any(|byte| {
            !byte.is_ascii_alphanumeric() && !punctuation.iter().any(|allowed| *allowed == byte)
        })
    {
        return invalid(format!("{label} is invalid"));
    }
    Ok(())
}

fn ensure_unique<T: Ord + Clone>(values: &[T], label: &str) -> Result<(), KernelTopologyError> {
    let mut unique = BTreeSet::new();
    if values.iter().any(|value| !unique.insert(value.clone())) {
        return invalid(format!("{label} contain duplicate identities"));
    }
    Ok(())
}

fn invalid<T>(message: impl Into<String>) -> Result<T, KernelTopologyError> {
    Err(KernelTopologyError::Invalid(message.into()))
}

fn limit<T>(message: impl Into<String>) -> Result<T, KernelTopologyError> {
    Err(KernelTopologyError::Limit(message.into()))
}

#[cfg(test)]
mod tests {
    use super::super::protocol::OperationRouteMode;
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    struct FakeBackend {
        next: Option<Result<KernelTopologyRead, KernelTopologyError>>,
        reads: usize,
    }

    impl FakeBackend {
        fn returning(read: KernelTopologyRead) -> Self {
            Self {
                next: Some(Ok(read)),
                reads: 0,
            }
        }
    }

    impl KernelTopologyReadBackend for FakeBackend {
        fn read_topology(
            &mut self,
            _query: &KernelTopologyQuery,
        ) -> Result<KernelTopologyRead, KernelTopologyError> {
            self.reads += 1;
            self.next
                .take()
                .unwrap_or_else(|| Err(KernelTopologyError::Backend("fake exhausted".to_string())))
        }
    }

    fn namespace() -> PrivateNamespaceRecord {
        PrivateNamespaceRecord {
            link_names: vec!["catf00112233".to_string(), "catf44556677".to_string()],
            link_aliases: vec![
                "cake-autotune-00112233".to_string(),
                "cake-autotune-44556677".to_string(),
            ],
            qdisc_handles: vec![0xa001_0000, 0xb001_0000, TC_H_CLSACT_HANDLE],
            filter_handles: vec![0x1001, 0x1002],
            filter_priorities: vec![49_152, 49_153],
            action_identities: vec![
                PrivateActionIdentity {
                    kind: "mirred".to_string(),
                    index: 0x1001 + 100,
                    cookie: Some(vec![0x65; 16]),
                },
                PrivateActionIdentity {
                    kind: "gact".to_string(),
                    index: 0x1001 + 200,
                    cookie: None,
                },
                PrivateActionIdentity {
                    kind: "mirred".to_string(),
                    index: 0x1002 + 100,
                    cookie: Some(vec![0x66; 16]),
                },
                PrivateActionIdentity {
                    kind: "gact".to_string(),
                    index: 0x1002 + 200,
                    cookie: None,
                },
            ],
        }
    }

    fn route() -> OperationRouteIdentity {
        OperationRouteIdentity {
            mode: OperationRouteMode::Main,
            mwan3_member: None,
            l3_device: "eth0".to_string(),
            source_ip: Some(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1))),
            fwmark: None,
            routing_table: Some(254),
        }
    }

    fn query() -> KernelTopologyQuery {
        KernelTopologyQuery {
            target_interface: "eth0".to_string(),
            route: route(),
            private_namespace: namespace(),
        }
    }

    fn config(value: impl Into<Vec<u8>>) -> CanonicalConfig {
        CanonicalConfig {
            attributes: vec![CanonicalConfigAttribute {
                path: vec![1],
                network_byte_order: false,
                value: value.into(),
            }],
        }
    }

    fn action(order: u16, index: u32, target_ifindex: u32) -> ActionRecord {
        ActionRecord {
            order,
            action_index: index,
            kind: "mirred".to_string(),
            cookie: Some(vec![(index & 0xff) as u8; 16]),
            options: config(vec![0x10, order as u8]),
            target: ActionTarget::Mirred {
                direction: MirredDirection::EgressRedirect,
                target_ifindex,
            },
        }
    }

    fn filter(priority: u16, handle: u32) -> FilterRecord {
        FilterRecord {
            ifindex: 10,
            parent: 0xffff_fff2,
            chain: 0,
            priority,
            protocol: 3,
            handle,
            kind: "u32".to_string(),
            options: config(vec![0x20, priority as u8]),
            actions: vec![
                action(1, handle + 100, 20),
                ActionRecord {
                    order: 2,
                    action_index: handle + 200,
                    kind: "gact".to_string(),
                    cookie: None,
                    options: config(vec![0x30]),
                    target: ActionTarget::None,
                },
            ],
        }
    }

    fn snapshot() -> KernelTopologySnapshot {
        KernelTopologySnapshot {
            netns_cookie: 0x1122_3344_5566_7788,
            route: route(),
            target: LinkRecord {
                ifindex: 10,
                name: "eth0".to_string(),
                alias: None,
                kind: KernelLinkKind::Absent,
                parent_ifindex: None,
            },
            private_namespace: namespace(),
            private_links: vec![
                LinkRecord {
                    ifindex: 20,
                    name: "catf00112233".to_string(),
                    alias: Some("cake-autotune-00112233".to_string()),
                    kind: KernelLinkKind::Named("ifb".to_string()),
                    parent_ifindex: None,
                },
                LinkRecord {
                    ifindex: 21,
                    name: "catf44556677".to_string(),
                    alias: Some("cake-autotune-44556677".to_string()),
                    kind: KernelLinkKind::Named("ifb".to_string()),
                    parent_ifindex: None,
                },
            ],
            root_qdiscs: vec![
                QdiscRecord {
                    ifindex: 10,
                    parent: TC_H_ROOT,
                    handle: 0,
                    kind: "noqueue".to_string(),
                    options: CanonicalConfig::default(),
                },
                QdiscRecord {
                    ifindex: 20,
                    parent: TC_H_ROOT,
                    handle: 0xb001_0000,
                    kind: "cake".to_string(),
                    options: config(vec![0x40, 0x41]),
                },
            ],
            ingress_qdiscs: vec![QdiscRecord {
                ifindex: 10,
                parent: 0xffff_fff1,
                handle: 0xffff_0000,
                kind: "clsact".to_string(),
                options: CanonicalConfig::default(),
            }],
            ingress_filters: vec![filter(49_152, 0x1001), filter(49_153, 0x1002)],
            egress_filters: Vec::new(),
            private_actions: namespace().action_identities,
        }
    }

    fn expected_owned() -> ExpectedOwnedTopology {
        let snapshot = snapshot();
        ExpectedOwnedTopology {
            private_links: snapshot.private_links,
            root_qdiscs: vec![snapshot.root_qdiscs[1].clone()],
            ingress_qdiscs: snapshot.ingress_qdiscs,
            ingress_filters: snapshot.ingress_filters,
            egress_filters: snapshot.egress_filters,
            private_actions: snapshot.private_actions,
        }
    }

    fn bare_snapshot() -> KernelTopologySnapshot {
        let mut bare = snapshot();
        bare.private_links.clear();
        bare.root_qdiscs.truncate(1);
        bare.ingress_qdiscs.clear();
        bare.ingress_filters.clear();
        bare.egress_filters.clear();
        bare.private_actions.clear();
        bare
    }

    fn read(snapshot: KernelTopologySnapshot) -> KernelTopologyRead {
        KernelTopologyRead {
            snapshot,
            observed_bytes: 8 * 1024,
            unknown_ownership_attributes: Vec::new(),
        }
    }

    fn capture(
        snapshot: KernelTopologySnapshot,
    ) -> Result<KernelTopologyWitness, KernelTopologyError> {
        let mut backend = FakeBackend::returning(read(snapshot));
        let witness = capture_read_only_witness(&mut backend, &query());
        assert_eq!(backend.reads, 1);
        witness
    }

    #[test]
    fn record_permutation_has_identical_canonical_bytes_and_digest() {
        let original = snapshot();
        let mut permuted = original.clone();
        permuted.private_links.reverse();
        permuted.root_qdiscs.reverse();
        permuted.ingress_filters.reverse();
        for filter in &mut permuted.ingress_filters {
            filter.actions.reverse();
        }
        permuted.private_namespace.link_names.reverse();
        permuted.private_namespace.link_aliases.reverse();
        permuted.private_namespace.qdisc_handles.reverse();
        permuted.private_namespace.filter_handles.reverse();
        permuted.private_namespace.filter_priorities.reverse();
        permuted.private_namespace.action_identities.reverse();
        permuted.private_actions.reverse();

        let original = capture(original).unwrap();
        let permuted = capture(permuted).unwrap();
        assert_eq!(original.canonical_bytes(), permuted.canonical_bytes());
        assert_eq!(original.digest(), permuted.digest());
        assert_eq!(original.digest().to_hex().len(), 64);
    }

    #[test]
    fn optional_link_kind_and_config_attribute_order_are_canonical() {
        let mut named = snapshot();
        named.target.kind = KernelLinkKind::Named("ether".to_string());
        assert_ne!(
            snapshot().canonical_bytes().unwrap(),
            named.canonical_bytes().unwrap()
        );

        let mut ordered = snapshot();
        ordered.root_qdiscs[1].options = CanonicalConfig {
            attributes: vec![
                CanonicalConfigAttribute {
                    path: vec![2],
                    network_byte_order: true,
                    value: vec![0x20],
                },
                CanonicalConfigAttribute {
                    path: vec![1, 3],
                    network_byte_order: false,
                    value: vec![0x10],
                },
            ],
        };
        let mut permuted = ordered.clone();
        permuted.root_qdiscs[1].options.attributes.reverse();
        assert_eq!(
            ordered.canonical_bytes().unwrap(),
            permuted.canonical_bytes().unwrap()
        );

        let mut duplicate = ordered;
        let repeated_attribute = duplicate.root_qdiscs[1].options.attributes[0].clone();
        duplicate.root_qdiscs[1]
            .options
            .attributes
            .push(repeated_attribute);
        assert!(matches!(
            duplicate.validate(),
            Err(KernelTopologyError::Invalid(message)) if message.contains("duplicate attribute path")
        ));
    }

    #[test]
    fn configuration_action_and_ifindex_changes_produce_distinct_digests() {
        let baseline = capture(snapshot()).unwrap();

        let mut config_changed = snapshot();
        config_changed.root_qdiscs[1].options.attributes[0]
            .value
            .push(0x99);
        assert_ne!(baseline.digest(), capture(config_changed).unwrap().digest());

        let mut action_changed = snapshot();
        let ActionTarget::Mirred { direction, .. } =
            &mut action_changed.ingress_filters[0].actions[0].target
        else {
            panic!("fixture action is not mirred")
        };
        *direction = MirredDirection::EgressMirror;
        assert_ne!(baseline.digest(), capture(action_changed).unwrap().digest());

        let mut ifindex_changed = snapshot();
        ifindex_changed.private_links[0].ifindex = 22;
        ifindex_changed.root_qdiscs[1].ifindex = 22;
        let ActionTarget::Mirred { target_ifindex, .. } =
            &mut ifindex_changed.ingress_filters[0].actions[0].target
        else {
            panic!("fixture action is not mirred")
        };
        *target_ifindex = 22;
        let ActionTarget::Mirred { target_ifindex, .. } =
            &mut ifindex_changed.ingress_filters[1].actions[0].target
        else {
            panic!("fixture action is not mirred")
        };
        *target_ifindex = 22;
        assert_ne!(
            baseline.digest(),
            capture(ifindex_changed).unwrap().digest()
        );

        let original_canonical = snapshot().canonical_bytes().unwrap();
        let mut namespace_changed = snapshot();
        namespace_changed.private_namespace.action_identities[0].index += 10_000;
        assert_ne!(
            original_canonical,
            namespace_changed.canonical_bytes().unwrap()
        );
    }

    #[test]
    fn malformed_and_unknown_ownership_state_is_rejected() {
        let mut malformed = snapshot();
        malformed
            .ingress_filters
            .push(malformed.ingress_filters[0].clone());
        assert!(matches!(
            capture(malformed),
            Err(KernelTopologyError::Invalid(message))
                if message.contains("duplicate filter identity")
        ));

        let mut backend = FakeBackend::returning(KernelTopologyRead {
            snapshot: snapshot(),
            observed_bytes: 1024,
            unknown_ownership_attributes: vec![UnknownOwnershipAttribute {
                object: TopologyObject::Action,
                attribute_type: 99,
                nested_path: vec![1, 2],
            }],
        });
        assert!(matches!(
            capture_read_only_witness(&mut backend, &query()),
            Err(KernelTopologyError::UnknownOwnershipAttribute {
                object: TopologyObject::Action,
                attribute_type: 99
            })
        ));

        let mut malformed_unknown = FakeBackend::returning(KernelTopologyRead {
            snapshot: snapshot(),
            observed_bytes: 1024,
            unknown_ownership_attributes: vec![UnknownOwnershipAttribute {
                object: TopologyObject::Filter,
                attribute_type: 7,
                nested_path: vec![1; MAX_ATTRIBUTE_PATH + 1],
            }],
        });
        assert!(matches!(
            capture_read_only_witness(&mut malformed_unknown, &query()),
            Err(KernelTopologyError::Invalid(message))
                if message.contains("metadata is malformed")
        ));
    }

    #[test]
    fn observed_record_and_canonical_byte_bounds_are_enforced() {
        let mut empty_dump = FakeBackend::returning(KernelTopologyRead {
            snapshot: snapshot(),
            observed_bytes: 0,
            unknown_ownership_attributes: Vec::new(),
        });
        assert!(matches!(
            capture_read_only_witness(&mut empty_dump, &query()),
            Err(KernelTopologyError::Invalid(message)) if message.contains("non-zero")
        ));

        let mut oversized_dump = FakeBackend::returning(KernelTopologyRead {
            snapshot: snapshot(),
            observed_bytes: MAX_OBSERVED_BYTES + 1,
            unknown_ownership_attributes: Vec::new(),
        });
        assert!(matches!(
            capture_read_only_witness(&mut oversized_dump, &query()),
            Err(KernelTopologyError::Limit(message)) if message.contains("dump")
        ));

        let mut too_many = snapshot();
        too_many.root_qdiscs = (0..=MAX_QDISCS)
            .map(|offset| QdiscRecord {
                ifindex: 10,
                parent: TC_H_ROOT,
                handle: offset as u32 + 1,
                kind: "cake".to_string(),
                options: CanonicalConfig::default(),
            })
            .collect();
        assert!(matches!(
            capture(too_many),
            Err(KernelTopologyError::Limit(message)) if message.contains("qdisc count")
        ));
    }

    #[test]
    fn representation_enforces_exact_tc_hooks_but_allows_empty_vectors() {
        bare_snapshot().validate().unwrap();

        let mut wrong_qdisc_parent = snapshot();
        wrong_qdisc_parent.ingress_qdiscs[0].parent = TC_H_MIN_INGRESS;
        assert!(matches!(
            wrong_qdisc_parent.validate(),
            Err(KernelTopologyError::Invalid(message)) if message.contains("ingress/clsact hook")
        ));

        let mut wrong_qdisc_handle = snapshot();
        wrong_qdisc_handle.ingress_qdiscs[0].handle = 0x1234_0000;
        assert!(matches!(
            wrong_qdisc_handle.validate(),
            Err(KernelTopologyError::Invalid(message)) if message.contains("ingress/clsact hook")
        ));

        let mut wrong_ingress_parent = snapshot();
        wrong_ingress_parent.ingress_filters[0].parent = TC_H_MIN_EGRESS;
        assert!(matches!(
            wrong_ingress_parent.validate(),
            Err(KernelTopologyError::Invalid(message)) if message.contains("target ingress hook")
        ));

        let mut ingress_only_with_egress = snapshot();
        ingress_only_with_egress.ingress_qdiscs[0].kind = "ingress".to_string();
        let mut egress = ingress_only_with_egress.ingress_filters.pop().unwrap();
        egress.parent = TC_H_MIN_EGRESS;
        ingress_only_with_egress.egress_filters.push(egress);
        assert!(matches!(
            ingress_only_with_egress.validate(),
            Err(KernelTopologyError::Invalid(message)) if message.contains("without a clsact")
        ));

        let mut clsact_with_egress = snapshot();
        let mut egress = clsact_with_egress.ingress_filters.pop().unwrap();
        egress.parent = TC_H_MIN_EGRESS;
        clsact_with_egress.egress_filters.push(egress);
        clsact_with_egress.validate().unwrap();
    }

    #[test]
    fn representation_rejects_ambiguous_observed_link_names_and_aliases() {
        let mut ambiguous = bare_snapshot();
        ambiguous.target.alias = Some("foreign-alias".to_string());
        ambiguous.private_links.push(LinkRecord {
            ifindex: 30,
            name: "foreign0".to_string(),
            alias: Some("eth0".to_string()),
            kind: KernelLinkKind::Named("ifb".to_string()),
            parent_ifindex: None,
        });
        assert!(matches!(
            ambiguous.validate(),
            Err(KernelTopologyError::Invalid(message)) if message.contains("ambiguous link")
        ));

        let mut duplicate_alias = bare_snapshot();
        duplicate_alias.private_links.extend([
            LinkRecord {
                ifindex: 30,
                name: "foreign0".to_string(),
                alias: Some("shared-alias".to_string()),
                kind: KernelLinkKind::Named("ifb".to_string()),
                parent_ifindex: None,
            },
            LinkRecord {
                ifindex: 31,
                name: "foreign1".to_string(),
                alias: Some("shared-alias".to_string()),
                kind: KernelLinkKind::Named("ifb".to_string()),
                parent_ifindex: None,
            },
        ]);
        assert!(matches!(
            duplicate_alias.validate(),
            Err(KernelTopologyError::Invalid(message)) if message.contains("ambiguous link")
        ));
    }

    #[test]
    fn representation_bounds_kernel_ifindexes_to_the_signed_uapi_range() {
        let mut oversized_target = bare_snapshot();
        oversized_target.target.ifindex = i32::MAX as u32 + 1;
        assert!(matches!(
            oversized_target.validate(),
            Err(KernelTopologyError::Invalid(message)) if message.contains("invalid ifindex")
        ));

        let mut oversized_parent = bare_snapshot();
        oversized_parent.target.parent_ifindex = Some(i32::MAX as u32 + 1);
        assert!(matches!(
            oversized_parent.validate(),
            Err(KernelTopologyError::Invalid(message)) if message.contains("invalid ifindex")
        ));
    }

    #[test]
    fn reserved_link_names_and_aliases_form_one_collision_namespace() {
        let private_namespace = PrivateNamespaceRecord {
            link_names: vec!["ownedifb0".to_string()],
            link_aliases: vec!["owned-alias".to_string()],
            ..PrivateNamespaceRecord::default()
        };
        let expected = ExpectedOwnedTopology {
            private_links: vec![LinkRecord {
                ifindex: 20,
                name: "ownedifb0".to_string(),
                alias: Some("owned-alias".to_string()),
                kind: KernelLinkKind::Named("ifb".to_string()),
                parent_ifindex: None,
            }],
            ..ExpectedOwnedTopology::default()
        };
        let colliding_links = [
            LinkRecord {
                ifindex: 30,
                name: "owned-alias".to_string(),
                alias: None,
                kind: KernelLinkKind::Named("ifb".to_string()),
                parent_ifindex: None,
            },
            LinkRecord {
                ifindex: 31,
                name: "foreign0".to_string(),
                alias: Some("ownedifb0".to_string()),
                kind: KernelLinkKind::Named("ifb".to_string()),
                parent_ifindex: None,
            },
        ];

        for colliding in colliding_links {
            let mut observed = bare_snapshot();
            observed.private_namespace = private_namespace.clone();
            observed.private_links = vec![colliding];
            assert!(observed
                .validate_stage_policy(&KernelTopologyStagePolicy::BarePreflight(expected.clone(),))
                .is_err());
        }
    }

    #[test]
    fn bare_preflight_accepts_absence_and_unrelated_topology() {
        let expected = expected_owned();
        bare_snapshot()
            .validate_stage_policy(&KernelTopologyStagePolicy::BarePreflight(expected.clone()))
            .unwrap();

        let mut unrelated = bare_snapshot();
        unrelated.private_links.push(LinkRecord {
            ifindex: 30,
            name: "otherifb0".to_string(),
            alias: Some("other-owner".to_string()),
            kind: KernelLinkKind::Named("ifb".to_string()),
            parent_ifindex: None,
        });
        unrelated.root_qdiscs.push(QdiscRecord {
            ifindex: 30,
            parent: TC_H_ROOT,
            handle: 0xc001_0000,
            kind: "fq_codel".to_string(),
            options: config(vec![1, 2, 3]),
        });
        unrelated
            .validate_stage_policy(&KernelTopologyStagePolicy::BarePreflight(expected))
            .unwrap();
    }

    #[test]
    fn bare_preflight_rejects_reserved_identities_and_planned_slots() {
        let expected = expected_owned();

        let mut reserved_link = bare_snapshot();
        reserved_link
            .private_links
            .push(expected.private_links[0].clone());
        assert!(reserved_link
            .validate_stage_policy(&KernelTopologyStagePolicy::BarePreflight(expected.clone()))
            .is_err());

        let mut reserved_target_alias = bare_snapshot();
        reserved_target_alias.target.alias = Some(namespace().link_aliases[0].clone());
        assert!(reserved_target_alias
            .validate_stage_policy(&KernelTopologyStagePolicy::BarePreflight(expected.clone()))
            .is_err());

        let mut reserved_qdisc = bare_snapshot();
        reserved_qdisc.private_links.push(LinkRecord {
            ifindex: 30,
            name: "otherifb0".to_string(),
            alias: None,
            kind: KernelLinkKind::Named("ifb".to_string()),
            parent_ifindex: None,
        });
        reserved_qdisc.root_qdiscs.push(QdiscRecord {
            ifindex: 30,
            parent: TC_H_ROOT,
            handle: namespace().qdisc_handles[0],
            kind: "cake".to_string(),
            options: CanonicalConfig::default(),
        });
        assert!(reserved_qdisc
            .validate_stage_policy(&KernelTopologyStagePolicy::BarePreflight(expected.clone()))
            .is_err());

        let mut incompatible_hook = bare_snapshot();
        incompatible_hook.ingress_qdiscs.push(QdiscRecord {
            ifindex: 10,
            parent: TC_H_INGRESS,
            handle: TC_H_CLSACT_HANDLE,
            kind: "ingress".to_string(),
            options: CanonicalConfig::default(),
        });
        assert!(incompatible_hook
            .validate_stage_policy(&KernelTopologyStagePolicy::BarePreflight(expected.clone()))
            .is_err());

        let mut target_root_expected = ExpectedOwnedTopology::default();
        target_root_expected.root_qdiscs.push(QdiscRecord {
            ifindex: 10,
            parent: TC_H_ROOT,
            handle: namespace().qdisc_handles[0],
            kind: "cake".to_string(),
            options: CanonicalConfig::default(),
        });
        assert!(bare_snapshot()
            .validate_stage_policy(&KernelTopologyStagePolicy::BarePreflight(
                target_root_expected
            ))
            .is_err());
    }

    #[test]
    fn expected_private_link_requires_an_exact_named_kind() {
        let mut expected = expected_owned();
        expected.private_links[0].kind = KernelLinkKind::Absent;
        assert!(matches!(
            bare_snapshot().validate_stage_policy(&KernelTopologyStagePolicy::BarePreflight(
                expected,
            )),
            Err(KernelTopologyError::Invalid(message)) if message.contains("exact named kind")
        ));
    }

    #[test]
    fn temporary_owned_requires_exact_full_records_and_allows_unrelated_state() {
        let expected = expected_owned();
        snapshot()
            .validate_stage_policy(&KernelTopologyStagePolicy::TemporaryOwned(expected.clone()))
            .unwrap();

        let mut reordered = snapshot();
        reordered.private_links.reverse();
        reordered.ingress_filters.reverse();
        for filter in &mut reordered.ingress_filters {
            filter.actions.reverse();
        }
        reordered
            .validate_stage_policy(&KernelTopologyStagePolicy::TemporaryOwned(expected.clone()))
            .unwrap();

        let mut missing = snapshot();
        missing.ingress_filters.pop();
        assert!(missing
            .validate_stage_policy(&KernelTopologyStagePolicy::TemporaryOwned(expected.clone()))
            .is_err());

        let mut mismatched = snapshot();
        mismatched.ingress_filters[0].actions[0].options.attributes[0]
            .value
            .push(0xff);
        assert!(mismatched
            .validate_stage_policy(&KernelTopologyStagePolicy::TemporaryOwned(expected.clone()))
            .is_err());

        let mut extra_collision = snapshot();
        let mut extra = extra_collision.ingress_filters[0].clone();
        extra.chain = 1;
        extra.actions[0].action_index = namespace().action_identities[2].index;
        extra.actions[1].action_index = namespace().action_identities[3].index;
        extra_collision.ingress_filters.push(extra);
        assert!(extra_collision
            .validate_stage_policy(&KernelTopologyStagePolicy::TemporaryOwned(expected.clone()))
            .is_err());

        let mut unrelated = snapshot();
        unrelated.private_links.push(LinkRecord {
            ifindex: 30,
            name: "otherifb0".to_string(),
            alias: Some("other-owner".to_string()),
            kind: KernelLinkKind::Named("ifb".to_string()),
            parent_ifindex: None,
        });
        unrelated.root_qdiscs.push(QdiscRecord {
            ifindex: 30,
            parent: TC_H_ROOT,
            handle: 0xc001_0000,
            kind: "fq_codel".to_string(),
            options: CanonicalConfig::default(),
        });
        unrelated
            .validate_stage_policy(&KernelTopologyStagePolicy::TemporaryOwned(expected))
            .unwrap();
    }

    #[test]
    fn reserved_filter_and_action_identities_collide_independently() {
        let mut observed = bare_snapshot();
        observed
            .private_namespace
            .qdisc_handles
            .retain(|handle| *handle != TC_H_CLSACT_HANDLE);
        observed.private_links.push(LinkRecord {
            ifindex: 30,
            name: "otherifb0".to_string(),
            alias: Some("other-owner".to_string()),
            kind: KernelLinkKind::Named("ifb".to_string()),
            parent_ifindex: None,
        });
        observed.ingress_qdiscs.push(QdiscRecord {
            ifindex: 10,
            parent: TC_H_INGRESS,
            handle: TC_H_CLSACT_HANDLE,
            kind: "clsact".to_string(),
            options: CanonicalConfig::default(),
        });
        let mut foreign = filter(40_000, 0x7001);
        foreign.actions = vec![action(1, 90_000, 30)];
        foreign.actions[0].cookie = Some(vec![9; 16]);
        observed.ingress_filters = vec![foreign];
        let policy = KernelTopologyStagePolicy::BarePreflight(ExpectedOwnedTopology::default());

        observed.validate_stage_policy(&policy).unwrap();

        let mut reserved_handle = observed.clone();
        reserved_handle.ingress_filters[0].handle = namespace().filter_handles[0];
        assert!(reserved_handle.validate_stage_policy(&policy).is_err());

        let mut reserved_priority = observed.clone();
        reserved_priority.ingress_filters[0].priority = namespace().filter_priorities[0];
        assert!(reserved_priority.validate_stage_policy(&policy).is_err());

        let mut reserved_index = observed.clone();
        reserved_index.ingress_filters[0].actions[0].action_index =
            namespace().action_identities[0].index;
        assert!(reserved_index.validate_stage_policy(&policy).is_err());

        let mut reserved_cookie = observed;
        reserved_cookie.ingress_filters[0].actions[0].cookie =
            namespace().action_identities[0].cookie.clone();
        assert!(reserved_cookie.validate_stage_policy(&policy).is_err());
    }

    #[test]
    fn canonical_writer_rejects_before_crossing_aggregate_limit() {
        let chunk = [0u8; 252];
        let mut aggregate = CanonicalWriter::default();
        for _ in 0..(MAX_CANONICAL_BYTES / 256) {
            aggregate.bytes(&chunk).unwrap();
        }
        assert_eq!(aggregate.bytes.len(), MAX_CANONICAL_BYTES);
        assert!(aggregate.bytes.capacity() <= MAX_CANONICAL_BYTES);
        let length_before = aggregate.bytes.len();
        let capacity_before = aggregate.bytes.capacity();
        assert!(matches!(
            aggregate.u8(1),
            Err(KernelTopologyError::Limit(message)) if message.contains("canonical")
        ));
        assert_eq!(aggregate.bytes.len(), length_before);
        assert_eq!(aggregate.bytes.capacity(), capacity_before);

        let mut small = CanonicalWriter::with_limit(32);
        small.bytes(&[0u8; 24]).unwrap();
        let length_before = small.bytes.len();
        let capacity_before = small.bytes.capacity();
        assert!(small.bytes(&[0u8; 1]).is_err());
        assert_eq!(small.bytes.len(), length_before);
        assert_eq!(small.bytes.capacity(), capacity_before);
    }

    #[test]
    fn exact_reattestation_compares_canonical_state_not_input_order() {
        let baseline = capture(snapshot()).unwrap();
        let mut reordered = snapshot();
        reordered.root_qdiscs.reverse();
        let reordered = capture(reordered).unwrap();
        baseline.ensure_exact_reattestation(&reordered).unwrap();

        let mut changed = snapshot();
        changed.netns_cookie += 1;
        let changed = capture(changed).unwrap();
        assert!(baseline.ensure_exact_reattestation(&changed).is_err());
    }
}
