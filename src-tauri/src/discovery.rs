//! # mDNS 服务发现模块
//!
//! 使用 mDNS（Bonjour/Avahi）在局域网内自动发现和注册 clip-mesh 节点。
//!
//! ## 服务类型
//!
//! `_clipmesh._tcp.local.`
//!
//! ## TXT 记录
//!
//! - `device_id`: 设备唯一标识
//! - `device_name`: 设备名称

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use std::collections::HashMap;
use std::net::Ipv4Addr;
use tokio::sync::mpsc;

/// mDNS 服务类型
const SERVICE_TYPE: &str = "_clipmesh._tcp.local.";

/// 发现的远端节点信息
#[derive(Debug, Clone)]
pub struct DiscoveredPeer {
    /// 设备 ID
    pub device_id: String,
    /// 设备名称
    pub device_name: String,
    /// IP 地址
    pub ip: Ipv4Addr,
    /// WebSocket 端口
    pub port: u16,
}

/// mDNS 服务发现管理器。
///
/// 同时负责：
/// 1. 注册本节点服务（让其他节点发现）
/// 2. 浏览局域网内的其他节点
pub struct DiscoveryManager {
    daemon: ServiceDaemon,
    our_device_id: String,
    /// 注册时的完整实例名（用于 unregister）
    instance_fullname: String,
}

impl DiscoveryManager {
    /// 创建发现管理器。
    pub fn new() -> Result<Self, String> {
        let daemon = ServiceDaemon::new()
            .map_err(|e| format!("Failed to create mDNS daemon: {}", e))?;
        Ok(Self {
            daemon,
            our_device_id: String::new(),
            instance_fullname: String::new(),
        })
    }

    /// 注册本节点的 mDNS 服务。
    ///
    /// 让局域网内其他 clip-mesh 节点能发现我们。
    pub fn register(
        &mut self,
        device_id: &str,
        device_name: &str,
        port: u16,
    ) -> Result<(), String> {
        self.our_device_id = device_id.to_string();

        // 获取本机主机名作为实例名
        let hostname = hostname::get()
            .map(|h| h.to_string_lossy().to_string())
            .unwrap_or_else(|_| device_id[..8].to_string());

        let short = crate::websocket::short_id(device_id);
        let instance_name = format!("clipmesh-{}", short);

        // TXT 记录
        let mut txt: HashMap<String, String> = HashMap::new();
        txt.insert("device_id".to_string(), device_id.to_string());
        txt.insert("device_name".to_string(), device_name.to_string());

        // 注册服务（使用 0.0.0.0 让系统自动选择 IP）
        let service = ServiceInfo::new(
            SERVICE_TYPE,
            &instance_name,
            &format!("{}.local.", hostname),
            "",  // 空 IP 让 mDNS 自动填充本机 IP
            port,
            txt,
        )
        .map_err(|e| format!("Failed to create service info: {}", e))?;

        // 保存完整实例名用于 unregister: "clipmesh-xxx._clipmesh._tcp.local."
        self.instance_fullname = format!("{}.{}", instance_name, SERVICE_TYPE);

        self.daemon
            .register(service)
            .map_err(|e| format!("Failed to register mDNS service: {}", e))?;

        log::info!(
            "[Discovery] Registered mDNS service: {} on port {}",
            instance_name,
            port
        );

        Ok(())
    }

    /// 开始浏览局域网内的其他 clip-mesh 节点。
    ///
    /// 发现的节点通过 `peer_tx` 通道返回。
    /// 此方法不阻塞，浏览在后台线程中运行。
    pub fn browse(&self, peer_tx: mpsc::Sender<DiscoveredPeer>) -> Result<(), String> {
        let receiver = self
            .daemon
            .browse(SERVICE_TYPE)
            .map_err(|e| format!("Failed to browse mDNS: {}", e))?;

        let our_id = self.our_device_id.clone();

        // 在后台线程中处理发现事件
        std::thread::spawn(move || {
            while let Ok(event) = receiver.recv() {
                match event {
                    ServiceEvent::ServiceResolved(info) => {
                        let props = info.get_properties();
                        let device_id = props
                            .iter()
                            .find(|p| p.key() == "device_id")
                            .map(|p| p.val_str().to_string())
                            .unwrap_or_default();

                        // 忽略自己
                        if device_id == our_id || device_id.is_empty() {
                            continue;
                        }

                        let device_name = props
                            .iter()
                            .find(|p| p.key() == "device_name")
                            .map(|p| p.val_str().to_string())
                            .unwrap_or_else(|| "Unknown".to_string());

                        // 获取 IP 地址
                        let ip = info.get_addresses().iter().next().copied();
                        let ip = match ip {
                            Some(std::net::IpAddr::V4(v4)) => v4,
                            _ => {
                                log::warn!("[Discovery] No IPv4 address for {}", device_id);
                                continue;
                            }
                        };

                        let port = info.get_port();

                        log::info!(
                            "[Discovery] Found peer: {} ({}) at {}:{}",
                            device_name,
                            crate::websocket::short_id(&device_id),
                            ip,
                            port
                        );

                        let peer = DiscoveredPeer {
                            device_id,
                            device_name,
                            ip,
                            port,
                        };

                        if peer_tx.blocking_send(peer).is_err() {
                            log::warn!("[Discovery] Peer channel closed, stopping browse");
                            return;
                        }
                    }
                    ServiceEvent::SearchStarted(_) => {
                        log::debug!("[Discovery] mDNS search started");
                    }
                    ServiceEvent::ServiceRemoved(_, fullname) => {
                        log::info!("[Discovery] Service removed: {}", fullname);
                    }
                    _ => {}
                }
            }
        });

        Ok(())
    }

    /// 停止 mDNS 服务并注销。
    pub fn shutdown(&self) {
        if !self.instance_fullname.is_empty() {
            let _ = self.daemon.unregister(&self.instance_fullname);
        }
        let _ = self.daemon.stop_browse(SERVICE_TYPE);
        let _ = self.daemon.shutdown();
        log::info!("[Discovery] mDNS daemon shut down");
    }
}

impl Drop for DiscoveryManager {
    fn drop(&mut self) {
        self.shutdown();
    }
}
