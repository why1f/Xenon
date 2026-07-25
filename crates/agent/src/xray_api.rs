use prost::Message;
use std::collections::HashMap;
use tonic::transport::{Channel, Endpoint};
use xray_core_protocol::{
    xray::{
        app::{
            proxyman::command::{
                handler_service_client::HandlerServiceClient, AddUserOperation,
                AlterInboundRequest, RemoveUserOperation,
            },
            stats::command::{stats_service_client::StatsServiceClient, QueryStatsRequest, Stat},
        },
        common::{protocol::User, serial::TypedMessage},
        proxy::vless::Account,
    },
    ADD_USER_OPERATION_TYPE, REMOVE_USER_OPERATION_TYPE, VLESS_ACCOUNT_TYPE,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserTraffic {
    pub email: String,
    pub uplink: u64,
    pub downlink: u64,
}

pub struct XrayApi {
    inbound_tag: String,
    handler: HandlerServiceClient<Channel>,
    stats: StatsServiceClient<Channel>,
}

impl XrayApi {
    pub fn new(endpoint: &str, inbound_tag: String) -> anyhow::Result<Self> {
        let channel = Endpoint::from_shared(endpoint.to_string())?
            .connect_timeout(std::time::Duration::from_secs(3))
            .timeout(std::time::Duration::from_secs(5))
            .connect_lazy();
        Ok(Self {
            inbound_tag,
            handler: HandlerServiceClient::new(channel.clone()),
            stats: StatsServiceClient::new(channel),
        })
    }

    pub async fn add_vless_user(
        &mut self,
        email: &str,
        uuid: &str,
        flow: &str,
    ) -> anyhow::Result<()> {
        let account = typed_message(
            VLESS_ACCOUNT_TYPE,
            Account {
                id: uuid.into(),
                flow: flow.into(),
                encryption: "none".into(),
                xor_mode: 0,
                seconds: 0,
                padding: String::new(),
            },
        );
        let operation = typed_message(
            ADD_USER_OPERATION_TYPE,
            AddUserOperation {
                user: Some(User {
                    level: 0,
                    email: email.into(),
                    account: Some(account),
                }),
            },
        );
        self.handler
            .alter_inbound(AlterInboundRequest {
                tag: self.inbound_tag.clone(),
                operation: Some(operation),
            })
            .await?;
        Ok(())
    }

    pub async fn remove_user(&mut self, email: &str) -> anyhow::Result<()> {
        let operation = typed_message(
            REMOVE_USER_OPERATION_TYPE,
            RemoveUserOperation {
                email: email.into(),
            },
        );
        self.handler
            .alter_inbound(AlterInboundRequest {
                tag: self.inbound_tag.clone(),
                operation: Some(operation),
            })
            .await?;
        Ok(())
    }

    pub async fn query_user_traffic(&mut self) -> anyhow::Result<Vec<UserTraffic>> {
        let response = self
            .stats
            .query_stats(QueryStatsRequest {
                pattern: "user>>>".into(),
                reset: false,
            })
            .await?
            .into_inner();
        Ok(parse_user_stats(response.stat))
    }
}

fn typed_message<M: Message>(message_type: &str, message: M) -> TypedMessage {
    TypedMessage {
        r#type: message_type.into(),
        value: message.encode_to_vec(),
    }
}

fn parse_user_stats(stats: Vec<Stat>) -> Vec<UserTraffic> {
    const PREFIX: &str = "user>>>";
    const UPLINK_SUFFIX: &str = ">>>traffic>>>uplink";
    const DOWNLINK_SUFFIX: &str = ">>>traffic>>>downlink";
    let mut users = HashMap::<String, (u64, u64)>::new();
    for stat in stats {
        let Some(name) = stat.name.strip_prefix(PREFIX) else {
            continue;
        };
        let value = stat.value.max(0) as u64;
        if let Some(email) = name.strip_suffix(UPLINK_SUFFIX) {
            users.entry(email.into()).or_default().0 = value;
        } else if let Some(email) = name.strip_suffix(DOWNLINK_SUFFIX) {
            users.entry(email.into()).or_default().1 = value;
        }
    }
    let mut users = users
        .into_iter()
        .map(|(email, (uplink, downlink))| UserTraffic {
            email,
            uplink,
            downlink,
        })
        .collect::<Vec<_>>();
    users.sort_by(|left, right| left.email.cmp(&right.email));
    users
}

#[derive(Debug, Default)]
pub struct TrafficTracker {
    previous: HashMap<String, (u64, u64)>,
}

impl TrafficTracker {
    pub fn observe(&mut self, current: Vec<UserTraffic>) -> Vec<UserTraffic> {
        current
            .into_iter()
            .filter_map(|traffic| {
                let previous = self
                    .previous
                    .insert(traffic.email.clone(), (traffic.uplink, traffic.downlink));
                let (previous_up, previous_down) = previous.unwrap_or_default();
                let uplink = traffic.uplink.saturating_sub(previous_up);
                let downlink = traffic.downlink.saturating_sub(previous_down);
                (uplink > 0 || downlink > 0).then_some(UserTraffic {
                    email: traffic.email,
                    uplink,
                    downlink,
                })
            })
            .collect()
    }

    pub fn reset(&mut self) {
        self.previous.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_user_stats, TrafficTracker, UserTraffic};
    use xray_core_protocol::xray::app::stats::command::Stat;

    #[test]
    fn parses_only_user_traffic_counters() {
        let traffic = parse_user_stats(vec![
            Stat {
                name: "user>>>sub-a@panel>>>traffic>>>downlink".into(),
                value: 20,
            },
            Stat {
                name: "user>>>sub-a@panel>>>traffic>>>uplink".into(),
                value: 10,
            },
            Stat {
                name: "inbound>>>api>>>traffic>>>uplink".into(),
                value: 999,
            },
        ]);
        assert_eq!(
            traffic,
            vec![UserTraffic {
                email: "sub-a@panel".into(),
                uplink: 10,
                downlink: 20,
            }]
        );
    }

    #[test]
    fn tracker_returns_deltas_and_rebases_after_reset() {
        let mut tracker = TrafficTracker::default();
        let first = tracker.observe(vec![UserTraffic {
            email: "a".into(),
            uplink: 10,
            downlink: 20,
        }]);
        assert_eq!(first[0].uplink, 10);
        let second = tracker.observe(vec![UserTraffic {
            email: "a".into(),
            uplink: 13,
            downlink: 25,
        }]);
        assert_eq!((second[0].uplink, second[0].downlink), (3, 5));
        let reset = tracker.observe(vec![UserTraffic {
            email: "a".into(),
            uplink: 1,
            downlink: 2,
        }]);
        assert!(reset.is_empty());
    }
}
