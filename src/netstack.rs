use std::{
	collections::{hash_map::Entry, HashMap, HashSet, VecDeque},
	io::{self, Read, Write},
	net::{Ipv4Addr, Shutdown, SocketAddr, SocketAddrV4, TcpListener, TcpStream, ToSocketAddrs, UdpSocket},
	sync::{
		Arc,
		atomic::{AtomicBool, Ordering::Relaxed}
	},
	time::Duration
};
use crossbeam_channel::{unbounded, Receiver, Sender};
use smoltcp::{
	iface::{Config as InterfaceConfig, Interface, SocketHandle, SocketSet},
	phy::{self, ChecksumCapabilities, Device, DeviceCapabilities, Medium},
	socket::tcp,
	time::Instant as NetInstant,
	wire::{
		ArpOperation, ArpPacket, ArpRepr,
		EthernetAddress, EthernetFrame, EthernetProtocol, EthernetRepr, HardwareAddress,
		IpAddress, IpCidr, IpListenEndpoint, IpProtocol, Ipv4Packet, Ipv4Repr, 
		TcpPacket, UdpPacket, UdpRepr
	}
};

pub const GATEWAY_IP: Ipv4Addr = Ipv4Addr::new(10, 69, 4, 20);
pub const GUEST_IP: Ipv4Addr = Ipv4Addr::new(10, 69, 4, 21);
const NETMASK: Ipv4Addr = Ipv4Addr::new(255, 255, 255, 0);
const SUBNET_BROADCAST_IP: Ipv4Addr = Ipv4Addr::new(10, 69, 4, 255);
const GATEWAY_MAC: EthernetAddress = EthernetAddress([0x52, 0x71, 0x69, 0x04, 0x20, 0x67]);
const ETHERNET_MTU: usize = 1514;
const MAX_FRAME_SIZE: usize = 0xffff;
const MAX_PENDING_TO_MAME: usize = 1024 * 1024;
const TCP_BUFFER_SIZE: usize = 64 * 1024;
const HOST_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const UDP_FLOW_TIMEOUT: Duration = Duration::from_secs(120);
const DHCP_SERVER_PORT: u16 = 67;
const DHCP_CLIENT_PORT: u16 = 68;
const DHCP_MAGIC_COOKIE: [u8; 4] = [99, 130, 83, 99];
const DHCP_LEASE_SECONDS: u32 = 24 * 60 * 60;
const DNS_PORT: u16 = 53;
const DNS_TTL: u32 = 60;
const IDLE_SLEEP: Duration = Duration::from_millis(1);
const ACCEPT_SLEEP: Duration = Duration::from_millis(20);

pub type NetworkLogger = Box<dyn Fn(String) + Send>;

pub struct NetworkStack {
	port: u16,
	shutdown: Arc<AtomicBool>
}

impl NetworkStack {
	pub fn start(dns_overrides: Vec<(String, String)>, ip_overrides: Vec<(String, Vec<String>)>, log: NetworkLogger) -> io::Result<NetworkStack> {
		let listener = TcpListener::bind("127.0.0.1:0")?;
		listener.set_nonblocking(true)?;

		let port = listener.local_addr()?.port();
		let shutdown = Arc::new(AtomicBool::new(false));
		let shutdown_cpy = shutdown.clone();
		let rules = NetworkRules {
			dns_overrides: normalize_dns_overrides(dns_overrides),
			ip_overrides: normalize_ip_overrides(ip_overrides, &log)
		};

		std::thread::spawn(move || {
			while !shutdown_cpy.load(Relaxed) {
				match listener.accept() {
					Ok((stream, _)) => {
						log("Ethernet: MAME connected to the launcher network.\n".into());

						match Session::new(stream, &rules, &log, &shutdown_cpy) {
							Ok(mut session) => {
								if let Err(e) = session.run() {
									log(format!("Ethernet: network session ended with an error: {}\n", e));
								}
							},
							Err(e) => {
								log(format!("Ethernet: couldn't start network session: {}\n", e));
							}
						}
					},
					Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
						std::thread::sleep(ACCEPT_SLEEP);
					},
					Err(e) => {
						log(format!("Ethernet: stopped accepting connections from MAME: {}\n", e));
						break;
					}
				}
			}
		});

		Ok(NetworkStack {
			port: port,
			shutdown: shutdown
		})
	}

	pub fn port(&self) -> u16 {
		self.port
	}
}

impl Drop for NetworkStack {
	fn drop(&mut self) {
		self.shutdown.store(true, Relaxed);
	}
}

fn normalize_dns_overrides(dns_overrides: Vec<(String, String)>) -> Vec<(String, String)> {
	dns_overrides
		.into_iter()
		.map(|(domain, target)| {
			let domain = domain.trim().trim_start_matches("*.").trim_matches('.').to_lowercase();

			(domain, target.trim().to_string())
		})
		.filter(|(domain, target)| domain != "" && target != "")
		.collect()
}

fn normalize_ip_overrides(ip_overrides: Vec<(String, Vec<String>)>, log: &NetworkLogger) -> HashMap<Ipv4Addr, HostTarget> {
	let mut normalized: HashMap<Ipv4Addr, HostTarget> = HashMap::new();

	for (target, guest_ips) in ip_overrides {
		let target = target.trim().trim_matches('.').to_lowercase();
		if target == "" {
			continue;
		}

		let host_target = match target.parse::<Ipv4Addr>() {
			Ok(ip) => HostTarget::Ip(ip),
			Err(_) => HostTarget::Name(target.clone())
		};

		for guest_ip in guest_ips {
			match guest_ip.trim().parse::<Ipv4Addr>() {
				Ok(guest_ip) => {
					normalized.insert(guest_ip, host_target.clone());
				},
				Err(_) => {
					log(format!("Ethernet: ignoring invalid IP override '{}' for {}\n", guest_ip, target));
				}
			}
		}
	}

	normalized
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum HostTarget {
	Ip(Ipv4Addr),
	Name(String)
}

impl std::fmt::Display for HostTarget {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			HostTarget::Ip(ip) => write!(f, "{}", ip),
			HostTarget::Name(name) => write!(f, "{}", name)
		}
	}
}

struct NetworkRules {
	dns_overrides: Vec<(String, String)>,
	ip_overrides: HashMap<Ipv4Addr, HostTarget>
}

fn resolve_ipv4s(name: &str, port: u16) -> Vec<Ipv4Addr> {
	(name, port)
		.to_socket_addrs()
		.map(|addrs| {
			addrs
				.filter_map(|addr| match addr {
					SocketAddr::V4(addr) => Some(*addr.ip()),
					_ => None
				})
				.collect()
		})
		.unwrap_or_default()
}

struct FrameQueue {
	rx: VecDeque<Vec<u8>>,
	tx: VecDeque<Vec<u8>>
}

struct QueueRxToken(Vec<u8>);

impl phy::RxToken for QueueRxToken {
	fn consume<R, F>(self, f: F) -> R where F: FnOnce(&[u8]) -> R {
		f(&self.0)
	}
}

struct QueueTxToken<'a>(&'a mut VecDeque<Vec<u8>>);

impl<'a> phy::TxToken for QueueTxToken<'a> {
	fn consume<R, F>(self, len: usize, f: F) -> R where F: FnOnce(&mut [u8]) -> R {
		let mut frame = vec![0u8; len];
		let result = f(&mut frame);
		self.0.push_back(frame);

		result
	}
}

impl Device for FrameQueue {
	type RxToken<'a> = QueueRxToken where Self: 'a;
	type TxToken<'a> = QueueTxToken<'a> where Self: 'a;

	fn receive(&mut self, _timestamp: NetInstant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
		let frame = self.rx.pop_front()?;

		Some((QueueRxToken(frame), QueueTxToken(&mut self.tx)))
	}

	fn transmit(&mut self, _timestamp: NetInstant) -> Option<Self::TxToken<'_>> {
		Some(QueueTxToken(&mut self.tx))
	}

	fn capabilities(&self) -> DeviceCapabilities {
		let mut caps = DeviceCapabilities::default();
		caps.medium = Medium::Ethernet;
		caps.max_transmission_unit = ETHERNET_MTU;

		caps
	}
}

type FlowKey = (u16, Ipv4Addr, u16);

struct TcpFlow {
	handle: SocketHandle,
	host: Option<TcpStream>,
	to_host: Vec<u8>,
	host_eof: bool,
	host_write_closed: bool
}

struct UdpFlow {
	socket: UdpSocket,
	guest_ip: Ipv4Addr,
	translated: HashMap<SocketAddrV4, Ipv4Addr>,
	last_used: std::time::Instant
}

struct DnsQuery {
	id: u16,
	flags: u16,
	name: String,
	qtype: u16,
	question: Vec<u8>,
	client_ip: Ipv4Addr,
	client_port: u16,
	server_ip: Ipv4Addr
}

enum HostEvent {
	Connected(FlowKey, String, io::Result<TcpStream>),
	DnsAnswer(DnsQuery, Option<Vec<Ipv4Addr>>),
	TargetResolved(String, Option<Ipv4Addr>)
}

enum GuestFrameAction {
	Drop,
	Inject,
	OpenTcp(FlowKey),
	Udp(EthernetAddress, Ipv4Addr, Ipv4Addr, u16, u16, Vec<u8>)
}

struct Session<'a> {
	mame: TcpStream,
	from_mame: Vec<u8>,
	to_mame: Vec<u8>,
	device: FrameQueue,
	iface: Interface,
	sockets: SocketSet<'static>,
	tcp_flows: HashMap<FlowKey, TcpFlow>,
	udp_flows: HashMap<u16, UdpFlow>,
	guest_mac: Option<EthernetAddress>,
	events_tx: Sender<HostEvent>,
	events_rx: Receiver<HostEvent>,
	rules: &'a NetworkRules,
	resolved_targets: HashMap<String, Ipv4Addr>,
	resolving_targets: HashSet<String>,
	log: &'a NetworkLogger,
	shutdown: &'a AtomicBool
}

impl<'a> Session<'a> {
	fn new(mame: TcpStream, rules: &'a NetworkRules, log: &'a NetworkLogger, shutdown: &'a AtomicBool) -> io::Result<Session<'a>> {
		mame.set_nonblocking(true)?;
		let _ = mame.set_nodelay(true);

		let mut device = FrameQueue {
			rx: VecDeque::new(),
			tx: VecDeque::new()
		};

		let mut iface_config = InterfaceConfig::new(HardwareAddress::Ethernet(GATEWAY_MAC));
		iface_config.random_seed = rand::random();

		let mut iface = Interface::new(iface_config, &mut device, NetInstant::now());
		iface.update_ip_addrs(|ip_addrs| {
			let _ = ip_addrs.push(IpCidr::new(IpAddress::Ipv4(GATEWAY_IP), 24));
		});

		iface.set_any_ip(true);

		let (events_tx, events_rx) = unbounded();

		Ok(Session {
			mame: mame,
			from_mame: vec![],
			to_mame: vec![],
			device: device,
			iface: iface,
			sockets: SocketSet::new(vec![]),
			tcp_flows: HashMap::new(),
			udp_flows: HashMap::new(),
			guest_mac: None,
			events_tx: events_tx,
			events_rx: events_rx,
			rules: rules,
			resolved_targets: HashMap::new(),
			resolving_targets: HashSet::new(),
			log: log,
			shutdown: shutdown
		})
	}

	fn run(&mut self) -> io::Result<()> {
		while !self.shutdown.load(Relaxed) {
			let mut busy = false;

			match self.read_mame() {
				Ok(did_read) => busy |= did_read,
				Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
					(self.log)("Ethernet: MAME disconnected from the launcher network.\n".into());
					return Ok(());
				},
				Err(e) => return Err(e)
			};

			busy |= self.handle_host_events();
			self.poll_iface();
			busy |= self.pump_tcp();
			busy |= self.pump_udp();
			self.poll_iface();
			busy |= self.write_mame()?;

			if !busy {
				std::thread::sleep(IDLE_SLEEP);
			}
		}

		Ok(())
	}

	fn poll_iface(&mut self) {
		self.iface.poll(NetInstant::now(), &mut self.device, &mut self.sockets);
	}

	fn read_mame(&mut self) -> io::Result<bool> {
		let mut buf = [0u8; 16 * 1024];
		let mut did_read = false;

		loop {
			match self.mame.read(&mut buf) {
				Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
				Ok(bytes_read) => {
					self.from_mame.extend_from_slice(&buf[0..bytes_read]);
					did_read = true;
				},
				Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
				Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
				Err(e) => return Err(e)
			}
		}

		let mut offset = 0;
		while self.from_mame.len() - offset >= 4 {
			let frame_len = u32::from_be_bytes(self.from_mame[offset..offset + 4].try_into().unwrap()) as usize;
			if frame_len > MAX_FRAME_SIZE {
				return Err(io::Error::new(io::ErrorKind::InvalidData, "MAME sent an oversized frame"));
			}

			if self.from_mame.len() - offset - 4 < frame_len {
				break;
			}

			let frame = self.from_mame[offset + 4..offset + 4 + frame_len].to_vec();
			offset += 4 + frame_len;

			self.handle_guest_frame(frame);
		}
		self.from_mame.drain(0..offset);

		Ok(did_read)
	}

	fn write_mame(&mut self) -> io::Result<bool> {
		while let Some(frame) = self.device.tx.pop_front() {
			if self.to_mame.len() + frame.len() + 4 <= MAX_PENDING_TO_MAME {
				self.to_mame.extend_from_slice(&(frame.len() as u32).to_be_bytes());
				self.to_mame.extend_from_slice(&frame);
			}
		}

		if self.to_mame.is_empty() {
			return Ok(false);
		}

		match self.mame.write(&self.to_mame) {
			Ok(bytes_written) => {
				self.to_mame.drain(0..bytes_written);

				Ok(bytes_written > 0)
			},
			Err(e) if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::Interrupted => Ok(false),
			Err(e) => Err(e)
		}
	}

	fn handle_guest_frame(&mut self, frame: Vec<u8>) {
		let action = match EthernetFrame::new_checked(&frame[..]) {
			Ok(eth) => {
				let src_mac = eth.src_addr();
				if src_mac.is_unicast() {
					self.guest_mac = Some(src_mac);
				}

				match eth.ethertype() {
					EthernetProtocol::Arp => {
						match ArpPacket::new_checked(eth.payload()).and_then(|arp| ArpRepr::parse(&arp)) {
							Ok(ArpRepr::EthernetIpv4 { operation: ArpOperation::Request, target_protocol_addr, .. }) if target_protocol_addr == GATEWAY_IP => GuestFrameAction::Inject,
							Ok(ArpRepr::EthernetIpv4 { operation: ArpOperation::Reply, .. }) => GuestFrameAction::Inject,
							_ => GuestFrameAction::Drop
						}
					},
					EthernetProtocol::Ipv4 => {
						match Ipv4Packet::new_checked(eth.payload()) {
							Ok(ip) => {
								match ip.next_header() {
									IpProtocol::Udp => {
										match UdpPacket::new_checked(ip.payload()) {
											Ok(udp) => GuestFrameAction::Udp(src_mac, ip.src_addr(), ip.dst_addr(), udp.src_port(), udp.dst_port(), udp.payload().to_vec()),
											Err(_) => GuestFrameAction::Drop
										}
									},
									IpProtocol::Tcp => {
										match TcpPacket::new_checked(ip.payload()) {
											Ok(tcp) if tcp.syn() && !tcp.ack() => GuestFrameAction::OpenTcp((tcp.src_port(), ip.dst_addr(), tcp.dst_port())),
											Ok(_) => GuestFrameAction::Inject,
											Err(_) => GuestFrameAction::Drop
										}
									},
									IpProtocol::Icmp => GuestFrameAction::Inject,
									_ => GuestFrameAction::Drop
								}
							},
							Err(_) => GuestFrameAction::Drop
						}
					},
					_ => GuestFrameAction::Drop
				}
			},
			Err(_) => GuestFrameAction::Drop
		};

		match action {
			GuestFrameAction::Drop => { },
			GuestFrameAction::Inject => {
				self.inject(frame);
			},
			GuestFrameAction::OpenTcp(key) => {
				self.open_tcp_flow(key);
				self.inject(frame);
			},
			GuestFrameAction::Udp(src_mac, src_ip, dst_ip, src_port, dst_port, payload) => {
				self.handle_guest_udp(src_mac, src_ip, dst_ip, src_port, dst_port, payload);
			}
		}
	}

	fn inject(&mut self, frame: Vec<u8>) {
		self.device.rx.push_back(frame);

		self.poll_iface();
	}

	fn send_udp_to_guest(&mut self, dst_mac: EthernetAddress, src_ip: Ipv4Addr, src_port: u16, dst_ip: Ipv4Addr, dst_port: u16, payload: &[u8]) {
		let checksum_caps = ChecksumCapabilities::default();
		let eth_repr = EthernetRepr {
			src_addr: GATEWAY_MAC,
			dst_addr: dst_mac,
			ethertype: EthernetProtocol::Ipv4
		};
		let udp_repr = UdpRepr {
			src_port: src_port,
			dst_port: dst_port
		};
		let ip_repr = Ipv4Repr {
			src_addr: src_ip,
			dst_addr: dst_ip,
			next_header: IpProtocol::Udp,
			payload_len: udp_repr.header_len() + payload.len(),
			hop_limit: 64
		};

		let mut buf = vec![0u8; eth_repr.buffer_len() + ip_repr.buffer_len() + udp_repr.header_len() + payload.len()];

		let mut eth_frame = EthernetFrame::new_unchecked(&mut buf[..]);
		eth_repr.emit(&mut eth_frame);

		let mut ip_packet = Ipv4Packet::new_unchecked(eth_frame.payload_mut());
		ip_repr.emit(&mut ip_packet, &checksum_caps);

		let mut udp_packet = UdpPacket::new_unchecked(ip_packet.payload_mut());
		udp_repr.emit(
			&mut udp_packet,
			&IpAddress::Ipv4(src_ip),
			&IpAddress::Ipv4(dst_ip),
			payload.len(),
			|udp_payload| udp_payload.copy_from_slice(payload),
			&checksum_caps
		);

		self.device.tx.push_back(buf);
	}

	// NAT

	fn host_target_for(&self, guest_dst_ip: Ipv4Addr) -> HostTarget {
		if let Some(target) = self.rules.ip_overrides.get(&guest_dst_ip) {
			target.clone()
		} else if guest_dst_ip == GATEWAY_IP {
			HostTarget::Ip(Ipv4Addr::LOCALHOST)
		} else {
			HostTarget::Ip(guest_dst_ip)
		}
	}

	fn open_tcp_flow(&mut self, key: FlowKey) {
		if self.tcp_flows.contains_key(&key) {
			return;
		}

		let (_, remote_ip, remote_port) = key;

		let mut socket = tcp::Socket::new(
			tcp::SocketBuffer::new(vec![0u8; TCP_BUFFER_SIZE]),
			tcp::SocketBuffer::new(vec![0u8; TCP_BUFFER_SIZE])
		);
		socket.pause_synack(true);
		socket.set_nagle_enabled(false);

		if socket.listen(IpListenEndpoint { addr: Some(IpAddress::Ipv4(remote_ip)), port: remote_port }).is_err() {
			return;
		}

		let handle = self.sockets.add(socket);
		self.tcp_flows.insert(key, TcpFlow {
			handle: handle,
			host: None,
			to_host: vec![],
			host_eof: false,
			host_write_closed: false
		});

		let host_target = self.host_target_for(remote_ip);
		if self.rules.ip_overrides.contains_key(&remote_ip) {
			(self.log)(format!("Ethernet: IP override {}:{} -> {}:{}\n", remote_ip, remote_port, host_target, remote_port));
		}

		let host_description = format!("{}:{}", host_target, remote_port);
		let events_tx = self.events_tx.clone();
		std::thread::spawn(move || {
			let host_ips = match host_target {
				HostTarget::Ip(ip) => vec![ip],
				HostTarget::Name(name) => resolve_ipv4s(&name, remote_port)
			};

			let mut result = Err(io::Error::new(io::ErrorKind::NotFound, "couldn't resolve host"));
			for host_ip in host_ips {
				result = TcpStream::connect_timeout(&SocketAddr::V4(SocketAddrV4::new(host_ip, remote_port)), HOST_CONNECT_TIMEOUT);
				if result.is_ok() {
					break;
				}
			}

			let _ = events_tx.send(HostEvent::Connected(key, host_description, result));
		});
	}

	fn pump_tcp(&mut self) -> bool {
		let mut busy = false;
		let mut finished_flows: Vec<FlowKey> = vec![];
		let mut buf = [0u8; 16 * 1024];

		for (key, flow) in self.tcp_flows.iter_mut() {
			let socket = self.sockets.get_mut::<tcp::Socket>(flow.handle);

			match socket.state() {
				tcp::State::Closed | tcp::State::TimeWait => {
					finished_flows.push(*key);
					continue;
				},
				tcp::State::Listen | tcp::State::SynSent | tcp::State::SynReceived => {
					continue;
				},
				_ => { }
			};

			let host = match flow.host.as_mut() {
				Some(host) => host,
				None => continue
			};

			// box -> host
			while socket.can_recv() && flow.to_host.len() < TCP_BUFFER_SIZE {
				match socket.recv_slice(&mut buf) {
					Ok(bytes_read) if bytes_read > 0 => {
						flow.to_host.extend_from_slice(&buf[0..bytes_read]);
						busy = true;
					},
					_ => break
				}
			}

			while !flow.to_host.is_empty() {
				match host.write(&flow.to_host) {
					Ok(bytes_written) if bytes_written > 0 => {
						flow.to_host.drain(0..bytes_written);
						busy = true;
					},
					Err(e) if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::Interrupted => break,
					_ => {
						socket.abort();
						break;
					}
				}
			}

			if !socket.may_recv() && flow.to_host.is_empty() && !flow.host_write_closed {
				let _ = host.shutdown(Shutdown::Write);
				flow.host_write_closed = true;
			}

			// host -> box
			while !flow.host_eof && socket.can_send() {
				let free_space = socket.send_capacity() - socket.send_queue();
				let read_size = free_space.min(buf.len());
				if read_size == 0 {
					break;
				}

				match host.read(&mut buf[0..read_size]) {
					Ok(0) => {
						flow.host_eof = true;
						socket.close();
						busy = true;
					},
					Ok(bytes_read) => {
						let _ = socket.send_slice(&buf[0..bytes_read]);
						busy = true;
					},
					Err(e) if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::Interrupted => break,
					Err(_) => {
						socket.abort();
						break;
					}
				}
			}
		}

		for key in finished_flows {
			if let Some(flow) = self.tcp_flows.remove(&key) {
				self.sockets.remove(flow.handle);
			}
		}

		busy
	}


	// UDP NAT

	fn handle_guest_udp(&mut self, src_mac: EthernetAddress, src_ip: Ipv4Addr, dst_ip: Ipv4Addr, src_port: u16, dst_port: u16, payload: Vec<u8>) {
		if dst_port == DHCP_SERVER_PORT {
			self.handle_dhcp(src_mac, &payload);
			return;
		}

		if dst_port == DNS_PORT {
			self.handle_dns(src_ip, src_port, dst_ip, &payload);
			return;
		}

		if src_ip.is_unspecified() || dst_ip.is_broadcast() || dst_ip.is_multicast() || dst_ip == SUBNET_BROADCAST_IP {
			return;
		}

		let host_ip = match self.host_target_for(dst_ip) {
			HostTarget::Ip(ip) => ip,
			HostTarget::Name(name) => {
				match self.resolved_targets.get(&name) {
					Some(ip) => *ip,
					None => {
						// Drop the datagram until the name is resolved; the guest will retry.
						if self.resolving_targets.insert(name.clone()) {
							let events_tx = self.events_tx.clone();
							std::thread::spawn(move || {
								let ip = resolve_ipv4s(&name, 0).into_iter().next();

								let _ = events_tx.send(HostEvent::TargetResolved(name, ip));
							});
						}

						return;
					}
				}
			}
		};
		let host_addr = SocketAddrV4::new(host_ip, dst_port);

		let flow = match self.udp_flows.entry(src_port) {
			Entry::Occupied(entry) => entry.into_mut(),
			Entry::Vacant(entry) => {
				let socket = match UdpSocket::bind("0.0.0.0:0") {
					Ok(socket) => socket,
					Err(_) => return
				};
				if socket.set_nonblocking(true).is_err() {
					return;
				}

				entry.insert(UdpFlow {
					socket: socket,
					guest_ip: src_ip,
					translated: HashMap::new(),
					last_used: std::time::Instant::now()
				})
			}
		};

		flow.guest_ip = src_ip;
		flow.last_used = std::time::Instant::now();

		if host_ip != dst_ip {
			flow.translated.insert(host_addr, dst_ip);
		}

		let _ = flow.socket.send_to(&payload, host_addr);
	}

	fn pump_udp(&mut self) -> bool {
		let now = std::time::Instant::now();
		let mut buf = vec![0u8; 0xffff];
		let mut replies: Vec<(Ipv4Addr, u16, Ipv4Addr, u16, Vec<u8>)> = vec![];

		self.udp_flows.retain(|guest_port, flow| {
			while let Ok((bytes_read, from)) = flow.socket.recv_from(&mut buf) {
				if let SocketAddr::V4(from) = from {
					let src_ip = match flow.translated.get(&from) {
						Some(guest_dst_ip) => *guest_dst_ip,
						None if from.ip().is_loopback() => GATEWAY_IP,
						None => *from.ip()
					};

					replies.push((src_ip, from.port(), flow.guest_ip, *guest_port, buf[0..bytes_read].to_vec()));
				}

				flow.last_used = now;
			}

			now.duration_since(flow.last_used) < UDP_FLOW_TIMEOUT
		});

		let busy = !replies.is_empty();

		if let Some(guest_mac) = self.guest_mac {
			for (src_ip, src_port, dst_ip, dst_port, payload) in replies {
				self.send_udp_to_guest(guest_mac, src_ip, src_port, dst_ip, dst_port, &payload);
			}
		}

		busy
	}

	// DHCP handling

	fn handle_dhcp(&mut self, src_mac: EthernetAddress, request: &[u8]) {
		// BOOTREQUEST with an ethernet hardware address and the DHCP magic cookie
		if request.len() < 240 || request[0] != 1 || request[1] != 1 || request[2] != 6 || request[236..240] != DHCP_MAGIC_COOKIE {
			return;
		}

		let mut message_type = 0u8;
		let mut requested_ip: Option<Ipv4Addr> = None;

		let mut i = 240;
		while i < request.len() {
			let code = request[i];
			if code == 255 {
				break;
			} else if code == 0 {
				i += 1;
				continue;
			} else if i + 1 >= request.len() {
				break;
			}

			let start = i + 2;
			let end = start + request[i + 1] as usize;
			if end > request.len() {
				break;
			}

			match (code, end - start) {
				(53, 1) => message_type = request[start],
				(50, 4) => requested_ip = Some(Ipv4Addr::new(request[start], request[start + 1], request[start + 2], request[start + 3])),
				_ => { }
			};

			i = end;
		}

		let client_ip = Ipv4Addr::new(request[12], request[13], request[14], request[15]);

		const DHCPDISCOVER: u8 = 1;
		const DHCPOFFER: u8 = 2;
		const DHCPREQUEST: u8 = 3;
		const DHCPACK: u8 = 5;
		const DHCPNAK: u8 = 6;
		const DHCPINFORM: u8 = 8;

		let reply_type = match message_type {
			DHCPDISCOVER => DHCPOFFER,
			DHCPREQUEST => {
				let wanted_ip = requested_ip.unwrap_or(client_ip);

				if wanted_ip == GUEST_IP || wanted_ip.is_unspecified() { DHCPACK } else { DHCPNAK }
			},
			DHCPINFORM => DHCPACK,
			_ => return
		};

		let mut reply = vec![0u8; 240];
		reply[0] = 2; // BOOTREPLY
		reply[1] = 1;
		reply[2] = 6;
		reply[4..8].copy_from_slice(&request[4..8]); // xid
		reply[10..12].copy_from_slice(&request[10..12]); // flags
		if message_type == DHCPINFORM {
			reply[12..16].copy_from_slice(&client_ip.octets());
		} else if reply_type != DHCPNAK {
			reply[16..20].copy_from_slice(&GUEST_IP.octets());
		}
		reply[20..24].copy_from_slice(&GATEWAY_IP.octets());
		reply[28..44].copy_from_slice(&request[28..44]); // chaddr
		reply[236..240].copy_from_slice(&DHCP_MAGIC_COOKIE);

		let mut add_option = |code: u8, data: &[u8]| {
			reply.push(code);
			reply.push(data.len() as u8);
			reply.extend_from_slice(data);
		};

		add_option(53, &[reply_type]);
		add_option(54, &GATEWAY_IP.octets());
		if reply_type != DHCPNAK {
			if message_type != DHCPINFORM {
				add_option(51, &DHCP_LEASE_SECONDS.to_be_bytes());
				add_option(58, &(DHCP_LEASE_SECONDS / 2).to_be_bytes());
				add_option(59, &(DHCP_LEASE_SECONDS / 8 * 7).to_be_bytes());
			}
			add_option(1, &NETMASK.octets());
			add_option(3, &GATEWAY_IP.octets());
			add_option(6, &GATEWAY_IP.octets());
			add_option(28, &SUBNET_BROADCAST_IP.octets());
		}
		reply.push(255);

		if reply.len() < 300 {
			reply.resize(300, 0);
		}

		let client_mac = EthernetAddress::from_bytes(&request[28..34]);
		let dst_mac = if client_mac.is_unicast() { client_mac } else { src_mac };

		if reply_type == DHCPACK && message_type == DHCPREQUEST {
			(self.log)(format!("Ethernet: DHCP assigned {} to {}\n", GUEST_IP, dst_mac));
		}

		self.send_udp_to_guest(dst_mac, GATEWAY_IP, DHCP_SERVER_PORT, Ipv4Addr::BROADCAST, DHCP_CLIENT_PORT, &reply);
	}

	// DNS handling

	fn find_dns_override(&self, name: &str) -> Option<String> {
		self.rules.dns_overrides
			.iter()
			.filter(|(domain, _)| name == domain || name.ends_with(&(".".to_owned() + domain)))
			.max_by_key(|(domain, _)| domain.len())
			.map(|(_, target)| target.clone())
	}

	fn handle_dns(&mut self, client_ip: Ipv4Addr, client_port: u16, server_ip: Ipv4Addr, request: &[u8]) {
		let query = match parse_dns_query(request, client_ip, client_port, server_ip) {
			Some(query) => query,
			None => return
		};

		let dns_override = self.find_dns_override(&query.name);
		if let Some(target) = &dns_override {
			(self.log)(format!("Ethernet: DNS override {} -> {}\n", query.name, target));
		}

		if query.qtype != 1 && query.qtype != 255 {
			self.send_dns_answer(query, Some(vec![]));
			return;
		}

		if let Some(ip) = dns_override.as_ref().and_then(|target| target.parse::<Ipv4Addr>().ok()) {
			self.send_dns_answer(query, Some(vec![ip]));
			return;
		}

		let lookup_name = dns_override.unwrap_or(query.name.clone());
		let events_tx = self.events_tx.clone();
		std::thread::spawn(move || {
			let addrs = (lookup_name.as_str(), 0)
				.to_socket_addrs()
				.ok()
				.map(|addrs| {
					addrs
						.filter_map(|addr| match addr {
							SocketAddr::V4(addr) => Some(*addr.ip()),
							_ => None
						})
						.collect::<Vec<Ipv4Addr>>()
				});

			let _ = events_tx.send(HostEvent::DnsAnswer(query, addrs));
		});
	}

	fn send_dns_answer(&mut self, query: DnsQuery, addrs: Option<Vec<Ipv4Addr>>) {
		let guest_mac = match self.guest_mac {
			Some(guest_mac) => guest_mac,
			None => return
		};

		let addrs = addrs.as_ref();
		let rcode: u16 = if addrs.is_some() { 0 } else { 3 }; // NOERROR or NXDOMAIN
		let answer_count = addrs.map(|addrs| addrs.len()).unwrap_or(0) as u16;

		let mut response: Vec<u8> = vec![];
		response.extend_from_slice(&query.id.to_be_bytes());

		response.extend_from_slice(&(0x8000 | (query.flags & 0x7900) | 0x0080 | rcode).to_be_bytes());
		response.extend_from_slice(&1u16.to_be_bytes());
		response.extend_from_slice(&answer_count.to_be_bytes());
		response.extend_from_slice(&0u16.to_be_bytes());
		response.extend_from_slice(&0u16.to_be_bytes());
		response.extend_from_slice(&query.question);

		for addr in addrs.into_iter().flatten() {
			response.extend_from_slice(&[0xc0, 0x0c]); // name points to the question
			response.extend_from_slice(&1u16.to_be_bytes()); // A
			response.extend_from_slice(&1u16.to_be_bytes()); // IN
			response.extend_from_slice(&DNS_TTL.to_be_bytes());
			response.extend_from_slice(&4u16.to_be_bytes());
			response.extend_from_slice(&addr.octets());
		}

		self.send_udp_to_guest(guest_mac, query.server_ip, DNS_PORT, query.client_ip, query.client_port, &response);
	}

	fn handle_host_events(&mut self) -> bool {
		let mut busy = false;

		while let Ok(event) = self.events_rx.try_recv() {
			busy = true;

			match event {
				HostEvent::Connected(key, host_description, result) => {
					if let Some(flow) = self.tcp_flows.get_mut(&key) {
						let socket = self.sockets.get_mut::<tcp::Socket>(flow.handle);

						match result {
							Ok(stream) => {
								if stream.set_nonblocking(true).is_ok() {
									let _ = stream.set_nodelay(true);
									flow.host = Some(stream);
									socket.pause_synack(false);
								} else {
									socket.abort();
								}
							},
							Err(e) => {
								(self.log)(format!("Ethernet: couldn't connect to {}: {}\n", host_description, e));
								socket.abort();
							}
						}
					}
				},
				HostEvent::DnsAnswer(query, addrs) => {
					if addrs.is_none() {
						(self.log)(format!("Ethernet: DNS couldn't resolve {}\n", query.name));
					}

					self.send_dns_answer(query, addrs);
				},
				HostEvent::TargetResolved(name, ip) => {
					self.resolving_targets.remove(&name);

					match ip {
						Some(ip) => {
							self.resolved_targets.insert(name, ip);
						},
						None => {
							(self.log)(format!("Ethernet: couldn't resolve IP override target {}\n", name));
						}
					}
				}
			}
		}

		busy
	}
}

fn parse_dns_query(request: &[u8], client_ip: Ipv4Addr, client_port: u16, server_ip: Ipv4Addr) -> Option<DnsQuery> {
	if request.len() < 12 {
		return None;
	}

	let id = u16::from_be_bytes([request[0], request[1]]);
	let flags = u16::from_be_bytes([request[2], request[3]]);
	let question_count = u16::from_be_bytes([request[4], request[5]]);

	if (flags & 0x8000) != 0 || (flags & 0x7800) != 0 || question_count != 1 {
		return None;
	}

	let mut labels: Vec<String> = vec![];
	let mut pos = 12;
	loop {
		let label_len = *request.get(pos)? as usize;
		pos += 1;

		if label_len == 0 {
			break;
		} else if (label_len & 0xc0) != 0 {
			return None;
		}

		labels.push(String::from_utf8_lossy(request.get(pos..pos + label_len)?).to_lowercase());
		pos += label_len;
	}

	let qtype_bytes = request.get(pos..pos + 2)?;
	let qtype = u16::from_be_bytes([qtype_bytes[0], qtype_bytes[1]]);
	pos += 4;
	if pos > request.len() {
		return None;
	}

	Some(DnsQuery {
		id: id,
		flags: flags,
		name: labels.join("."),
		qtype: qtype,
		question: request[12..pos].to_vec(),
		client_ip: client_ip,
		client_port: client_port,
		server_ip: server_ip
	})
}
