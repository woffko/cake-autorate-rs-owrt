//! Read-only `NETLINK_ROUTE` framing for native bootstrap topology witnesses.
//!
//! This parser is part of the production AbsentBootstrap admission boundary.
//! It exposes no mutation request. Volatile and descriptive link metadata is
//! deliberately excluded from the ownership fingerprint, while namespace,
//! master, XDP, VF and unknown future attributes remain fail-closed. CAKE,
//! u32 and mirred configuration is accepted only through the typed parsers
//! below.

use super::kernel_topology::{
    ActionRecord, ActionTarget, CanonicalConfig, CanonicalConfigAttribute, FilterRecord,
    KernelLinkKind, KernelTopologyError, KernelTopologyQuery, KernelTopologyRead,
    KernelTopologyReadBackend, KernelTopologySnapshot, LinkRecord, MirredDirection,
    PrivateActionIdentity, QdiscRecord, TopologyObject, UnknownOwnershipAttribute,
    MAX_ACTIONS_PER_FILTER, MAX_ACTION_COOKIE_BYTES, MAX_ATTRIBUTE_PATH, MAX_CONFIG_ATTRIBUTES,
    MAX_FILTERS, MAX_NAMESPACE_IDENTITIES, MAX_OBSERVED_BYTES, MAX_PRIVATE_LINKS, MAX_QDISCS,
    MAX_UNKNOWN_ATTRIBUTES, TC_H_INGRESS, TC_H_MIN_EGRESS, TC_H_MIN_INGRESS, TC_H_ROOT,
};
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
use std::mem::{self, size_of};
#[cfg(target_os = "linux")]
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

const NLMSG_HEADER_LEN: usize = 16;
const IFINFO_LEN: usize = 16;
const TCMSG_LEN: usize = 20;
const TCAMSG_LEN: usize = 4;
const NLA_HEADER_LEN: usize = 4;
const RECEIVE_BUFFER_BYTES: usize = 64 * 1024;

const NLM_F_REQUEST: u16 = 0x0001;
const NLM_F_MULTI: u16 = 0x0002;
const NLM_F_DUMP_INTR: u16 = 0x0010;
const NLM_F_DUMP: u16 = 0x0300;

const NLMSG_ERROR: u16 = 2;
const NLMSG_DONE: u16 = 3;
const NLMSG_OVERRUN: u16 = 4;
const RTM_NEWLINK: u16 = 16;
const RTM_GETLINK: u16 = 18;
const RTM_NEWQDISC: u16 = 36;
const RTM_GETQDISC: u16 = 38;
const RTM_NEWTFILTER: u16 = 44;
const RTM_GETTFILTER: u16 = 46;
const RTM_GETACTION: u16 = 50;

const IFLA_ADDRESS: u16 = 1;
const IFLA_BROADCAST: u16 = 2;
const IFLA_IFNAME: u16 = 3;
const IFLA_MTU: u16 = 4;
const IFLA_LINK: u16 = 5;
const IFLA_QDISC: u16 = 6;
const IFLA_STATS: u16 = 7;
const IFLA_COST: u16 = 8;
const IFLA_PRIORITY: u16 = 9;
const IFLA_MASTER: u16 = 10;
const IFLA_WIRELESS: u16 = 11;
const IFLA_PROTINFO: u16 = 12;
const IFLA_TXQLEN: u16 = 13;
const IFLA_MAP: u16 = 14;
const IFLA_WEIGHT: u16 = 15;
const IFLA_OPERSTATE: u16 = 16;
const IFLA_LINKMODE: u16 = 17;
const IFLA_LINKINFO: u16 = 18;
const IFLA_NET_NS_PID: u16 = 19;
const IFLA_IFALIAS: u16 = 20;
const IFLA_NUM_VF: u16 = 21;
const IFLA_VFINFO_LIST: u16 = 22;
const IFLA_STATS64: u16 = 23;
const IFLA_VF_PORTS: u16 = 24;
const IFLA_PORT_SELF: u16 = 25;
const IFLA_AF_SPEC: u16 = 26;
const IFLA_GROUP: u16 = 27;
const IFLA_NET_NS_FD: u16 = 28;
const IFLA_EXT_MASK: u16 = 29;
const IFLA_PROMISCUITY: u16 = 30;
const IFLA_NUM_TX_QUEUES: u16 = 31;
const IFLA_NUM_RX_QUEUES: u16 = 32;
const IFLA_CARRIER: u16 = 33;
const IFLA_PHYS_PORT_ID: u16 = 34;
const IFLA_CARRIER_CHANGES: u16 = 35;
const IFLA_PHYS_SWITCH_ID: u16 = 36;
const IFLA_LINK_NETNSID: u16 = 37;
const IFLA_PHYS_PORT_NAME: u16 = 38;
const IFLA_PROTO_DOWN: u16 = 39;
const IFLA_GSO_MAX_SEGS: u16 = 40;
const IFLA_GSO_MAX_SIZE: u16 = 41;
const IFLA_PAD: u16 = 42;
const IFLA_XDP: u16 = 43;
const IFLA_EVENT: u16 = 44;
const IFLA_NEW_NETNSID: u16 = 45;
const IFLA_IF_NETNSID: u16 = 46;
const IFLA_CARRIER_UP_COUNT: u16 = 47;
const IFLA_CARRIER_DOWN_COUNT: u16 = 48;
const IFLA_NEW_IFINDEX: u16 = 49;
const IFLA_MIN_MTU: u16 = 50;
const IFLA_MAX_MTU: u16 = 51;
const IFLA_PROP_LIST: u16 = 52;
const IFLA_ALT_IFNAME: u16 = 53;
const IFLA_PERM_ADDRESS: u16 = 54;
const IFLA_PROTO_DOWN_REASON: u16 = 55;
const IFLA_PARENT_DEV_NAME: u16 = 56;
const IFLA_PARENT_DEV_BUS_NAME: u16 = 57;
const IFLA_GRO_MAX_SIZE: u16 = 58;
const IFLA_TSO_MAX_SIZE: u16 = 59;
const IFLA_TSO_MAX_SEGS: u16 = 60;
const IFLA_ALLMULTI: u16 = 61;
const IFLA_DEVLINK_PORT: u16 = 62;
const IFLA_GSO_IPV4_MAX_SIZE: u16 = 63;
const IFLA_GRO_IPV4_MAX_SIZE: u16 = 64;
const IFLA_DPLL_PIN: u16 = 65;
const IFLA_INFO_KIND: u16 = 1;
const IFLA_INFO_DATA: u16 = 2;
const IFLA_XDP_ATTACHED: u16 = 2;
const XDP_ATTACHED_NONE: u8 = 0;

const TCA_KIND: u16 = 1;
const TCA_OPTIONS: u16 = 2;
const TCA_STATS: u16 = 3;
const TCA_XSTATS: u16 = 4;
const TCA_RATE: u16 = 5;
const TCA_FCNT: u16 = 6;
const TCA_STATS2: u16 = 7;
const TCA_STAB: u16 = 8;
const TCA_PAD: u16 = 9;
const TCA_DUMP_INVISIBLE: u16 = 10;
const TCA_CHAIN: u16 = 11;
const TCA_HW_OFFLOAD: u16 = 12;

const TCA_ROOT_TAB: u16 = 1;
const TCA_ACT_TAB: u16 = TCA_ROOT_TAB;
const TCA_ROOT_FLAGS: u16 = 2;
const TCA_ROOT_COUNT: u16 = 3;
const TCA_ROOT_TIME_DELTA: u16 = 4;
const TCA_ROOT_EXT_WARN_MSG: u16 = 5;

const TCA_ACT_KIND: u16 = 1;
const TCA_ACT_OPTIONS: u16 = 2;
const TCA_ACT_INDEX: u16 = 3;
const TCA_ACT_STATS: u16 = 4;
const TCA_ACT_PAD: u16 = 5;
const TCA_ACT_COOKIE: u16 = 6;
const TCA_ACT_FLAGS: u16 = 7;
const TCA_ACT_HW_STATS: u16 = 8;
const TCA_ACT_USED_HW_STATS: u16 = 9;
const TCA_ACT_IN_HW_COUNT: u16 = 10;

const TCA_ACT_FLAG_LARGE_DUMP_ON: u32 = 1;
const TCA_ACT_FLAG_TERSE_DUMP: u32 = 1 << 1;

const TCA_U32_CLASSID: u16 = 1;
const TCA_U32_HASH: u16 = 2;
const TCA_U32_LINK: u16 = 3;
const TCA_U32_DIVISOR: u16 = 4;
const TCA_U32_SEL: u16 = 5;
const TCA_U32_POLICE: u16 = 6;
const TCA_U32_ACT: u16 = 7;
const TCA_U32_INDEV: u16 = 8;
const TCA_U32_PCNT: u16 = 9;
const TCA_U32_MARK: u16 = 10;
const TCA_U32_FLAGS: u16 = 11;
const TCA_U32_PAD: u16 = 12;
const TC_U32_SEL_LEN: usize = 16;
const TC_U32_KEY_LEN: usize = 16;

const TCA_MIRRED_TM: u16 = 1;
const TCA_MIRRED_PARMS: u16 = 2;
const TCA_MIRRED_PAD: u16 = 3;
const TCA_MIRRED_BLOCKID: u16 = 4;
const TC_MIRRED_LEN: usize = 28;
const TCA_EGRESS_REDIR: i32 = 1;
const TCA_EGRESS_MIRROR: i32 = 2;
const TCA_INGRESS_REDIR: i32 = 3;
const TCA_INGRESS_MIRROR: i32 = 4;

const TCA_CAKE_PAD: u16 = 1;
const TCA_CAKE_BASE_RATE64: u16 = 2;
const TCA_CAKE_DIFFSERV_MODE: u16 = 3;
const TCA_CAKE_ATM: u16 = 4;
const TCA_CAKE_FLOW_MODE: u16 = 5;
const TCA_CAKE_OVERHEAD: u16 = 6;
const TCA_CAKE_RTT: u16 = 7;
const TCA_CAKE_TARGET: u16 = 8;
const TCA_CAKE_AUTORATE: u16 = 9;
const TCA_CAKE_MEMORY: u16 = 10;
const TCA_CAKE_NAT: u16 = 11;
const TCA_CAKE_RAW: u16 = 12;
const TCA_CAKE_WASH: u16 = 13;
const TCA_CAKE_MPU: u16 = 14;
const TCA_CAKE_INGRESS: u16 = 15;
const TCA_CAKE_ACK_FILTER: u16 = 16;
const TCA_CAKE_SPLIT_GSO: u16 = 17;
const TCA_CAKE_FWMARK: u16 = 18;

const TCA_FQ_CODEL_TARGET: u16 = 1;
const TCA_FQ_CODEL_LIMIT: u16 = 2;
const TCA_FQ_CODEL_INTERVAL: u16 = 3;
const TCA_FQ_CODEL_ECN: u16 = 4;
const TCA_FQ_CODEL_FLOWS: u16 = 5;
const TCA_FQ_CODEL_QUANTUM: u16 = 6;
const TCA_FQ_CODEL_CE_THRESHOLD: u16 = 7;
const TCA_FQ_CODEL_DROP_BATCH_SIZE: u16 = 8;
const TCA_FQ_CODEL_MEMORY_LIMIT: u16 = 9;
const TCA_FQ_CODEL_CE_THRESHOLD_SELECTOR: u16 = 10;
const TCA_FQ_CODEL_CE_THRESHOLD_MASK: u16 = 11;
const NLA_F_NESTED: u16 = 0x8000;
const NLA_F_NET_BYTEORDER: u16 = 0x4000;
const NLA_TYPE_MASK: u16 = !(NLA_F_NESTED | NLA_F_NET_BYTEORDER);

const DEFAULT_MAX_DATAGRAMS: usize = 256;
const DEFAULT_MAX_MESSAGES: usize = 2_048;
const DEFAULT_MAX_ATTRIBUTES: usize = 8_192;
const HARD_MAX_DATAGRAMS: usize = 1_024;
const HARD_MAX_MESSAGES: usize = 8_192;
const HARD_MAX_ATTRIBUTES: usize = 32_768;
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(2);
const HARD_MAX_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DumpKind {
    Links,
    Qdiscs,
    IngressFilters { ifindex: u32 },
    EgressFilters { ifindex: u32 },
}

impl DumpKind {
    fn request_type(self) -> u16 {
        match self {
            Self::Links => RTM_GETLINK,
            Self::Qdiscs => RTM_GETQDISC,
            Self::IngressFilters { .. } | Self::EgressFilters { .. } => RTM_GETTFILTER,
        }
    }

    fn reply_type(self) -> u16 {
        match self {
            Self::Links => RTM_NEWLINK,
            Self::Qdiscs => RTM_NEWQDISC,
            Self::IngressFilters { .. } | Self::EgressFilters { .. } => RTM_NEWTFILTER,
        }
    }
}

fn encode_dump_request(
    dump: DumpKind,
    sequence: u32,
    local_port_id: u32,
) -> Result<Vec<u8>, KernelTopologyError> {
    if sequence == 0 || local_port_id == 0 {
        return Err(KernelTopologyError::Invalid(
            "netlink request sequence and port id must be non-zero".to_string(),
        ));
    }
    let payload_len = match dump {
        DumpKind::Links => IFINFO_LEN,
        DumpKind::Qdiscs => TCMSG_LEN + NLA_HEADER_LEN,
        DumpKind::IngressFilters { .. } | DumpKind::EgressFilters { .. } => TCMSG_LEN,
    };
    let total_len = NLMSG_HEADER_LEN
        .checked_add(payload_len)
        .ok_or_else(|| KernelTopologyError::Limit("netlink request length overflow".to_string()))?;
    let mut request = vec![0u8; total_len];
    put_u32(&mut request, 0, total_len as u32)?;
    put_u16(&mut request, 4, dump.request_type())?;
    put_u16(&mut request, 6, NLM_F_REQUEST | NLM_F_DUMP)?;
    put_u32(&mut request, 8, sequence)?;
    put_u32(&mut request, 12, local_port_id)?;
    match dump {
        DumpKind::Links => {}
        DumpKind::Qdiscs => {
            put_u16(
                &mut request,
                NLMSG_HEADER_LEN + TCMSG_LEN,
                NLA_HEADER_LEN as u16,
            )?;
            put_u16(
                &mut request,
                NLMSG_HEADER_LEN + TCMSG_LEN + 2,
                TCA_DUMP_INVISIBLE,
            )?;
        }
        DumpKind::IngressFilters { ifindex } | DumpKind::EgressFilters { ifindex } => {
            if ifindex == 0 || ifindex > i32::MAX as u32 {
                return Err(KernelTopologyError::Invalid(
                    "netlink filter dump ifindex is invalid".to_string(),
                ));
            }
            put_u32(&mut request, NLMSG_HEADER_LEN + 4, ifindex)?;
            let parent = match dump {
                DumpKind::IngressFilters { .. } => TC_H_MIN_INGRESS,
                DumpKind::EgressFilters { .. } => TC_H_MIN_EGRESS,
                _ => unreachable!(),
            };
            put_u32(&mut request, NLMSG_HEADER_LEN + 12, parent)?;
        }
    }
    Ok(request)
}

fn encode_action_dump_request(
    kind: &str,
    sequence: u32,
    local_port_id: u32,
) -> Result<Vec<u8>, KernelTopologyError> {
    validate_action_kind(kind)?;
    if sequence == 0 || local_port_id == 0 {
        return Err(KernelTopologyError::Invalid(
            "netlink request sequence and port id must be non-zero".to_string(),
        ));
    }

    let mut action_kind = kind.as_bytes().to_vec();
    action_kind.push(0);
    let mut action_entry = Vec::new();
    append_nla(&mut action_entry, TCA_ACT_KIND, &action_kind)?;
    let mut action_table = Vec::new();
    append_nla(&mut action_table, 1, &action_entry)?;

    let flags = TCA_ACT_FLAG_LARGE_DUMP_ON | TCA_ACT_FLAG_TERSE_DUMP;
    let mut payload = vec![0u8; TCAMSG_LEN];
    append_nla(&mut payload, TCA_ACT_TAB | NLA_F_NESTED, &action_table)?;
    let mut bitfield = [0u8; 8];
    bitfield[..4].copy_from_slice(&flags.to_ne_bytes());
    bitfield[4..].copy_from_slice(&flags.to_ne_bytes());
    append_nla(&mut payload, TCA_ROOT_FLAGS, &bitfield)?;

    let total_len = NLMSG_HEADER_LEN
        .checked_add(payload.len())
        .ok_or_else(|| KernelTopologyError::Limit("netlink request length overflow".to_string()))?;
    let mut request = vec![0u8; NLMSG_HEADER_LEN];
    put_u32(&mut request, 0, total_len as u32)?;
    put_u16(&mut request, 4, RTM_GETACTION)?;
    put_u16(&mut request, 6, NLM_F_REQUEST | NLM_F_DUMP)?;
    put_u32(&mut request, 8, sequence)?;
    put_u32(&mut request, 12, local_port_id)?;
    request.extend_from_slice(&payload);
    Ok(request)
}

fn append_nla(
    destination: &mut Vec<u8>,
    raw_type: u16,
    payload: &[u8],
) -> Result<(), KernelTopologyError> {
    let length = NLA_HEADER_LEN.checked_add(payload.len()).ok_or_else(|| {
        KernelTopologyError::Limit("netlink attribute length overflow".to_string())
    })?;
    let encoded_length = u16::try_from(length).map_err(|_| {
        KernelTopologyError::Limit("netlink attribute exceeds u16 length".to_string())
    })?;
    let aligned = align4(length)?;
    let start = destination.len();
    let end = start
        .checked_add(aligned)
        .ok_or_else(|| KernelTopologyError::Limit("netlink request length overflow".to_string()))?;
    destination.resize(end, 0);
    destination[start..start + 2].copy_from_slice(&encoded_length.to_ne_bytes());
    destination[start + 2..start + 4].copy_from_slice(&raw_type.to_ne_bytes());
    destination[start + 4..start + length].copy_from_slice(payload);
    Ok(())
}

fn validate_action_kind(kind: &str) -> Result<(), KernelTopologyError> {
    if kind.is_empty()
        || kind.len() > 32
        || kind
            .bytes()
            .any(|byte| !byte.is_ascii_alphanumeric() && !matches!(byte, b'_' | b'-'))
    {
        return Err(KernelTopologyError::Invalid(
            "netlink action kind is invalid".to_string(),
        ));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReceivedDatagram {
    pub len: usize,
    pub sender_family: u16,
    pub sender_port_id: u32,
    pub sender_groups: u32,
    pub message_flags: i32,
}

/// Bounded, read-only transport abstraction used by offline fixtures.
pub trait NetlinkIo {
    fn local_port_id(&self) -> u32;
    fn netns_cookie(&self) -> Result<u64, KernelTopologyError>;
    fn send_request(
        &mut self,
        request: &[u8],
        deadline: Instant,
    ) -> Result<(), KernelTopologyError>;
    fn receive_datagram(
        &mut self,
        buffer: &mut [u8],
        deadline: Instant,
    ) -> Result<ReceivedDatagram, KernelTopologyError>;
}

#[derive(Clone, Debug)]
pub struct NetlinkReadLimits {
    pub max_observed_bytes: usize,
    pub max_datagrams: usize,
    pub max_messages: usize,
    pub max_attributes: usize,
    pub timeout: Duration,
}

impl Default for NetlinkReadLimits {
    fn default() -> Self {
        Self {
            max_observed_bytes: MAX_OBSERVED_BYTES,
            max_datagrams: DEFAULT_MAX_DATAGRAMS,
            max_messages: DEFAULT_MAX_MESSAGES,
            max_attributes: DEFAULT_MAX_ATTRIBUTES,
            timeout: DEFAULT_TIMEOUT,
        }
    }
}

impl NetlinkReadLimits {
    fn validate(&self) -> Result<(), KernelTopologyError> {
        if self.max_observed_bytes == 0 || self.max_observed_bytes > MAX_OBSERVED_BYTES {
            return Err(KernelTopologyError::Limit(
                "netlink observed-byte bound is invalid".to_string(),
            ));
        }
        if self.max_datagrams == 0 || self.max_datagrams > HARD_MAX_DATAGRAMS {
            return Err(KernelTopologyError::Limit(
                "netlink datagram bound is invalid".to_string(),
            ));
        }
        if self.max_messages == 0 || self.max_messages > HARD_MAX_MESSAGES {
            return Err(KernelTopologyError::Limit(
                "netlink message bound is invalid".to_string(),
            ));
        }
        if self.max_attributes == 0 || self.max_attributes > HARD_MAX_ATTRIBUTES {
            return Err(KernelTopologyError::Limit(
                "netlink attribute bound is invalid".to_string(),
            ));
        }
        if self.timeout.is_zero() || self.timeout > HARD_MAX_TIMEOUT {
            return Err(KernelTopologyError::Limit(
                "netlink deadline is invalid".to_string(),
            ));
        }
        Ok(())
    }
}

#[derive(Default)]
struct ReadBudget {
    observed_bytes: usize,
    datagrams: usize,
    messages: usize,
    attributes: usize,
}

impl ReadBudget {
    fn charge_datagram(
        &mut self,
        len: usize,
        limits: &NetlinkReadLimits,
    ) -> Result<(), KernelTopologyError> {
        self.datagrams = checked_increment(self.datagrams, limits.max_datagrams, "datagrams")?;
        self.observed_bytes = self
            .observed_bytes
            .checked_add(len)
            .ok_or_else(|| KernelTopologyError::Limit("netlink byte count overflow".to_string()))?;
        if self.observed_bytes > limits.max_observed_bytes {
            return Err(KernelTopologyError::Limit(
                "netlink dump exceeds its observed-byte bound".to_string(),
            ));
        }
        Ok(())
    }

    fn charge_message(&mut self, limits: &NetlinkReadLimits) -> Result<(), KernelTopologyError> {
        self.messages = checked_increment(self.messages, limits.max_messages, "messages")?;
        Ok(())
    }

    fn charge_attribute(&mut self, limits: &NetlinkReadLimits) -> Result<(), KernelTopologyError> {
        self.attributes = checked_increment(self.attributes, limits.max_attributes, "attributes")?;
        Ok(())
    }
}

fn checked_increment(
    current: usize,
    maximum: usize,
    label: &str,
) -> Result<usize, KernelTopologyError> {
    let next = current
        .checked_add(1)
        .ok_or_else(|| KernelTopologyError::Limit(format!("netlink {label} count overflow")))?;
    if next > maximum {
        return Err(KernelTopologyError::Limit(format!(
            "netlink {label} count exceeds its bound"
        )));
    }
    Ok(next)
}

pub struct NetlinkTopologyReader<I> {
    io: I,
    limits: NetlinkReadLimits,
    next_sequence: u32,
}

impl<I: NetlinkIo> NetlinkTopologyReader<I> {
    pub fn new(io: I) -> Self {
        Self {
            io,
            limits: NetlinkReadLimits::default(),
            next_sequence: 1,
        }
    }

    #[cfg(test)]
    pub fn with_limits(io: I, limits: NetlinkReadLimits) -> Result<Self, KernelTopologyError> {
        limits.validate()?;
        Ok(Self {
            io,
            limits,
            next_sequence: 1,
        })
    }

    fn sequence(&mut self) -> u32 {
        let sequence = self.next_sequence.max(1);
        self.next_sequence = sequence.wrapping_add(1).max(1);
        sequence
    }

    fn read_pass(
        &mut self,
        query: &KernelTopologyQuery,
        netns_cookie: u64,
        deadline: Instant,
        buffer: &mut [u8],
        budget: &mut ReadBudget,
    ) -> Result<FinalizedPass, KernelTopologyError> {
        let mut pass = ParsedPass::new(query, netns_cookie);
        let limits = self.limits.clone();
        self.run_dump(
            DumpKind::Links,
            deadline,
            buffer,
            budget,
            |payload, budget| pass.parse_link(payload, budget, &limits),
        )?;
        let target_ifindex = pass.target_ifindex()?;
        self.run_dump(
            DumpKind::Qdiscs,
            deadline,
            buffer,
            budget,
            |payload, budget| pass.parse_qdisc(payload, budget, &limits),
        )?;
        self.run_dump(
            DumpKind::IngressFilters {
                ifindex: target_ifindex,
            },
            deadline,
            buffer,
            budget,
            |payload, budget| pass.parse_filter(payload, TC_H_MIN_INGRESS, budget, &limits),
        )?;
        self.run_dump(
            DumpKind::EgressFilters {
                ifindex: target_ifindex,
            },
            deadline,
            buffer,
            budget,
            |payload, budget| pass.parse_filter(payload, TC_H_MIN_EGRESS, budget, &limits),
        )?;
        let action_kinds = pass
            .query
            .private_namespace
            .action_identities
            .iter()
            .map(|identity| identity.kind.clone())
            .collect::<BTreeSet<_>>();
        for kind in action_kinds {
            self.run_action_dump(&kind, deadline, buffer, budget, |payload, budget| {
                pass.parse_action_table(payload, &kind, budget, &limits)
            })?;
        }
        pass.finish()
    }

    fn run_dump(
        &mut self,
        dump: DumpKind,
        deadline: Instant,
        buffer: &mut [u8],
        budget: &mut ReadBudget,
        mut parse_payload: impl FnMut(&[u8], &mut ReadBudget) -> Result<(), KernelTopologyError>,
    ) -> Result<(), KernelTopologyError> {
        let sequence = self.sequence();
        let local_port_id = self.io.local_port_id();
        let request = encode_dump_request(dump, sequence, local_port_id)?;
        self.run_encoded_dump(
            &request,
            dump.reply_type(),
            sequence,
            local_port_id,
            deadline,
            buffer,
            budget,
            &mut parse_payload,
        )
    }

    fn run_action_dump(
        &mut self,
        kind: &str,
        deadline: Instant,
        buffer: &mut [u8],
        budget: &mut ReadBudget,
        mut parse_payload: impl FnMut(&[u8], &mut ReadBudget) -> Result<(), KernelTopologyError>,
    ) -> Result<(), KernelTopologyError> {
        let sequence = self.sequence();
        let local_port_id = self.io.local_port_id();
        let request = encode_action_dump_request(kind, sequence, local_port_id)?;
        self.run_encoded_dump(
            &request,
            RTM_GETACTION,
            sequence,
            local_port_id,
            deadline,
            buffer,
            budget,
            &mut parse_payload,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn run_encoded_dump(
        &mut self,
        request: &[u8],
        expected_reply_type: u16,
        sequence: u32,
        local_port_id: u32,
        deadline: Instant,
        buffer: &mut [u8],
        budget: &mut ReadBudget,
        parse_payload: &mut impl FnMut(&[u8], &mut ReadBudget) -> Result<(), KernelTopologyError>,
    ) -> Result<(), KernelTopologyError> {
        self.io.send_request(&request, deadline)?;
        let mut done = false;
        while !done {
            let received = self.io.receive_datagram(buffer, deadline)?;
            if received.len == 0 || received.len > buffer.len() {
                return Err(KernelTopologyError::Backend(
                    "netlink returned an invalid datagram length".to_string(),
                ));
            }
            if received.sender_family != libc::AF_NETLINK as u16
                || received.sender_port_id != 0
                || received.sender_groups != 0
            {
                return Err(KernelTopologyError::Backend(
                    "netlink datagram sender is not the kernel unicast endpoint".to_string(),
                ));
            }
            if received.message_flags & (libc::MSG_TRUNC | libc::MSG_CTRUNC) != 0 {
                return Err(KernelTopologyError::Limit(
                    "netlink datagram was truncated".to_string(),
                ));
            }
            budget.charge_datagram(received.len, &self.limits)?;
            done = parse_multipart_datagram(
                &buffer[..received.len],
                expected_reply_type,
                sequence,
                local_port_id,
                budget,
                &self.limits,
                parse_payload,
            )?;
        }
        Ok(())
    }
}

impl<I: NetlinkIo> KernelTopologyReadBackend for NetlinkTopologyReader<I> {
    fn read_topology(
        &mut self,
        query: &KernelTopologyQuery,
    ) -> Result<KernelTopologyRead, KernelTopologyError> {
        query.validate()?;
        self.limits.validate()?;
        if self.io.local_port_id() == 0 {
            return Err(KernelTopologyError::Backend(
                "netlink socket has no local port id".to_string(),
            ));
        }
        let deadline = Instant::now()
            .checked_add(self.limits.timeout)
            .ok_or_else(|| KernelTopologyError::Limit("netlink deadline overflow".to_string()))?;
        let initial_cookie = self.io.netns_cookie()?;
        if initial_cookie == 0 {
            return Err(KernelTopologyError::Invalid(
                "network namespace cookie must be non-zero".to_string(),
            ));
        }
        let mut buffer = Box::new([0u8; RECEIVE_BUFFER_BYTES]);
        let mut budget = ReadBudget::default();
        let first = self.read_pass(
            query,
            initial_cookie,
            deadline,
            buffer.as_mut_slice(),
            &mut budget,
        )?;
        if self.io.netns_cookie()? != initial_cookie {
            return Err(KernelTopologyError::Backend(
                "network namespace changed between topology passes".to_string(),
            ));
        }
        let second = self.read_pass(
            query,
            initial_cookie,
            deadline,
            buffer.as_mut_slice(),
            &mut budget,
        )?;
        if self.io.netns_cookie()? != initial_cookie {
            return Err(KernelTopologyError::Backend(
                "network namespace changed during topology capture".to_string(),
            ));
        }
        if first.canonical != second.canonical || first.unknown != second.unknown {
            return Err(KernelTopologyError::Backend(
                "kernel topology changed between bounded read passes".to_string(),
            ));
        }
        Ok(KernelTopologyRead {
            snapshot: second.snapshot,
            observed_bytes: budget.observed_bytes,
            unknown_ownership_attributes: second.unknown,
        })
    }
}

fn parse_multipart_datagram(
    datagram: &[u8],
    expected_reply_type: u16,
    sequence: u32,
    local_port_id: u32,
    budget: &mut ReadBudget,
    limits: &NetlinkReadLimits,
    parse_payload: &mut impl FnMut(&[u8], &mut ReadBudget) -> Result<(), KernelTopologyError>,
) -> Result<bool, KernelTopologyError> {
    let mut offset = 0usize;
    let mut done = false;
    while offset < datagram.len() {
        if done || datagram.len() - offset < NLMSG_HEADER_LEN {
            return Err(KernelTopologyError::Backend(
                "netlink multipart framing is malformed".to_string(),
            ));
        }
        budget.charge_message(limits)?;
        let message_len = get_u32(datagram, offset)? as usize;
        if message_len < NLMSG_HEADER_LEN || message_len > datagram.len() - offset {
            return Err(KernelTopologyError::Backend(
                "netlink message length is malformed".to_string(),
            ));
        }
        let aligned_len = align4(message_len)?;
        if aligned_len > datagram.len() - offset {
            return Err(KernelTopologyError::Backend(
                "netlink message alignment exceeds the datagram".to_string(),
            ));
        }
        let message_type = get_u16(datagram, offset + 4)?;
        let flags = get_u16(datagram, offset + 6)?;
        let actual_sequence = get_u32(datagram, offset + 8)?;
        let actual_port = get_u32(datagram, offset + 12)?;
        if actual_sequence != sequence || actual_port != local_port_id {
            return Err(KernelTopologyError::Backend(
                "netlink reply sequence or destination port id mismatched".to_string(),
            ));
        }
        if flags & NLM_F_DUMP_INTR != 0 {
            return Err(KernelTopologyError::Backend(
                "netlink multipart dump was interrupted".to_string(),
            ));
        }
        let payload = &datagram[offset + NLMSG_HEADER_LEN..offset + message_len];
        match message_type {
            NLMSG_DONE => {
                if payload.len() < 4 || get_i32(payload, 0)? != 0 {
                    return Err(KernelTopologyError::Backend(
                        "netlink dump terminated with an error".to_string(),
                    ));
                }
                done = true;
            }
            NLMSG_ERROR => {
                let error = if payload.len() >= 4 {
                    get_i32(payload, 0)?
                } else {
                    i32::MIN
                };
                return Err(KernelTopologyError::Backend(format!(
                    "netlink returned NLMSG_ERROR ({error})"
                )));
            }
            NLMSG_OVERRUN => {
                return Err(KernelTopologyError::Limit(
                    "netlink reported a receive overrun".to_string(),
                ));
            }
            expected if expected == expected_reply_type => {
                if flags & NLM_F_MULTI == 0 {
                    return Err(KernelTopologyError::Backend(
                        "netlink dump data is not marked multipart".to_string(),
                    ));
                }
                parse_payload(payload, budget)?;
            }
            other => {
                return Err(KernelTopologyError::Backend(format!(
                    "unexpected netlink message type {other}"
                )));
            }
        }
        offset = offset
            .checked_add(aligned_len)
            .ok_or_else(|| KernelTopologyError::Limit("netlink offset overflow".to_string()))?;
    }
    Ok(done)
}

struct ParsedPass {
    query: KernelTopologyQuery,
    netns_cookie: u64,
    target: Option<LinkRecord>,
    private_links: BTreeMap<u32, LinkRecord>,
    root_qdiscs: Vec<QdiscRecord>,
    ingress_qdiscs: Vec<QdiscRecord>,
    ingress_filters: Vec<FilterRecord>,
    egress_filters: Vec<FilterRecord>,
    private_actions: BTreeMap<(String, u32), PrivateActionIdentity>,
    unknown: Vec<UnknownOwnershipAttribute>,
}

struct FinalizedPass {
    snapshot: KernelTopologySnapshot,
    canonical: Vec<u8>,
    unknown: Vec<UnknownOwnershipAttribute>,
}

impl ParsedPass {
    fn new(query: &KernelTopologyQuery, netns_cookie: u64) -> Self {
        Self {
            query: query.clone(),
            netns_cookie,
            target: None,
            private_links: BTreeMap::new(),
            root_qdiscs: Vec::new(),
            ingress_qdiscs: Vec::new(),
            ingress_filters: Vec::new(),
            egress_filters: Vec::new(),
            private_actions: BTreeMap::new(),
            unknown: Vec::new(),
        }
    }

    fn target_ifindex(&self) -> Result<u32, KernelTopologyError> {
        self.target
            .as_ref()
            .map(|link| link.ifindex)
            .ok_or_else(|| {
                KernelTopologyError::Invalid(
                    "netlink link dump did not contain the target interface".to_string(),
                )
            })
    }

    fn parse_link(
        &mut self,
        payload: &[u8],
        budget: &mut ReadBudget,
        limits: &NetlinkReadLimits,
    ) -> Result<(), KernelTopologyError> {
        if payload.len() < IFINFO_LEN {
            return Err(KernelTopologyError::Backend(
                "RTM_NEWLINK payload is truncated".to_string(),
            ));
        }
        let raw_ifindex = get_i32(payload, 4)?;
        if raw_ifindex <= 0 {
            return Err(KernelTopologyError::Invalid(
                "RTM_NEWLINK contains an invalid ifindex".to_string(),
            ));
        }
        let ifindex = raw_ifindex as u32;
        let mut name = None;
        let mut alias = None;
        let mut kind = KernelLinkKind::Absent;
        let mut parent_ifindex = None;
        let mut alternate_names = Vec::new();
        let mut link_info_seen = false;
        let mut property_list_seen = false;
        let mut xdp_seen = false;
        let mut unsupported_paths = BTreeSet::new();
        let mut cursor = AttributeCursor::new(&payload[IFINFO_LEN..]);
        while let Some(attribute) = cursor.next(budget, limits)? {
            if attribute.kind == IFLA_LINKINFO {
                if link_info_seen {
                    return Err(KernelTopologyError::Invalid(
                        "netlink object contains duplicate IFLA_LINKINFO".to_string(),
                    ));
                }
                link_info_seen = true;
            }
            match attribute.kind {
                IFLA_IFNAME if attribute.flags == 0 => {
                    set_once(
                        &mut name,
                        parse_nul_string(attribute.payload, "IFLA_IFNAME")?,
                        "IFLA_IFNAME",
                    )?;
                }
                IFLA_IFALIAS if attribute.flags == 0 => {
                    let parsed = parse_nul_string(attribute.payload, "IFLA_IFALIAS")?;
                    if !parsed.is_empty() {
                        set_once(&mut alias, parsed, "IFLA_IFALIAS")?;
                    }
                }
                IFLA_LINK if attribute.flags == 0 => {
                    if attribute.payload.len() != 4 {
                        return Err(KernelTopologyError::Backend(
                            "IFLA_LINK has an invalid length".to_string(),
                        ));
                    }
                    let parent = get_u32(attribute.payload, 0)?;
                    if parent == 0 || parent > i32::MAX as u32 {
                        return Err(KernelTopologyError::Invalid(
                            "IFLA_LINK contains an invalid ifindex".to_string(),
                        ));
                    }
                    set_once(&mut parent_ifindex, parent, "IFLA_LINK")?;
                }
                IFLA_LINKINFO if attribute.flags & !NLA_F_NESTED == 0 => {
                    let (parsed_kind, unsupported) =
                        parse_link_info(attribute.payload, budget, limits)?;
                    if let Some(parsed_kind) = parsed_kind {
                        kind = KernelLinkKind::Named(parsed_kind);
                    }
                    unsupported_paths.extend(unsupported);
                }
                IFLA_ALT_IFNAME if attribute.flags == 0 => {
                    push_alternate_name(
                        &mut alternate_names,
                        parse_nul_string(attribute.payload, "IFLA_ALT_IFNAME")?,
                    )?;
                }
                IFLA_PROP_LIST if attribute.flags & !NLA_F_NESTED == 0 => {
                    if property_list_seen {
                        return Err(KernelTopologyError::Invalid(
                            "netlink object contains duplicate IFLA_PROP_LIST".to_string(),
                        ));
                    }
                    property_list_seen = true;
                    let (parsed_names, unsupported) =
                        parse_link_property_list(attribute.payload, budget, limits)?;
                    for alternate in parsed_names {
                        push_alternate_name(&mut alternate_names, alternate)?;
                    }
                    unsupported_paths.extend(unsupported);
                }
                IFLA_XDP => {
                    if xdp_seen {
                        return Err(KernelTopologyError::Invalid(
                            "netlink object contains duplicate IFLA_XDP".to_string(),
                        ));
                    }
                    xdp_seen = true;
                    if attribute.flags & !NLA_F_NESTED != 0 {
                        return Err(KernelTopologyError::Invalid(
                            "IFLA_XDP has invalid nesting flags".to_string(),
                        ));
                    }
                    unsupported_paths.extend(parse_unattached_xdp(
                        attribute.payload,
                        budget,
                        limits,
                    )?);
                }
                other if is_observation_only_link_attribute(other) => {}
                other if is_known_ownership_link_attribute(other) => {
                    unsupported_paths.insert(vec![other]);
                }
                other => {
                    unsupported_paths.insert(vec![other]);
                }
            }
        }
        let name = name.ok_or_else(|| {
            KernelTopologyError::Invalid("RTM_NEWLINK is missing IFLA_IFNAME".to_string())
        })?;
        let is_target = name == self.query.target_interface;
        let namespace = &self.query.private_namespace;
        let is_private = namespace.matches_link_identity(&name)
            || alias
                .as_deref()
                .is_some_and(|observed| namespace.matches_link_identity(observed));
        let alternate_collision = alternate_names.iter().any(|alternate| {
            alternate == &self.query.target_interface || namespace.matches_link_identity(alternate)
        });
        let alias_collides_target = alias
            .as_ref()
            .is_some_and(|observed| observed == &self.query.target_interface && !is_target);
        if alternate_collision || alias_collides_target {
            self.push_unknown(TopologyObject::Link, IFLA_ALT_IFNAME, vec![IFLA_ALT_IFNAME])?;
        }
        if !is_target && !is_private && !alias_collides_target && !alternate_collision {
            return Ok(());
        }
        for path in unsupported_paths {
            let attribute_type = *path.last().ok_or_else(|| {
                KernelTopologyError::Invalid("unsupported link attribute path is empty".to_string())
            })?;
            self.push_unknown(TopologyObject::Link, attribute_type, path)?;
        }
        let link = LinkRecord {
            ifindex,
            name,
            alias,
            kind,
            parent_ifindex,
        };
        if is_target {
            if self.target.replace(link).is_some() {
                return Err(KernelTopologyError::Invalid(
                    "netlink link dump contains duplicate target identity".to_string(),
                ));
            }
        } else {
            if self.private_links.len() >= MAX_PRIVATE_LINKS {
                return Err(KernelTopologyError::Limit(
                    "private link count exceeds its bound".to_string(),
                ));
            }
            if self.private_links.insert(ifindex, link).is_some() {
                return Err(KernelTopologyError::Invalid(
                    "netlink link dump contains duplicate ifindex".to_string(),
                ));
            }
        }
        Ok(())
    }

    fn parse_qdisc(
        &mut self,
        payload: &[u8],
        budget: &mut ReadBudget,
        limits: &NetlinkReadLimits,
    ) -> Result<(), KernelTopologyError> {
        let header = parse_tc_header(payload, "RTM_NEWQDISC")?;
        let target_ifindex = self.target_ifindex()?;
        let relevant =
            header.ifindex == target_ifindex || self.private_links.contains_key(&header.ifindex);
        if !relevant {
            return Ok(());
        }
        let owned_slot = header.parent == TC_H_ROOT
            || (header.ifindex == target_ifindex && header.parent == TC_H_INGRESS);
        if !owned_slot {
            if self
                .query
                .private_namespace
                .qdisc_handles
                .contains(&header.handle)
            {
                return Err(KernelTopologyError::Invalid(
                    "non-owned qdisc attachment collides with a reserved handle".to_string(),
                ));
            }
            // Multi-queue physical interfaces expose one kernel-managed leaf
            // qdisc per TX queue.  Bootstrap owns only the root and target
            // ingress/clsact slots represented by KernelTopologySnapshot; a
            // non-reserved leaf cannot collide with either ownership slot and
            // is therefore deliberately outside the canonical witness.
            return Ok(());
        }
        let parsed =
            parse_tc_attributes(&payload[TCMSG_LEN..], TopologyObject::Qdisc, budget, limits)?;
        let kind = parsed.kind.ok_or_else(|| {
            KernelTopologyError::Invalid("relevant qdisc is missing TCA_KIND".to_string())
        })?;
        self.extend_unknown(parsed.unknown)?;
        let record = QdiscRecord {
            ifindex: header.ifindex,
            parent: header.parent,
            handle: header.handle,
            kind,
            options: parsed.options,
        };
        if self
            .root_qdiscs
            .len()
            .saturating_add(self.ingress_qdiscs.len())
            >= MAX_QDISCS
        {
            return Err(KernelTopologyError::Limit(
                "qdisc count exceeds its bound".to_string(),
            ));
        }
        if header.parent == TC_H_ROOT {
            self.root_qdiscs.push(record);
        } else if header.ifindex == target_ifindex && header.parent == TC_H_INGRESS {
            self.ingress_qdiscs.push(record);
        } else {
            return Err(KernelTopologyError::Invalid(
                "unsupported qdisc attachment exists on an observed interface".to_string(),
            ));
        }
        Ok(())
    }

    fn parse_filter(
        &mut self,
        payload: &[u8],
        expected_parent: u32,
        budget: &mut ReadBudget,
        limits: &NetlinkReadLimits,
    ) -> Result<(), KernelTopologyError> {
        let header = parse_tc_header(payload, "RTM_NEWTFILTER")?;
        if header.ifindex != self.target_ifindex()? || header.parent != expected_parent {
            return Err(KernelTopologyError::Invalid(
                "filter dump returned a record outside the requested target hook".to_string(),
            ));
        }
        let parsed = parse_tc_attributes(
            &payload[TCMSG_LEN..],
            TopologyObject::Filter,
            budget,
            limits,
        )?;
        let kind = parsed.kind.ok_or_else(|| {
            KernelTopologyError::Invalid("filter is missing TCA_KIND".to_string())
        })?;
        self.extend_unknown(parsed.unknown)?;
        let priority = (header.info >> 16) as u16;
        let protocol = u16::from_be((header.info & 0xffff) as u16);
        let record = FilterRecord {
            ifindex: header.ifindex,
            parent: header.parent,
            chain: parsed.chain.unwrap_or(0),
            priority,
            protocol,
            handle: header.handle,
            kind,
            options: parsed.options,
            actions: parsed.actions,
        };
        if self
            .ingress_filters
            .len()
            .saturating_add(self.egress_filters.len())
            >= MAX_FILTERS
        {
            return Err(KernelTopologyError::Limit(
                "filter count exceeds its bound".to_string(),
            ));
        }
        let destination = if expected_parent == TC_H_MIN_INGRESS {
            &mut self.ingress_filters
        } else {
            &mut self.egress_filters
        };
        destination.push(record);
        Ok(())
    }

    fn parse_action_table(
        &mut self,
        payload: &[u8],
        expected_kind: &str,
        budget: &mut ReadBudget,
        limits: &NetlinkReadLimits,
    ) -> Result<(), KernelTopologyError> {
        if payload.len() < TCAMSG_LEN || payload[..TCAMSG_LEN].iter().any(|byte| *byte != 0) {
            return Err(KernelTopologyError::Backend(
                "RTM_GETACTION reply has noncanonical tcamsg fields".to_string(),
            ));
        }
        let mut table = None;
        let mut count = None;
        let mut cursor = AttributeCursor::new(&payload[TCAMSG_LEN..]);
        while let Some(attribute) = cursor.next(budget, limits)? {
            match attribute.kind {
                TCA_ACT_TAB if matches!(attribute.flags, 0 | NLA_F_NESTED) => {
                    set_once(&mut table, attribute.payload, "TCA_ACT_TAB")?;
                }
                TCA_ROOT_COUNT if attribute.flags == 0 => {
                    if attribute.payload.len() != 4 {
                        return Err(KernelTopologyError::Backend(
                            "TCA_ROOT_COUNT has an invalid length".to_string(),
                        ));
                    }
                    set_once(&mut count, get_u32(attribute.payload, 0)?, "TCA_ROOT_COUNT")?;
                }
                TCA_ROOT_EXT_WARN_MSG => {
                    return Err(KernelTopologyError::Backend(
                        "RTM_GETACTION returned an extended warning".to_string(),
                    ));
                }
                TCA_ROOT_FLAGS | TCA_ROOT_TIME_DELTA => {
                    self.push_unknown(
                        TopologyObject::Action,
                        attribute.kind,
                        vec![attribute.kind],
                    )?;
                }
                other => {
                    self.push_unknown(TopologyObject::Action, other, vec![other])?;
                }
            }
        }
        let table = table.ok_or_else(|| {
            KernelTopologyError::Invalid("RTM_GETACTION reply is missing TCA_ACT_TAB".to_string())
        })?;
        let mut action_count = 0usize;
        let mut table_cursor = AttributeCursor::new_allow_zero(table);
        while let Some(entry) = table_cursor.next(budget, limits)? {
            if entry.flags != 0 {
                return Err(KernelTopologyError::Invalid(
                    "RTM_GETACTION table entry has unsupported flags".to_string(),
                ));
            }
            action_count = action_count.checked_add(1).ok_or_else(|| {
                KernelTopologyError::Limit("action table count overflow".to_string())
            })?;
            let parsed = parse_action_identity_entry(entry.payload, budget, limits)?;
            if parsed.identity.kind != expected_kind {
                return Err(KernelTopologyError::Invalid(
                    "RTM_GETACTION returned a different action kind".to_string(),
                ));
            }
            if self
                .query
                .private_namespace
                .matches_action_identity(&parsed.identity)
            {
                self.extend_unknown(parsed.unknown)?;
                let key = (parsed.identity.kind.clone(), parsed.identity.index);
                if self.private_actions.insert(key, parsed.identity).is_some() {
                    return Err(KernelTopologyError::Invalid(
                        "RTM_GETACTION returned a duplicate action identity".to_string(),
                    ));
                }
                if self.private_actions.len() > MAX_NAMESPACE_IDENTITIES {
                    return Err(KernelTopologyError::Limit(
                        "private action count exceeds its bound".to_string(),
                    ));
                }
            }
        }
        if count.is_some_and(|reported| reported < action_count as u32) {
            return Err(KernelTopologyError::Invalid(
                "RTM_GETACTION count is smaller than its action table".to_string(),
            ));
        }
        Ok(())
    }

    fn push_unknown(
        &mut self,
        object: TopologyObject,
        attribute_type: u16,
        nested_path: Vec<u16>,
    ) -> Result<(), KernelTopologyError> {
        if attribute_type == 0
            || nested_path.len() > MAX_ATTRIBUTE_PATH
            || self.unknown.len() >= MAX_UNKNOWN_ATTRIBUTES
        {
            return Err(KernelTopologyError::Limit(
                "unknown ownership attribute metadata exceeds its bound".to_string(),
            ));
        }
        self.unknown.push(UnknownOwnershipAttribute {
            object,
            attribute_type,
            nested_path,
        });
        Ok(())
    }

    fn extend_unknown(
        &mut self,
        unknown: Vec<UnknownOwnershipAttribute>,
    ) -> Result<(), KernelTopologyError> {
        for attribute in unknown {
            self.push_unknown(
                attribute.object,
                attribute.attribute_type,
                attribute.nested_path,
            )?;
        }
        Ok(())
    }

    fn finish(mut self) -> Result<FinalizedPass, KernelTopologyError> {
        let target = self.target.take().ok_or_else(|| {
            KernelTopologyError::Invalid(
                "netlink link dump did not contain the target interface".to_string(),
            )
        })?;
        let snapshot = KernelTopologySnapshot {
            netns_cookie: self.netns_cookie,
            route: self.query.route,
            target,
            private_namespace: self.query.private_namespace,
            private_links: self.private_links.into_values().collect(),
            root_qdiscs: self.root_qdiscs,
            ingress_qdiscs: self.ingress_qdiscs,
            ingress_filters: self.ingress_filters,
            egress_filters: self.egress_filters,
            private_actions: self.private_actions.into_values().collect(),
        };
        let canonical = snapshot.canonical_bytes()?;
        self.unknown.sort();
        Ok(FinalizedPass {
            snapshot,
            canonical,
            unknown: self.unknown,
        })
    }
}

#[derive(Clone, Copy)]
struct TcHeader {
    ifindex: u32,
    handle: u32,
    parent: u32,
    info: u32,
}

fn parse_tc_header(payload: &[u8], label: &str) -> Result<TcHeader, KernelTopologyError> {
    if payload.len() < TCMSG_LEN {
        return Err(KernelTopologyError::Backend(format!(
            "{label} payload is truncated"
        )));
    }
    let raw_ifindex = get_i32(payload, 4)?;
    if raw_ifindex <= 0 {
        return Err(KernelTopologyError::Invalid(format!(
            "{label} contains an invalid ifindex"
        )));
    }
    Ok(TcHeader {
        ifindex: raw_ifindex as u32,
        handle: get_u32(payload, 8)?,
        parent: get_u32(payload, 12)?,
        info: get_u32(payload, 16)?,
    })
}

struct ParsedTcAttributes {
    kind: Option<String>,
    chain: Option<u32>,
    options: CanonicalConfig,
    actions: Vec<ActionRecord>,
    unknown: Vec<UnknownOwnershipAttribute>,
}

struct ParsedActionIdentity {
    identity: PrivateActionIdentity,
    unknown: Vec<UnknownOwnershipAttribute>,
}

fn parse_action_identity_entry(
    payload: &[u8],
    budget: &mut ReadBudget,
    limits: &NetlinkReadLimits,
) -> Result<ParsedActionIdentity, KernelTopologyError> {
    let mut kind = None;
    let mut index = None;
    let mut cookie = None;
    let mut unknown = Vec::new();
    let mut cursor = AttributeCursor::new(payload);
    while let Some(attribute) = cursor.next(budget, limits)? {
        match attribute.kind {
            TCA_ACT_KIND if attribute.flags == 0 => {
                set_once(
                    &mut kind,
                    parse_nul_string(attribute.payload, "TCA_ACT_KIND")?,
                    "TCA_ACT_KIND",
                )?;
            }
            TCA_ACT_INDEX if attribute.flags == 0 => {
                if attribute.payload.len() != 4 {
                    return Err(KernelTopologyError::Backend(
                        "TCA_ACT_INDEX has an invalid length".to_string(),
                    ));
                }
                set_once(&mut index, get_u32(attribute.payload, 0)?, "TCA_ACT_INDEX")?;
            }
            TCA_ACT_COOKIE if attribute.flags == 0 => {
                if attribute.payload.is_empty() || attribute.payload.len() > MAX_ACTION_COOKIE_BYTES
                {
                    return Err(KernelTopologyError::Limit(
                        "TCA_ACT_COOKIE is empty or exceeds its bound".to_string(),
                    ));
                }
                set_once(&mut cookie, attribute.payload.to_vec(), "TCA_ACT_COOKIE")?;
            }
            TCA_ACT_STATS | TCA_ACT_USED_HW_STATS | TCA_ACT_IN_HW_COUNT => {}
            TCA_ACT_PAD if attribute.flags == 0 && attribute.payload.is_empty() => {}
            TCA_ACT_OPTIONS | TCA_ACT_FLAGS | TCA_ACT_HW_STATS => {
                unknown.push(UnknownOwnershipAttribute {
                    object: TopologyObject::Action,
                    attribute_type: attribute.kind,
                    nested_path: vec![TCA_ACT_TAB, attribute.kind],
                });
            }
            other => {
                unknown.push(UnknownOwnershipAttribute {
                    object: TopologyObject::Action,
                    attribute_type: other,
                    nested_path: vec![TCA_ACT_TAB, other],
                });
            }
        }
    }
    let kind = kind.ok_or_else(|| {
        KernelTopologyError::Invalid("action table entry is missing TCA_ACT_KIND".to_string())
    })?;
    validate_action_kind(&kind)?;
    let index = index.filter(|index| *index != 0).ok_or_else(|| {
        KernelTopologyError::Invalid(
            "action table entry is missing a non-zero TCA_ACT_INDEX".to_string(),
        )
    })?;
    Ok(ParsedActionIdentity {
        identity: PrivateActionIdentity {
            kind,
            index,
            cookie,
        },
        unknown,
    })
}

fn parse_tc_attributes(
    payload: &[u8],
    object: TopologyObject,
    budget: &mut ReadBudget,
    limits: &NetlinkReadLimits,
) -> Result<ParsedTcAttributes, KernelTopologyError> {
    let mut kind = None;
    let mut chain = None;
    let mut raw_options = None;
    let mut hardware_offload = None;
    let mut unknown = Vec::new();
    let mut cursor = AttributeCursor::new(payload);
    while let Some(attribute) = cursor.next(budget, limits)? {
        match attribute.kind {
            TCA_KIND if attribute.flags == 0 => {
                set_once(
                    &mut kind,
                    parse_nul_string(attribute.payload, "TCA_KIND")?,
                    "TCA_KIND",
                )?;
            }
            TCA_CHAIN if attribute.flags == 0 && object == TopologyObject::Filter => {
                if attribute.payload.len() != 4 {
                    return Err(KernelTopologyError::Backend(
                        "TCA_CHAIN has an invalid length".to_string(),
                    ));
                }
                set_once(&mut chain, get_u32(attribute.payload, 0)?, "TCA_CHAIN")?;
            }
            TCA_OPTIONS => {
                set_once(
                    &mut raw_options,
                    (attribute.flags, attribute.payload),
                    "TCA_OPTIONS",
                )?;
            }
            TCA_HW_OFFLOAD => {
                if attribute.flags != 0 || attribute.payload.len() != 1 {
                    return Err(KernelTopologyError::Invalid(
                        "TCA_HW_OFFLOAD is not an exact u8 value".to_string(),
                    ));
                }
                let value = attribute.payload[0];
                set_once(&mut hardware_offload, value, "TCA_HW_OFFLOAD")?;
                if value != 0 {
                    unknown.push(UnknownOwnershipAttribute {
                        object,
                        attribute_type: TCA_HW_OFFLOAD,
                        nested_path: vec![TCA_HW_OFFLOAD],
                    });
                }
            }
            TCA_STATS | TCA_XSTATS | TCA_FCNT | TCA_STATS2 | TCA_PAD => {}
            TCA_RATE | TCA_STAB | TCA_DUMP_INVISIBLE => {
                unknown.push(UnknownOwnershipAttribute {
                    object,
                    attribute_type: attribute.kind,
                    nested_path: vec![attribute.kind],
                });
            }
            other => {
                unknown.push(UnknownOwnershipAttribute {
                    object,
                    attribute_type: other,
                    nested_path: vec![other],
                });
            }
        }
        if unknown.len() > MAX_UNKNOWN_ATTRIBUTES {
            return Err(KernelTopologyError::Limit(
                "unknown ownership attribute count exceeds its bound".to_string(),
            ));
        }
    }
    let mut options = CanonicalConfig::default();
    let mut actions = Vec::new();
    if let Some((flags, payload)) = raw_options {
        let parsed = match (object, kind.as_deref()) {
            (TopologyObject::Qdisc, Some("cake")) => {
                parse_cake_options(payload, flags, budget, limits)?
            }
            (TopologyObject::Qdisc, Some("fq_codel")) => {
                parse_fq_codel_options(payload, flags, budget, limits)?
            }
            (TopologyObject::Filter, Some("u32")) => {
                parse_u32_options(payload, flags, budget, limits)?
            }
            _ if payload.is_empty() && flags == 0 => ParsedConfigPayload::default(),
            _ => ParsedConfigPayload {
                unknown: vec![UnknownOwnershipAttribute {
                    object,
                    attribute_type: TCA_OPTIONS,
                    nested_path: vec![TCA_OPTIONS],
                }],
                ..ParsedConfigPayload::default()
            },
        };
        options = parsed.config;
        actions = parsed.actions;
        unknown.extend(parsed.unknown);
    }
    if unknown.len() > MAX_UNKNOWN_ATTRIBUTES {
        return Err(KernelTopologyError::Limit(
            "unknown ownership attribute count exceeds its bound".to_string(),
        ));
    }
    Ok(ParsedTcAttributes {
        kind,
        chain,
        options,
        actions,
        unknown,
    })
}

#[derive(Default)]
struct ParsedConfigPayload {
    config: CanonicalConfig,
    actions: Vec<ActionRecord>,
    unknown: Vec<UnknownOwnershipAttribute>,
}

fn parse_cake_options(
    payload: &[u8],
    flags: u16,
    budget: &mut ReadBudget,
    limits: &NetlinkReadLimits,
) -> Result<ParsedConfigPayload, KernelTopologyError> {
    if !matches!(flags, 0 | NLA_F_NESTED) {
        return Ok(unknown_options(TopologyObject::Qdisc));
    }
    let mut attributes = Vec::new();
    let mut unknown = Vec::new();
    let mut cursor = AttributeCursor::new(payload);
    while let Some(attribute) = cursor.next(budget, limits)? {
        if attribute.flags != 0 {
            unknown.push(unknown_nested(
                TopologyObject::Qdisc,
                attribute.kind,
                vec![TCA_OPTIONS, attribute.kind],
            ));
            continue;
        }
        match attribute.kind {
            TCA_CAKE_PAD if attribute.payload.is_empty() => {}
            TCA_CAKE_BASE_RATE64 if attribute.payload.len() == 8 => push_config(
                &mut attributes,
                vec![attribute.kind],
                false,
                get_u64(attribute.payload, 0)?.to_be_bytes().to_vec(),
            )?,
            TCA_CAKE_OVERHEAD if attribute.payload.len() == 4 => push_config(
                &mut attributes,
                vec![attribute.kind],
                false,
                get_i32(attribute.payload, 0)?.to_be_bytes().to_vec(),
            )?,
            TCA_CAKE_DIFFSERV_MODE
            | TCA_CAKE_ATM
            | TCA_CAKE_FLOW_MODE
            | TCA_CAKE_RTT
            | TCA_CAKE_TARGET
            | TCA_CAKE_AUTORATE
            | TCA_CAKE_MEMORY
            | TCA_CAKE_NAT
            | TCA_CAKE_RAW
            | TCA_CAKE_WASH
            | TCA_CAKE_MPU
            | TCA_CAKE_INGRESS
            | TCA_CAKE_ACK_FILTER
            | TCA_CAKE_SPLIT_GSO
            | TCA_CAKE_FWMARK
                if attribute.payload.len() == 4 =>
            {
                push_config(
                    &mut attributes,
                    vec![attribute.kind],
                    false,
                    get_u32(attribute.payload, 0)?.to_be_bytes().to_vec(),
                )?
            }
            other => unknown.push(unknown_nested(
                TopologyObject::Qdisc,
                other,
                vec![TCA_OPTIONS, other],
            )),
        }
    }
    Ok(ParsedConfigPayload {
        config: CanonicalConfig { attributes },
        actions: Vec::new(),
        unknown,
    })
}

fn parse_fq_codel_options(
    payload: &[u8],
    flags: u16,
    budget: &mut ReadBudget,
    limits: &NetlinkReadLimits,
) -> Result<ParsedConfigPayload, KernelTopologyError> {
    if !matches!(flags, 0 | NLA_F_NESTED) {
        return Ok(unknown_options(TopologyObject::Qdisc));
    }
    let mut attributes = Vec::new();
    let mut unknown = Vec::new();
    let mut cursor = AttributeCursor::new(payload);
    while let Some(attribute) = cursor.next(budget, limits)? {
        if attribute.flags != 0 {
            unknown.push(unknown_nested(
                TopologyObject::Qdisc,
                attribute.kind,
                vec![TCA_OPTIONS, attribute.kind],
            ));
            continue;
        }
        match attribute.kind {
            TCA_FQ_CODEL_TARGET
            | TCA_FQ_CODEL_LIMIT
            | TCA_FQ_CODEL_INTERVAL
            | TCA_FQ_CODEL_ECN
            | TCA_FQ_CODEL_FLOWS
            | TCA_FQ_CODEL_QUANTUM
            | TCA_FQ_CODEL_CE_THRESHOLD
            | TCA_FQ_CODEL_DROP_BATCH_SIZE
            | TCA_FQ_CODEL_MEMORY_LIMIT
                if attribute.payload.len() == 4 =>
            {
                push_config(
                    &mut attributes,
                    vec![attribute.kind],
                    false,
                    get_u32(attribute.payload, 0)?.to_be_bytes().to_vec(),
                )?
            }
            TCA_FQ_CODEL_CE_THRESHOLD_SELECTOR | TCA_FQ_CODEL_CE_THRESHOLD_MASK
                if attribute.payload.len() == 1 =>
            {
                push_config(
                    &mut attributes,
                    vec![attribute.kind],
                    false,
                    attribute.payload.to_vec(),
                )?
            }
            other => unknown.push(unknown_nested(
                TopologyObject::Qdisc,
                other,
                vec![TCA_OPTIONS, other],
            )),
        }
    }
    Ok(ParsedConfigPayload {
        config: CanonicalConfig { attributes },
        actions: Vec::new(),
        unknown,
    })
}

fn parse_u32_options(
    payload: &[u8],
    flags: u16,
    budget: &mut ReadBudget,
    limits: &NetlinkReadLimits,
) -> Result<ParsedConfigPayload, KernelTopologyError> {
    if !matches!(flags, 0 | NLA_F_NESTED) {
        return Ok(unknown_options(TopologyObject::Filter));
    }
    let mut attributes = Vec::new();
    let mut actions = Vec::new();
    let mut unknown = Vec::new();
    let mut cursor = AttributeCursor::new(payload);
    while let Some(attribute) = cursor.next(budget, limits)? {
        if attribute.flags != 0 {
            unknown.push(unknown_nested(
                TopologyObject::Filter,
                attribute.kind,
                vec![TCA_OPTIONS, attribute.kind],
            ));
            continue;
        }
        match attribute.kind {
            TCA_U32_CLASSID | TCA_U32_HASH | TCA_U32_LINK | TCA_U32_DIVISOR | TCA_U32_FLAGS
                if attribute.payload.len() == 4 =>
            {
                push_config(
                    &mut attributes,
                    vec![attribute.kind],
                    false,
                    get_u32(attribute.payload, 0)?.to_be_bytes().to_vec(),
                )?
            }
            TCA_U32_SEL => parse_u32_selector(attribute.payload, &mut attributes)?,
            TCA_U32_ACT => {
                if !actions.is_empty() {
                    return Err(KernelTopologyError::Invalid(
                        "u32 classifier contains duplicate action lists".to_string(),
                    ));
                }
                let parsed = parse_embedded_actions(attribute.payload, budget, limits)?;
                actions = parsed.actions;
                unknown.extend(parsed.unknown);
            }
            TCA_U32_INDEV => {
                let value = parse_nul_string(attribute.payload, "TCA_U32_INDEV")?;
                push_config(
                    &mut attributes,
                    vec![attribute.kind],
                    false,
                    value.into_bytes(),
                )?;
            }
            TCA_U32_MARK if attribute.payload.len() == 12 => {
                push_config(
                    &mut attributes,
                    vec![attribute.kind, 1],
                    false,
                    get_u32(attribute.payload, 0)?.to_be_bytes().to_vec(),
                )?;
                push_config(
                    &mut attributes,
                    vec![attribute.kind, 2],
                    false,
                    get_u32(attribute.payload, 4)?.to_be_bytes().to_vec(),
                )?;
            }
            TCA_U32_PCNT => {}
            TCA_U32_PAD if attribute.payload.is_empty() => {}
            TCA_U32_POLICE | 0 => unknown.push(unknown_nested(
                TopologyObject::Filter,
                attribute.kind,
                vec![TCA_OPTIONS, attribute.kind],
            )),
            other => unknown.push(unknown_nested(
                TopologyObject::Filter,
                other,
                vec![TCA_OPTIONS, other],
            )),
        }
    }
    Ok(ParsedConfigPayload {
        config: CanonicalConfig { attributes },
        actions,
        unknown,
    })
}

fn parse_u32_selector(
    payload: &[u8],
    attributes: &mut Vec<CanonicalConfigAttribute>,
) -> Result<(), KernelTopologyError> {
    if payload.len() < TC_U32_SEL_LEN || payload[3] != 0 {
        return Err(KernelTopologyError::Invalid(
            "TCA_U32_SEL has an invalid fixed header".to_string(),
        ));
    }
    let key_count = payload[2] as usize;
    let expected = TC_U32_SEL_LEN
        .checked_add(key_count.checked_mul(TC_U32_KEY_LEN).ok_or_else(|| {
            KernelTopologyError::Limit("u32 selector key length overflow".to_string())
        })?)
        .ok_or_else(|| KernelTopologyError::Limit("u32 selector length overflow".to_string()))?;
    if payload.len() != expected {
        return Err(KernelTopologyError::Invalid(
            "TCA_U32_SEL key count does not match its payload".to_string(),
        ));
    }
    push_config(attributes, vec![TCA_U32_SEL, 1], false, vec![payload[0]])?;
    push_config(attributes, vec![TCA_U32_SEL, 2], false, vec![payload[1]])?;
    push_config(attributes, vec![TCA_U32_SEL, 3], false, vec![payload[2]])?;
    push_config(
        attributes,
        vec![TCA_U32_SEL, 4],
        true,
        payload[4..6].to_vec(),
    )?;
    push_config(
        attributes,
        vec![TCA_U32_SEL, 5],
        false,
        get_u16(payload, 6)?.to_be_bytes().to_vec(),
    )?;
    push_config(
        attributes,
        vec![TCA_U32_SEL, 6],
        false,
        get_i16(payload, 8)?.to_be_bytes().to_vec(),
    )?;
    push_config(
        attributes,
        vec![TCA_U32_SEL, 7],
        false,
        get_i16(payload, 10)?.to_be_bytes().to_vec(),
    )?;
    push_config(
        attributes,
        vec![TCA_U32_SEL, 8],
        true,
        payload[12..16].to_vec(),
    )?;
    for index in 0..key_count {
        let offset = TC_U32_SEL_LEN + index * TC_U32_KEY_LEN;
        let item = u16::try_from(index + 1).map_err(|_| {
            KernelTopologyError::Limit("u32 selector key index exceeds u16".to_string())
        })?;
        push_config(
            attributes,
            vec![TCA_U32_SEL, 9, item, 1],
            true,
            payload[offset..offset + 4].to_vec(),
        )?;
        push_config(
            attributes,
            vec![TCA_U32_SEL, 9, item, 2],
            true,
            payload[offset + 4..offset + 8].to_vec(),
        )?;
        push_config(
            attributes,
            vec![TCA_U32_SEL, 9, item, 3],
            false,
            get_i32(payload, offset + 8)?.to_be_bytes().to_vec(),
        )?;
        push_config(
            attributes,
            vec![TCA_U32_SEL, 9, item, 4],
            false,
            get_i32(payload, offset + 12)?.to_be_bytes().to_vec(),
        )?;
    }
    Ok(())
}

#[derive(Default)]
struct ParsedEmbeddedActions {
    actions: Vec<ActionRecord>,
    unknown: Vec<UnknownOwnershipAttribute>,
}

fn parse_embedded_actions(
    payload: &[u8],
    budget: &mut ReadBudget,
    limits: &NetlinkReadLimits,
) -> Result<ParsedEmbeddedActions, KernelTopologyError> {
    let mut actions = Vec::new();
    let mut unknown = Vec::new();
    let mut cursor = AttributeCursor::new(payload);
    while let Some(entry) = cursor.next(budget, limits)? {
        if entry.flags != 0 || entry.kind == 0 || usize::from(entry.kind) > MAX_ACTIONS_PER_FILTER {
            return Err(KernelTopologyError::Invalid(
                "u32 action list has an invalid order".to_string(),
            ));
        }
        let parsed = parse_embedded_action(entry.kind, entry.payload, budget, limits)?;
        actions.push(parsed.action);
        unknown.extend(parsed.unknown);
    }
    if actions.len() > MAX_ACTIONS_PER_FILTER {
        return Err(KernelTopologyError::Limit(
            "u32 action count exceeds its bound".to_string(),
        ));
    }
    Ok(ParsedEmbeddedActions { actions, unknown })
}

struct ParsedEmbeddedAction {
    action: ActionRecord,
    unknown: Vec<UnknownOwnershipAttribute>,
}

fn parse_embedded_action(
    order: u16,
    payload: &[u8],
    budget: &mut ReadBudget,
    limits: &NetlinkReadLimits,
) -> Result<ParsedEmbeddedAction, KernelTopologyError> {
    let mut kind = None;
    let mut top_index = None;
    let mut cookie = None;
    let mut raw_options = None;
    let mut unknown = Vec::new();
    let mut cursor = AttributeCursor::new(payload);
    while let Some(attribute) = cursor.next(budget, limits)? {
        match attribute.kind {
            TCA_ACT_KIND if attribute.flags == 0 => set_once(
                &mut kind,
                parse_nul_string(attribute.payload, "TCA_ACT_KIND")?,
                "TCA_ACT_KIND",
            )?,
            TCA_ACT_INDEX if attribute.flags == 0 && attribute.payload.len() == 4 => set_once(
                &mut top_index,
                get_u32(attribute.payload, 0)?,
                "TCA_ACT_INDEX",
            )?,
            TCA_ACT_COOKIE if attribute.flags == 0 => {
                if attribute.payload.is_empty() || attribute.payload.len() > MAX_ACTION_COOKIE_BYTES
                {
                    return Err(KernelTopologyError::Limit(
                        "embedded action cookie is empty or exceeds its bound".to_string(),
                    ));
                }
                set_once(&mut cookie, attribute.payload.to_vec(), "TCA_ACT_COOKIE")?;
            }
            TCA_ACT_OPTIONS => set_once(
                &mut raw_options,
                (attribute.flags, attribute.payload),
                "TCA_ACT_OPTIONS",
            )?,
            TCA_ACT_STATS | TCA_ACT_USED_HW_STATS | TCA_ACT_IN_HW_COUNT => {}
            TCA_ACT_PAD if attribute.flags == 0 && attribute.payload.is_empty() => {}
            TCA_ACT_FLAGS | TCA_ACT_HW_STATS => unknown.push(unknown_nested(
                TopologyObject::Action,
                attribute.kind,
                vec![TCA_U32_ACT, order, attribute.kind],
            )),
            other => unknown.push(unknown_nested(
                TopologyObject::Action,
                other,
                vec![TCA_U32_ACT, order, other],
            )),
        }
    }
    let kind = kind.ok_or_else(|| {
        KernelTopologyError::Invalid("embedded action is missing TCA_ACT_KIND".to_string())
    })?;
    validate_action_kind(&kind)?;
    let (action_index, options, target, nested_unknown) = match (kind.as_str(), raw_options) {
        ("mirred", Some((flags, payload))) => {
            parse_mirred_options(payload, flags, order, budget, limits)?
        }
        _ => {
            return Err(KernelTopologyError::Invalid(
                "only typed mirred actions are supported in the u32 ownership path".to_string(),
            ));
        }
    };
    if top_index.is_some_and(|index| index != action_index) {
        return Err(KernelTopologyError::Invalid(
            "embedded action index disagrees with its typed options".to_string(),
        ));
    }
    unknown.extend(nested_unknown);
    Ok(ParsedEmbeddedAction {
        action: ActionRecord {
            order,
            action_index,
            kind,
            cookie,
            options,
            target,
        },
        unknown,
    })
}

fn parse_mirred_options(
    payload: &[u8],
    flags: u16,
    order: u16,
    budget: &mut ReadBudget,
    limits: &NetlinkReadLimits,
) -> Result<
    (
        u32,
        CanonicalConfig,
        ActionTarget,
        Vec<UnknownOwnershipAttribute>,
    ),
    KernelTopologyError,
> {
    if !matches!(flags, 0 | NLA_F_NESTED) {
        return Err(KernelTopologyError::Invalid(
            "mirred options have invalid nesting flags".to_string(),
        ));
    }
    let mut parameters = None;
    let mut unknown = Vec::new();
    let mut cursor = AttributeCursor::new(payload);
    while let Some(attribute) = cursor.next(budget, limits)? {
        match attribute.kind {
            TCA_MIRRED_PARMS
                if attribute.flags == 0 && attribute.payload.len() == TC_MIRRED_LEN =>
            {
                set_once(&mut parameters, attribute.payload, "TCA_MIRRED_PARMS")?;
            }
            TCA_MIRRED_TM => {}
            TCA_MIRRED_PAD if attribute.flags == 0 && attribute.payload.is_empty() => {}
            TCA_MIRRED_BLOCKID | 0 => unknown.push(unknown_nested(
                TopologyObject::Action,
                attribute.kind,
                vec![TCA_U32_ACT, order, TCA_ACT_OPTIONS, attribute.kind],
            )),
            other => unknown.push(unknown_nested(
                TopologyObject::Action,
                other,
                vec![TCA_U32_ACT, order, TCA_ACT_OPTIONS, other],
            )),
        }
    }
    let parameters = parameters.ok_or_else(|| {
        KernelTopologyError::Invalid("mirred action is missing exact parameters".to_string())
    })?;
    let index = get_u32(parameters, 0)?;
    let capab = get_u32(parameters, 4)?;
    let action = get_i32(parameters, 8)?;
    let eaction = get_i32(parameters, 20)?;
    let ifindex = get_u32(parameters, 24)?;
    if index == 0 || ifindex == 0 || ifindex > i32::MAX as u32 {
        return Err(KernelTopologyError::Invalid(
            "mirred action has an invalid index or target ifindex".to_string(),
        ));
    }
    let direction = match eaction {
        TCA_EGRESS_REDIR => MirredDirection::EgressRedirect,
        TCA_INGRESS_REDIR => MirredDirection::IngressRedirect,
        TCA_EGRESS_MIRROR => MirredDirection::EgressMirror,
        TCA_INGRESS_MIRROR => MirredDirection::IngressMirror,
        _ => {
            return Err(KernelTopologyError::Invalid(
                "mirred action has an unknown direction".to_string(),
            ));
        }
    };
    let mut attributes = Vec::new();
    for (field, value) in [
        (1, index.to_be_bytes().to_vec()),
        (2, capab.to_be_bytes().to_vec()),
        (3, action.to_be_bytes().to_vec()),
        (4, eaction.to_be_bytes().to_vec()),
        (5, ifindex.to_be_bytes().to_vec()),
    ] {
        push_config(&mut attributes, vec![TCA_MIRRED_PARMS, field], false, value)?;
    }
    Ok((
        index,
        CanonicalConfig { attributes },
        ActionTarget::Mirred {
            direction,
            target_ifindex: ifindex,
        },
        unknown,
    ))
}

fn push_config(
    attributes: &mut Vec<CanonicalConfigAttribute>,
    path: Vec<u16>,
    network_byte_order: bool,
    value: Vec<u8>,
) -> Result<(), KernelTopologyError> {
    if attributes.len() >= MAX_CONFIG_ATTRIBUTES {
        return Err(KernelTopologyError::Limit(
            "canonical configuration attribute count exceeds its bound".to_string(),
        ));
    }
    if attributes.iter().any(|attribute| attribute.path == path) {
        return Err(KernelTopologyError::Invalid(
            "canonical configuration contains a duplicate path".to_string(),
        ));
    }
    attributes.push(CanonicalConfigAttribute {
        path,
        network_byte_order,
        value,
    });
    Ok(())
}

fn unknown_options(object: TopologyObject) -> ParsedConfigPayload {
    ParsedConfigPayload {
        unknown: vec![unknown_nested(object, TCA_OPTIONS, vec![TCA_OPTIONS])],
        ..ParsedConfigPayload::default()
    }
}

fn unknown_nested(
    object: TopologyObject,
    attribute_type: u16,
    nested_path: Vec<u16>,
) -> UnknownOwnershipAttribute {
    UnknownOwnershipAttribute {
        object,
        attribute_type,
        nested_path,
    }
}

fn parse_link_info(
    payload: &[u8],
    budget: &mut ReadBudget,
    limits: &NetlinkReadLimits,
) -> Result<(Option<String>, Vec<Vec<u16>>), KernelTopologyError> {
    let mut kind = None;
    let mut info_data = None;
    let mut unsupported = Vec::new();
    let mut cursor = AttributeCursor::new(payload);
    while let Some(attribute) = cursor.next(budget, limits)? {
        match attribute.kind {
            IFLA_INFO_KIND if attribute.flags == 0 => {
                set_once(
                    &mut kind,
                    parse_nul_string(attribute.payload, "IFLA_INFO_KIND")?,
                    "IFLA_INFO_KIND",
                )?;
            }
            IFLA_INFO_DATA if attribute.flags & !NLA_F_NESTED == 0 => {
                if info_data.replace(attribute.payload.is_empty()).is_some() {
                    return Err(KernelTopologyError::Invalid(
                        "netlink object contains duplicate IFLA_INFO_DATA".to_string(),
                    ));
                }
            }
            other => unsupported.push(vec![IFLA_LINKINFO, other]),
        }
    }
    if let Some(empty) = info_data {
        // Linux emits an empty IFLA_INFO_DATA nest for a PPP link even though
        // no kind-specific ownership state is present (iproute2 renders only
        // `info_kind: ppp`).  Accept exactly that empty, typed shape. Any PPP
        // payload, or INFO_DATA for a kind with real configurable topology,
        // remains ownership-relevant and fail-closed.
        if kind.as_deref() != Some("ppp") || !empty {
            unsupported.push(vec![IFLA_LINKINFO, IFLA_INFO_DATA]);
        }
    }
    Ok((kind, unsupported))
}

fn parse_link_property_list(
    payload: &[u8],
    budget: &mut ReadBudget,
    limits: &NetlinkReadLimits,
) -> Result<(Vec<String>, Vec<Vec<u16>>), KernelTopologyError> {
    let mut alternate_names = Vec::new();
    let mut unsupported = Vec::new();
    let mut cursor = AttributeCursor::new(payload);
    while let Some(attribute) = cursor.next(budget, limits)? {
        match attribute.kind {
            IFLA_ALT_IFNAME if attribute.flags == 0 => push_alternate_name(
                &mut alternate_names,
                parse_nul_string(attribute.payload, "IFLA_PROP_LIST/IFLA_ALT_IFNAME")?,
            )?,
            other => unsupported.push(vec![IFLA_PROP_LIST, other]),
        }
    }
    Ok((alternate_names, unsupported))
}

fn parse_unattached_xdp(
    payload: &[u8],
    budget: &mut ReadBudget,
    limits: &NetlinkReadLimits,
) -> Result<Vec<Vec<u16>>, KernelTopologyError> {
    let mut attached = None;
    let mut unsupported = Vec::new();
    let mut cursor = AttributeCursor::new(payload);
    while let Some(attribute) = cursor.next(budget, limits)? {
        match attribute.kind {
            IFLA_XDP_ATTACHED => {
                if attribute.flags != 0 || attribute.payload.len() != 1 {
                    return Err(KernelTopologyError::Invalid(
                        "IFLA_XDP_ATTACHED is not an exact u8 value".to_string(),
                    ));
                }
                set_once(&mut attached, attribute.payload[0], "IFLA_XDP_ATTACHED")?;
            }
            other => unsupported.push(vec![IFLA_XDP, other]),
        }
    }
    match attached {
        Some(XDP_ATTACHED_NONE) => {}
        Some(_) => unsupported.push(vec![IFLA_XDP, IFLA_XDP_ATTACHED]),
        None => unsupported.push(vec![IFLA_XDP]),
    }
    Ok(unsupported)
}

fn push_alternate_name(
    alternate_names: &mut Vec<String>,
    alternate: String,
) -> Result<(), KernelTopologyError> {
    if alternate_names.len() >= MAX_NAMESPACE_IDENTITIES {
        return Err(KernelTopologyError::Limit(
            "alternate interface name count exceeds its bound".to_string(),
        ));
    }
    if alternate_names
        .iter()
        .any(|existing| existing == &alternate)
    {
        return Err(KernelTopologyError::Invalid(
            "netlink link contains a duplicate alternate interface name".to_string(),
        ));
    }
    alternate_names.push(alternate);
    Ok(())
}

/// Link attributes which do not grant or change ownership of a link, qdisc,
/// filter or action slot. They are intentionally omitted from the canonical
/// witness: many are counters or carrier state and would make two exact reads
/// disagree, while IFLA_QDISC is independently covered by RTM_GETQDISC.
///
/// Namespace movement, master membership, VF port assignment and future
/// attributes are deliberately absent from this list. IFLA_XDP is handled by
/// a strict nested parser which accepts only the kernel's explicit
/// XDP_ATTACHED_NONE state; every real attachment remains fail-closed.
fn is_observation_only_link_attribute(attribute_type: u16) -> bool {
    matches!(
        attribute_type,
        IFLA_ADDRESS
            | IFLA_BROADCAST
            | IFLA_MTU
            | IFLA_QDISC
            | IFLA_STATS
            | IFLA_COST
            | IFLA_PRIORITY
            | IFLA_WIRELESS
            | IFLA_PROTINFO
            | IFLA_TXQLEN
            | IFLA_MAP
            | IFLA_WEIGHT
            | IFLA_OPERSTATE
            | IFLA_LINKMODE
            | IFLA_NUM_VF
            | IFLA_STATS64
            | IFLA_AF_SPEC
            | IFLA_GROUP
            | IFLA_EXT_MASK
            | IFLA_PROMISCUITY
            | IFLA_NUM_TX_QUEUES
            | IFLA_NUM_RX_QUEUES
            | IFLA_CARRIER
            | IFLA_PHYS_PORT_ID
            | IFLA_CARRIER_CHANGES
            | IFLA_PHYS_SWITCH_ID
            | IFLA_PHYS_PORT_NAME
            | IFLA_PROTO_DOWN
            | IFLA_GSO_MAX_SEGS
            | IFLA_GSO_MAX_SIZE
            | IFLA_PAD
            | IFLA_EVENT
            | IFLA_CARRIER_UP_COUNT
            | IFLA_CARRIER_DOWN_COUNT
            | IFLA_MIN_MTU
            | IFLA_MAX_MTU
            | IFLA_PERM_ADDRESS
            | IFLA_PROTO_DOWN_REASON
            | IFLA_PARENT_DEV_NAME
            | IFLA_PARENT_DEV_BUS_NAME
            | IFLA_GRO_MAX_SIZE
            | IFLA_TSO_MAX_SIZE
            | IFLA_TSO_MAX_SEGS
            | IFLA_ALLMULTI
            | IFLA_DEVLINK_PORT
            | IFLA_GSO_IPV4_MAX_SIZE
            | IFLA_GRO_IPV4_MAX_SIZE
            | IFLA_DPLL_PIN
    )
}

fn is_known_ownership_link_attribute(attribute_type: u16) -> bool {
    matches!(
        attribute_type,
        IFLA_MASTER
            | IFLA_NET_NS_PID
            | IFLA_VFINFO_LIST
            | IFLA_VF_PORTS
            | IFLA_PORT_SELF
            | IFLA_NET_NS_FD
            | IFLA_LINK_NETNSID
            | IFLA_NEW_NETNSID
            | IFLA_IF_NETNSID
            | IFLA_NEW_IFINDEX
    )
}

struct NetlinkAttribute<'a> {
    kind: u16,
    flags: u16,
    payload: &'a [u8],
}

struct AttributeCursor<'a> {
    bytes: &'a [u8],
    offset: usize,
    allow_zero_kind: bool,
}

impl<'a> AttributeCursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            offset: 0,
            allow_zero_kind: false,
        }
    }

    fn new_allow_zero(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            offset: 0,
            allow_zero_kind: true,
        }
    }

    fn next(
        &mut self,
        budget: &mut ReadBudget,
        limits: &NetlinkReadLimits,
    ) -> Result<Option<NetlinkAttribute<'a>>, KernelTopologyError> {
        if self.offset == self.bytes.len() {
            return Ok(None);
        }
        if self.bytes.len() - self.offset < NLA_HEADER_LEN {
            return Err(KernelTopologyError::Backend(
                "netlink attribute framing is truncated".to_string(),
            ));
        }
        budget.charge_attribute(limits)?;
        let length = get_u16(self.bytes, self.offset)? as usize;
        let raw_type = get_u16(self.bytes, self.offset + 2)?;
        let kind = raw_type & NLA_TYPE_MASK;
        if length < NLA_HEADER_LEN
            || (kind == 0 && !self.allow_zero_kind)
            || length > self.bytes.len() - self.offset
        {
            return Err(KernelTopologyError::Backend(
                "netlink attribute header is malformed".to_string(),
            ));
        }
        let aligned = align4(length)?;
        if aligned > self.bytes.len() - self.offset {
            return Err(KernelTopologyError::Backend(
                "netlink attribute alignment exceeds its container".to_string(),
            ));
        }
        let payload = &self.bytes[self.offset + NLA_HEADER_LEN..self.offset + length];
        self.offset = self
            .offset
            .checked_add(aligned)
            .ok_or_else(|| KernelTopologyError::Limit("attribute offset overflow".to_string()))?;
        Ok(Some(NetlinkAttribute {
            kind,
            flags: raw_type & !NLA_TYPE_MASK,
            payload,
        }))
    }
}

fn set_once<T>(slot: &mut Option<T>, value: T, label: &str) -> Result<(), KernelTopologyError> {
    if slot.replace(value).is_some() {
        return Err(KernelTopologyError::Invalid(format!(
            "netlink object contains duplicate {label}"
        )));
    }
    Ok(())
}

fn parse_nul_string(bytes: &[u8], label: &str) -> Result<String, KernelTopologyError> {
    if bytes.is_empty() || *bytes.last().unwrap_or(&1) != 0 || bytes[..bytes.len() - 1].contains(&0)
    {
        return Err(KernelTopologyError::Invalid(format!(
            "{label} is not exactly one NUL-terminated string"
        )));
    }
    let value = std::str::from_utf8(&bytes[..bytes.len() - 1])
        .map_err(|_| KernelTopologyError::Invalid(format!("{label} is not valid UTF-8")))?;
    Ok(value.to_string())
}

fn align4(value: usize) -> Result<usize, KernelTopologyError> {
    value
        .checked_add(3)
        .map(|aligned| aligned & !3)
        .ok_or_else(|| KernelTopologyError::Limit("netlink alignment overflow".to_string()))
}

fn get_u16(bytes: &[u8], offset: usize) -> Result<u16, KernelTopologyError> {
    let value = bytes
        .get(offset..offset + 2)
        .ok_or_else(|| KernelTopologyError::Backend("netlink integer is truncated".to_string()))?;
    Ok(u16::from_ne_bytes([value[0], value[1]]))
}

fn get_u32(bytes: &[u8], offset: usize) -> Result<u32, KernelTopologyError> {
    let value = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| KernelTopologyError::Backend("netlink integer is truncated".to_string()))?;
    Ok(u32::from_ne_bytes([value[0], value[1], value[2], value[3]]))
}

fn get_u64(bytes: &[u8], offset: usize) -> Result<u64, KernelTopologyError> {
    let value = bytes
        .get(offset..offset + 8)
        .ok_or_else(|| KernelTopologyError::Backend("netlink integer is truncated".to_string()))?;
    Ok(u64::from_ne_bytes([
        value[0], value[1], value[2], value[3], value[4], value[5], value[6], value[7],
    ]))
}

fn get_i16(bytes: &[u8], offset: usize) -> Result<i16, KernelTopologyError> {
    let value = bytes
        .get(offset..offset + 2)
        .ok_or_else(|| KernelTopologyError::Backend("netlink integer is truncated".to_string()))?;
    Ok(i16::from_ne_bytes([value[0], value[1]]))
}

fn get_i32(bytes: &[u8], offset: usize) -> Result<i32, KernelTopologyError> {
    let value = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| KernelTopologyError::Backend("netlink integer is truncated".to_string()))?;
    Ok(i32::from_ne_bytes([value[0], value[1], value[2], value[3]]))
}

fn put_u16(bytes: &mut [u8], offset: usize, value: u16) -> Result<(), KernelTopologyError> {
    let destination = bytes.get_mut(offset..offset + 2).ok_or_else(|| {
        KernelTopologyError::Limit("netlink request buffer is too small".to_string())
    })?;
    destination.copy_from_slice(&value.to_ne_bytes());
    Ok(())
}

fn put_u32(bytes: &mut [u8], offset: usize, value: u32) -> Result<(), KernelTopologyError> {
    let destination = bytes.get_mut(offset..offset + 4).ok_or_else(|| {
        KernelTopologyError::Limit("netlink request buffer is too small".to_string())
    })?;
    destination.copy_from_slice(&value.to_ne_bytes());
    Ok(())
}

/// Linux read-only adapter. Construction enables strict checking and binds a
/// dedicated unicast socket; methods only send `RTM_GET*` requests produced by
/// this module.
#[cfg(target_os = "linux")]
pub struct LinuxNetlinkIo {
    fd: OwnedFd,
    local_port_id: u32,
    netns_cookie: u64,
}

#[cfg(target_os = "linux")]
impl LinuxNetlinkIo {
    pub fn open() -> Result<Self, KernelTopologyError> {
        let raw_fd = unsafe {
            libc::socket(
                libc::AF_NETLINK,
                libc::SOCK_RAW | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
                libc::NETLINK_ROUTE,
            )
        };
        if raw_fd < 0 {
            return Err(last_io_error("open NETLINK_ROUTE socket"));
        }
        let fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
        let enabled: libc::c_int = 1;
        let strict_result = unsafe {
            libc::setsockopt(
                fd.as_raw_fd(),
                libc::SOL_NETLINK,
                libc::NETLINK_GET_STRICT_CHK,
                (&enabled as *const libc::c_int).cast(),
                size_of::<libc::c_int>() as libc::socklen_t,
            )
        };
        if strict_result != 0 {
            return Err(last_io_error("enable NETLINK_GET_STRICT_CHK"));
        }

        let mut local: libc::sockaddr_nl = unsafe { mem::zeroed() };
        local.nl_family = libc::AF_NETLINK as libc::sa_family_t;
        local.nl_pid = 0;
        local.nl_groups = 0;
        let bind_result = unsafe {
            libc::bind(
                fd.as_raw_fd(),
                (&local as *const libc::sockaddr_nl).cast(),
                size_of::<libc::sockaddr_nl>() as libc::socklen_t,
            )
        };
        if bind_result != 0 {
            return Err(last_io_error("bind NETLINK_ROUTE socket"));
        }

        let mut bound: libc::sockaddr_nl = unsafe { mem::zeroed() };
        let mut bound_len = size_of::<libc::sockaddr_nl>() as libc::socklen_t;
        let name_result = unsafe {
            libc::getsockname(
                fd.as_raw_fd(),
                (&mut bound as *mut libc::sockaddr_nl).cast(),
                &mut bound_len,
            )
        };
        if name_result != 0 {
            return Err(last_io_error("read NETLINK_ROUTE socket identity"));
        }
        if bound_len as usize != size_of::<libc::sockaddr_nl>()
            || bound.nl_family != libc::AF_NETLINK as libc::sa_family_t
            || bound.nl_pid == 0
            || bound.nl_groups != 0
        {
            return Err(KernelTopologyError::Backend(
                "NETLINK_ROUTE socket identity is invalid".to_string(),
            ));
        }

        let netns_cookie = read_socket_netns_cookie(fd.as_raw_fd())?;
        Ok(Self {
            fd,
            local_port_id: bound.nl_pid,
            netns_cookie,
        })
    }
}

#[cfg(target_os = "linux")]
impl NetlinkIo for LinuxNetlinkIo {
    fn local_port_id(&self) -> u32 {
        self.local_port_id
    }

    fn netns_cookie(&self) -> Result<u64, KernelTopologyError> {
        let observed = read_socket_netns_cookie(self.fd.as_raw_fd())?;
        if observed != self.netns_cookie {
            return Err(KernelTopologyError::Backend(
                "NETLINK_ROUTE socket namespace cookie changed".to_string(),
            ));
        }
        Ok(observed)
    }

    fn send_request(
        &mut self,
        request: &[u8],
        deadline: Instant,
    ) -> Result<(), KernelTopologyError> {
        validate_canonical_get_request(request, self.local_port_id)?;
        let mut kernel: libc::sockaddr_nl = unsafe { mem::zeroed() };
        kernel.nl_family = libc::AF_NETLINK as libc::sa_family_t;
        loop {
            if Instant::now() >= deadline {
                return Err(KernelTopologyError::Backend(
                    "NETLINK_ROUTE deadline expired".to_string(),
                ));
            }
            let sent = unsafe {
                libc::sendto(
                    self.fd.as_raw_fd(),
                    request.as_ptr().cast(),
                    request.len(),
                    libc::MSG_DONTWAIT,
                    (&kernel as *const libc::sockaddr_nl).cast(),
                    size_of::<libc::sockaddr_nl>() as libc::socklen_t,
                )
            };
            if sent == request.len() as isize {
                return Ok(());
            }
            if sent >= 0 {
                return Err(KernelTopologyError::Backend(
                    "NETLINK_ROUTE request was only partially sent".to_string(),
                ));
            }
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            if error.kind() == io::ErrorKind::WouldBlock {
                poll_fd(self.fd.as_raw_fd(), libc::POLLOUT, deadline)?;
                continue;
            }
            return Err(KernelTopologyError::Backend(format!(
                "send NETLINK_ROUTE request: {error}"
            )));
        }
    }

    fn receive_datagram(
        &mut self,
        buffer: &mut [u8],
        deadline: Instant,
    ) -> Result<ReceivedDatagram, KernelTopologyError> {
        if buffer.len() != RECEIVE_BUFFER_BYTES {
            return Err(KernelTopologyError::Invalid(
                "NETLINK_ROUTE receive buffer is not the fixed bounded size".to_string(),
            ));
        }
        loop {
            poll_fd(self.fd.as_raw_fd(), libc::POLLIN, deadline)?;
            let mut sender: libc::sockaddr_nl = unsafe { mem::zeroed() };
            let mut iovec = libc::iovec {
                iov_base: buffer.as_mut_ptr().cast(),
                iov_len: buffer.len(),
            };
            let mut message: libc::msghdr = unsafe { mem::zeroed() };
            message.msg_name = (&mut sender as *mut libc::sockaddr_nl).cast();
            message.msg_namelen = size_of::<libc::sockaddr_nl>() as libc::socklen_t;
            message.msg_iov = &mut iovec;
            message.msg_iovlen = 1;
            let received =
                unsafe { libc::recvmsg(self.fd.as_raw_fd(), &mut message, libc::MSG_DONTWAIT) };
            if received > 0 {
                if message.msg_namelen as usize != size_of::<libc::sockaddr_nl>() {
                    return Err(KernelTopologyError::Backend(
                        "NETLINK_ROUTE sender address has an invalid length".to_string(),
                    ));
                }
                return Ok(ReceivedDatagram {
                    len: received as usize,
                    sender_family: sender.nl_family,
                    sender_port_id: sender.nl_pid,
                    sender_groups: sender.nl_groups,
                    message_flags: message.msg_flags,
                });
            }
            if received == 0 {
                return Err(KernelTopologyError::Backend(
                    "NETLINK_ROUTE socket returned end-of-file".to_string(),
                ));
            }
            let error = io::Error::last_os_error();
            if matches!(
                error.kind(),
                io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
            ) {
                continue;
            }
            return Err(KernelTopologyError::Backend(format!(
                "receive NETLINK_ROUTE datagram: {error}"
            )));
        }
    }
}

#[cfg(target_os = "linux")]
fn poll_fd(
    fd: libc::c_int,
    events: libc::c_short,
    deadline: Instant,
) -> Result<(), KernelTopologyError> {
    loop {
        let now = Instant::now();
        if now >= deadline {
            return Err(KernelTopologyError::Backend(
                "NETLINK_ROUTE deadline expired".to_string(),
            ));
        }
        let remaining = deadline.saturating_duration_since(now);
        let millis = remaining.as_millis().min(i32::MAX as u128) as i32;
        let timeout = millis.max(1);
        let mut descriptor = libc::pollfd {
            fd,
            events,
            revents: 0,
        };
        let result = unsafe { libc::poll(&mut descriptor, 1, timeout) };
        if result > 0 {
            if descriptor.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                return Err(KernelTopologyError::Backend(
                    "NETLINK_ROUTE socket reported a poll failure".to_string(),
                ));
            }
            if descriptor.revents & events != 0 {
                return Ok(());
            }
            continue;
        }
        if result == 0 {
            continue;
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted {
            continue;
        }
        return Err(KernelTopologyError::Backend(format!(
            "poll NETLINK_ROUTE socket: {error}"
        )));
    }
}

#[cfg(target_os = "linux")]
fn last_io_error(action: &str) -> KernelTopologyError {
    KernelTopologyError::Backend(format!("{action}: {}", io::Error::last_os_error()))
}

#[cfg(target_os = "linux")]
fn validate_canonical_get_request(
    request: &[u8],
    local_port_id: u32,
) -> Result<(), KernelTopologyError> {
    if local_port_id == 0
        || request.len() < NLMSG_HEADER_LEN
        || get_u32(request, 0)? as usize != request.len()
        || get_u16(request, 6)? != (NLM_F_REQUEST | NLM_F_DUMP)
        || get_u32(request, 8)? == 0
        || get_u32(request, 12)? != local_port_id
    {
        return Err(KernelTopologyError::Invalid(
            "Linux netlink adapter rejected a non-canonical read request".to_string(),
        ));
    }
    let request_type = get_u16(request, 4)?;
    let canonical = match request_type {
        RTM_GETLINK => {
            request.len() == NLMSG_HEADER_LEN + IFINFO_LEN
                && request[NLMSG_HEADER_LEN..].iter().all(|byte| *byte == 0)
        }
        RTM_GETQDISC => {
            request.len() == NLMSG_HEADER_LEN + TCMSG_LEN + NLA_HEADER_LEN
                && request[NLMSG_HEADER_LEN..NLMSG_HEADER_LEN + TCMSG_LEN]
                    .iter()
                    .all(|byte| *byte == 0)
                && get_u16(request, NLMSG_HEADER_LEN + TCMSG_LEN)? == NLA_HEADER_LEN as u16
                && get_u16(request, NLMSG_HEADER_LEN + TCMSG_LEN + 2)? == TCA_DUMP_INVISIBLE
        }
        RTM_GETTFILTER => {
            if request.len() != NLMSG_HEADER_LEN + TCMSG_LEN {
                false
            } else {
                let ifindex = get_u32(request, NLMSG_HEADER_LEN + 4)?;
                request[NLMSG_HEADER_LEN..NLMSG_HEADER_LEN + 4]
                    .iter()
                    .all(|byte| *byte == 0)
                    && ifindex != 0
                    && ifindex <= i32::MAX as u32
                    && get_u32(request, NLMSG_HEADER_LEN + 8)? == 0
                    && matches!(
                        get_u32(request, NLMSG_HEADER_LEN + 12)?,
                        TC_H_MIN_INGRESS | TC_H_MIN_EGRESS
                    )
                    && get_u32(request, NLMSG_HEADER_LEN + 16)? == 0
            }
        }
        RTM_GETACTION => decode_canonical_action_request_kind(request)
            .and_then(|kind| encode_action_dump_request(&kind, get_u32(request, 8)?, local_port_id))
            .is_ok_and(|encoded| encoded == request),
        _ => false,
    };
    if !canonical {
        return Err(KernelTopologyError::Invalid(
            "Linux netlink adapter rejected a non-canonical read request".to_string(),
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn decode_canonical_action_request_kind(request: &[u8]) -> Result<String, KernelTopologyError> {
    if request.len() <= NLMSG_HEADER_LEN + TCAMSG_LEN
        || request[NLMSG_HEADER_LEN..NLMSG_HEADER_LEN + TCAMSG_LEN]
            .iter()
            .any(|byte| *byte != 0)
    {
        return Err(KernelTopologyError::Invalid(
            "RTM_GETACTION request has noncanonical tcamsg fields".to_string(),
        ));
    }
    let limits = NetlinkReadLimits::default();
    let mut budget = ReadBudget::default();
    let mut table = None;
    let mut flags = None;
    let mut cursor = AttributeCursor::new(&request[NLMSG_HEADER_LEN + TCAMSG_LEN..]);
    while let Some(attribute) = cursor.next(&mut budget, &limits)? {
        match attribute.kind {
            TCA_ACT_TAB if attribute.flags == NLA_F_NESTED => {
                set_once(&mut table, attribute.payload, "TCA_ACT_TAB")?;
            }
            TCA_ROOT_FLAGS if attribute.flags == 0 && attribute.payload.len() == 8 => {
                let value = get_u32(attribute.payload, 0)?;
                let selector = get_u32(attribute.payload, 4)?;
                if value != TCA_ACT_FLAG_LARGE_DUMP_ON | TCA_ACT_FLAG_TERSE_DUMP
                    || selector != value
                {
                    return Err(KernelTopologyError::Invalid(
                        "RTM_GETACTION request has noncanonical flags".to_string(),
                    ));
                }
                set_once(&mut flags, (), "TCA_ROOT_FLAGS")?;
            }
            _ => {
                return Err(KernelTopologyError::Invalid(
                    "RTM_GETACTION request has an unexpected root attribute".to_string(),
                ));
            }
        }
    }
    flags.ok_or_else(|| {
        KernelTopologyError::Invalid("RTM_GETACTION request is missing flags".to_string())
    })?;
    let table = table.ok_or_else(|| {
        KernelTopologyError::Invalid("RTM_GETACTION request is missing its table".to_string())
    })?;
    let mut table_cursor = AttributeCursor::new(table);
    let entry = table_cursor
        .next(&mut budget, &limits)?
        .filter(|entry| entry.kind == 1 && entry.flags == 0)
        .ok_or_else(|| {
            KernelTopologyError::Invalid("RTM_GETACTION request table is malformed".to_string())
        })?;
    if table_cursor.next(&mut budget, &limits)?.is_some() {
        return Err(KernelTopologyError::Invalid(
            "RTM_GETACTION request has multiple action selectors".to_string(),
        ));
    }
    let mut kind = None;
    let mut entry_cursor = AttributeCursor::new(entry.payload);
    while let Some(attribute) = entry_cursor.next(&mut budget, &limits)? {
        if attribute.kind != TCA_ACT_KIND || attribute.flags != 0 {
            return Err(KernelTopologyError::Invalid(
                "RTM_GETACTION request selector is malformed".to_string(),
            ));
        }
        set_once(
            &mut kind,
            parse_nul_string(attribute.payload, "TCA_ACT_KIND")?,
            "TCA_ACT_KIND",
        )?;
    }
    kind.ok_or_else(|| {
        KernelTopologyError::Invalid("RTM_GETACTION request has no action kind".to_string())
    })
}

#[cfg(target_os = "linux")]
fn read_socket_netns_cookie(fd: libc::c_int) -> Result<u64, KernelTopologyError> {
    let mut cookie = 0u64;
    let mut cookie_len = size_of::<u64>() as libc::socklen_t;
    let result = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_NETNS_COOKIE,
            (&mut cookie as *mut u64).cast(),
            &mut cookie_len,
        )
    };
    if result != 0 {
        return Err(last_io_error("read SO_NETNS_COOKIE"));
    }
    if cookie_len as usize != size_of::<u64>() || cookie == 0 {
        return Err(KernelTopologyError::Backend(
            "SO_NETNS_COOKIE returned an invalid value".to_string(),
        ));
    }
    Ok(cookie)
}

#[cfg(test)]
mod tests {
    use super::super::kernel_topology::capture_read_only_witness;
    use super::super::protocol::{OperationRouteIdentity, OperationRouteMode};
    use super::*;
    use std::cell::Cell;
    use std::collections::VecDeque;
    use std::net::{IpAddr, Ipv4Addr};

    const PORT_ID: u32 = 4_242;
    const NETNS_COOKIE: u64 = 0x1020_3040_5060_7080;

    struct FakeNetlinkIo {
        replies: VecDeque<(Vec<u8>, u32, i32)>,
        sent: Vec<Vec<u8>>,
        cookie: u64,
        local_port_reads: Cell<usize>,
        cookie_reads: Cell<usize>,
        receive_calls: usize,
    }

    impl FakeNetlinkIo {
        fn new(replies: Vec<Vec<u8>>) -> Self {
            Self {
                replies: replies.into_iter().map(|reply| (reply, 0, 0)).collect(),
                sent: Vec::new(),
                cookie: NETNS_COOKIE,
                local_port_reads: Cell::new(0),
                cookie_reads: Cell::new(0),
                receive_calls: 0,
            }
        }
    }

    impl NetlinkIo for FakeNetlinkIo {
        fn local_port_id(&self) -> u32 {
            self.local_port_reads
                .set(self.local_port_reads.get().saturating_add(1));
            PORT_ID
        }

        fn netns_cookie(&self) -> Result<u64, KernelTopologyError> {
            self.cookie_reads
                .set(self.cookie_reads.get().saturating_add(1));
            Ok(self.cookie)
        }

        fn send_request(
            &mut self,
            request: &[u8],
            _deadline: Instant,
        ) -> Result<(), KernelTopologyError> {
            self.sent.push(request.to_vec());
            Ok(())
        }

        fn receive_datagram(
            &mut self,
            buffer: &mut [u8],
            _deadline: Instant,
        ) -> Result<ReceivedDatagram, KernelTopologyError> {
            self.receive_calls = self.receive_calls.saturating_add(1);
            let (reply, sender_port_id, message_flags) =
                self.replies.pop_front().ok_or_else(|| {
                    KernelTopologyError::Backend("fake netlink reply queue exhausted".to_string())
                })?;
            if reply.len() > buffer.len() {
                return Err(KernelTopologyError::Limit(
                    "fake reply exceeds buffer".to_string(),
                ));
            }
            buffer[..reply.len()].copy_from_slice(&reply);
            Ok(ReceivedDatagram {
                len: reply.len(),
                sender_family: libc::AF_NETLINK as u16,
                sender_port_id,
                sender_groups: 0,
                message_flags,
            })
        }
    }

    fn query() -> KernelTopologyQuery {
        KernelTopologyQuery {
            target_interface: "eth0".to_string(),
            route: OperationRouteIdentity {
                mode: OperationRouteMode::Main,
                mwan3_member: None,
                l3_device: "eth0".to_string(),
                source_ip: Some(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10))),
                fwmark: None,
                routing_table: Some(254),
            },
            private_namespace: Default::default(),
        }
    }

    fn nla(kind: u16, payload: &[u8]) -> Vec<u8> {
        let length = NLA_HEADER_LEN + payload.len();
        let aligned = (length + 3) & !3;
        let mut bytes = vec![0u8; aligned];
        bytes[..2].copy_from_slice(&(length as u16).to_ne_bytes());
        bytes[2..4].copy_from_slice(&kind.to_ne_bytes());
        bytes[4..4 + payload.len()].copy_from_slice(payload);
        bytes
    }

    fn custom_link_payload(
        ifindex: u32,
        name: &str,
        alias: Option<&str>,
        include_kind: bool,
    ) -> Vec<u8> {
        let mut payload = vec![0u8; IFINFO_LEN];
        payload[4..8].copy_from_slice(&(ifindex as i32).to_ne_bytes());
        let mut encoded_name = name.as_bytes().to_vec();
        encoded_name.push(0);
        payload.extend_from_slice(&nla(IFLA_IFNAME, &encoded_name));
        if let Some(alias) = alias {
            let mut encoded_alias = alias.as_bytes().to_vec();
            encoded_alias.push(0);
            payload.extend_from_slice(&nla(IFLA_IFALIAS, &encoded_alias));
        }
        if include_kind {
            let nested = nla(IFLA_INFO_KIND, b"ether\0");
            payload.extend_from_slice(&nla(IFLA_LINKINFO | NLA_F_NESTED, &nested));
        }
        payload
    }

    fn link_payload(ifindex: u32, include_kind: bool) -> Vec<u8> {
        custom_link_payload(ifindex, "eth0", None, include_kind)
    }

    fn ordinary_openwrt_link_payload(ifindex: u32) -> Vec<u8> {
        let mut payload = link_payload(ifindex, false);
        for attribute_type in [
            IFLA_ADDRESS,
            IFLA_BROADCAST,
            IFLA_MTU,
            IFLA_QDISC,
            IFLA_STATS,
            IFLA_COST,
            IFLA_PRIORITY,
            IFLA_WIRELESS,
            IFLA_PROTINFO,
            IFLA_TXQLEN,
            IFLA_MAP,
            IFLA_WEIGHT,
            IFLA_OPERSTATE,
            IFLA_LINKMODE,
            IFLA_NUM_VF,
            IFLA_STATS64,
            IFLA_AF_SPEC,
            IFLA_GROUP,
            IFLA_EXT_MASK,
            IFLA_PROMISCUITY,
            IFLA_NUM_TX_QUEUES,
            IFLA_NUM_RX_QUEUES,
            IFLA_CARRIER,
            IFLA_PHYS_PORT_ID,
            IFLA_CARRIER_CHANGES,
            IFLA_PHYS_SWITCH_ID,
            IFLA_PHYS_PORT_NAME,
            IFLA_PROTO_DOWN,
            IFLA_GSO_MAX_SEGS,
            IFLA_GSO_MAX_SIZE,
            IFLA_PAD,
            IFLA_EVENT,
            IFLA_CARRIER_UP_COUNT,
            IFLA_CARRIER_DOWN_COUNT,
            IFLA_MIN_MTU,
            IFLA_MAX_MTU,
            IFLA_PERM_ADDRESS,
            IFLA_PROTO_DOWN_REASON,
            IFLA_PARENT_DEV_NAME,
            IFLA_PARENT_DEV_BUS_NAME,
            IFLA_GRO_MAX_SIZE,
            IFLA_TSO_MAX_SIZE,
            IFLA_TSO_MAX_SEGS,
            IFLA_ALLMULTI,
            IFLA_DEVLINK_PORT,
            IFLA_GSO_IPV4_MAX_SIZE,
            IFLA_GRO_IPV4_MAX_SIZE,
            IFLA_DPLL_PIN,
        ] {
            payload.extend_from_slice(&nla(attribute_type, &[]));
        }
        let xdp = nla(IFLA_XDP_ATTACHED, &[XDP_ATTACHED_NONE]);
        payload.extend_from_slice(&nla(IFLA_XDP | NLA_F_NESTED, &xdp));
        payload
    }

    fn private_link_payload(ifindex: u32, name: &str, alias: &str) -> Vec<u8> {
        let mut payload = custom_link_payload(ifindex, name, Some(alias), false);
        let nested = nla(IFLA_INFO_KIND, b"ifb\0");
        payload.extend_from_slice(&nla(IFLA_LINKINFO | NLA_F_NESTED, &nested));
        payload
    }

    fn link_with_info_data(ifindex: u32, kind: &str, data: &[u8]) -> Vec<u8> {
        let mut payload = custom_link_payload(ifindex, "eth0", None, false);
        let mut encoded_kind = kind.as_bytes().to_vec();
        encoded_kind.push(0);
        let mut nested = nla(IFLA_INFO_KIND, &encoded_kind);
        nested.extend_from_slice(&nla(IFLA_INFO_DATA | NLA_F_NESTED, data));
        payload.extend_from_slice(&nla(IFLA_LINKINFO | NLA_F_NESTED, &nested));
        payload
    }

    fn qdisc_payload(options: bool) -> Vec<u8> {
        let mut payload = vec![0u8; TCMSG_LEN];
        payload[4..8].copy_from_slice(&10i32.to_ne_bytes());
        payload[12..16].copy_from_slice(&TC_H_ROOT.to_ne_bytes());
        payload.extend_from_slice(&nla(TCA_KIND, b"cake\0"));
        if options {
            payload.extend_from_slice(&nla(TCA_OPTIONS, &[1, 2, 3, 4]));
        }
        payload
    }

    fn fq_codel_qdisc_payload(ifindex: u32) -> Vec<u8> {
        fq_codel_qdisc_payload_with_offload(ifindex, &[0])
    }

    fn mq_qdisc_payload(ifindex: u32) -> Vec<u8> {
        let mut payload = vec![0u8; TCMSG_LEN];
        payload[4..8].copy_from_slice(&(ifindex as i32).to_ne_bytes());
        payload[12..16].copy_from_slice(&TC_H_ROOT.to_ne_bytes());
        payload.extend_from_slice(&nla(TCA_KIND, b"mq\0"));
        payload
    }

    fn fq_codel_leaf_qdisc_payload(ifindex: u32, parent: u32, handle: u32) -> Vec<u8> {
        let mut payload = fq_codel_qdisc_payload(ifindex);
        payload[8..12].copy_from_slice(&handle.to_ne_bytes());
        payload[12..16].copy_from_slice(&parent.to_ne_bytes());
        payload
    }

    fn fq_codel_qdisc_payload_with_offload(ifindex: u32, offload: &[u8]) -> Vec<u8> {
        let mut payload = vec![0u8; TCMSG_LEN];
        payload[4..8].copy_from_slice(&(ifindex as i32).to_ne_bytes());
        payload[12..16].copy_from_slice(&TC_H_ROOT.to_ne_bytes());
        payload.extend_from_slice(&nla(TCA_KIND, b"fq_codel\0"));
        let mut options = Vec::new();
        for (kind, value) in [
            (TCA_FQ_CODEL_TARGET, 5_000u32),
            (TCA_FQ_CODEL_LIMIT, 10_240),
            (TCA_FQ_CODEL_INTERVAL, 100_000),
            (TCA_FQ_CODEL_ECN, 1),
            (TCA_FQ_CODEL_FLOWS, 1_024),
            (TCA_FQ_CODEL_QUANTUM, 1_514),
            (TCA_FQ_CODEL_DROP_BATCH_SIZE, 64),
            (TCA_FQ_CODEL_MEMORY_LIMIT, 32 * 1024 * 1024),
        ] {
            options.extend_from_slice(&nla(kind, &value.to_ne_bytes()));
        }
        payload.extend_from_slice(&nla(TCA_OPTIONS, &options));
        payload.extend_from_slice(&nla(TCA_HW_OFFLOAD, offload));
        payload
    }

    fn cake_qdisc_payload(ifindex: u32, handle: u32, rate_bps: u64) -> Vec<u8> {
        let mut payload = vec![0u8; TCMSG_LEN];
        payload[4..8].copy_from_slice(&(ifindex as i32).to_ne_bytes());
        payload[8..12].copy_from_slice(&handle.to_ne_bytes());
        payload[12..16].copy_from_slice(&TC_H_ROOT.to_ne_bytes());
        payload.extend_from_slice(&nla(TCA_KIND, b"cake\0"));
        let mut options = Vec::new();
        options.extend_from_slice(&nla(TCA_CAKE_BASE_RATE64, &rate_bps.to_ne_bytes()));
        options.extend_from_slice(&nla(TCA_CAKE_FLOW_MODE, &4u32.to_ne_bytes()));
        options.extend_from_slice(&nla(TCA_CAKE_DIFFSERV_MODE, &4u32.to_ne_bytes()));
        options.extend_from_slice(&nla(TCA_CAKE_OVERHEAD, &18i32.to_ne_bytes()));
        options.extend_from_slice(&nla(TCA_CAKE_NAT, &1u32.to_ne_bytes()));
        payload.extend_from_slice(&nla(TCA_OPTIONS, &options));
        payload
    }

    fn ingress_qdisc_payload(ifindex: u32) -> Vec<u8> {
        let mut payload = vec![0u8; TCMSG_LEN];
        payload[4..8].copy_from_slice(&(ifindex as i32).to_ne_bytes());
        payload[8..12].copy_from_slice(&0xffff_0000u32.to_ne_bytes());
        payload[12..16].copy_from_slice(&TC_H_INGRESS.to_ne_bytes());
        payload.extend_from_slice(&nla(TCA_KIND, b"ingress\0"));
        payload
    }

    fn u32_filter_payload(
        ifindex: u32,
        parent: u32,
        handle: u32,
        priority: u16,
        action_index: u32,
        target_ifindex: u32,
        cookie: &[u8],
        refcnt: i32,
        bindcnt: i32,
    ) -> Vec<u8> {
        let mut payload = vec![0u8; TCMSG_LEN];
        payload[4..8].copy_from_slice(&(ifindex as i32).to_ne_bytes());
        payload[8..12].copy_from_slice(&handle.to_ne_bytes());
        payload[12..16].copy_from_slice(&parent.to_ne_bytes());
        let info = ((priority as u32) << 16) | u32::from(u16::to_be(3));
        payload[16..20].copy_from_slice(&info.to_ne_bytes());
        payload.extend_from_slice(&nla(TCA_KIND, b"u32\0"));

        let mut selector = vec![0u8; TC_U32_SEL_LEN + TC_U32_KEY_LEN];
        selector[0] = 1;
        selector[2] = 1;
        selector[16..20].copy_from_slice(&0u32.to_be_bytes());
        selector[20..24].copy_from_slice(&0u32.to_be_bytes());

        let mut parameters = vec![0u8; TC_MIRRED_LEN];
        parameters[0..4].copy_from_slice(&action_index.to_ne_bytes());
        parameters[8..12].copy_from_slice(&3i32.to_ne_bytes());
        parameters[12..16].copy_from_slice(&refcnt.to_ne_bytes());
        parameters[16..20].copy_from_slice(&bindcnt.to_ne_bytes());
        parameters[20..24].copy_from_slice(&TCA_EGRESS_REDIR.to_ne_bytes());
        parameters[24..28].copy_from_slice(&target_ifindex.to_ne_bytes());
        let mut mirred_options = Vec::new();
        mirred_options.extend_from_slice(&nla(TCA_MIRRED_PARMS, &parameters));
        mirred_options.extend_from_slice(&nla(TCA_MIRRED_TM, &[0x5a; 32]));

        let mut action = Vec::new();
        action.extend_from_slice(&nla(TCA_ACT_KIND, b"mirred\0"));
        action.extend_from_slice(&nla(TCA_ACT_COOKIE, cookie));
        action.extend_from_slice(&nla(TCA_ACT_OPTIONS, &mirred_options));
        action.extend_from_slice(&nla(TCA_ACT_STATS | NLA_F_NESTED, &[]));
        let mut actions = Vec::new();
        actions.extend_from_slice(&nla(1, &action));

        let mut options = Vec::new();
        options.extend_from_slice(&nla(TCA_U32_SEL, &selector));
        options.extend_from_slice(&nla(TCA_U32_ACT, &actions));
        options.extend_from_slice(&nla(TCA_U32_PCNT, &[0xa5; 24]));
        payload.extend_from_slice(&nla(TCA_OPTIONS, &options));
        payload
    }

    fn action_table_payload(kind: &str, index: u32, cookie: &[u8]) -> Vec<u8> {
        let mut action = Vec::new();
        let mut encoded_kind = kind.as_bytes().to_vec();
        encoded_kind.push(0);
        action.extend_from_slice(&nla(TCA_ACT_KIND, &encoded_kind));
        action.extend_from_slice(&nla(TCA_ACT_INDEX, &index.to_ne_bytes()));
        action.extend_from_slice(&nla(TCA_ACT_COOKIE, cookie));
        action.extend_from_slice(&nla(TCA_ACT_STATS | NLA_F_NESTED, &[]));
        let mut table = Vec::new();
        table.extend_from_slice(&nla(0, &action));
        let mut payload = vec![0u8; TCAMSG_LEN];
        payload.extend_from_slice(&nla(TCA_ROOT_COUNT, &1u32.to_ne_bytes()));
        payload.extend_from_slice(&nla(TCA_ACT_TAB, &table));
        payload
    }

    fn message(message_type: u16, flags: u16, sequence: u32, payload: &[u8]) -> Vec<u8> {
        let length = NLMSG_HEADER_LEN + payload.len();
        let aligned = (length + 3) & !3;
        let mut bytes = vec![0u8; aligned];
        bytes[..4].copy_from_slice(&(length as u32).to_ne_bytes());
        bytes[4..6].copy_from_slice(&message_type.to_ne_bytes());
        bytes[6..8].copy_from_slice(&flags.to_ne_bytes());
        bytes[8..12].copy_from_slice(&sequence.to_ne_bytes());
        bytes[12..16].copy_from_slice(&PORT_ID.to_ne_bytes());
        bytes[16..16 + payload.len()].copy_from_slice(payload);
        bytes
    }

    fn done(sequence: u32) -> Vec<u8> {
        message(NLMSG_DONE, NLM_F_MULTI, sequence, &0i32.to_ne_bytes())
    }

    fn data_and_done(message_type: u16, sequence: u32, payload: &[u8]) -> Vec<u8> {
        let mut datagram = message(message_type, NLM_F_MULTI, sequence, payload);
        datagram.extend_from_slice(&done(sequence));
        datagram
    }

    fn messages_and_done(message_type: u16, sequence: u32, payloads: &[Vec<u8>]) -> Vec<u8> {
        let mut datagram = Vec::new();
        for payload in payloads {
            datagram.extend_from_slice(&message(message_type, NLM_F_MULTI, sequence, payload));
        }
        datagram.extend_from_slice(&done(sequence));
        datagram
    }

    fn stable_replies_for_link(link: &[u8], qdisc_options: bool) -> Vec<Vec<u8>> {
        let mut replies = Vec::new();
        for pass in 0..2u32 {
            let base = pass * 4;
            replies.push(data_and_done(RTM_NEWLINK, base + 1, link));
            if qdisc_options {
                replies.push(data_and_done(RTM_NEWQDISC, base + 2, &qdisc_payload(true)));
            } else {
                replies.push(done(base + 2));
            }
            replies.push(done(base + 3));
            replies.push(done(base + 4));
        }
        replies
    }

    fn stable_replies(link_ifindex: u32, qdisc_options: bool) -> Vec<Vec<u8>> {
        stable_replies_for_link(&link_payload(link_ifindex, false), qdisc_options)
    }

    fn typed_query() -> KernelTopologyQuery {
        let mut value = query();
        value.private_namespace.link_names = vec!["ownedifb0".to_string()];
        value.private_namespace.link_aliases = vec!["cake-autotune-owned".to_string()];
        value.private_namespace.qdisc_handles = vec![0xa001_0000, 0xb001_0000, 0xffff_0000];
        value.private_namespace.filter_handles = vec![0x8000_0000];
        value.private_namespace.filter_priorities = vec![49_152];
        value.private_namespace.action_identities = vec![PrivateActionIdentity {
            kind: "mirred".to_string(),
            index: 70_001,
            cookie: Some(vec![0x55; 16]),
        }];
        value
    }

    fn typed_replies() -> Vec<Vec<u8>> {
        let mut replies = Vec::new();
        for pass in 0..2u32 {
            let base = pass * 5;
            replies.push(messages_and_done(
                RTM_NEWLINK,
                base + 1,
                &[
                    link_payload(10, false),
                    private_link_payload(20, "ownedifb0", "cake-autotune-owned"),
                ],
            ));
            replies.push(messages_and_done(
                RTM_NEWQDISC,
                base + 2,
                &[
                    cake_qdisc_payload(10, 0xa001_0000, 100_000_000),
                    cake_qdisc_payload(20, 0xb001_0000, 90_000_000),
                    ingress_qdisc_payload(10),
                ],
            ));
            replies.push(data_and_done(
                RTM_NEWTFILTER,
                base + 3,
                &u32_filter_payload(
                    10,
                    TC_H_MIN_INGRESS,
                    0x8000_0000,
                    49_152,
                    70_001,
                    20,
                    &[0x55; 16],
                    1 + pass as i32 * 100,
                    2 + pass as i32 * 100,
                ),
            ));
            replies.push(done(base + 4));
            replies.push(data_and_done(
                RTM_GETACTION,
                base + 5,
                &action_table_payload("mirred", 70_001, &[0x55; 16]),
            ));
        }
        replies
    }

    #[test]
    fn request_encoder_emits_only_canonical_get_dumps() {
        let links = encode_dump_request(DumpKind::Links, 1, PORT_ID).unwrap();
        assert_eq!(get_u16(&links, 4).unwrap(), RTM_GETLINK);
        assert_eq!(get_u16(&links, 6).unwrap(), NLM_F_REQUEST | NLM_F_DUMP);
        assert_eq!(links.len(), NLMSG_HEADER_LEN + IFINFO_LEN);

        let qdiscs = encode_dump_request(DumpKind::Qdiscs, 2, PORT_ID).unwrap();
        assert_eq!(get_u16(&qdiscs, 4).unwrap(), RTM_GETQDISC);
        assert_eq!(
            get_u16(&qdiscs, NLMSG_HEADER_LEN + TCMSG_LEN + 2).unwrap(),
            TCA_DUMP_INVISIBLE
        );

        let ingress =
            encode_dump_request(DumpKind::IngressFilters { ifindex: 10 }, 3, PORT_ID).unwrap();
        assert_eq!(get_u16(&ingress, 4).unwrap(), RTM_GETTFILTER);
        assert_eq!(
            get_u32(&ingress, NLMSG_HEADER_LEN + 12).unwrap(),
            TC_H_MIN_INGRESS
        );
        let egress =
            encode_dump_request(DumpKind::EgressFilters { ifindex: 10 }, 4, PORT_ID).unwrap();
        assert_eq!(
            get_u32(&egress, NLMSG_HEADER_LEN + 12).unwrap(),
            TC_H_MIN_EGRESS
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_adapter_validator_rejects_noncanonical_fixed_fields_and_ifindexes() {
        let links = encode_dump_request(DumpKind::Links, 1, PORT_ID).unwrap();
        validate_canonical_get_request(&links, PORT_ID).unwrap();
        let mut altered_links = links;
        altered_links[NLMSG_HEADER_LEN + 2] = 1;
        assert!(validate_canonical_get_request(&altered_links, PORT_ID).is_err());

        let qdiscs = encode_dump_request(DumpKind::Qdiscs, 2, PORT_ID).unwrap();
        validate_canonical_get_request(&qdiscs, PORT_ID).unwrap();
        let mut altered_qdiscs = qdiscs.clone();
        altered_qdiscs[NLMSG_HEADER_LEN] = 1;
        assert!(validate_canonical_get_request(&altered_qdiscs, PORT_ID).is_err());
        let mut flagged_attribute = qdiscs;
        put_u16(
            &mut flagged_attribute,
            NLMSG_HEADER_LEN + TCMSG_LEN + 2,
            TCA_DUMP_INVISIBLE | NLA_F_NESTED,
        )
        .unwrap();
        assert!(validate_canonical_get_request(&flagged_attribute, PORT_ID).is_err());

        let filters =
            encode_dump_request(DumpKind::IngressFilters { ifindex: 10 }, 3, PORT_ID).unwrap();
        validate_canonical_get_request(&filters, PORT_ID).unwrap();
        let mut altered_parent = filters.clone();
        put_u32(&mut altered_parent, NLMSG_HEADER_LEN + 12, TC_H_INGRESS).unwrap();
        assert!(validate_canonical_get_request(&altered_parent, PORT_ID).is_err());
        let mut altered_handle = filters.clone();
        put_u32(&mut altered_handle, NLMSG_HEADER_LEN + 8, 1).unwrap();
        assert!(validate_canonical_get_request(&altered_handle, PORT_ID).is_err());
        let mut oversized_ifindex = filters;
        put_u32(
            &mut oversized_ifindex,
            NLMSG_HEADER_LEN + 4,
            i32::MAX as u32 + 1,
        )
        .unwrap();
        assert!(validate_canonical_get_request(&oversized_ifindex, PORT_ID).is_err());
        assert!(validate_canonical_get_request(&oversized_ifindex, 0).is_err());
    }

    #[test]
    fn action_request_is_kind_scoped_terse_and_canonical() {
        let request = encode_action_dump_request("mirred", 5, PORT_ID).unwrap();
        assert_eq!(get_u16(&request, 4).unwrap(), RTM_GETACTION);
        assert_eq!(get_u16(&request, 6).unwrap(), NLM_F_REQUEST | NLM_F_DUMP);
        #[cfg(target_os = "linux")]
        {
            validate_canonical_get_request(&request, PORT_ID).unwrap();
            assert_eq!(
                decode_canonical_action_request_kind(&request).unwrap(),
                "mirred"
            );
            let mut tampered = request.clone();
            *tampered.last_mut().unwrap() = 1;
            assert!(validate_canonical_get_request(&tampered, PORT_ID).is_err());
        }
        assert!(encode_action_dump_request("bad kind", 5, PORT_ID).is_err());
    }

    #[test]
    fn link_reader_uses_one_reserved_name_and_alias_identity_namespace() {
        let mut identity_query = query();
        identity_query.private_namespace.link_names = vec!["ownedifb0".to_string()];
        identity_query.private_namespace.link_aliases = vec!["owned-alias".to_string()];
        let limits = NetlinkReadLimits::default();
        let mut budget = ReadBudget::default();
        let mut pass = ParsedPass::new(&identity_query, NETNS_COOKIE);
        pass.parse_link(
            &custom_link_payload(10, "eth0", None, false),
            &mut budget,
            &limits,
        )
        .unwrap();
        pass.parse_link(
            &custom_link_payload(20, "owned-alias", None, false),
            &mut budget,
            &limits,
        )
        .unwrap();
        pass.parse_link(
            &custom_link_payload(21, "foreign0", Some("ownedifb0"), false),
            &mut budget,
            &limits,
        )
        .unwrap();
        assert_eq!(
            pass.private_links.keys().copied().collect::<Vec<_>>(),
            vec![20, 21]
        );
    }

    #[test]
    fn duplicate_link_info_and_out_of_range_parent_ifindex_fail_closed() {
        let limits = NetlinkReadLimits::default();
        let mut duplicate = link_payload(10, true);
        let nested = nla(IFLA_INFO_KIND, b"ether\0");
        duplicate.extend_from_slice(&nla(IFLA_LINKINFO | NLA_F_NESTED, &nested));
        let mut pass = ParsedPass::new(&query(), NETNS_COOKIE);
        let mut budget = ReadBudget::default();
        assert!(matches!(
            pass.parse_link(&duplicate, &mut budget, &limits),
            Err(KernelTopologyError::Invalid(message)) if message.contains("duplicate IFLA_LINKINFO")
        ));

        let mut oversized_parent = link_payload(10, false);
        oversized_parent.extend_from_slice(&nla(IFLA_LINK, &(i32::MAX as u32 + 1).to_ne_bytes()));
        let mut pass = ParsedPass::new(&query(), NETNS_COOKIE);
        let mut budget = ReadBudget::default();
        assert!(matches!(
            pass.parse_link(&oversized_parent, &mut budget, &limits),
            Err(KernelTopologyError::Invalid(message)) if message.contains("IFLA_LINK")
        ));
    }

    #[test]
    fn two_stable_passes_capture_physical_link_without_kind() {
        let fake = FakeNetlinkIo::new(stable_replies(10, false));
        let mut reader = NetlinkTopologyReader::new(fake);
        let read = reader.read_topology(&query()).unwrap();
        assert_eq!(read.snapshot.target.ifindex, 10);
        assert_eq!(read.snapshot.target.kind, KernelLinkKind::Absent);
        assert!(read.unknown_ownership_attributes.is_empty());
        assert!(read.observed_bytes > 0);
        assert_eq!(reader.io.sent.len(), 8);
        assert!(reader.io.sent.iter().all(|request| {
            matches!(
                get_u16(request, 4).unwrap(),
                RTM_GETLINK | RTM_GETQDISC | RTM_GETTFILTER
            )
        }));
    }

    #[test]
    fn ordinary_openwrt_link_metadata_is_excluded_but_ownership_and_future_state_fail_closed() {
        let ordinary = ordinary_openwrt_link_payload(10);
        let fake = FakeNetlinkIo::new(stable_replies_for_link(&ordinary, false));
        let mut reader = NetlinkTopologyReader::new(fake);
        let read = reader.read_topology(&query()).unwrap();
        assert!(read.unknown_ownership_attributes.is_empty());

        for attribute_type in [IFLA_MASTER, 66] {
            let mut occupied = link_payload(10, false);
            occupied.extend_from_slice(&nla(attribute_type, &1u32.to_ne_bytes()));
            let fake = FakeNetlinkIo::new(stable_replies_for_link(&occupied, false));
            let mut reader = NetlinkTopologyReader::new(fake);
            let read = reader.read_topology(&query()).unwrap();
            assert_eq!(read.unknown_ownership_attributes.len(), 1);
            assert_eq!(
                read.unknown_ownership_attributes[0].attribute_type,
                attribute_type
            );
            assert_eq!(
                read.unknown_ownership_attributes[0].nested_path,
                vec![attribute_type]
            );
        }
    }

    #[test]
    fn only_empty_ppp_info_data_is_observation_only() {
        let empty_ppp = link_with_info_data(10, "ppp", &[]);
        let fake = FakeNetlinkIo::new(stable_replies_for_link(&empty_ppp, false));
        let mut reader = NetlinkTopologyReader::new(fake);
        let read = reader.read_topology(&query()).unwrap();
        assert!(read.unknown_ownership_attributes.is_empty());
        assert_eq!(
            read.snapshot.target.kind,
            KernelLinkKind::Named("ppp".to_string())
        );

        for payload in [
            link_with_info_data(10, "ppp", &[1, 0, 0, 0]),
            link_with_info_data(10, "vlan", &[]),
        ] {
            let fake = FakeNetlinkIo::new(stable_replies_for_link(&payload, false));
            let mut reader = NetlinkTopologyReader::new(fake);
            let read = reader.read_topology(&query()).unwrap();
            assert_eq!(read.unknown_ownership_attributes.len(), 1);
            assert_eq!(
                read.unknown_ownership_attributes[0].nested_path,
                vec![IFLA_LINKINFO, IFLA_INFO_DATA]
            );
        }
    }

    #[test]
    fn only_exact_unattached_xdp_state_is_accepted() {
        for outer_flags in [0, NLA_F_NESTED] {
            let mut unattached = link_payload(10, false);
            let nested = nla(IFLA_XDP_ATTACHED, &[XDP_ATTACHED_NONE]);
            unattached.extend_from_slice(&nla(IFLA_XDP | outer_flags, &nested));
            let fake = FakeNetlinkIo::new(stable_replies_for_link(&unattached, false));
            let mut reader = NetlinkTopologyReader::new(fake);
            let read = reader.read_topology(&query()).unwrap();
            assert!(read.unknown_ownership_attributes.is_empty());
        }

        for mode in 1..=4 {
            let mut attached = link_payload(10, false);
            let nested = nla(IFLA_XDP_ATTACHED, &[mode]);
            attached.extend_from_slice(&nla(IFLA_XDP | NLA_F_NESTED, &nested));
            let fake = FakeNetlinkIo::new(stable_replies_for_link(&attached, false));
            let mut reader = NetlinkTopologyReader::new(fake);
            let read = reader.read_topology(&query()).unwrap();
            assert_eq!(read.unknown_ownership_attributes.len(), 1);
            assert_eq!(
                read.unknown_ownership_attributes[0].nested_path,
                vec![IFLA_XDP, IFLA_XDP_ATTACHED]
            );
        }

        for extra_type in [1, 3, 4, 5, 6, 7, 8, 9, 250] {
            let mut with_extra = link_payload(10, false);
            let mut nested = nla(IFLA_XDP_ATTACHED, &[XDP_ATTACHED_NONE]);
            nested.extend_from_slice(&nla(extra_type, &77u32.to_ne_bytes()));
            with_extra.extend_from_slice(&nla(IFLA_XDP | NLA_F_NESTED, &nested));
            let fake = FakeNetlinkIo::new(stable_replies_for_link(&with_extra, false));
            let mut reader = NetlinkTopologyReader::new(fake);
            let read = reader.read_topology(&query()).unwrap();
            assert_eq!(read.unknown_ownership_attributes.len(), 1);
            assert_eq!(
                read.unknown_ownership_attributes[0].nested_path,
                vec![IFLA_XDP, extra_type]
            );
        }

        let mut empty = link_payload(10, false);
        empty.extend_from_slice(&nla(IFLA_XDP | NLA_F_NESTED, &[]));
        let fake = FakeNetlinkIo::new(stable_replies_for_link(&empty, false));
        let mut reader = NetlinkTopologyReader::new(fake);
        let read = reader.read_topology(&query()).unwrap();
        assert_eq!(read.unknown_ownership_attributes.len(), 1);
        assert_eq!(
            read.unknown_ownership_attributes[0].nested_path,
            vec![IFLA_XDP]
        );

        for malformed in [
            {
                let mut payload = link_payload(10, false);
                let nested = nla(IFLA_XDP_ATTACHED, &[0, 0]);
                payload.extend_from_slice(&nla(IFLA_XDP | NLA_F_NESTED, &nested));
                payload
            },
            {
                let mut payload = link_payload(10, false);
                let mut nested = nla(IFLA_XDP_ATTACHED, &[XDP_ATTACHED_NONE]);
                nested.extend_from_slice(&nla(IFLA_XDP_ATTACHED, &[XDP_ATTACHED_NONE]));
                payload.extend_from_slice(&nla(IFLA_XDP | NLA_F_NESTED, &nested));
                payload
            },
            {
                let mut payload = link_payload(10, false);
                let nested = nla(IFLA_XDP_ATTACHED, &[XDP_ATTACHED_NONE]);
                payload.extend_from_slice(&nla(IFLA_XDP | NLA_F_NET_BYTEORDER, &nested));
                payload
            },
            {
                let mut payload = link_payload(10, false);
                let nested = nla(IFLA_XDP_ATTACHED | NLA_F_NESTED, &[XDP_ATTACHED_NONE]);
                payload.extend_from_slice(&nla(IFLA_XDP | NLA_F_NESTED, &nested));
                payload
            },
            {
                let mut payload = link_payload(10, false);
                let nested = nla(IFLA_XDP_ATTACHED, &[XDP_ATTACHED_NONE]);
                payload.extend_from_slice(&nla(IFLA_XDP | NLA_F_NESTED, &nested));
                payload.extend_from_slice(&nla(IFLA_XDP | NLA_F_NESTED, &nested));
                payload
            },
        ] {
            let fake = FakeNetlinkIo::new(stable_replies_for_link(&malformed, false));
            let mut reader = NetlinkTopologyReader::new(fake);
            assert!(reader.read_topology(&query()).is_err());
        }
    }

    #[test]
    fn default_openwrt_fq_codel_configuration_is_typed_and_fingerprinted() {
        let link = ordinary_openwrt_link_payload(10);
        let qdisc = fq_codel_qdisc_payload(10);
        let mut replies = Vec::new();
        for pass in 0..2u32 {
            let base = pass * 4;
            replies.push(data_and_done(RTM_NEWLINK, base + 1, &link));
            replies.push(data_and_done(RTM_NEWQDISC, base + 2, &qdisc));
            replies.push(done(base + 3));
            replies.push(done(base + 4));
        }
        let fake = FakeNetlinkIo::new(replies);
        let mut reader = NetlinkTopologyReader::new(fake);
        let read = reader.read_topology(&query()).unwrap();
        assert!(read.unknown_ownership_attributes.is_empty());
        assert_eq!(read.snapshot.root_qdiscs.len(), 1);
        assert_eq!(read.snapshot.root_qdiscs[0].kind, "fq_codel");
        assert_eq!(read.snapshot.root_qdiscs[0].options.attributes.len(), 8);

        let mut unsupported = fq_codel_qdisc_payload(10);
        let options = nla(12, &1u32.to_ne_bytes());
        unsupported.extend_from_slice(&nla(TCA_OPTIONS, &options));
        let mut pass = ParsedPass::new(&query(), NETNS_COOKIE);
        let limits = NetlinkReadLimits::default();
        let mut budget = ReadBudget::default();
        pass.parse_link(&link, &mut budget, &limits).unwrap();
        assert!(matches!(
            pass.parse_qdisc(&unsupported, &mut budget, &limits),
            Err(KernelTopologyError::Invalid(message)) if message.contains("duplicate TCA_OPTIONS")
        ));
    }

    #[test]
    fn physical_multi_queue_leaves_are_outside_ownership_but_reserved_handles_fail_closed() {
        let link = ordinary_openwrt_link_payload(10);
        let limits = NetlinkReadLimits::default();
        let query = typed_query();
        let mut pass = ParsedPass::new(&query, NETNS_COOKIE);
        let mut budget = ReadBudget::default();
        pass.parse_link(&link, &mut budget, &limits).unwrap();
        pass.parse_qdisc(&mq_qdisc_payload(10), &mut budget, &limits)
            .unwrap();

        for parent in 1..=4 {
            pass.parse_qdisc(
                &fq_codel_leaf_qdisc_payload(10, parent, 0),
                &mut budget,
                &limits,
            )
            .unwrap();
        }
        assert_eq!(pass.root_qdiscs.len(), 1);
        assert_eq!(pass.root_qdiscs[0].kind, "mq");
        assert!(pass.ingress_qdiscs.is_empty());
        assert!(pass.unknown.is_empty());

        let reserved_handle = query.private_namespace.qdisc_handles[0];
        let error = pass
            .parse_qdisc(
                &fq_codel_leaf_qdisc_payload(10, 1, reserved_handle),
                &mut budget,
                &limits,
            )
            .unwrap_err();
        assert!(matches!(
            error,
            KernelTopologyError::Invalid(message) if message.contains("reserved handle")
        ));

        let mut malformed_unowned = fq_codel_leaf_qdisc_payload(10, 2, 0);
        malformed_unowned.extend_from_slice(&nla(TCA_OPTIONS, &[1, 2, 3, 4]));
        pass.parse_qdisc(&malformed_unowned, &mut budget, &limits)
            .unwrap();
    }

    #[test]
    fn tc_hardware_offload_must_be_an_exact_disabled_u8() {
        let link = ordinary_openwrt_link_payload(10);
        let limits = NetlinkReadLimits::default();

        let mut offloaded = ParsedPass::new(&query(), NETNS_COOKIE);
        let mut budget = ReadBudget::default();
        offloaded.parse_link(&link, &mut budget, &limits).unwrap();
        offloaded
            .parse_qdisc(
                &fq_codel_qdisc_payload_with_offload(10, &[1]),
                &mut budget,
                &limits,
            )
            .unwrap();
        assert_eq!(offloaded.unknown.len(), 1);
        assert_eq!(offloaded.unknown[0].nested_path, vec![TCA_HW_OFFLOAD]);

        for malformed_value in [&[][..], &[0, 0][..], &[0, 0, 0, 0][..]] {
            let mut malformed = ParsedPass::new(&query(), NETNS_COOKIE);
            let mut budget = ReadBudget::default();
            malformed.parse_link(&link, &mut budget, &limits).unwrap();
            assert!(malformed
                .parse_qdisc(
                    &fq_codel_qdisc_payload_with_offload(10, malformed_value),
                    &mut budget,
                    &limits,
                )
                .is_err());
        }

        let mut duplicate_payload = fq_codel_qdisc_payload(10);
        duplicate_payload.extend_from_slice(&nla(TCA_HW_OFFLOAD, &[0]));
        let mut duplicate = ParsedPass::new(&query(), NETNS_COOKIE);
        let mut budget = ReadBudget::default();
        duplicate.parse_link(&link, &mut budget, &limits).unwrap();
        assert!(duplicate
            .parse_qdisc(&duplicate_payload, &mut budget, &limits)
            .is_err());
    }

    #[test]
    fn property_list_alternate_names_are_bounded_identity_state() {
        let mut target = link_payload(10, false);
        let properties = nla(IFLA_ALT_IFNAME, b"wan-alt\0");
        target.extend_from_slice(&nla(IFLA_PROP_LIST | NLA_F_NESTED, &properties));
        let fake = FakeNetlinkIo::new(stable_replies_for_link(&target, false));
        let mut reader = NetlinkTopologyReader::new(fake);
        let read = reader.read_topology(&query()).unwrap();
        assert!(read.unknown_ownership_attributes.is_empty());

        let mut collision_query = query();
        collision_query.private_namespace.link_names = vec!["wan-alt".to_string()];
        let fake = FakeNetlinkIo::new(stable_replies_for_link(&target, false));
        let mut reader = NetlinkTopologyReader::new(fake);
        let read = reader.read_topology(&collision_query).unwrap();
        assert_eq!(read.unknown_ownership_attributes.len(), 1);
        assert_eq!(
            read.unknown_ownership_attributes[0].nested_path,
            vec![IFLA_ALT_IFNAME]
        );

        let mut unknown_property = link_payload(10, false);
        unknown_property.extend_from_slice(&nla(IFLA_PROP_LIST | NLA_F_NESTED, &nla(99, &[1])));
        let fake = FakeNetlinkIo::new(stable_replies_for_link(&unknown_property, false));
        let mut reader = NetlinkTopologyReader::new(fake);
        let read = reader.read_topology(&query()).unwrap();
        assert_eq!(
            read.unknown_ownership_attributes[0].nested_path,
            vec![IFLA_PROP_LIST, 99]
        );
    }

    #[test]
    fn cake_u32_mirred_and_action_table_are_typed_across_two_passes() {
        let fake = FakeNetlinkIo::new(typed_replies());
        let mut reader = NetlinkTopologyReader::new(fake);
        let read = reader.read_topology(&typed_query()).unwrap();
        assert!(read.unknown_ownership_attributes.is_empty());
        assert_eq!(read.snapshot.root_qdiscs.len(), 2);
        assert!(read
            .snapshot
            .root_qdiscs
            .iter()
            .all(|qdisc| qdisc.kind == "cake" && !qdisc.options.attributes.is_empty()));
        assert_eq!(read.snapshot.ingress_filters.len(), 1);
        let filter = &read.snapshot.ingress_filters[0];
        assert_eq!(filter.kind, "u32");
        assert!(!filter.options.attributes.is_empty());
        assert_eq!(filter.actions.len(), 1);
        assert!(matches!(
            filter.actions[0].target,
            ActionTarget::Mirred {
                direction: MirredDirection::EgressRedirect,
                target_ifindex: 20,
            }
        ));
        assert_eq!(read.snapshot.private_actions.len(), 1);
        assert_eq!(read.snapshot.private_actions[0].kind, "mirred");
        assert_eq!(read.snapshot.private_actions[0].index, 70_001);
        assert_eq!(reader.io.sent.len(), 10);
        assert_eq!(
            reader
                .io
                .sent
                .iter()
                .filter(|request| get_u16(request, 4).unwrap() == RTM_GETACTION)
                .count(),
            2
        );
    }

    #[test]
    fn two_pass_drift_fails_closed() {
        let mut replies = stable_replies(10, false);
        replies[4] = data_and_done(RTM_NEWLINK, 5, &link_payload(11, false));
        let mut reader = NetlinkTopologyReader::new(FakeNetlinkIo::new(replies));
        assert!(matches!(
            reader.read_topology(&query()),
            Err(KernelTopologyError::Backend(message)) if message.contains("changed between")
        ));
    }

    #[test]
    fn malformed_cake_options_fail_closed() {
        let fake = FakeNetlinkIo::new(stable_replies(10, true));
        let mut reader = NetlinkTopologyReader::new(fake);
        assert!(matches!(
            capture_read_only_witness(&mut reader, &query()),
            Err(KernelTopologyError::Backend(_) | KernelTopologyError::Invalid(_))
        ));
    }

    #[test]
    fn strict_sender_sequence_truncation_and_dump_interrupt_are_rejected() {
        let mut wrong_sender = FakeNetlinkIo::new(stable_replies(10, false));
        wrong_sender.replies.front_mut().unwrap().1 = 99;
        let mut reader = NetlinkTopologyReader::new(wrong_sender);
        assert!(reader.read_topology(&query()).is_err());

        let mut truncated = FakeNetlinkIo::new(stable_replies(10, false));
        truncated.replies.front_mut().unwrap().2 = libc::MSG_TRUNC;
        let mut reader = NetlinkTopologyReader::new(truncated);
        assert!(matches!(
            reader.read_topology(&query()),
            Err(KernelTopologyError::Limit(message)) if message.contains("truncated")
        ));

        let mut interrupted = stable_replies(10, false);
        interrupted[0][6..8].copy_from_slice(&(NLM_F_MULTI | NLM_F_DUMP_INTR).to_ne_bytes());
        let mut reader = NetlinkTopologyReader::new(FakeNetlinkIo::new(interrupted));
        assert!(reader.read_topology(&query()).is_err());

        let mut wrong_sequence = stable_replies(10, false);
        wrong_sequence[0][8..12].copy_from_slice(&99u32.to_ne_bytes());
        let mut reader = NetlinkTopologyReader::new(FakeNetlinkIo::new(wrong_sequence));
        assert!(reader.read_topology(&query()).is_err());

        let mut error_replies = stable_replies(10, false);
        error_replies[0] = message(NLMSG_ERROR, 0, 1, &(-22i32).to_ne_bytes());
        let mut reader = NetlinkTopologyReader::new(FakeNetlinkIo::new(error_replies));
        assert!(matches!(
            reader.read_topology(&query()),
            Err(KernelTopologyError::Backend(message)) if message.contains("NLMSG_ERROR")
        ));

        let mut errored_done = stable_replies(10, false);
        errored_done[0] = message(NLMSG_DONE, NLM_F_MULTI, 1, &(-4i32).to_ne_bytes());
        let mut reader = NetlinkTopologyReader::new(FakeNetlinkIo::new(errored_done));
        assert!(reader.read_topology(&query()).is_err());
    }

    #[test]
    fn malformed_attributes_and_aggregate_byte_bound_fail_before_growth() {
        let mut malformed_payload = vec![0u8; IFINFO_LEN];
        malformed_payload[4..8].copy_from_slice(&10i32.to_ne_bytes());
        malformed_payload.extend_from_slice(&[3, 0, IFLA_IFNAME as u8, 0]);
        let mut replies = stable_replies(10, false);
        replies[0] = data_and_done(RTM_NEWLINK, 1, &malformed_payload);
        let mut reader = NetlinkTopologyReader::new(FakeNetlinkIo::new(replies));
        assert!(reader.read_topology(&query()).is_err());

        let replies = stable_replies(10, false);
        let first_len = replies[0].len();
        let limits = NetlinkReadLimits {
            max_observed_bytes: first_len.saturating_sub(1),
            ..NetlinkReadLimits::default()
        };
        let mut reader =
            NetlinkTopologyReader::with_limits(FakeNetlinkIo::new(replies), limits).unwrap();
        assert!(matches!(
            reader.read_topology(&query()),
            Err(KernelTopologyError::Limit(message)) if message.contains("observed-byte")
        ));

        let limits = NetlinkReadLimits {
            max_messages: 1,
            ..NetlinkReadLimits::default()
        };
        let mut reader = NetlinkTopologyReader::with_limits(
            FakeNetlinkIo::new(stable_replies(10, false)),
            limits,
        )
        .unwrap();
        assert!(matches!(
            reader.read_topology(&query()),
            Err(KernelTopologyError::Limit(message)) if message.contains("messages")
        ));

        let limits = NetlinkReadLimits {
            max_attributes: 1,
            ..NetlinkReadLimits::default()
        };
        let mut reader = NetlinkTopologyReader::with_limits(
            FakeNetlinkIo::new(stable_replies(10, false)),
            limits,
        )
        .unwrap();
        assert!(matches!(
            reader.read_topology(&query()),
            Err(KernelTopologyError::Limit(message)) if message.contains("attributes")
        ));
    }
}
