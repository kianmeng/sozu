use std::{collections::BTreeMap, fmt, net::SocketAddr};

use crate::{
    proxy::{
        AggregatedMetricsData, HttpFrontend, ProxyEvent, ProxyRequestOrder, QueryAnswer,
        TcpFrontend,
    },
    state::ConfigState,
};

pub const PROTOCOL_VERSION: u8 = 0;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CommandRequestOrder {
    Proxy(Box<ProxyRequestOrder>),
    SaveState { path: String },
    LoadState { path: String },
    DumpState,
    ListWorkers,
    ListFrontends(FrontendFilters),
    LaunchWorker(String),
    UpgradeMain,
    UpgradeWorker(u32),
    SubscribeEvents,
    ReloadConfiguration { path: Option<String> },
    Status,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FrontendFilters {
    pub http: bool,
    pub https: bool,
    pub tcp: bool,
    pub domain: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CommandRequest {
    pub id: String,
    pub version: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worker_id: Option<u32>,
    #[serde(flatten)]
    pub order: CommandRequestOrder,
}

impl CommandRequest {
    pub fn new(id: String, order: CommandRequestOrder, worker_id: Option<u32>) -> CommandRequest {
        CommandRequest {
            version: PROTOCOL_VERSION,
            id,
            order,
            worker_id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CommandStatus {
    Ok,
    Processing,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CommandResponseContent {
    Workers(Vec<WorkerInfo>),
    /// used by the main process to respond to the CLI
    Metrics(AggregatedMetricsData),
    /// worker_id -> query_answer
    Query(BTreeMap<String, QueryAnswer>),
    State(Box<ConfigState>),
    Event(Event),
    FrontendList(ListedFrontends),
    // this is new
    Status(Vec<WorkerInfo>),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ListedFrontends {
    pub http_frontends: Vec<HttpFrontend>,
    pub https_frontends: Vec<HttpFrontend>,
    pub tcp_frontends: Vec<TcpFrontend>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandResponse {
    pub id: String,
    pub version: u8,
    pub status: CommandStatus,
    pub message: String,
    pub content: Option<CommandResponseContent>,
}

impl CommandResponse {
    pub fn new(
        id: String,
        status: CommandStatus,
        message: String,
        content: Option<CommandResponseContent>,
    ) -> CommandResponse {
        CommandResponse {
            version: PROTOCOL_VERSION,
            id,
            status,
            message,
            content,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RunState {
    Running,
    Stopping,
    Stopped,
    NotAnswering,
}

impl fmt::Display for RunState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WorkerInfo {
    pub id: u32,
    pub pid: i32,
    pub run_state: RunState,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Event {
    BackendDown(String, SocketAddr),
    BackendUp(String, SocketAddr),
    NoAvailableBackends(String),
    /// indicates a backend that was removed from configuration has no lingering connections
    /// so it can be safely stopped
    RemovedBackendHasNoConnections(String, SocketAddr),
}

impl From<ProxyEvent> for Event {
    fn from(e: ProxyEvent) -> Self {
        match e {
            ProxyEvent::BackendDown(id, addr) => Event::BackendDown(id, addr),
            ProxyEvent::BackendUp(id, addr) => Event::BackendUp(id, addr),
            ProxyEvent::NoAvailableBackends(cluster_id) => Event::NoAvailableBackends(cluster_id),
            ProxyEvent::RemovedBackendHasNoConnections(id, addr) => {
                Event::RemovedBackendHasNoConnections(id, addr)
            }
        }
    }
}

#[derive(Serialize)]
struct StatePath {
    path: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::certificate::split_certificate_chain;
    use crate::config::ProxyProtocolConfig;
    use crate::proxy::{
        AddCertificate, Backend, CertificateAndKey, CertificateFingerprint, Cluster,
        ClusterMetricsData, FilteredData, HttpFrontend, LoadBalancingAlgorithms,
        LoadBalancingParams, PathRule, Percentiles, ProxyRequestOrder, QueryAnswerMetrics,
        RemoveBackend, RemoveCertificate, Route, RulePosition, TlsVersion, WorkerMetrics,
    };
    use hex::FromHex;
    use serde_json;

    #[test]
    fn config_message_test() {
        let raw_json = r#"{ "id": "ID_TEST", "version": 0, "type": "PROXY", "data":{"type": "ADD_HTTP_FRONTEND", "data": { "route": {"CLUSTER_ID": "xxx"}, "hostname": "yyy", "path": {"PREFIX": "xxx"}, "address": "0.0.0.0:8080"}} }"#;
        let message: CommandRequest = serde_json::from_str(raw_json).unwrap();
        println!("{:?}", message);
        assert_eq!(
            message.order,
            CommandRequestOrder::Proxy(Box::new(ProxyRequestOrder::AddHttpFrontend(
                HttpFrontend {
                    route: Route::ClusterId(String::from("xxx")),
                    hostname: String::from("yyy"),
                    path: PathRule::Prefix(String::from("xxx")),
                    method: None,
                    address: "0.0.0.0:8080".parse().unwrap(),
                    position: RulePosition::Tree,
                    tags: None,
                }
            )))
        );
    }

    macro_rules! test_message (
    ($name: ident, $filename: expr, $expected_message: expr) => (

      #[test]
      fn $name() {
        let data = include_str!($filename);
        let pretty_print = serde_json::to_string_pretty(&$expected_message).expect("should have serialized");
        assert_eq!(&pretty_print, data, "\nserialized message:\n{}\n\nexpected message:\n{}", pretty_print, data);

        let message: CommandRequest = serde_json::from_str(data).unwrap();
        assert_eq!(message, $expected_message, "\ndeserialized message:\n{:#?}\n\nexpected message:\n{:#?}", message, $expected_message);

      }

    )
  );

    macro_rules! test_message_answer (
    ($name: ident, $filename: expr, $expected_message: expr) => (

      #[test]
      fn $name() {
        let data = include_str!($filename);
        let pretty_print = serde_json::to_string_pretty(&$expected_message).expect("should have serialized");
        assert_eq!(&pretty_print, data, "\nserialized message:\n{}\n\nexpected message:\n{}", pretty_print, data);

        let message: CommandResponse = serde_json::from_str(data).unwrap();
        assert_eq!(message, $expected_message, "\ndeserialized message:\n{:#?}\n\nexpected message:\n{:#?}", message, $expected_message);

      }

    )
  );

    test_message!(
        add_cluster,
        "../assets/add_cluster.json",
        CommandRequest {
            id: "ID_TEST".to_string(),
            version: 0,
            order: CommandRequestOrder::Proxy(Box::new(ProxyRequestOrder::AddCluster(Cluster {
                cluster_id: String::from("xxx"),
                sticky_session: true,
                https_redirect: true,
                proxy_protocol: Some(ProxyProtocolConfig::ExpectHeader),
                load_balancing: LoadBalancingAlgorithms::RoundRobin,
                load_metric: None,
                answer_503: None,
            }))),
            worker_id: None
        }
    );

    test_message!(
        remove_cluster,
        "../assets/remove_cluster.json",
        CommandRequest {
            id: "ID_TEST".to_string(),
            version: 0,
            order: CommandRequestOrder::Proxy(Box::new(ProxyRequestOrder::RemoveCluster {
                cluster_id: String::from("xxx")
            })),
            worker_id: None
        }
    );

    test_message!(
        add_http_front,
        "../assets/add_http_front.json",
        CommandRequest {
            id: "ID_TEST".to_string(),
            version: 0,
            order: CommandRequestOrder::Proxy(Box::new(ProxyRequestOrder::AddHttpFrontend(
                HttpFrontend {
                    route: Route::ClusterId(String::from("xxx")),
                    hostname: String::from("yyy"),
                    path: PathRule::Prefix(String::from("xxx")),
                    method: None,
                    address: "0.0.0.0:8080".parse().unwrap(),
                    position: RulePosition::Tree,
                    tags: None,
                }
            ))),
            worker_id: None
        }
    );

    test_message!(
        remove_http_front,
        "../assets/remove_http_front.json",
        CommandRequest {
            id: "ID_TEST".to_string(),
            version: 0,
            order: CommandRequestOrder::Proxy(Box::new(ProxyRequestOrder::RemoveHttpFrontend(
                HttpFrontend {
                    route: Route::ClusterId(String::from("xxx")),
                    hostname: String::from("yyy"),
                    path: PathRule::Prefix(String::from("xxx")),
                    method: None,
                    address: "0.0.0.0:8080".parse().unwrap(),
                    position: RulePosition::Tree,
                    tags: Some(BTreeMap::from([
                        ("owner".to_owned(), "John".to_owned()),
                        (
                            "uuid".to_owned(),
                            "0dd8d7b1-a50a-461a-b1f9-5211a5f45a83".to_owned()
                        )
                    ]))
                }
            ))),
            worker_id: None
        }
    );

    test_message!(
        add_https_front,
        "../assets/add_https_front.json",
        CommandRequest {
            id: "ID_TEST".to_string(),
            version: 0,
            order: CommandRequestOrder::Proxy(Box::new(ProxyRequestOrder::AddHttpsFrontend(
                HttpFrontend {
                    route: Route::ClusterId(String::from("xxx")),
                    hostname: String::from("yyy"),
                    path: PathRule::Prefix(String::from("xxx")),
                    method: None,
                    address: "0.0.0.0:8443".parse().unwrap(),
                    position: RulePosition::Tree,
                    tags: None,
                }
            ))),
            worker_id: None
        }
    );

    test_message!(
        remove_https_front,
        "../assets/remove_https_front.json",
        CommandRequest {
            id: "ID_TEST".to_string(),
            version: 0,
            order: CommandRequestOrder::Proxy(Box::new(ProxyRequestOrder::RemoveHttpsFrontend(
                HttpFrontend {
                    route: Route::ClusterId(String::from("xxx")),
                    hostname: String::from("yyy"),
                    path: PathRule::Prefix(String::from("xxx")),
                    method: None,
                    address: "0.0.0.0:8443".parse().unwrap(),
                    position: RulePosition::Tree,
                    tags: Some(BTreeMap::from([
                        ("owner".to_owned(), "John".to_owned()),
                        (
                            "uuid".to_owned(),
                            "0dd8d7b1-a50a-461a-b1f9-5211a5f45a83".to_owned()
                        )
                    ]))
                }
            ))),
            worker_id: None
        }
    );

    const KEY: &str = include_str!("../../lib/assets/key.pem");
    const CERTIFICATE: &str = include_str!("../../lib/assets/certificate.pem");
    const CHAIN: &str = include_str!("../../lib/assets/certificate_chain.pem");

    test_message!(
        add_certificate,
        "../assets/add_certificate.json",
        CommandRequest {
            id: "ID_TEST".to_string(),
            version: 0,
            order: CommandRequestOrder::Proxy(Box::new(ProxyRequestOrder::AddCertificate(
                AddCertificate {
                    address: "0.0.0.0:443".parse().unwrap(),
                    certificate: CertificateAndKey {
                        certificate: String::from(CERTIFICATE),
                        certificate_chain: split_certificate_chain(String::from(CHAIN)),
                        key: String::from(KEY),
                        versions: vec![TlsVersion::TLSv1_2, TlsVersion::TLSv1_3],
                    },
                    names: vec![],
                    expired_at: None,
                }
            ))),
            worker_id: None
        }
    );

    test_message!(
        remove_certificate,
        "../assets/remove_certificate.json",
        CommandRequest {
            id: "ID_TEST".to_string(),
            version: 0,
            order: CommandRequestOrder::Proxy(Box::new(ProxyRequestOrder::RemoveCertificate(
                RemoveCertificate {
                    address: "0.0.0.0:443".parse().unwrap(),
                    fingerprint: CertificateFingerprint(
                        FromHex::from_hex(
                            "ab2618b674e15243fd02a5618c66509e4840ba60e7d64cebec84cdbfeceee0c5"
                        )
                        .unwrap()
                    ),
                }
            ))),
            worker_id: None
        }
    );

    test_message!(
        add_backend,
        "../assets/add_backend.json",
        CommandRequest {
            id: "ID_TEST".to_string(),
            version: 0,
            order: CommandRequestOrder::Proxy(Box::new(ProxyRequestOrder::AddBackend(Backend {
                cluster_id: String::from("xxx"),
                backend_id: String::from("xxx-0"),
                address: "127.0.0.1:8080".parse().unwrap(),
                load_balancing_parameters: Some(LoadBalancingParams { weight: 0 }),
                sticky_id: Some(String::from("xxx-0")),
                backup: Some(false),
            }))),
            worker_id: None
        }
    );

    test_message!(
        remove_backend,
        "../assets/remove_backend.json",
        CommandRequest {
            id: "ID_TEST".to_string(),
            version: 0,
            order: CommandRequestOrder::Proxy(Box::new(ProxyRequestOrder::RemoveBackend(
                RemoveBackend {
                    cluster_id: String::from("xxx"),
                    backend_id: String::from("xxx-0"),
                    address: "127.0.0.1:8080".parse().unwrap(),
                }
            ))),
            worker_id: None
        }
    );

    test_message!(
        soft_stop,
        "../assets/soft_stop.json",
        CommandRequest {
            id: "ID_TEST".to_string(),
            version: 0,
            order: CommandRequestOrder::Proxy(Box::new(ProxyRequestOrder::SoftStop)),
            worker_id: Some(0),
        }
    );

    test_message!(
        hard_stop,
        "../assets/hard_stop.json",
        CommandRequest {
            id: "ID_TEST".to_string(),
            version: 0,
            order: CommandRequestOrder::Proxy(Box::new(ProxyRequestOrder::HardStop)),
            worker_id: Some(0),
        }
    );

    test_message!(
        status,
        "../assets/status.json",
        CommandRequest {
            id: "ID_TEST".to_string(),
            version: 0,
            order: CommandRequestOrder::Proxy(Box::new(ProxyRequestOrder::Status)),
            worker_id: Some(0),
        }
    );

    test_message!(
        load_state,
        "../assets/load_state.json",
        CommandRequest {
            id: "ID_TEST".to_string(),
            version: 0,
            order: CommandRequestOrder::LoadState {
                path: String::from("./config_dump.json")
            },
            worker_id: None
        }
    );

    test_message!(
        save_state,
        "../assets/save_state.json",
        CommandRequest {
            id: "ID_TEST".to_string(),
            version: 0,
            order: CommandRequestOrder::SaveState {
                path: String::from("./config_dump.json")
            },
            worker_id: None
        }
    );

    test_message!(
        dump_state,
        "../assets/dump_state.json",
        CommandRequest {
            id: "ID_TEST".to_string(),
            version: 0,
            order: CommandRequestOrder::DumpState,
            worker_id: None
        }
    );

    test_message!(
        list_workers,
        "../assets/list_workers.json",
        CommandRequest {
            id: "ID_TEST".to_string(),
            version: 0,
            order: CommandRequestOrder::ListWorkers,
            worker_id: None
        }
    );

    test_message!(
        upgrade_main,
        "../assets/upgrade_main.json",
        CommandRequest {
            id: "ID_TEST".to_string(),
            version: 0,
            order: CommandRequestOrder::UpgradeMain,
            worker_id: None
        }
    );

    test_message!(
        upgrade_worker,
        "../assets/upgrade_worker.json",
        CommandRequest {
            id: "ID_TEST".to_string(),
            version: 0,
            order: CommandRequestOrder::UpgradeWorker(0),
            worker_id: None
        }
    );

    test_message_answer!(
        answer_workers_status,
        "../assets/answer_workers_status.json",
        CommandResponse {
            id: "ID_TEST".to_string(),
            version: 0,
            status: CommandStatus::Ok,
            message: String::from(""),
            content: Some(CommandResponseContent::Workers(vec!(
                WorkerInfo {
                    id: 1,
                    pid: 5678,
                    run_state: RunState::Running,
                },
                WorkerInfo {
                    id: 0,
                    pid: 1234,
                    run_state: RunState::Stopping,
                },
            ))),
        }
    );

    test_message_answer!(
        answer_metrics,
        "../assets/answer_metrics.json",
        CommandResponse {
            id: "ID_TEST".to_string(),
            version: 0,
            status: CommandStatus::Ok,
            message: String::from(""),
            content: Some(CommandResponseContent::Metrics(AggregatedMetricsData {
                main: [
                    (String::from("sozu.gauge"), FilteredData::Gauge(1)),
                    (String::from("sozu.count"), FilteredData::Count(-2)),
                    (String::from("sozu.time"), FilteredData::Time(1234)),
                ]
                .iter()
                .cloned()
                .collect(),
                workers: [(
                    String::from("0"),
                    QueryAnswer::Metrics(QueryAnswerMetrics::All(WorkerMetrics {
                        proxy: Some(
                            [
                                (String::from("sozu.gauge"), FilteredData::Gauge(1)),
                                (String::from("sozu.count"), FilteredData::Count(-2)),
                                (String::from("sozu.time"), FilteredData::Time(1234)),
                            ]
                            .iter()
                            .cloned()
                            .collect()
                        ),
                        clusters: Some(
                            [(
                                String::from("cluster_1"),
                                ClusterMetricsData {
                                    cluster: Some(
                                        [(
                                            String::from("request_time"),
                                            FilteredData::Percentiles(Percentiles {
                                                samples: 42,
                                                p_50: 1,
                                                p_90: 2,
                                                p_99: 10,
                                                p_99_9: 12,
                                                p_99_99: 20,
                                                p_99_999: 22,
                                                p_100: 30,
                                            })
                                        )]
                                        .iter()
                                        .cloned()
                                        .collect()
                                    ),
                                    backends: Some(
                                        [(
                                            String::from("cluster_1-0"),
                                            [
                                                (
                                                    String::from("bytes_in"),
                                                    FilteredData::Count(256)
                                                ),
                                                (
                                                    String::from("bytes_out"),
                                                    FilteredData::Count(128)
                                                ),
                                                (
                                                    String::from("percentiles"),
                                                    FilteredData::Percentiles(Percentiles {
                                                        samples: 42,
                                                        p_50: 1,
                                                        p_90: 2,
                                                        p_99: 10,
                                                        p_99_9: 12,
                                                        p_99_99: 20,
                                                        p_99_999: 22,
                                                        p_100: 30,
                                                    })
                                                )
                                            ]
                                            .iter()
                                            .cloned()
                                            .collect()
                                        )]
                                        .iter()
                                        .cloned()
                                        .collect()
                                    ),
                                }
                            )]
                            .iter()
                            .cloned()
                            .collect()
                        )
                    }))
                )]
                .iter()
                .cloned()
                .collect()
            }))
        }
    );
}
