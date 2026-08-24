#[cfg(test)]
mod tests {
    use super::*;
    use crate::ActivityQuery;
    use easydeploymesh_core::{PxeConfig, PxeMode};

    fn valid_config() -> (tempfile::TempDir, PxeConfig) {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("undionly.kpxe"), b"bios").unwrap();
        std::fs::write(root.path().join("ipxe.efi"), b"uefi").unwrap();
        let config = PxeConfig {
            mode: PxeMode::StandaloneDhcp,
            bind_address: "192.168.10.1".into(),
            subnet_mask: "255.255.255.0".into(),
            pool_start: "192.168.10.100".into(),
            pool_end: "192.168.10.200".into(),
            lease_seconds: 28_800,
            gateway: None,
            dns_servers: vec![],
            tftp_root: root.path().to_string_lossy().into_owned(),
            bios_boot_file: "undionly.kpxe".into(),
            uefi_x64_boot_file: "ipxe.efi".into(),
        };
        (root, config)
    }

    fn write_fat_image_fixture(path: &Path, files: &[(&str, &[u8])]) {
        const PARTITION_START_SECTORS: u32 = 2_048;
        const PARTITION_SECTORS: u32 = 32_768;
        let partition_start = PARTITION_START_SECTORS as usize * 512;
        let mut image = vec![0_u8; (PARTITION_START_SECTORS + PARTITION_SECTORS) as usize * 512];
        image[446 + 4] = 0x0c;
        image[446 + 8..446 + 12].copy_from_slice(&PARTITION_START_SECTORS.to_le_bytes());
        image[446 + 12..446 + 16].copy_from_slice(&PARTITION_SECTORS.to_le_bytes());
        image[510..512].copy_from_slice(b"\x55\xAA");

        fatfs::format_volume(
            std::io::Cursor::new(&mut image[partition_start..]),
            fatfs::FormatVolumeOptions::new(),
        )
        .unwrap();
        {
            let filesystem = fatfs::FileSystem::new(
                std::io::Cursor::new(&mut image[partition_start..]),
                fatfs::FsOptions::new(),
            )
            .unwrap();
            let root = filesystem.root_dir();
            root.create_dir("WEPE").unwrap();
            for (relative, contents) in files {
                let mut file = if let Some((directory, name)) = relative.split_once('/') {
                    root.open_dir(directory).unwrap().create_file(name).unwrap()
                } else {
                    root.create_file(relative).unwrap()
                };
                file.write_all(contents).unwrap();
            }
        }
        std::fs::write(path, image).unwrap();
    }

    #[test]
    fn standalone_configuration_accepts_an_isolated_slash_24_pool() {
        let (_root, config) = valid_config();
        assert!(validate_pxe_config(&config).is_ok());
    }

    #[test]
    fn pool_cannot_include_the_server_address() {
        let (_root, mut config) = valid_config();
        config.pool_start = "192.168.10.1".into();
        assert_eq!(
            validate_pxe_config(&config).unwrap_err().to_string(),
            "DHCP pool must not contain the server address"
        );
    }

    #[test]
    fn boot_file_must_be_relative_and_cannot_escape_the_tftp_root() {
        let (_root, mut config) = valid_config();
        config.bios_boot_file = "../secret".into();
        assert_eq!(
            validate_pxe_config(&config).unwrap_err().to_string(),
            "boot file must be a safe relative path: ../secret"
        );
    }

    fn discover(architecture: u16) -> Vec<u8> {
        let mut packet = vec![0u8; 240];
        packet[0] = 1;
        packet[1] = 1;
        packet[2] = 6;
        packet[4..8].copy_from_slice(&0x12345678u32.to_be_bytes());
        packet[28..34].copy_from_slice(&[0x00, 0x0c, 0x29, 0x8b, 0xe0, 0xed]);
        packet[236..240].copy_from_slice(DHCP_MAGIC_COOKIE);
        push_option(&mut packet, 53, &[1]);
        push_option(&mut packet, 93, &architecture.to_be_bytes());
        packet.push(255);
        packet
    }

    #[tokio::test]
    async fn discover_receives_an_offer_with_the_bios_boot_file() {
        let (_root, config) = valid_config();
        let leases = Arc::new(RwLock::new(HashMap::new()));
        let (reply, _, mac, _, offered) = process_dhcp(
            &discover(0),
            "0.0.0.0:68".parse().unwrap(),
            &config,
            &leases,
        )
        .await
        .unwrap();
        assert_eq!(mac, "00:0C:29:8B:E0:ED");
        assert_eq!(offered, Some("192.168.10.100".parse().unwrap()));
        assert_eq!(dhcp_option(&reply, 53), Some(&[2][..]));
        assert_eq!(dhcp_option(&reply, 67), Some(&b"undionly.kpxe"[..]));
    }

    #[tokio::test]
    async fn synthetic_conflict_probe_is_not_treated_as_a_client() {
        let (_root, config) = valid_config();
        let leases = Arc::new(RwLock::new(HashMap::new()));
        let mut request = discover(0);
        request.pop();
        push_option(&mut request, 60, DHCP_PROBE_VENDOR_CLASS);
        request.push(255);

        assert!(
            process_dhcp(&request, "0.0.0.0:68".parse().unwrap(), &config, &leases)
                .await
                .is_none()
        );
        assert!(leases.read().await.is_empty());
    }

    #[tokio::test]
    async fn discovered_clients_prunes_stale_entries_but_keeps_boot_progress() {
        let service = PxeService::new();
        record_client(
            &service.clients,
            "02:00:00:00:00:01",
            Some("192.168.10.100".into()),
            Architecture::X86_64,
            PxeClientStage::Discovered,
        )
        .await;
        record_client(
            &service.clients,
            "02:00:00:00:00:02",
            Some("192.168.10.101".into()),
            Architecture::X86_64,
            PxeClientStage::WaitingForAgent,
        )
        .await;
        {
            let mut clients = service.clients.write().await;
            let stale_at = Utc::now() - PXE_DISCOVERY_TTL - Duration::seconds(1);
            clients.get_mut("02:00:00:00:00:01").unwrap().last_seen_at = stale_at;
            clients.get_mut("02:00:00:00:00:02").unwrap().last_seen_at = stale_at;
        }

        let clients = service.discovered_clients().await;
        assert_eq!(clients.len(), 1);
        assert_eq!(clients[0].mac_address, "02:00:00:00:00:02");
    }

    #[tokio::test]
    async fn uefi_x64_discover_selects_the_efi_boot_file() {
        let (_root, config) = valid_config();
        let leases = Arc::new(RwLock::new(HashMap::new()));
        let (reply, ..) = process_dhcp(
            &discover(7),
            "0.0.0.0:68".parse().unwrap(),
            &config,
            &leases,
        )
        .await
        .unwrap();
        assert_eq!(dhcp_option(&reply, 67), Some(&b"ipxe.efi"[..]));
    }

    #[tokio::test]
    async fn ipxe_client_is_directed_to_the_second_stage_script() {
        let (_root, config) = valid_config();
        let leases = Arc::new(RwLock::new(HashMap::new()));
        let mut request = discover(0);
        request.pop();
        push_option(&mut request, 77, b"iPXE");
        request.push(255);
        let (reply, ..) = process_dhcp(&request, "0.0.0.0:68".parse().unwrap(), &config, &leases)
            .await
            .unwrap();
        assert_eq!(dhcp_option(&reply, 67), Some(&b"boot.ipxe"[..]));
    }

    #[tokio::test]
    async fn normalized_wepe_uefi_boot_cannot_reenter_the_vendor_bcd_chain() {
        let source = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(source.path().join("wepe")).unwrap();
        std::fs::create_dir_all(source.path().join("efi/microsoft/boot")).unwrap();
        std::fs::write(source.path().join("wepe/WEPE64.WIM"), b"winpe").unwrap();
        std::fs::write(source.path().join("wepe/WEPE.SDI"), b"sdi").unwrap();
        let vendor_bcd = r"\WEPE\B64"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>();
        std::fs::write(source.path().join("efi/microsoft/boot/BCD"), vendor_bcd).unwrap();
        std::fs::write(source.path().join("bootmgr"), b"bootmgr").unwrap();
        let parent = tempfile::tempdir().unwrap();
        let package =
            build_winpe_package(source.path(), &parent.path().join("managed"), None).unwrap();
        let (_root, mut config) = valid_config();
        config.tftp_root = package.root.clone();
        config.bios_boot_file = package.bios_boot_file.clone();
        config.uefi_x64_boot_file = package.uefi_x64_boot_file.clone();
        let leases = Arc::new(RwLock::new(HashMap::new()));

        let (uefi_reply, ..) = process_dhcp(
            &discover(7),
            "0.0.0.0:68".parse().unwrap(),
            &config,
            &leases,
        )
        .await
        .unwrap();
        assert_eq!(dhcp_option(&uefi_reply, 67), Some(&b"ipxe.efi"[..]));

        let mut ipxe_request = discover(7);
        ipxe_request.pop();
        push_option(&mut ipxe_request, 77, b"iPXE");
        ipxe_request.push(255);
        let (ipxe_reply, ..) = process_dhcp(
            &ipxe_request,
            "0.0.0.0:68".parse().unwrap(),
            &config,
            &leases,
        )
        .await
        .unwrap();
        assert_eq!(dhcp_option(&ipxe_reply, 67), Some(&b"boot.ipxe"[..]));

        let root = Path::new(&package.root);
        assert_eq!(
            std::fs::read(root.join("ipxe.efi")).unwrap(),
            EMBEDDED_IPXE_EFI
        );
        assert!(!root.join("efi/microsoft/boot/BCD").exists());
        assert!(!root.join("efi/boot/bootx64.efi").exists());
        assert!(
            !std::fs::read(root.join("boot/BCD"))
                .unwrap()
                .windows(r"\WEPE\B64".len())
                .any(|window| window.eq_ignore_ascii_case(r"\WEPE\B64".as_bytes()))
        );
    }

    #[tokio::test]
    async fn unsupported_architecture_does_not_receive_a_boot_file() {
        let (_root, config) = valid_config();
        let leases = Arc::new(RwLock::new(HashMap::new()));
        let (reply, ..) = process_dhcp(
            &discover(11),
            "0.0.0.0:68".parse().unwrap(),
            &config,
            &leases,
        )
        .await
        .unwrap();
        assert_eq!(dhcp_option(&reply, 67), None);
    }

    #[tokio::test]
    async fn request_for_an_address_outside_the_pool_receives_nak() {
        let (_root, config) = valid_config();
        let leases = Arc::new(RwLock::new(HashMap::new()));
        let mut request = discover(0);
        request[242] = 3;
        request.pop();
        push_option(&mut request, 50, &[192, 168, 10, 250]);
        request.push(255);
        let (reply, ..) = process_dhcp(&request, "0.0.0.0:68".parse().unwrap(), &config, &leases)
            .await
            .unwrap();
        assert_eq!(dhcp_option(&reply, 53), Some(&[6][..]));
        assert_eq!(&reply[16..20], &[0, 0, 0, 0]);
    }

    #[tokio::test]
    async fn completed_winpe_download_is_not_demoted_by_followup_dhcp_request() {
        let (_root, config) = valid_config();
        let leases = Arc::new(RwLock::new(HashMap::new()));
        let clients = Arc::new(RwLock::new(HashMap::new()));

        let (_, _, mac, architecture, offered) = process_dhcp(
            &discover(0),
            "0.0.0.0:68".parse().unwrap(),
            &config,
            &leases,
        )
        .await
        .unwrap();
        let offered = offered.unwrap();
        record_client(
            &clients,
            &mac,
            Some(offered.to_string()),
            architecture,
            PxeClientStage::Discovered,
        )
        .await;
        update_client_stage_by_ip(
            &clients,
            IpAddr::V4(offered),
            PxeClientStage::WaitingForAgent,
        )
        .await;

        let previous_last_seen = Utc::now() - Duration::minutes(1);
        {
            let mut clients = clients.write().await;
            let client = clients.get_mut(&mac).unwrap();
            client.architecture = Architecture::Unknown;
            client.last_seen_at = previous_last_seen;
        }

        let mut request = discover(7);
        request[242] = 3;
        request.pop();
        push_option(&mut request, 50, &[192, 168, 10, 101]);
        request.push(255);
        let (_, _, request_mac, architecture, observed_ip) =
            process_dhcp(&request, "0.0.0.0:68".parse().unwrap(), &config, &leases)
                .await
                .unwrap();
        record_client(
            &clients,
            &request_mac,
            observed_ip.map(|ip| ip.to_string()),
            architecture,
            PxeClientStage::Discovered,
        )
        .await;

        let clients = clients.read().await;
        let client = clients.get(&mac).unwrap();
        assert_eq!(client.stage, PxeClientStage::WaitingForAgent);
        assert_eq!(client.ip_address.as_deref(), Some("192.168.10.101"));
        assert_eq!(client.architecture, Architecture::X86_64);
        assert!(client.last_seen_at > previous_last_seen);
    }

    #[tokio::test]
    async fn tftp_rrq_transfers_a_file_and_rejects_path_traversal() {
        assert_eq!(
            tftp_request_filename(b"\0\x01/boot.bin\0octet\0"),
            Some("/boot.bin".into())
        );
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("boot.bin"), b"EasyDeployMesh").unwrap();
        let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let address = server.local_addr().unwrap();
        let clients = Arc::new(RwLock::new(HashMap::new()));
        record_client(
            &clients,
            "00:0C:29:8B:E0:ED",
            Some("127.0.0.1".into()),
            Architecture::X86_64,
            PxeClientStage::Discovered,
        )
        .await;
        let (_stop_tx, stop_rx) = oneshot::channel();
        let task = tokio::spawn(run_tftp(
            server,
            root.path().to_path_buf(),
            Arc::clone(&clients),
            None,
            stop_rx,
        ));
        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        client
            .send_to(b"\0\x01/boot.bin\0octet\0", address)
            .await
            .unwrap();
        let mut response = [0u8; 64];
        let (size, transfer) = client.recv_from(&mut response).await.unwrap();
        assert_eq!(&response[..4], &[0, 3, 0, 1]);
        assert_eq!(&response[4..size], b"EasyDeployMesh");
        client.send_to(&[0, 4, 0, 1], transfer).await.unwrap();
        tokio::task::yield_now().await;
        assert_eq!(
            clients.read().await.get("00:0C:29:8B:E0:ED").unwrap().stage,
            PxeClientStage::Downloading
        );

        client
            .send_to(b"\0\x01../secret\0octet\0", address)
            .await
            .unwrap();
        let (size, _) = client.recv_from(&mut response).await.unwrap();
        assert_eq!(&response[..4], &[0, 5, 0, 2]);
        assert!(size > 4);

        client
            .send_to(b"\0\x02upload.bin\0octet\0", address)
            .await
            .unwrap();
        let (_, _) = client.recv_from(&mut response).await.unwrap();
        assert_eq!(&response[..4], &[0, 5, 0, 2]);
        task.abort();
    }

    #[tokio::test]
    async fn tftp_transfers_the_complete_embedded_uefi_ipxe_loader() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("ipxe.efi"), EMBEDDED_IPXE_EFI).unwrap();
        let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let address = server.local_addr().unwrap();
        let clients = Arc::new(RwLock::new(HashMap::new()));
        let (_stop_tx, stop_rx) = oneshot::channel();
        let task = tokio::spawn(run_tftp(
            server,
            root.path().to_path_buf(),
            clients,
            None,
            stop_rx,
        ));
        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        client
            .send_to(b"\0\x01ipxe.efi\0octet\0blksize\01468\0tsize\00\0", address)
            .await
            .unwrap();

        let mut packet = [0u8; 1472];
        let (size, transfer) = client.recv_from(&mut packet).await.unwrap();
        assert_eq!(&packet[..2], &[0, 6]);
        let options = String::from_utf8_lossy(&packet[2..size]);
        assert!(options.contains("blksize\01468\0"));
        assert!(options.contains(&format!("tsize\0{}\0", EMBEDDED_IPXE_EFI.len())));
        client.send_to(&[0, 4, 0, 0], transfer).await.unwrap();

        let mut downloaded = Vec::with_capacity(EMBEDDED_IPXE_EFI.len());
        let mut expected_block = 1u16;
        loop {
            let size = client.recv(&mut packet).await.unwrap();
            assert_eq!(&packet[..2], &[0, 3]);
            assert_eq!(u16::from_be_bytes([packet[2], packet[3]]), expected_block);
            downloaded.extend_from_slice(&packet[4..size]);
            client
                .send_to(&[0, 4, packet[2], packet[3]], transfer)
                .await
                .unwrap();
            expected_block = expected_block.wrapping_add(1);
            if size < packet.len() {
                break;
            }
        }

        assert_eq!(downloaded, EMBEDDED_IPXE_EFI);
        task.abort();
    }

    async fn assert_tftp_ack_failure(
        root: &Path,
        request: &'static [u8],
        expected_block: u16,
        acknowledge_first_block: bool,
        clients: Arc<RwLock<HashMap<String, PxeDiscoveredClient>>>,
    ) -> std::io::Result<()> {
        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let peer = client.local_addr().unwrap();
        let root = root.to_path_buf();
        let transfer =
            tokio::spawn(async move { serve_tftp_request(request, peer, &root, &clients).await });
        let mut packet = [0u8; 2048];
        let mut acknowledged_first_block = false;
        for _ in 0..(4 + usize::from(acknowledge_first_block)) {
            let (_size, server) = client.recv_from(&mut packet).await.unwrap();
            let opcode = u16::from_be_bytes([packet[0], packet[1]]);
            let block = if opcode == 6 {
                0
            } else {
                assert_eq!(opcode, 3);
                u16::from_be_bytes([packet[2], packet[3]])
            };
            if acknowledge_first_block && block == 1 && !acknowledged_first_block {
                client.send_to(&[0, 4, 0, 1], server).await.unwrap();
                acknowledged_first_block = true;
                continue;
            }
            assert_eq!(block, expected_block);
            let wrong_block = expected_block.wrapping_add(1).to_be_bytes();
            client
                .send_to(&[0, 4, wrong_block[0], wrong_block[1]], server)
                .await
                .unwrap();
            if transfer.is_finished() {
                break;
            }
        }
        transfer.await.unwrap()
    }

    async fn wait_for_activity_count(activities: &ActivityRepository, count: usize) {
        timeout(TokioDuration::from_secs(1), async {
            loop {
                let current = activities
                    .query(&ActivityQuery {
                        limit: 10,
                        ..Default::default()
                    })
                    .unwrap()
                    .len();
                if current >= count {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn tftp_reports_oack_ack_exhaustion_as_failure() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("boot.bin"), b"small").unwrap();
        let result = assert_tftp_ack_failure(
            root.path(),
            b"\0\x01boot.bin\0octet\0blksize\01468\0",
            0,
            false,
            Arc::new(RwLock::new(HashMap::new())),
        )
        .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("OACK"));
    }

    #[tokio::test]
    async fn tftp_reports_data_ack_exhaustion_as_failure() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("boot.bin"), b"small").unwrap();
        let result = assert_tftp_ack_failure(
            root.path(),
            b"\0\x01boot.bin\0octet\0",
            1,
            false,
            Arc::new(RwLock::new(HashMap::new())),
        )
        .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("block 1"));
    }

    #[tokio::test]
    async fn boot_wim_final_empty_block_requires_an_ack_before_waiting_for_agent() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("boot")).unwrap();
        std::fs::write(root.path().join("boot/boot.wim"), vec![0u8; 512]).unwrap();
        let clients = Arc::new(RwLock::new(HashMap::new()));
        record_client(
            &clients,
            "00:0C:29:8B:E0:ED",
            Some("127.0.0.1".into()),
            Architecture::X86_64,
            PxeClientStage::Discovered,
        )
        .await;
        let result = assert_tftp_ack_failure(
            root.path(),
            b"\0\x01boot/boot.wim\0octet\0",
            2,
            true,
            Arc::clone(&clients),
        )
        .await;
        assert!(result.is_err());
        assert_eq!(
            clients.read().await.get("00:0C:29:8B:E0:ED").unwrap().stage,
            PxeClientStage::Downloading
        );
    }

    #[tokio::test]
    async fn tftp_failure_records_only_a_sanitized_failure_event() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("boot.bin"), b"small").unwrap();
        let activity_dir = tempfile::tempdir().unwrap();
        let activities = Arc::new(
            ActivityRepository::open(activity_dir.path().join("activities.json")).unwrap(),
        );
        let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let address = server.local_addr().unwrap();
        let (_stop_tx, stop_rx) = oneshot::channel();
        let task = tokio::spawn(run_tftp(
            server,
            root.path().to_path_buf(),
            Arc::new(RwLock::new(HashMap::new())),
            Some(Arc::clone(&activities)),
            stop_rx,
        ));
        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        client
            .send_to(b"\0\x01boot.bin\0octet\0", address)
            .await
            .unwrap();
        let mut packet = [0u8; 64];
        for _ in 0..4 {
            let (_, transfer) = client.recv_from(&mut packet).await.unwrap();
            client.send_to(&[0, 4, 0, 2], transfer).await.unwrap();
        }
        wait_for_activity_count(&activities, 1).await;

        let events = activities
            .query(&ActivityQuery {
                limit: 10,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "tftp_failed");
        let message = events[0].raw_message.as_deref().unwrap();
        assert!(message.contains("boot.bin"));
        assert!(message.contains("127.0.0.1"));
        assert!(message.contains("block 1"));
        assert!(!message.contains(&root.path().to_string_lossy().to_string()));
        task.abort();
    }

    #[tokio::test]
    async fn complete_boot_wim_records_one_success_and_waits_for_agent() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("boot")).unwrap();
        std::fs::write(root.path().join("boot/boot.wim"), vec![0u8; 512]).unwrap();
        let activity_dir = tempfile::tempdir().unwrap();
        let activities = Arc::new(
            ActivityRepository::open(activity_dir.path().join("activities.json")).unwrap(),
        );
        let clients = Arc::new(RwLock::new(HashMap::new()));
        record_client(
            &clients,
            "00:0C:29:8B:E0:ED",
            Some("127.0.0.1".into()),
            Architecture::X86_64,
            PxeClientStage::Discovered,
        )
        .await;
        let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let address = server.local_addr().unwrap();
        let (_stop_tx, stop_rx) = oneshot::channel();
        let task = tokio::spawn(run_tftp(
            server,
            root.path().to_path_buf(),
            Arc::clone(&clients),
            Some(Arc::clone(&activities)),
            stop_rx,
        ));
        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        client
            .send_to(b"\0\x01boot/boot.wim\0octet\0", address)
            .await
            .unwrap();
        let mut packet = [0u8; 516];
        for expected_block in [1u16, 2] {
            let (size, transfer) = client.recv_from(&mut packet).await.unwrap();
            assert_eq!(u16::from_be_bytes([packet[2], packet[3]]), expected_block);
            if expected_block == 2 {
                assert_eq!(size, 4);
            }
            client
                .send_to(&[0, 4, packet[2], packet[3]], transfer)
                .await
                .unwrap();
        }
        wait_for_activity_count(&activities, 1).await;

        assert_eq!(
            clients.read().await.get("00:0C:29:8B:E0:ED").unwrap().stage,
            PxeClientStage::WaitingForAgent
        );
        let events = activities
            .query(&ActivityQuery {
                limit: 10,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "boot_file_sent");
        task.abort();
    }

    #[tokio::test]
    async fn tftp_large_transfer_wraps_the_block_number() {
        let root = tempfile::tempdir().unwrap();
        let contents = vec![0x5a; 8 * 65_536 + 3];
        std::fs::write(root.path().join("large.bin"), &contents).unwrap();
        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let peer = client.local_addr().unwrap();
        let root_path = root.path().to_path_buf();
        let transfer = tokio::spawn(async move {
            serve_tftp_request(
                b"\0\x01large.bin\0octet\0blksize\08\0",
                peer,
                &root_path,
                &Arc::new(RwLock::new(HashMap::new())),
            )
            .await
        });
        let mut packet = [0u8; 12];
        let (size, server) = client.recv_from(&mut packet).await.unwrap();
        assert_eq!(&packet[..2], &[0, 6]);
        assert!(size > 2);
        client.send_to(&[0, 4, 0, 0], server).await.unwrap();
        let mut downloaded = Vec::with_capacity(contents.len());
        for sequence in 1..=65_537u32 {
            let size = client.recv(&mut packet).await.unwrap();
            let expected_block = sequence as u16;
            assert_eq!(u16::from_be_bytes([packet[2], packet[3]]), expected_block);
            downloaded.extend_from_slice(&packet[4..size]);
            client
                .send_to(&[0, 4, packet[2], packet[3]], server)
                .await
                .unwrap();
        }
        assert!(transfer.await.unwrap().is_ok());
        assert_eq!(downloaded, contents);
    }

    #[tokio::test]
    async fn leases_survive_service_reopen() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("leases.json");
        let service = PxeService::open(&path).unwrap();
        service.leases.write().await.insert(
            "00:0C:29:8B:E0:ED".into(),
            Lease {
                ip: "192.168.10.100".parse().unwrap(),
                expires_at: Utc::now() + Duration::hours(1),
            },
        );
        persist_leases(&path, &service.leases).await.unwrap();
        let reopened = PxeService::open(&path).unwrap();
        assert_eq!(
            reopened
                .leases
                .read()
                .await
                .get("00:0C:29:8B:E0:ED")
                .unwrap()
                .ip,
            "192.168.10.100".parse::<Ipv4Addr>().unwrap()
        );
    }

    #[test]
    fn importing_a_boot_package_is_independent_of_the_source_afterwards() {
        let source = tempfile::tempdir().unwrap();
        std::fs::write(source.path().join("undionly.kpxe"), b"bios").unwrap();
        std::fs::write(source.path().join("ipxe.efi"), b"uefi").unwrap();
        let parent = tempfile::tempdir().unwrap();
        let managed = parent.path().join("managed");
        let imported =
            BootPackage::import(source.path(), &managed, "undionly.kpxe", "ipxe.efi").unwrap();
        drop(source);
        assert_eq!(
            std::fs::read(Path::new(&imported.root).join("undionly.kpxe")).unwrap(),
            b"bios"
        );
    }

    #[test]
    fn boot_directory_import_injects_the_agent_before_replacing_the_managed_package() {
        let source = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(source.path().join("boot")).unwrap();
        std::fs::write(source.path().join("undionly.kpxe"), b"bios").unwrap();
        std::fs::write(source.path().join("ipxe.efi"), b"uefi").unwrap();
        std::fs::write(source.path().join("boot/boot.wim"), b"wim").unwrap();
        let agent = source.path().join("easydeploymesh-agent.exe");
        std::fs::write(&agent, b"agent").unwrap();

        let parent = tempfile::tempdir().unwrap();
        let managed = parent.path().join("managed");
        std::fs::create_dir_all(&managed).unwrap();
        std::fs::write(managed.join("existing-package.marker"), b"keep").unwrap();

        let error = BootPackage::import_with_agent(
            source.path(),
            &managed,
            "undionly.kpxe",
            "ipxe.efi",
            &agent,
        )
        .unwrap_err();

        assert_eq!(
            error.to_string(),
            "invalid PXE configuration: injecting EasyDeployMesh Agent into WinPE requires Windows DISM"
        );
        assert_eq!(
            std::fs::read(managed.join("existing-package.marker")).unwrap(),
            b"keep"
        );
    }

    #[test]
    fn legacy_agent_only_marker_does_not_skip_a_runtime_layout_upgrade() {
        let package = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(package.path().join("boot")).unwrap();
        let wim = package.path().join("boot/boot.wim");
        std::fs::write(&wim, b"wim").unwrap();
        let agent = package.path().join("easydeploymesh-agent.exe");
        std::fs::write(&agent, b"agent").unwrap();
        std::fs::write(
            package.path().join("boot/easydeploymesh-agent.sha256"),
            agent_binary_sha256(&agent).unwrap(),
        )
        .unwrap();

        let error = BootPackage::ensure_agent_runtime(&wim, &agent).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("injecting EasyDeployMesh Agent into WinPE requires Windows DISM")
        );
    }

    #[test]
    fn current_agent_and_runtime_markers_skip_reinjecting_winpe() {
        let package = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(package.path().join("boot")).unwrap();
        let wim = package.path().join("boot/boot.wim");
        std::fs::write(&wim, b"wim").unwrap();
        let agent = package.path().join("easydeploymesh-agent.exe");
        std::fs::write(&agent, b"agent").unwrap();
        std::fs::write(
            package.path().join("boot/easydeploymesh-agent.sha256"),
            agent_binary_sha256(&agent).unwrap(),
        )
        .unwrap();
        std::fs::write(
            package.path().join("boot/easydeploymesh-runtime.sha256"),
            winpe_runtime_sha256(&agent).unwrap(),
        )
        .unwrap();

        assert!(!BootPackage::ensure_agent_runtime(&wim, &agent).unwrap());
    }

    #[test]
    fn stale_agent_runtime_marker_is_not_advanced_when_reinjection_fails() {
        let package = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(package.path().join("boot")).unwrap();
        let wim = package.path().join("boot/boot.wim");
        std::fs::write(&wim, b"wim").unwrap();
        let agent = package.path().join("easydeploymesh-agent.exe");
        std::fs::write(&agent, b"agent").unwrap();
        let marker = package.path().join("boot/easydeploymesh-agent.sha256");
        std::fs::write(&marker, "00").unwrap();

        let error = BootPackage::ensure_agent_runtime(&wim, &agent).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("injecting EasyDeployMesh Agent into WinPE requires Windows DISM")
        );
        assert_eq!(std::fs::read_to_string(marker).unwrap(), "00");
    }

    #[test]
    fn same_version_marker_does_not_hide_a_changed_agent_binary() {
        let package = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(package.path().join("boot")).unwrap();
        let wim = package.path().join("boot/boot.wim");
        std::fs::write(&wim, b"wim").unwrap();
        let agent = package.path().join("easydeploymesh-agent.exe");
        std::fs::write(&agent, b"rebuilt-agent-bytes").unwrap();
        std::fs::write(
            package.path().join("boot/easydeploymesh-agent.version"),
            env!("CARGO_PKG_VERSION"),
        )
        .unwrap();

        let error = BootPackage::ensure_agent_runtime(&wim, &agent).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("injecting EasyDeployMesh Agent into WinPE requires Windows DISM")
        );
    }

    #[test]
    fn third_party_winpe_layout_is_normalized_for_tftp() {
        let source = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(source.path().join("BOOT")).unwrap();
        std::fs::create_dir_all(source.path().join("EFI/BOOT")).unwrap();
        std::fs::write(source.path().join("BOOT/custom-x64.wim"), b"wim").unwrap();
        std::fs::write(source.path().join("BOOT/BOOT.SDI"), b"sdi").unwrap();
        std::fs::write(source.path().join("EFI/BOOT/BOOTX64.EFI"), b"efi").unwrap();
        std::fs::write(source.path().join("bootmgr"), b"bootmgr").unwrap();
        let parent = tempfile::tempdir().unwrap();
        let package =
            build_winpe_package(source.path(), &parent.path().join("managed"), None).unwrap();
        assert_eq!(package.uefi_x64_boot_file, "ipxe.efi");
        assert_eq!(package.bios_boot_file, "undionly.kpxe");
        assert!(Path::new(&package.root).join("ipxe.efi").is_file());
        assert_ne!(
            std::fs::read(Path::new(&package.root).join("ipxe.efi")).unwrap(),
            b"efi"
        );
        assert_eq!(
            std::fs::read(Path::new(&package.root).join("boot/boot.wim")).unwrap(),
            b"wim"
        );
        assert_eq!(
            std::fs::read(Path::new(&package.root).join("boot/bootmgr")).unwrap(),
            b"bootmgr"
        );
    }

    #[test]
    fn standard_windows_media_prefers_sources_boot_wim_over_larger_install_wim() {
        let source = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(source.path().join("SoUrCeS")).unwrap();
        std::fs::create_dir_all(source.path().join("boot")).unwrap();
        std::fs::create_dir_all(source.path().join("efi/boot")).unwrap();
        std::fs::write(source.path().join("SoUrCeS/BoOt.WiM"), b"winpe").unwrap();
        std::fs::write(source.path().join("SoUrCeS/INSTALL.WIM"), vec![b'i'; 1024]).unwrap();
        std::fs::write(source.path().join("boot/boot.sdi"), b"sdi").unwrap();
        std::fs::write(source.path().join("efi/boot/bootx64.efi"), b"efi").unwrap();
        std::fs::write(source.path().join("bootmgr"), b"bootmgr").unwrap();
        let parent = tempfile::tempdir().unwrap();

        let package =
            build_winpe_package(source.path(), &parent.path().join("managed"), None).unwrap();

        assert_eq!(
            std::fs::read(Path::new(&package.root).join("boot/boot.wim")).unwrap(),
            b"winpe"
        );
    }

    #[test]
    fn existing_normalized_package_receives_the_current_bios_and_uefi_ipxe_loaders() {
        let package = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(package.path().join("boot")).unwrap();
        for (path, bytes) in [
            ("boot/BCD", br"\WEPE\B64".as_slice()),
            ("boot/boot.sdi", b"sdi"),
            ("boot/boot.wim", br"\Windows\System32\boot\winload.exe"),
            ("boot/bootmgr", b"bootmgr"),
            ("boot/wimboot", b"old-wimboot"),
        ] {
            std::fs::write(package.path().join(path), bytes).unwrap();
        }
        std::fs::write(
            package.path().join("boot.ipxe"),
            BROKEN_NAMED_MANAGED_IPXE_SCRIPT,
        )
        .unwrap();

        assert!(BootPackage::ensure_managed_network_boot(package.path()).unwrap());
        assert_eq!(
            std::fs::read(package.path().join("undionly.kpxe")).unwrap(),
            EMBEDDED_UNDIONLY
        );
        assert_eq!(
            std::fs::read(package.path().join("ipxe.efi")).unwrap(),
            EMBEDDED_IPXE_EFI
        );
        assert_eq!(
            std::fs::read(package.path().join("boot/wimboot")).unwrap(),
            EMBEDDED_WIMBOOT
        );
        let bcd = std::fs::read(package.path().join("boot/easydeploymesh.bcd")).unwrap();
        assert!(!output_contains_ascii_case_insensitive(&bcd, r"\WEPE\B64"));
        assert!(output_contains_ascii_case_insensitive(
            &bcd,
            r"\Boot\boot.wim"
        ));
        assert!(package.path().join(MANAGED_LAYOUT_MARKER).is_file());
        assert_eq!(
            std::fs::read(package.path().join("boot/BCD")).unwrap(),
            br"\WEPE\B64"
        );
        assert_eq!(
            std::fs::read_to_string(package.path().join("boot.ipxe")).unwrap(),
            MANAGED_IPXE_SCRIPT
        );
    }

    #[test]
    fn arbitrary_boot_directory_is_not_rewritten_as_a_managed_ipxe_package() {
        let package = tempfile::tempdir().unwrap();
        std::fs::write(package.path().join("boot.ipxe"), b"#!ipxe\necho custom\n").unwrap();

        assert!(!BootPackage::ensure_managed_network_boot(package.path()).unwrap());
        assert!(!package.path().join("ipxe.efi").exists());
    }

    #[test]
    fn managed_ipxe_script_names_each_initrd_for_bios_and_uefi() {
        let initrds = MANAGED_IPXE_SCRIPT
            .lines()
            .filter(|line| line.starts_with("initrd "))
            .collect::<Vec<_>>();
        assert_eq!(initrds.len(), 5);
        for line in initrds {
            let fields = line.split_ascii_whitespace().collect::<Vec<_>>();
            assert_eq!(fields.len(), 5, "invalid dual-mode initrd line: {line}");
            assert_eq!(fields[0], "initrd");
            assert_eq!(fields[1], "--name");
            assert_eq!(fields[2], fields[4]);
            assert!(!fields[2].contains('/'));
        }
        assert!(MANAGED_IPXE_SCRIPT.contains("initrd --name BCD boot/easydeploymesh.bcd BCD"));
        assert!(!MANAGED_IPXE_SCRIPT.contains("--name BCD boot/BCD BCD"));
    }

    #[test]
    fn package_from_the_broken_named_script_is_upgraded_even_with_a_layout_marker() {
        let package = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(package.path().join("boot")).unwrap();
        for (path, bytes) in [
            ("boot/BCD", br"\WEPE\B64".as_slice()),
            ("boot/boot.sdi", b"sdi"),
            ("boot/boot.wim", b"wim"),
            ("boot/bootmgr", b"bootmgr"),
            ("boot/wimboot", b"wimboot"),
        ] {
            std::fs::write(package.path().join(path), bytes).unwrap();
        }
        std::fs::write(
            package.path().join("boot.ipxe"),
            BROKEN_NAMED_MANAGED_IPXE_SCRIPT,
        )
        .unwrap();
        std::fs::write(package.path().join(LEGACY_MANAGED_LAYOUT_MARKER), b"2\n").unwrap();

        assert!(BootPackage::ensure_managed_network_boot(package.path()).unwrap());
        assert_eq!(
            std::fs::read_to_string(package.path().join("boot.ipxe")).unwrap(),
            MANAGED_IPXE_SCRIPT
        );
        let bcd = std::fs::read(package.path().join("boot/easydeploymesh.bcd")).unwrap();
        assert!(!output_contains_ascii_case_insensitive(&bcd, r"\WEPE\B64"));
        assert!(output_contains_ascii_case_insensitive(
            &bcd,
            r"\Boot\boot.wim"
        ));
        assert!(package.path().join(MANAGED_LAYOUT_MARKER).is_file());
        assert!(!package.path().join(LEGACY_MANAGED_LAYOUT_MARKER).exists());
    }

    #[test]
    fn media_with_multiple_unidentified_custom_wims_is_rejected_as_ambiguous() {
        let source = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(source.path().join("images")).unwrap();
        std::fs::create_dir_all(source.path().join("boot")).unwrap();
        std::fs::create_dir_all(source.path().join("efi/boot")).unwrap();
        std::fs::write(source.path().join("images/custom-a.wim"), b"custom-a").unwrap();
        std::fs::write(source.path().join("images/custom-b.wim"), vec![b'b'; 1024]).unwrap();
        std::fs::write(source.path().join("boot/boot.sdi"), b"sdi").unwrap();
        std::fs::write(source.path().join("efi/boot/bootx64.efi"), b"efi").unwrap();
        std::fs::write(source.path().join("bootmgr"), b"bootmgr").unwrap();
        let parent = tempfile::tempdir().unwrap();

        let error =
            build_winpe_package(source.path(), &parent.path().join("managed"), None).unwrap_err();

        assert_eq!(
            error.to_string(),
            "invalid PXE configuration: media contains multiple WIM images but no unique WinPE boot image"
        );
    }

    #[test]
    fn third_party_media_uses_the_unique_existing_wim_referenced_by_uefi_bcd() {
        let source = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(source.path().join("boot")).unwrap();
        std::fs::create_dir_all(source.path().join("wxpe")).unwrap();
        std::fs::create_dir_all(source.path().join("efi/microsoft/boot")).unwrap();
        std::fs::create_dir_all(source.path().join("efi/boot")).unwrap();
        let mut x64_wim = vec![0; WIM_BOOT_INDEX_OFFSET + 4];
        x64_wim[..WIM_SIGNATURE.len()].copy_from_slice(WIM_SIGNATURE);
        x64_wim[WIM_HEADER_SIZE_OFFSET..WIM_HEADER_SIZE_OFFSET + 4]
            .copy_from_slice(&0xd0_u32.to_le_bytes());
        x64_wim[WIM_PART_NUMBER_OFFSET..WIM_PART_NUMBER_OFFSET + 2]
            .copy_from_slice(&1_u16.to_le_bytes());
        x64_wim[WIM_TOTAL_PARTS_OFFSET..WIM_TOTAL_PARTS_OFFSET + 2]
            .copy_from_slice(&1_u16.to_le_bytes());
        x64_wim[WIM_IMAGE_COUNT_OFFSET..WIM_IMAGE_COUNT_OFFSET + 4]
            .copy_from_slice(&1_u32.to_le_bytes());
        x64_wim[WIM_BOOT_INDEX_OFFSET..WIM_BOOT_INDEX_OFFSET + 4]
            .copy_from_slice(&1_u32.to_le_bytes());
        std::fs::write(source.path().join("boot/10PEx64.wim"), &x64_wim).unwrap();
        std::fs::write(source.path().join("wxpe/03PE.wim"), &x64_wim).unwrap();
        let bcd = r"\boot\10pex64.wim"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>();
        std::fs::write(source.path().join("efi/microsoft/boot/BCD"), bcd).unwrap();
        std::fs::write(source.path().join("boot/boot.sdi"), b"sdi").unwrap();
        std::fs::write(source.path().join("efi/boot/bootx64.efi"), b"efi").unwrap();
        std::fs::write(source.path().join("bootmgr"), b"bootmgr").unwrap();
        let parent = tempfile::tempdir().unwrap();

        let package =
            build_winpe_package(source.path(), &parent.path().join("managed"), None).unwrap();

        assert_eq!(
            std::fs::read(Path::new(&package.root).join("boot/boot.wim")).unwrap(),
            x64_wim
        );
    }

    #[test]
    fn third_party_media_uses_the_sdi_referenced_by_uefi_bcd() {
        let source = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(source.path().join("wepe")).unwrap();
        std::fs::create_dir_all(source.path().join("other")).unwrap();
        std::fs::create_dir_all(source.path().join("efi/microsoft/boot")).unwrap();
        std::fs::create_dir_all(source.path().join("efi/boot")).unwrap();
        std::fs::write(source.path().join("wepe/WEPE64.WIM"), b"winpe").unwrap();
        std::fs::write(source.path().join("wepe/WEPE.SDI"), b"vendor-sdi").unwrap();
        std::fs::write(source.path().join("other/unused.SDI"), b"unused-sdi").unwrap();
        let bcd = r"\WEPE\WEPE.SDI"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>();
        std::fs::write(source.path().join("efi/microsoft/boot/BCD"), bcd).unwrap();
        std::fs::write(source.path().join("efi/boot/bootx64.efi"), b"efi").unwrap();
        std::fs::write(source.path().join("bootmgr"), b"bootmgr").unwrap();
        let parent = tempfile::tempdir().unwrap();

        let package =
            build_winpe_package(source.path(), &parent.path().join("managed"), None).unwrap();

        assert_eq!(
            std::fs::read(Path::new(&package.root).join("boot/boot.sdi")).unwrap(),
            b"vendor-sdi"
        );
    }

    #[test]
    fn wepe64_media_uses_managed_uefi_ipxe_instead_of_the_vendor_boot_chain() {
        let source = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(source.path().join("wepe")).unwrap();
        std::fs::create_dir_all(source.path().join("efi/microsoft/boot")).unwrap();
        std::fs::create_dir_all(source.path().join("efi/boot")).unwrap();
        std::fs::write(source.path().join("wepe/WEPE64.WIM"), b"winpe").unwrap();
        std::fs::write(source.path().join("wepe/WEPE.SDI"), b"sdi").unwrap();
        let vendor_bcd = r"\WEPE\B64"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>();
        std::fs::write(source.path().join("efi/microsoft/boot/BCD"), vendor_bcd).unwrap();
        std::fs::write(source.path().join("efi/boot/bootx64.efi"), b"efi").unwrap();
        std::fs::write(source.path().join("bootmgr"), b"bootmgr").unwrap();
        let parent = tempfile::tempdir().unwrap();

        let package =
            build_winpe_package(source.path(), &parent.path().join("managed"), None).unwrap();
        let root = Path::new(&package.root);
        let managed_bcd = std::fs::read(root.join("boot/BCD")).unwrap();

        assert!(output_contains_ascii_case_insensitive(
            &managed_bcd,
            r"\Boot\boot.wim"
        ));
        assert!(!output_contains_ascii_case_insensitive(
            &managed_bcd,
            r"\WEPE\B64"
        ));
        assert_eq!(package.uefi_x64_boot_file, "ipxe.efi");
        assert_eq!(
            std::fs::read(root.join("ipxe.efi")).unwrap(),
            EMBEDDED_IPXE_EFI
        );
        assert!(!root.join("efi/boot/bootx64.efi").exists());
        assert!(!root.join("efi/microsoft/boot/BCD").exists());
        assert!(root.join("boot/boot.wim").is_file());
        assert!(root.join("boot/boot.sdi").is_file());
    }

    #[test]
    fn unique_boot_index_selects_a_custom_wim_without_using_file_size() {
        let source = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(source.path().join("images")).unwrap();
        std::fs::create_dir_all(source.path().join("boot")).unwrap();
        std::fs::create_dir_all(source.path().join("efi/boot")).unwrap();
        let mut boot_wim = vec![0; WIM_BOOT_INDEX_OFFSET + 4];
        boot_wim[..WIM_SIGNATURE.len()].copy_from_slice(WIM_SIGNATURE);
        boot_wim[WIM_HEADER_SIZE_OFFSET..WIM_HEADER_SIZE_OFFSET + 4]
            .copy_from_slice(&0xd0_u32.to_le_bytes());
        boot_wim[WIM_PART_NUMBER_OFFSET..WIM_PART_NUMBER_OFFSET + 2]
            .copy_from_slice(&1_u16.to_le_bytes());
        boot_wim[WIM_TOTAL_PARTS_OFFSET..WIM_TOTAL_PARTS_OFFSET + 2]
            .copy_from_slice(&1_u16.to_le_bytes());
        boot_wim[WIM_IMAGE_COUNT_OFFSET..WIM_IMAGE_COUNT_OFFSET + 4]
            .copy_from_slice(&2_u32.to_le_bytes());
        boot_wim[WIM_BOOT_INDEX_OFFSET..WIM_BOOT_INDEX_OFFSET + 4]
            .copy_from_slice(&2_u32.to_le_bytes());
        std::fs::write(source.path().join("images/rescue-x64.wim"), &boot_wim).unwrap();
        std::fs::write(source.path().join("images/archive.wim"), vec![b'a'; 1024]).unwrap();
        std::fs::write(source.path().join("boot/boot.sdi"), b"sdi").unwrap();
        std::fs::write(source.path().join("efi/boot/bootx64.efi"), b"efi").unwrap();
        std::fs::write(source.path().join("bootmgr"), b"bootmgr").unwrap();
        let parent = tempfile::tempdir().unwrap();

        let package =
            build_winpe_package(source.path(), &parent.path().join("managed"), None).unwrap();

        assert_eq!(
            std::fs::read(Path::new(&package.root).join("boot/boot.wim")).unwrap(),
            boot_wim
        );
    }

    #[test]
    fn clean_winpe_bcd_targets_the_managed_ramdisk_image() {
        let commands = winpe_bcd_commands(WINPE_STANDARD_LOADER_PATH)
            .into_iter()
            .map(|command| command.join(" "))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(commands.contains("ramdisksdidevice boot"));
        assert!(commands.contains(r"ramdisksdipath \Boot\boot.sdi"));
        assert!(commands.contains(r"device ramdisk=[boot]\Boot\boot.wim,{ramdiskoptions}"));
        assert!(commands.contains(r"osdevice ramdisk=[boot]\Boot\boot.wim,{ramdiskoptions}"));
        assert!(commands.contains(r"path \windows\system32\winload.exe"));
        assert!(commands.contains("winpe Yes"));
        assert!(commands.contains(&format!("default {WINPE_LOADER_ID}")));
        assert!(!commands.to_ascii_lowercase().contains("grldr"));
    }

    #[test]
    fn injected_startnet_launches_agent_and_preserves_the_original_pe_shell() {
        assert!(EASYDEPLOYMESH_STARTNET.contains(
            r#"start "" /b cmd.exe /d /c "X:\EasyDeployMesh\easydeploymesh-agent.exe --bootstrap X:\EasyDeployMesh\easydeploymesh-bootstrap.json >> X:\EasyDeployMesh\easydeploymesh-agent.log 2>&1""#
        ));
        assert!(
            EASYDEPLOYMESH_STARTNET.contains(r#"X:\EasyDeployMesh\easydeploymesh-agent.log 2>&1"#)
        );
        assert!(
            EASYDEPLOYMESH_STARTNET
                .contains(r#"call X:\Windows\System32\startnet.easydeploymesh-original.cmd"#)
        );
        assert!(EASYDEPLOYMESH_STARTNET.starts_with("@echo off\r\nwpeinit\r\n"));
    }

    #[test]
    fn reads_the_wepe_setup_shell_from_reg_query_output() {
        let output = b"\r\nHKEY_LOCAL_MACHINE\\EDM_TEST\\Setup\r\n    CmdLine    REG_SZ    PECMD.EXE MAIN %SystemRoot%\\PECMD.INI\r\n";

        assert_eq!(
            reg_sz_value(output, "CmdLine").as_deref(),
            Some(r"PECMD.EXE MAIN %SystemRoot%\PECMD.INI")
        );
        assert_eq!(reg_sz_value(output, "Missing"), None);
    }

    #[test]
    fn easyu_winpeshl_hook_takes_precedence_over_the_setup_cmdline_fallback() {
        assert!(!should_patch_setup_cmdline(true));
        assert!(should_patch_setup_cmdline(false));
    }

    #[test]
    fn runtime_diagnostic_collector_is_bundled_for_injected_winpe() {
        let collector = std::str::from_utf8(EASYDEPLOYMESH_RUNTIME_COLLECTOR)
            .expect("the WinPE collector should remain an ASCII-compatible CMD script");

        assert!(collector.contains("easydeploymesh-agent.exe\" --version"));
        assert!(!collector.to_ascii_lowercase().contains("ghost64.exe"));
        assert!(collector.contains("echo list disk"));
        assert!(collector.contains("echo list volume"));
        assert!(!collector.contains("echo clean"));
        assert!(!collector.contains("echo format"));
    }

    #[test]
    fn custom_winpe_launchapps_starts_agent_before_the_vendor_shell() {
        let original = b"[LaunchApps]\r\n%SYSTEMROOT%\\System32\\pecmd.exe MAIN %Windir%\\System32\\pecmd.ini\r\n";
        let patched = patch_winpeshl_for_agent(original).unwrap();
        let text = String::from_utf8(patched.ini).unwrap();
        assert!(text.contains("[LaunchApps]\r\nX:\\EasyDeployMesh\\easydeploymesh-shell.exe\r\n"));
        assert!(text.contains("pecmd.exe MAIN"));
        assert!(patched.original_shell_script.is_none());
    }

    #[test]
    fn custom_winpe_single_launchapp_is_chained_after_the_agent() {
        let original =
            b"[LaunchApp]\r\nAppPath = %SYSTEMROOT%\\System32\\pecmd.exe MAIN EasyU.ini\r\n";
        let patched = patch_winpeshl_for_agent(original).unwrap();
        let text = String::from_utf8(patched.ini).unwrap();
        assert!(text.contains("AppPath = X:\\EasyDeployMesh\\easydeploymesh-shell.exe"));
        assert_eq!(
            patched.original_shell_script.unwrap(),
            b"@echo off\r\n%SYSTEMROOT%\\System32\\pecmd.exe MAIN EasyU.ini\r\n"
        );
    }

    #[test]
    fn reinjecting_an_already_patched_launchapps_file_is_idempotent() {
        let already_patched = b"[LaunchApps]\r\nX:\\EasyDeployMesh\\easydeploymesh-shell.exe\r\n%SYSTEMROOT%\\System32\\pecmd.exe MAIN %Windir%\\System32\\pecmd.ini\r\n";

        let patched = patch_winpeshl_for_agent(already_patched).unwrap();

        assert_eq!(patched.ini, already_patched);
        assert!(patched.original_shell_script.is_none());
    }

    #[test]
    fn reinjecting_an_already_patched_launchapp_preserves_the_original_shell_script() {
        let already_patched =
            b"[LaunchApp]\r\nAppPath = X:\\EasyDeployMesh\\easydeploymesh-shell.exe\r\n";

        let patched = patch_winpeshl_for_agent(already_patched).unwrap();

        assert_eq!(patched.ini, already_patched);
        assert!(patched.original_shell_script.is_none());
    }

    #[test]
    fn easyu_wim_uses_the_loader_inside_the_system32_boot_directory() {
        let listing = br"\Windows\System32\boot\winload.efi
\Windows\System32\boot\winload.exe";
        assert_eq!(
            winpe_loader_path_from_listing(listing),
            Some(WINPE_SYSTEM32_BOOT_LOADER_PATH)
        );
        let commands = winpe_bcd_commands(WINPE_SYSTEM32_BOOT_LOADER_PATH);
        assert!(commands.iter().any(|command| {
            command
                == &[
                    "/set",
                    WINPE_LOADER_ID,
                    "path",
                    WINPE_SYSTEM32_BOOT_LOADER_PATH,
                ]
        }));
        assert!(!commands.iter().any(|command| {
            command == &["/set", WINPE_LOADER_ID, "path", WINPE_STANDARD_LOADER_PATH]
        }));
    }

    #[test]
    fn loader_detection_accepts_utf16_output_but_not_similar_filenames() {
        let utf16_listing = WINPE_SYSTEM32_BOOT_LOADER_PATH
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>();
        assert_eq!(
            winpe_loader_path_from_listing(&utf16_listing),
            Some(WINPE_SYSTEM32_BOOT_LOADER_PATH)
        );
        assert_eq!(
            winpe_loader_path_from_listing(br"\Windows\System32\boot\winload.exe.bak"),
            None
        );
        assert_eq!(
            winpe_loader_path_from_listing(br"\Windows\System32\boot\winload.efi"),
            None
        );
        assert!(output_contains_ascii_case_insensitive(
            b"p\0a\0t\0h\0 \0\\\0W\0i\0n\0d\0o\0w\0s\0",
            r"path \windows"
        ));
    }

    #[test]
    fn wim_header_reports_the_selected_boot_image() {
        let mut header = vec![0; WIM_BOOT_INDEX_OFFSET + 4];
        header[..WIM_SIGNATURE.len()].copy_from_slice(WIM_SIGNATURE);
        header[WIM_HEADER_SIZE_OFFSET..WIM_HEADER_SIZE_OFFSET + 4]
            .copy_from_slice(&0xd0_u32.to_le_bytes());
        header[WIM_PART_NUMBER_OFFSET..WIM_PART_NUMBER_OFFSET + 2]
            .copy_from_slice(&1_u16.to_le_bytes());
        header[WIM_TOTAL_PARTS_OFFSET..WIM_TOTAL_PARTS_OFFSET + 2]
            .copy_from_slice(&1_u16.to_le_bytes());
        header[WIM_IMAGE_COUNT_OFFSET..WIM_IMAGE_COUNT_OFFSET + 4]
            .copy_from_slice(&3_u32.to_le_bytes());
        header[WIM_BOOT_INDEX_OFFSET..WIM_BOOT_INDEX_OFFSET + 4]
            .copy_from_slice(&2_u32.to_le_bytes());

        assert_eq!(wim_boot_index_from_header(&header), Some(2));

        header[WIM_BOOT_INDEX_OFFSET..WIM_BOOT_INDEX_OFFSET + 4]
            .copy_from_slice(&4_u32.to_le_bytes());
        assert_eq!(wim_boot_index_from_header(&header), None);

        header[WIM_BOOT_INDEX_OFFSET..WIM_BOOT_INDEX_OFFSET + 4]
            .copy_from_slice(&0_u32.to_le_bytes());
        assert_eq!(wim_boot_index_from_header(&header), None);
        assert_eq!(wim_boot_index_from_header(&header[..16]), None);
        header[..WIM_SIGNATURE.len()].copy_from_slice(b"NOTWIM!!");
        assert_eq!(wim_boot_index_from_header(&header), None);
    }

    #[test]
    fn dism_image_listing_arguments_preserve_the_wim_path_as_one_argument() {
        let wim = Path::new(r"C:\PXE media\易启优.wim");
        let arguments = dism_list_image_args(wim, 1);

        assert_eq!(arguments.len(), 4);
        assert_eq!(arguments[0], "/English");
        assert_eq!(arguments[1], "/List-Image");
        assert_eq!(arguments[2], r"/ImageFile:C:\PXE media\易启优.wim");
        assert_eq!(arguments[3], "/Index:1");
    }

    #[test]
    fn windows_background_processes_use_no_window_creation_flag() {
        assert_eq!(WINDOWS_CREATE_NO_WINDOW, 0x0800_0000);
    }

    #[test]
    fn easyu_package_bcd_targets_its_nonstandard_loader_path() {
        let source = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(source.path().join("BOOT")).unwrap();
        std::fs::create_dir_all(source.path().join("EFI/BOOT")).unwrap();
        std::fs::write(
            source.path().join("BOOT/custom-x64.wim"),
            br"\Windows\System32\boot\winload.exe",
        )
        .unwrap();
        std::fs::write(source.path().join("BOOT/BOOT.SDI"), b"sdi").unwrap();
        std::fs::write(source.path().join("EFI/BOOT/BOOTX64.EFI"), b"efi").unwrap();
        std::fs::write(source.path().join("bootmgr"), b"bootmgr").unwrap();
        let parent = tempfile::tempdir().unwrap();

        let package =
            build_winpe_package(source.path(), &parent.path().join("managed"), None).unwrap();
        let bcd = std::fs::read_to_string(Path::new(&package.root).join("boot/BCD")).unwrap();

        assert!(bcd.contains(r"path \windows\system32\boot\winload.exe"));
        assert!(!bcd.contains(r"path \windows\system32\winload.exe"));
    }

    #[test]
    fn ipxe_and_wimboot_are_configured_for_bios_and_uefi() {
        let source = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(source.path().join("boot/grub")).unwrap();
        std::fs::create_dir_all(source.path().join("efi/boot")).unwrap();
        for (path, data) in [
            ("boot/custom-x64.wim", b"wim".as_slice()),
            ("boot/BCD", b"grldr-marker"),
            ("boot/boot.sdi", b"sdi"),
            ("efi/boot/bootx64.efi", b"efi"),
            ("grldr", b"grldr"),
            ("bootmgr", b"bootmgr"),
        ] {
            std::fs::write(source.path().join(path), data).unwrap();
        }
        let parent = tempfile::tempdir().unwrap();
        let package =
            build_winpe_package(source.path(), &parent.path().join("managed"), None).unwrap();
        assert_eq!(package.bios_boot_file, "undionly.kpxe");
        assert_eq!(package.uefi_x64_boot_file, "ipxe.efi");
        let script = std::fs::read_to_string(Path::new(&package.root).join("boot.ipxe")).unwrap();
        assert_eq!(script, MANAGED_IPXE_SCRIPT);
        assert_eq!(
            std::fs::read(Path::new(&package.root).join("boot/bootmgr")).unwrap(),
            b"bootmgr"
        );
        let generated_bcd = std::fs::read(Path::new(&package.root).join("boot/BCD")).unwrap();
        assert_ne!(generated_bcd, b"grldr-marker");
        assert!(
            !generated_bcd
                .windows(b"grldr".len())
                .any(|window| window.eq_ignore_ascii_case(b"grldr"))
        );
        assert_eq!(
            std::fs::read(Path::new(&package.root).join("boot/wimboot")).unwrap(),
            EMBEDDED_WIMBOOT
        );
    }

    #[test]
    #[ignore = "requires the user-provided external media samples"]
    fn imports_supported_easyu_and_firpe_media_samples() {
        for variable in ["EASYDEPLOYMESH_EASYU_ISO", "EASYDEPLOYMESH_FIRPE_IMG"] {
            let source = std::env::var(variable)
                .unwrap_or_else(|_| panic!("{variable} must point to the external media sample"));
            std::thread::Builder::new()
                .stack_size(1024 * 1024)
                .spawn(move || {
                    let parent = tempfile::tempdir().unwrap();
                    let package =
                        BootPackage::import_media(&source, parent.path().join("managed")).unwrap();
                    assert!(Path::new(&package.root).join("boot/boot.wim").is_file());
                    assert!(Path::new(&package.root).join("boot/BCD").is_file());
                    assert!(Path::new(&package.root).join("boot/bootmgr").is_file());
                    assert!(Path::new(&package.root).join("boot/boot.sdi").is_file());
                    assert!(Path::new(&package.root).join("ipxe.efi").is_file());
                    assert_eq!(package.bios_boot_file, "undionly.kpxe");
                    assert_eq!(package.uefi_x64_boot_file, "ipxe.efi");
                    assert!(Path::new(&package.root).join("boot.ipxe").is_file());
                })
                .unwrap()
                .join()
                .unwrap();
        }
    }

    #[test]
    fn public_media_import_rejects_wepe_and_cleans_staging() {
        let parent = tempfile::tempdir().unwrap();
        let source = parent.path().join("WePE64.img");
        write_fat_image_fixture(
            &source,
            &[
                ("BOOTMGR", b"private-loader"),
                ("WEPE/WEPE64", b"private-loader"),
                ("WEPE/B64", b"bcd"),
                ("WEPE/WEPE.SDI", b"sdi"),
                ("WEPE/WEPE64.WIM", b"wim"),
            ],
        );
        let managed = parent.path().join("managed");

        let error = BootPackage::import_media(&source, &managed).unwrap_err();

        assert_eq!(
            error.to_string(),
            "invalid PXE configuration: WePE media is unsupported because its private boot chain cannot reliably start an Agent-injected deployment image"
        );
        assert!(!managed.exists());
        assert!(std::fs::read_dir(parent.path()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".pxe-media-")
        }));
    }

    #[test]
    #[ignore = "requires EASYDEPLOYMESH_WEPE_ISO to point to the WEPE64 v2.2 ISO"]
    fn rejects_real_wepe_v2_2_before_creating_a_managed_package() {
        let source = std::env::var("EASYDEPLOYMESH_WEPE_ISO")
            .expect("EASYDEPLOYMESH_WEPE_ISO must point to the WEPE64 v2.2 ISO");
        std::thread::Builder::new()
            .stack_size(1024 * 1024)
            .spawn(move || {
                let parent = tempfile::tempdir().unwrap();
                let managed = parent.path().join("managed");
                let error = BootPackage::import_media(&source, &managed).unwrap_err();
                assert_eq!(
                    error.to_string(),
                    "invalid PXE configuration: WePE media is unsupported because its private boot chain cannot reliably start an Agent-injected deployment image"
                );
                assert!(!managed.exists());
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn recognizes_wepe_private_boot_layout_without_misclassifying_standard_winpe() {
        let private = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(private.path().join("WEPE")).unwrap();
        std::fs::write(private.path().join("BOOTMGR"), b"private-loader").unwrap();
        std::fs::write(private.path().join("WEPE/WEPE64"), b"private-loader").unwrap();
        std::fs::write(private.path().join("WEPE/B64"), b"bcd").unwrap();
        std::fs::write(private.path().join("WEPE/WEPE.SDI"), b"sdi").unwrap();
        std::fs::write(private.path().join("WEPE/WEPE64.WIM"), b"wim").unwrap();
        assert!(is_private_wepe_layout(private.path()));
        assert_eq!(
            ensure_supported_pe_layout(private.path())
                .unwrap_err()
                .to_string(),
            "invalid PXE configuration: WePE media is unsupported because its private boot chain cannot reliably start an Agent-injected deployment image"
        );

        std::fs::create_dir_all(private.path().join("BOOT")).unwrap();
        std::fs::write(private.path().join("BOOT/BCD"), b"standard-bcd").unwrap();
        std::fs::write(private.path().join("WEPE/WEPE64"), b"different-loader").unwrap();
        assert!(!is_private_wepe_layout(private.path()));
    }

    #[test]
    fn parses_http_ranges_used_for_native_iso_boot() {
        use axum::http::HeaderValue;

        assert_eq!(parse_http_byte_range(None, 100), Ok(None));
        assert_eq!(
            parse_http_byte_range(Some(&HeaderValue::from_static("bytes=10-19")), 100),
            Ok(Some((10, 19)))
        );
        assert_eq!(
            parse_http_byte_range(Some(&HeaderValue::from_static("bytes=90-")), 100),
            Ok(Some((90, 99)))
        );
        assert_eq!(
            parse_http_byte_range(Some(&HeaderValue::from_static("bytes=-10")), 100),
            Ok(Some((90, 99)))
        );
        assert_eq!(
            parse_http_byte_range(Some(&HeaderValue::from_static("bytes=100-")), 100),
            Err(())
        );
    }

    #[tokio::test]
    async fn native_iso_http_server_streams_only_the_requested_range() {
        let media = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(media.path(), b"0123456789abcdef").unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let clients = Arc::new(RwLock::new(HashMap::from([(
            "00:0c:29:ee:3e:8d".into(),
            PxeDiscoveredClient {
                mac_address: "00:0c:29:ee:3e:8d".into(),
                ip_address: Some("127.0.0.1".into()),
                architecture: Architecture::X86_64,
                stage: PxeClientStage::Downloading,
                first_seen_at: Utc::now(),
                last_seen_at: Utc::now(),
            },
        )])));
        let router = Router::new()
            .route("/boot/native.iso", get(serve_native_iso))
            .with_state(NativeIsoHttpState {
                iso: media.path().to_path_buf(),
                clients: Arc::clone(&clients),
            });
        let task = tokio::spawn(async move {
            axum::serve(
                listener,
                router.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap()
        });

        let response = reqwest::Client::new()
            .get(format!("http://{address}/boot/native.iso"))
            .header(header::RANGE, "bytes=4-9")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(response.headers()[header::CONTENT_RANGE], "bytes 4-9/16");
        assert_eq!(response.bytes().await.unwrap().as_ref(), b"456789");
        assert_eq!(
            clients.read().await.get("00:0c:29:ee:3e:8d").unwrap().stage,
            PxeClientStage::WaitingForAgent
        );

        task.abort();
    }
}
use crate::ActivityRepository;
use axum::{
    Router,
    body::Body,
    extract::{ConnectInfo, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use chrono::{DateTime, Duration, Utc};
use easydeploymesh_core::{
    ActivitySeverity, ActivitySource, ActivitySubject, Architecture, PxeClientStage, PxeConfig,
    PxeDiscoveredClient, PxeMode, PxeServiceStatus,
};
use hadris_iso::{
    boot::{
        EmulationType, PlatformId,
        options::{BootEntryOptions, BootOptions, BootSectionOptions},
    },
    joliet::JolietLevel,
    read::PathSeparator,
    write::{
        InputTree, IsoImageWriter,
        options::{BaseIsoLevel, CreationFeatures, IsoFormatOptions},
    },
};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use socket2::{Domain, Protocol, Socket, Type};
use std::{
    collections::HashMap,
    fs,
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::{Component, Path, PathBuf},
    sync::Arc,
};
use thiserror::Error;
use tokio::{
    io::{AsyncReadExt, AsyncSeekExt},
    net::{TcpListener, UdpSocket},
    sync::{Mutex, RwLock, oneshot},
    task::JoinHandle,
    time::{Duration as TokioDuration, timeout},
};
use tokio_util::io::ReaderStream;

// iPXE wimboot v2.9.0 (GPLv2), vendored with its license under assets/wimboot.
const EMBEDDED_WIMBOOT: &[u8] = include_bytes!("../assets/wimboot/wimboot");
// iPXE undionly.kpxe, vendored with its GPLv2 license under assets/ipxe.
const EMBEDDED_UNDIONLY: &[u8] = include_bytes!("../assets/ipxe/undionly.kpxe");
// iPXE x86-64 UEFI executable, vendored with its GPLv2 license under assets/ipxe.
const EMBEDDED_IPXE_EFI: &[u8] = include_bytes!("../assets/ipxe/ipxe.efi");
const LEGACY_MANAGED_LAYOUT_MARKER: &str = ".easydeploymesh-pxe-layout-v2";
const V3_MANAGED_LAYOUT_MARKER: &str = ".easydeploymesh-pxe-layout-v3";
const MANAGED_LAYOUT_MARKER: &str = ".easydeploymesh-pxe-layout-v4";
const LEGACY_NATIVE_ISO_LAYOUT_MARKER: &str = ".easydeploymesh-native-iso-layout-v1";
const NATIVE_ISO_LAYOUT_MARKER: &str = ".easydeploymesh-native-iso-layout-v2";
const NATIVE_ISO_PATH: &str = "boot/native.iso";
const NATIVE_ISO_PLACEHOLDER_SCRIPT: &str =
    "#!ipxe\nsanboot http://${next-server}:0/boot/native.iso || shell\n";
const LEGACY_MANAGED_IPXE_SCRIPT: &str = "#!ipxe\nkernel boot/wimboot\ninitrd boot/bootmgr bootmgr\ninitrd boot/BCD BCD\ninitrd boot/boot.sdi boot.sdi\ninitrd boot/easydeploymesh-bootstrap.json easydeploymesh-bootstrap.json\ninitrd boot/boot.wim boot.wim\nboot\n";
const BROKEN_NAMED_MANAGED_IPXE_SCRIPT: &str = "#!ipxe\nkernel boot/wimboot\ninitrd --name bootmgr boot/bootmgr\ninitrd --name BCD boot/BCD\ninitrd --name boot.sdi boot/boot.sdi\ninitrd --name easydeploymesh-bootstrap.json boot/easydeploymesh-bootstrap.json\ninitrd --name boot.wim boot/boot.wim\nboot\n";
const V3_MANAGED_IPXE_SCRIPT: &str = "#!ipxe\nkernel boot/wimboot\ninitrd --name bootmgr boot/bootmgr bootmgr\ninitrd --name BCD boot/BCD BCD\ninitrd --name boot.sdi boot/boot.sdi boot.sdi\ninitrd --name easydeploymesh-bootstrap.json boot/easydeploymesh-bootstrap.json easydeploymesh-bootstrap.json\ninitrd --name boot.wim boot/boot.wim boot.wim\nboot\n";
const MANAGED_IPXE_SCRIPT: &str = "#!ipxe\nkernel boot/wimboot\ninitrd --name bootmgr boot/bootmgr bootmgr\ninitrd --name BCD boot/easydeploymesh.bcd BCD\ninitrd --name boot.sdi boot/boot.sdi boot.sdi\ninitrd --name easydeploymesh-bootstrap.json boot/easydeploymesh-bootstrap.json easydeploymesh-bootstrap.json\ninitrd --name boot.wim boot/boot.wim boot.wim\nboot\n";
const DHCP_SERVER_PORT: u16 = 67;
const DHCP_CLIENT_PORT: u16 = 68;
const TFTP_PORT: u16 = 69;
const PROXY_DHCP_PORT: u16 = 4011;
const DHCP_MAGIC_COOKIE: &[u8; 4] = &[99, 130, 83, 99];
const DHCP_PROBE_VENDOR_CLASS: &[u8] = b"EasyDeployMesh-DHCP-Probe";
const PXE_DISCOVERY_TTL: Duration = Duration::seconds(15);
const PXE_BOOT_PROGRESS_TTL: Duration = Duration::minutes(5);
#[cfg(any(target_os = "windows", test))]
const WINPE_LOADER_ID: &str = "{9f2f2f90-0f5a-4d8b-9e9f-5decb22bbf0a}";
#[cfg(any(target_os = "windows", test))]
const WINPE_RAMDISK_DEVICE: &str = "ramdisk=[boot]\\Boot\\boot.wim,{ramdiskoptions}";
#[cfg(any(target_os = "windows", test))]
const WINPE_STANDARD_LOADER_PATH: &str = r"\windows\system32\winload.exe";
#[cfg(any(target_os = "windows", test))]
const WINPE_SYSTEM32_BOOT_LOADER_PATH: &str = r"\windows\system32\boot\winload.exe";
const WIM_SIGNATURE: &[u8; 8] = b"MSWIM\0\0\0";
const WIM_HEADER_SIZE_OFFSET: usize = 0x08;
const WIM_PART_NUMBER_OFFSET: usize = 0x28;
const WIM_TOTAL_PARTS_OFFSET: usize = 0x2a;
const WIM_IMAGE_COUNT_OFFSET: usize = 0x2c;
const WIM_BOOT_INDEX_OFFSET: usize = 0x78;

#[cfg(any(target_os = "windows", test))]
const WINDOWS_CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[cfg(all(target_os = "windows", not(test)))]
fn background_command(program: &str) -> std::process::Command {
    use std::os::windows::process::CommandExt;

    let mut command = std::process::Command::new(program);
    command.creation_flags(WINDOWS_CREATE_NO_WINDOW);
    command
}

#[derive(Debug, Error)]
pub enum PxeServiceError {
    #[error("PXE service is already running")]
    AlreadyRunning,
    #[error("PXE service is not running")]
    NotRunning,
    #[error("invalid PXE configuration: {0}")]
    InvalidConfig(String),
    #[error("DHCP pool must not contain the server address")]
    PoolContainsServer,
    #[error("boot file must be a safe relative path: {0}")]
    UnsafeBootFile(String),
    #[error("boot package file is missing: {0}")]
    MissingBootFile(String),
    #[error("another DHCP server responded on this network; use ProxyDHCP or disable it")]
    DhcpConflict,
    #[error("could not bind UDP {port} on {address}: {source}")]
    Bind {
        address: String,
        port: u16,
        #[source]
        source: std::io::Error,
    },
    #[error("could not bind native ISO HTTP service on {address}: {source}")]
    HttpBind {
        address: String,
        #[source]
        source: std::io::Error,
    },
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("PXE service task failed: {0}")]
    Task(#[from] tokio::task::JoinError),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BootPackage {
    pub root: String,
    pub bios_boot_file: String,
    pub uefi_x64_boot_file: String,
}

impl BootPackage {
    /// Refreshes the bundled network loaders for a package created by media import.
    ///
    /// The exact managed iPXE script identifies normalized packages without
    /// rewriting arbitrary user-supplied boot directories.
    pub fn ensure_managed_network_boot(root: impl AsRef<Path>) -> Result<bool, PxeServiceError> {
        let root = root.as_ref();
        if root.join(LEGACY_NATIVE_ISO_LAYOUT_MARKER).is_file()
            && !root.join(NATIVE_ISO_LAYOUT_MARKER).is_file()
        {
            return Err(PxeServiceError::InvalidConfig(
                "the managed native ISO predates Agent injection; reimport the source ISO".into(),
            ));
        }
        if root.join(NATIVE_ISO_LAYOUT_MARKER).is_file() && root.join(NATIVE_ISO_PATH).is_file() {
            fs::write(root.join("undionly.kpxe"), EMBEDDED_UNDIONLY)?;
            fs::write(root.join("ipxe.efi"), EMBEDDED_IPXE_EFI)?;
            fs::write(root.join("boot.ipxe"), NATIVE_ISO_PLACEHOLDER_SCRIPT)?;
            return Ok(true);
        }
        let script = fs::read(root.join("boot.ipxe")).ok();
        if !matches!(
            script.as_deref(),
            Some(bytes) if bytes == MANAGED_IPXE_SCRIPT.as_bytes()
                || bytes == LEGACY_MANAGED_IPXE_SCRIPT.as_bytes()
                || bytes == BROKEN_NAMED_MANAGED_IPXE_SCRIPT.as_bytes()
                || bytes == V3_MANAGED_IPXE_SCRIPT.as_bytes()
        ) || ![
            "boot/boot.sdi",
            "boot/boot.wim",
            "boot/bootmgr",
            "boot/wimboot",
        ]
        .iter()
        .all(|path| root.join(path).is_file())
        {
            return Ok(false);
        }
        let loader_path = detect_winpe_loader_path(&root.join("boot/boot.wim"))?;
        let replacement = root
            .join("boot")
            .join(format!(".BCD-{}", uuid::Uuid::new_v4()));
        if let Err(error) = write_clean_winpe_bcd(&replacement, loader_path) {
            let _ = fs::remove_file(replacement);
            return Err(error);
        }
        let bcd = root.join("boot/easydeploymesh.bcd");
        let backup = root
            .join("boot")
            .join(format!(".BCD-old-{}", uuid::Uuid::new_v4()));
        let had_bcd = bcd.is_file();
        if had_bcd && let Err(error) = fs::rename(&bcd, &backup) {
            let _ = fs::remove_file(replacement);
            return Err(error.into());
        }
        if let Err(error) = fs::rename(&replacement, &bcd) {
            if had_bcd {
                let _ = fs::rename(backup, bcd);
            }
            let _ = fs::remove_file(replacement);
            return Err(error.into());
        }
        if had_bcd {
            let _ = fs::remove_file(backup);
        }
        fs::write(root.join("undionly.kpxe"), EMBEDDED_UNDIONLY)?;
        fs::write(root.join("ipxe.efi"), EMBEDDED_IPXE_EFI)?;
        fs::write(root.join("boot/wimboot"), EMBEDDED_WIMBOOT)?;
        fs::write(root.join("boot.ipxe"), MANAGED_IPXE_SCRIPT)?;
        fs::write(root.join(MANAGED_LAYOUT_MARKER), b"4\n")?;
        let _ = fs::remove_file(root.join(LEGACY_MANAGED_LAYOUT_MARKER));
        let _ = fs::remove_file(root.join(V3_MANAGED_LAYOUT_MARKER));
        Ok(true)
    }

    pub fn import(
        source: impl AsRef<Path>,
        managed_root: impl AsRef<Path>,
        bios: &str,
        uefi: &str,
    ) -> Result<Self, PxeServiceError> {
        Self::import_inner(source.as_ref(), managed_root.as_ref(), bios, uefi, None)
    }

    pub fn import_with_agent(
        source: impl AsRef<Path>,
        managed_root: impl AsRef<Path>,
        bios: &str,
        uefi: &str,
        agent: impl AsRef<Path>,
    ) -> Result<Self, PxeServiceError> {
        Self::import_inner(
            source.as_ref(),
            managed_root.as_ref(),
            bios,
            uefi,
            Some(agent.as_ref()),
        )
    }

    fn import_inner(
        source: &Path,
        managed_root: &Path,
        bios: &str,
        uefi: &str,
        agent: Option<&Path>,
    ) -> Result<Self, PxeServiceError> {
        validate_relative_path(bios)?;
        validate_relative_path(uefi)?;
        for file in [bios, uefi] {
            if !source.join(file).is_file() {
                return Err(PxeServiceError::MissingBootFile(file.to_owned()));
            }
        }
        let parent = managed_root.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)?;
        let temp = parent.join(format!(".pxe-import-{}", uuid::Uuid::new_v4()));
        if let Err(error) = copy_tree(source, &temp) {
            let _ = fs::remove_dir_all(&temp);
            return Err(error.into());
        }
        if let Some(agent) = agent {
            let staged_wim = temp.join("boot/boot.wim");
            if !staged_wim.is_file() {
                let _ = fs::remove_dir_all(&temp);
                return Err(PxeServiceError::MissingBootFile("boot/boot.wim".into()));
            }
            if let Err(error) = Self::ensure_agent_runtime(&staged_wim, agent) {
                let _ = fs::remove_dir_all(&temp);
                return Err(error);
            }
        }
        if managed_root.exists() {
            let old = parent.join(format!(".pxe-old-{}", uuid::Uuid::new_v4()));
            fs::rename(managed_root, &old)?;
            match fs::rename(&temp, managed_root) {
                Ok(()) => {
                    let _ = fs::remove_dir_all(old);
                }
                Err(error) => {
                    let _ = fs::rename(old, managed_root);
                    return Err(error.into());
                }
            }
        } else {
            fs::rename(&temp, managed_root)?;
        }
        Ok(Self {
            root: managed_root.to_string_lossy().into_owned(),
            bios_boot_file: bios.into(),
            uefi_x64_boot_file: uefi.into(),
        })
    }

    pub fn import_media(
        source: impl AsRef<Path>,
        managed_root: impl AsRef<Path>,
    ) -> Result<Self, PxeServiceError> {
        Self::import_media_with_agent(source, managed_root, None::<&Path>)
    }

    pub fn import_media_with_agent(
        source: impl AsRef<Path>,
        managed_root: impl AsRef<Path>,
        agent: Option<impl AsRef<Path>>,
    ) -> Result<Self, PxeServiceError> {
        let source = source.as_ref();
        let extension = source
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let managed_root = managed_root.as_ref();
        let parent = managed_root.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)?;
        let extracted = parent.join(format!(".pxe-media-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&extracted)?;
        let extraction = match extension.as_str() {
            "iso" => extract_iso(source, &extracted),
            "img" => extract_fat_image(source, &extracted),
            _ => Err(PxeServiceError::InvalidConfig(
                "boot media must be an .iso or .img file".into(),
            )),
        };
        if let Err(error) = extraction {
            let _ = fs::remove_dir_all(&extracted);
            return Err(error);
        }
        if let Err(error) = ensure_supported_pe_layout(&extracted) {
            let _ = fs::remove_dir_all(&extracted);
            return Err(error);
        }
        let result = build_winpe_package_with_native_iso(
            &extracted,
            managed_root,
            agent.as_ref().map(AsRef::as_ref),
            None,
        );
        let _ = fs::remove_dir_all(&extracted);
        result
    }

    /// Places the runtime control-plane configuration inside the WinPE image.
    ///
    /// Files supplied to wimboot as separate initrds are available to Windows
    /// Boot Manager, but they are not part of the X: RAM disk after WinPE has
    /// mounted boot.wim. The Agent therefore needs its bootstrap copied into
    /// the WIM itself before PXE clients boot it.
    pub fn inject_agent_bootstrap(
        wim: impl AsRef<Path>,
        bootstrap: &[u8],
    ) -> Result<(), PxeServiceError> {
        let wim = wim.as_ref();
        inject_bootstrap_into_winpe(wim, bootstrap)?;
        let package_root = wim
            .parent()
            .and_then(Path::parent)
            .unwrap_or_else(|| Path::new("."));
        if package_root.join(NATIVE_ISO_LAYOUT_MARKER).is_file() {
            refresh_native_iso_bootstrap(package_root, bootstrap)?;
        }
        Ok(())
    }

    /// Ensures the managed WinPE image contains the current Agent and runtime layout.
    ///
    /// The SHA-256 markers live beside `boot.wim`, so an installer upgrade,
    /// same-version Agent rebuild, or runtime-layout change can refresh an
    /// already imported image once without mounting it on every PXE start.
    pub fn ensure_agent_runtime(
        wim: impl AsRef<Path>,
        agent: impl AsRef<Path>,
    ) -> Result<bool, PxeServiceError> {
        let wim = wim.as_ref();
        let agent = agent.as_ref();
        if !wim.is_file() {
            return Err(PxeServiceError::MissingBootFile(
                wim.to_string_lossy().into_owned(),
            ));
        }
        if !agent.is_file() {
            return Err(PxeServiceError::MissingBootFile(
                agent.to_string_lossy().into_owned(),
            ));
        }
        let expected_agent_digest = agent_binary_sha256(agent)?;
        let expected_runtime_digest = winpe_runtime_sha256(agent)?;
        let marker_directory = wim.parent().unwrap_or_else(|| Path::new("."));
        let agent_marker = marker_directory.join("easydeploymesh-agent.sha256");
        let runtime_marker = marker_directory.join("easydeploymesh-runtime.sha256");
        if marker_matches(&agent_marker, &expected_agent_digest)
            && marker_matches(&runtime_marker, &expected_runtime_digest)
        {
            return Ok(false);
        }

        inject_agent_into_winpe(wim, agent)?;
        fs::write(agent_marker, expected_agent_digest)?;
        fs::write(runtime_marker, expected_runtime_digest)?;
        Ok(true)
    }
}

fn marker_matches(path: &Path, expected_digest: &str) -> bool {
    fs::read_to_string(path)
        .ok()
        .is_some_and(|digest| digest.trim().eq_ignore_ascii_case(expected_digest))
}

fn agent_binary_sha256(path: &Path) -> Result<String, PxeServiceError> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

// Bump this revision whenever WinPE injection behavior changes without changing
// one of the embedded byte payloads below. Existing packages will then be
// mounted and refreshed once on their next runtime check.
const EASYDEPLOYMESH_RUNTIME_LAYOUT_REVISION: &str = "easydeploymesh-winpe-runtime-layout-v3";

fn winpe_runtime_sha256(agent: &Path) -> Result<String, PxeServiceError> {
    let agent_digest = agent_binary_sha256(agent)?;
    let startnet_digest = bytes_sha256(EASYDEPLOYMESH_STARTNET.as_bytes());
    let collector_digest = bytes_sha256(EASYDEPLOYMESH_RUNTIME_COLLECTOR);
    let manifest = format!(
        "revision={EASYDEPLOYMESH_RUNTIME_LAYOUT_REVISION}\nagent={agent_digest}\nstartnet={startnet_digest}\ncollector={collector_digest}\n"
    );
    Ok(bytes_sha256(manifest.as_bytes()))
}

fn bytes_sha256(contents: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(contents);
    format!("{:x}", hasher.finalize())
}

fn extract_iso(source: &Path, target: &Path) -> Result<(), PxeServiceError> {
    let block_size = gpt_disk_io::gpt_disk_types::BlockSize::new(2048).unwrap();
    let mut media = gpt_disk_io::BlockIoAdapter::new(File::open(source)?, block_size);
    let volume = iso9660::mount(&mut media, 0)
        .map_err(|error| PxeServiceError::InvalidConfig(format!("could not read ISO: {error}")))?;
    copy_iso_directory(
        &mut media,
        volume.root_extent_lba,
        volume.root_extent_len,
        target,
    )
}

#[derive(Debug, Clone)]
struct ElToritoBootImages {
    bios: Vec<u8>,
    bios_load_sectors: u16,
    uefi: Vec<u8>,
    uefi_load_sectors: u16,
}

fn read_el_torito_boot_images(source: &Path) -> Result<ElToritoBootImages, PxeServiceError> {
    const ISO_SECTOR: u64 = 2048;
    let mut media = File::open(source)?;
    let media_len = media.metadata()?.len();
    let mut descriptor = [0u8; 2048];
    let mut catalog_lba = None;
    for lba in 16..64u64 {
        media.seek(SeekFrom::Start(lba * ISO_SECTOR))?;
        media.read_exact(&mut descriptor)?;
        if &descriptor[1..6] != b"CD001" {
            return Err(PxeServiceError::InvalidConfig(
                "ISO has an invalid volume descriptor".into(),
            ));
        }
        if descriptor[0] == 0 && descriptor[7..39].starts_with(b"EL TORITO SPECIFICATION") {
            catalog_lba = Some(u32::from_le_bytes(descriptor[71..75].try_into().unwrap()));
            break;
        }
        if descriptor[0] == 255 {
            break;
        }
    }
    let catalog_lba = catalog_lba.ok_or_else(|| {
        PxeServiceError::InvalidConfig("ISO has no El Torito boot catalog".into())
    })?;
    let mut catalog = [0u8; 2048];
    media.seek(SeekFrom::Start(u64::from(catalog_lba) * ISO_SECTOR))?;
    media.read_exact(&mut catalog)?;
    let checksum = catalog[..32].chunks_exact(2).fold(0u16, |sum, word| {
        sum.wrapping_add(u16::from_le_bytes([word[0], word[1]]))
    });
    if catalog[0] != 1 || catalog[30..32] != [0x55, 0xaa] || checksum != 0 {
        return Err(PxeServiceError::InvalidConfig(
            "ISO has an invalid El Torito validation entry".into(),
        ));
    }
    let bios = parse_el_torito_entry(&catalog[32..64], 0x00)?;
    let mut uefi = None;
    let mut offset = 64usize;
    while offset + 32 <= catalog.len() {
        let entry = &catalog[offset..offset + 32];
        if matches!(entry[0], 0x90 | 0x91) {
            let platform = entry[1];
            let count = usize::from(u16::from_le_bytes([entry[2], entry[3]]));
            for index in 0..count {
                let start = offset + 32 * (index + 1);
                if start + 32 > catalog.len() {
                    return Err(PxeServiceError::InvalidConfig(
                        "El Torito boot catalog is truncated".into(),
                    ));
                }
                if platform == 0xef {
                    uefi = Some(parse_el_torito_entry(&catalog[start..start + 32], 0xef)?);
                }
            }
            offset += 32 * (count + 1);
            if entry[0] == 0x91 {
                break;
            }
        } else {
            offset += 32;
        }
    }
    let uefi = uefi.ok_or_else(|| {
        PxeServiceError::InvalidConfig("ISO has no x64 UEFI El Torito image".into())
    })?;
    let bios_bytes = read_iso_extent(&mut media, media_len, bios.1, u64::from(bios.0) * 512)?;
    let uefi_prefix = read_iso_extent(&mut media, media_len, uefi.1, 512)?;
    let uefi_size = fat_image_size(&uefi_prefix)?;
    let uefi_bytes = read_iso_extent(&mut media, media_len, uefi.1, uefi_size)?;
    Ok(ElToritoBootImages {
        bios: bios_bytes,
        bios_load_sectors: bios.0,
        uefi: uefi_bytes,
        uefi_load_sectors: uefi.0,
    })
}

fn parse_el_torito_entry(entry: &[u8], platform: u8) -> Result<(u16, u32), PxeServiceError> {
    if entry.len() != 32 || entry[0] != 0x88 || entry[1] != 0 {
        return Err(PxeServiceError::InvalidConfig(format!(
            "ISO {platform:#04x} El Torito entry is not bootable no-emulation media"
        )));
    }
    let sectors = u16::from_le_bytes([entry[6], entry[7]]);
    let lba = u32::from_le_bytes(entry[8..12].try_into().unwrap());
    if sectors == 0 || lba == 0 {
        return Err(PxeServiceError::InvalidConfig(
            "El Torito entry has an empty boot image".into(),
        ));
    }
    Ok((sectors, lba))
}

fn fat_image_size(boot_sector: &[u8]) -> Result<u64, PxeServiceError> {
    if boot_sector.len() < 512 || boot_sector[510..512] != [0x55, 0xaa] {
        return Err(PxeServiceError::InvalidConfig(
            "UEFI El Torito image has no valid FAT boot sector".into(),
        ));
    }
    let bytes_per_sector = u64::from(u16::from_le_bytes([boot_sector[11], boot_sector[12]]));
    let short_sectors = u64::from(u16::from_le_bytes([boot_sector[19], boot_sector[20]]));
    let long_sectors = u64::from(u32::from_le_bytes(boot_sector[32..36].try_into().unwrap()));
    let sectors = if short_sectors != 0 {
        short_sectors
    } else {
        long_sectors
    };
    let size = bytes_per_sector.checked_mul(sectors).ok_or_else(|| {
        PxeServiceError::InvalidConfig("UEFI El Torito image size overflows".into())
    })?;
    if !matches!(bytes_per_sector, 512 | 1024 | 2048 | 4096)
        || size < 512
        || size > 64 * 1024 * 1024
    {
        return Err(PxeServiceError::InvalidConfig(
            "UEFI El Torito FAT image has an invalid size".into(),
        ));
    }
    Ok(size)
}

fn read_iso_extent(
    media: &mut File,
    media_len: u64,
    lba: u32,
    size: u64,
) -> Result<Vec<u8>, PxeServiceError> {
    let offset = u64::from(lba)
        .checked_mul(2048)
        .ok_or_else(|| PxeServiceError::InvalidConfig("El Torito image offset overflows".into()))?;
    let end = offset
        .checked_add(size)
        .ok_or_else(|| PxeServiceError::InvalidConfig("El Torito image length overflows".into()))?;
    if end > media_len || size > 64 * 1024 * 1024 {
        return Err(PxeServiceError::InvalidConfig(
            "El Torito image is outside the ISO".into(),
        ));
    }
    let mut bytes = vec![
        0u8;
        usize::try_from(size).map_err(|_| {
            PxeServiceError::InvalidConfig("El Torito image is too large".into())
        })?
    ];
    media.seek(SeekFrom::Start(offset))?;
    media.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn remaster_wepe_iso(
    extracted: &Path,
    boot_images: &ElToritoBootImages,
    destination: &Path,
) -> Result<(), PxeServiceError> {
    let bios_name = "EDM_BIOS.IMG";
    let uefi_name = "EDM_UEFI.IMG";
    let catalog_name = "boot.catalog";
    fs::write(extracted.join(bios_name), &boot_images.bios)?;
    fs::write(extracted.join(uefi_name), &boot_images.uefi)?;
    fs::write(extracted.join(catalog_name), vec![0u8; 2048])?;
    let input = InputTree::from_fs(extracted, PathSeparator::ForwardSlash).map_err(|error| {
        PxeServiceError::InvalidConfig(format!("could not stage remastered ISO: {error}"))
    })?;
    let options = IsoFormatOptions {
        volume_name: "EASYDEPLOYMESH_WEPE".into(),
        system_id: None,
        volume_set_id: None,
        publisher_id: None,
        preparer_id: Some("EasyDeployMesh".into()),
        application_id: Some("EasyDeployMesh managed WinPE".into()),
        sector_size: 2048,
        path_separator: PathSeparator::ForwardSlash,
        features: CreationFeatures {
            filenames: BaseIsoLevel::Level2 {
                supports_lowercase: true,
                supports_rrip: false,
            },
            long_filenames: false,
            joliet: Some(JolietLevel::Level3),
            rock_ridge: None,
            el_torito: Some(BootOptions {
                write_boot_catalog: true,
                default: BootEntryOptions {
                    load_size: std::num::NonZeroU16::new(boot_images.bios_load_sectors),
                    boot_image_path: bios_name.into(),
                    boot_info_table: false,
                    grub2_boot_info: false,
                    emulation: EmulationType::NoEmulation,
                },
                entries: vec![(
                    BootSectionOptions {
                        platform: PlatformId::UEFI,
                    },
                    BootEntryOptions {
                        load_size: std::num::NonZeroU16::new(boot_images.uefi_load_sectors),
                        boot_image_path: uefi_name.into(),
                        boot_info_table: false,
                        grub2_boot_info: false,
                        emulation: EmulationType::NoEmulation,
                    },
                )],
            }),
            hybrid_boot: None,
        },
        strict_charset: false,
    };
    let output = fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(destination)?;
    IsoImageWriter::create(output, input, options).map_err(|error| {
        PxeServiceError::InvalidConfig(format!(
            "could not create remastered ISO: {error} ({error:?})"
        ))
    })?;
    let _ = fs::remove_file(extracted.join(bios_name));
    let _ = fs::remove_file(extracted.join(uefi_name));
    let _ = fs::remove_file(extracted.join(catalog_name));
    Ok(())
}

fn copy_iso_directory<B: gpt_disk_io::BlockIo>(
    media: &mut B,
    extent_lba: u32,
    extent_len: u32,
    target: &Path,
) -> Result<(), PxeServiceError> {
    fs::create_dir_all(target)?;
    let entries: Vec<_> = iso9660::DirectoryIterator::new(media, extent_lba, extent_len)
        .collect::<Result<_, _>>()
        .map_err(|error| {
            PxeServiceError::InvalidConfig(format!("could not read ISO directory: {error}"))
        })?;
    for entry in entries {
        if matches!(entry.name.as_str(), "." | "..") {
            continue;
        }
        let destination = target.join(&entry.name);
        if entry.flags.directory {
            copy_iso_directory(media, entry.extent_lba, entry.data_length, &destination)?
        } else {
            let mut reader = iso9660::FileReader::new(media, entry);
            let mut output = File::create(destination)?;
            let mut buffer = vec![0u8; 128 * 1024];
            loop {
                let count = reader.read(&mut buffer).map_err(|error| {
                    PxeServiceError::InvalidConfig(format!("could not extract ISO file: {error}"))
                })?;
                if count == 0 {
                    break;
                }
                output.write_all(&buffer[..count])?
            }
        }
    }
    Ok(())
}

struct PartitionReader {
    file: File,
    start: u64,
    length: u64,
    position: u64,
}
impl Read for PartitionReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.position >= self.length {
            return Ok(0);
        }
        let count = buffer.len().min((self.length - self.position) as usize);
        self.file
            .seek(SeekFrom::Start(self.start + self.position))?;
        let read = self.file.read(&mut buffer[..count])?;
        self.position += read as u64;
        Ok(read)
    }
}
impl Write for PartitionReader {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "image is read-only",
        ))
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl Seek for PartitionReader {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        let next = match position {
            SeekFrom::Start(value) => value as i128,
            SeekFrom::Current(value) => self.position as i128 + value as i128,
            SeekFrom::End(value) => self.length as i128 + value as i128,
        };
        if next < 0 || next > self.length as i128 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek outside partition",
            ));
        }
        self.position = next as u64;
        Ok(self.position)
    }
}

fn extract_fat_image(source: &Path, target: &Path) -> Result<(), PxeServiceError> {
    let mut file = File::open(source)?;
    let mut mbr = [0u8; 512];
    file.read_exact(&mut mbr)?;
    if &mbr[510..512] != b"\x55\xAA" {
        return Err(PxeServiceError::InvalidConfig(
            "IMG has no valid MBR".into(),
        ));
    }
    let partition = &mbr[446..462];
    let kind = partition[4];
    if !matches!(kind, 0x0b | 0x0c | 0x0e) {
        return Err(PxeServiceError::InvalidConfig(format!(
            "IMG first partition is not FAT (type {kind:#04x})"
        )));
    }
    let start = u32::from_le_bytes(partition[8..12].try_into().unwrap()) as u64 * 512;
    let length = u32::from_le_bytes(partition[12..16].try_into().unwrap()) as u64 * 512;
    let reader = PartitionReader {
        file,
        start,
        length,
        position: 0,
    };
    let filesystem = fatfs::FileSystem::new(reader, fatfs::FsOptions::new()).map_err(|error| {
        PxeServiceError::InvalidConfig(format!("could not read IMG FAT partition: {error}"))
    })?;
    copy_fat_directory(&filesystem.root_dir(), target)
}
fn copy_fat_directory<T: fatfs::ReadWriteSeek>(
    directory: &fatfs::Dir<'_, T>,
    target: &Path,
) -> Result<(), PxeServiceError> {
    fs::create_dir_all(target)?;
    for entry in directory.iter() {
        let entry = entry?;
        let name = entry.file_name();
        if matches!(name.as_str(), "." | "..") {
            continue;
        }
        let destination = target.join(&name);
        if entry.is_dir() {
            copy_fat_directory(&entry.to_dir(), &destination)?
        } else if entry.is_file() {
            let mut input = entry.to_file();
            let mut output = File::create(destination)?;
            io::copy(&mut input, &mut output)?;
        }
    }
    Ok(())
}

#[cfg(test)]
fn build_winpe_package(
    extracted: &Path,
    managed_root: &Path,
    agent: Option<&Path>,
) -> Result<BootPackage, PxeServiceError> {
    build_winpe_package_with_native_iso(extracted, managed_root, agent, None)
}

fn build_winpe_package_with_native_iso(
    extracted: &Path,
    managed_root: &Path,
    agent: Option<&Path>,
    native_boot_images: Option<&ElToritoBootImages>,
) -> Result<BootPackage, PxeServiceError> {
    let files = walk_files(extracted)?;
    let wims = files
        .iter()
        .filter(|path| {
            path.extension()
                .is_some_and(|value| value.eq_ignore_ascii_case("wim"))
        })
        .collect::<Vec<_>>();
    let preferred_wim = [["sources", "boot.wim"], ["boot", "boot.wim"]]
        .iter()
        .find_map(|expected| {
            wims.iter().copied().find(|path| {
                path.strip_prefix(extracted).is_ok_and(|relative| {
                    path_components_eq_ascii_case_insensitive(relative, expected)
                })
            })
        });
    let bcd_wim = find_unique_uefi_bcd_reference(extracted, &files, "wim");
    let boot_wims = wims
        .iter()
        .copied()
        .filter(|wim| wim_has_boot_image(wim))
        .collect::<Vec<_>>();
    let wim = match (
        preferred_wim.or(bcd_wim),
        boot_wims.as_slice(),
        wims.as_slice(),
    ) {
        (Some(wim), _, _) => wim,
        (None, [wim], _) => *wim,
        (None, _, [wim]) => *wim,
        (None, _, []) => {
            return Err(PxeServiceError::InvalidConfig(
                "media contains no WinPE WIM image".into(),
            ));
        }
        (None, _, _) => {
            return Err(PxeServiceError::InvalidConfig(
                "media contains multiple WIM images but no unique WinPE boot image".into(),
            ));
        }
    };
    let loader_path = detect_winpe_loader_path(wim)?;
    let bootmgr = find_named(&files, "bootmgr")
        .ok_or_else(|| PxeServiceError::MissingBootFile("bootmgr".into()))?;
    let sdi = find_named(&files, "boot.sdi")
        .or_else(|| find_unique_uefi_bcd_reference(extracted, &files, "sdi"))
        .or_else(|| find_unique_file_with_extension(&files, "sdi"))
        .ok_or_else(|| PxeServiceError::MissingBootFile("boot.sdi".into()))?;
    let staging = managed_root
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!(".pxe-package-{}", uuid::Uuid::new_v4()));
    let populate = (|| -> Result<(), PxeServiceError> {
        fs::create_dir_all(staging.join("boot"))?;
        let bios_bcd = staging.join("boot/BCD");
        write_clean_winpe_bcd(&bios_bcd, loader_path)?;
        fs::copy(&bios_bcd, staging.join("boot/easydeploymesh.bcd"))?;
        let staged_wim = staging.join("boot/boot.wim");
        fs::copy(wim, &staged_wim)?;
        if let Some(agent) = agent {
            BootPackage::ensure_agent_runtime(&staged_wim, agent)?;
        }
        fs::copy(bootmgr, staging.join("boot/bootmgr"))?;
        fs::copy(sdi, staging.join("boot/boot.sdi"))?;
        fs::write(staging.join("undionly.kpxe"), EMBEDDED_UNDIONLY)?;
        fs::write(staging.join("ipxe.efi"), EMBEDDED_IPXE_EFI)?;
        fs::write(staging.join("boot/wimboot"), EMBEDDED_WIMBOOT)?;
        if let Some(boot_images) = native_boot_images {
            remaster_wepe_iso(extracted, boot_images, &staging.join(NATIVE_ISO_PATH))?;
            fs::write(staging.join("boot.ipxe"), NATIVE_ISO_PLACEHOLDER_SCRIPT)?;
            fs::write(staging.join(NATIVE_ISO_LAYOUT_MARKER), b"2\n")?;
        } else {
            fs::write(staging.join("boot.ipxe"), MANAGED_IPXE_SCRIPT)?;
            fs::write(staging.join(MANAGED_LAYOUT_MARKER), b"4\n")?;
        }
        Ok(())
    })();
    if let Err(error) = populate {
        let _ = fs::remove_dir_all(&staging);
        return Err(error);
    }
    let bios_boot_file = "undionly.kpxe".to_owned();
    if managed_root.exists() {
        let backup = managed_root.with_extension(format!("old-{}", uuid::Uuid::new_v4()));
        if let Err(error) = fs::rename(managed_root, &backup) {
            let _ = fs::remove_dir_all(&staging);
            return Err(error.into());
        }
        if let Err(error) = fs::rename(&staging, managed_root) {
            let _ = fs::rename(backup, managed_root);
            let _ = fs::remove_dir_all(&staging);
            return Err(error.into());
        }
        let _ = fs::remove_dir_all(backup);
    } else if let Err(error) = fs::rename(&staging, managed_root) {
        let _ = fs::remove_dir_all(&staging);
        return Err(error.into());
    }
    Ok(BootPackage {
        root: managed_root.to_string_lossy().into_owned(),
        bios_boot_file,
        uefi_x64_boot_file: "ipxe.efi".into(),
    })
}

fn find_unique_file_with_extension<'a>(
    files: &'a [PathBuf],
    extension: &str,
) -> Option<&'a PathBuf> {
    let mut matches = files.iter().filter(|path| {
        path.extension()
            .is_some_and(|value| value.eq_ignore_ascii_case(extension))
    });
    let candidate = matches.next()?;
    matches.next().is_none().then_some(candidate)
}

fn find_unique_uefi_bcd_reference<'a>(
    extracted: &Path,
    files: &'a [PathBuf],
    extension: &str,
) -> Option<&'a PathBuf> {
    let candidates = files
        .iter()
        .filter(|path| {
            path.extension()
                .is_some_and(|value| value.eq_ignore_ascii_case(extension))
        })
        .collect::<Vec<_>>();
    let uefi_bcds = files.iter().filter(|path| {
        path.strip_prefix(extracted).is_ok_and(|relative| {
            path_components_eq_ascii_case_insensitive(
                relative,
                &["efi", "microsoft", "boot", "bcd"],
            )
        })
    });
    let mut referenced = Vec::new();
    for bcd in uefi_bcds {
        let Ok(contents) = fs::read(bcd) else {
            continue;
        };
        let strings = utf16le_ascii_strings(&contents);
        for candidate in &candidates {
            let Ok(relative) = candidate.strip_prefix(extracted) else {
                continue;
            };
            let relative = relative
                .components()
                .map(|component| component.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("\\");
            if strings.iter().any(|value| {
                value
                    .trim_start_matches(['\\', '/'])
                    .eq_ignore_ascii_case(&relative)
            }) && !referenced.contains(candidate)
            {
                referenced.push(*candidate);
            }
        }
    }
    if referenced.len() == 1 {
        Some(referenced[0])
    } else {
        None
    }
}

fn utf16le_ascii_strings(contents: &[u8]) -> Vec<String> {
    let mut strings = Vec::new();
    for offset in 0..=1 {
        let mut current = String::new();
        let mut index = offset;
        while index + 1 < contents.len() {
            let low = contents[index];
            let high = contents[index + 1];
            if high == 0 && (0x20..=0x7e).contains(&low) {
                current.push(char::from(low));
            } else {
                if current.len() >= 4 {
                    strings.push(std::mem::take(&mut current));
                } else {
                    current.clear();
                }
            }
            index += 2;
        }
        if current.len() >= 4 {
            strings.push(current);
        }
    }
    strings
}

const EASYDEPLOYMESH_STARTNET: &str = "@echo off\r\n\
wpeinit\r\n\
if not exist X:\\EasyDeployMesh\\shell-hook.enabled start \"\" /b cmd.exe /d /c \"X:\\EasyDeployMesh\\easydeploymesh-agent.exe --bootstrap X:\\EasyDeployMesh\\easydeploymesh-bootstrap.json >> X:\\EasyDeployMesh\\easydeploymesh-agent.log 2>&1\"\r\n\
if exist X:\\Windows\\System32\\startnet.easydeploymesh-original.cmd call X:\\Windows\\System32\\startnet.easydeploymesh-original.cmd\r\n";

const EASYDEPLOYMESH_RUNTIME_COLLECTOR: &[u8] =
    include_bytes!("../../../scripts/collect-winpe-runtime.cmd");

#[cfg(any(target_os = "windows", test))]
const EASYDEPLOYMESH_SHELL_LAUNCHER: &[u8] = b"X:\\EasyDeployMesh\\easydeploymesh-shell.exe\r\n";

#[cfg(any(target_os = "windows", test))]
struct PatchedWinpeshl {
    ini: Vec<u8>,
    original_shell_script: Option<Vec<u8>>,
}

#[cfg(any(target_os = "windows", test))]
fn patch_winpeshl_for_agent(input: &[u8]) -> Result<PatchedWinpeshl, PxeServiceError> {
    let mut lines = Vec::new();
    let mut start = 0;
    for (index, byte) in input.iter().enumerate() {
        if *byte == b'\n' {
            lines.push(&input[start..=index]);
            start = index + 1;
        }
    }
    if start < input.len() {
        lines.push(&input[start..]);
    }

    let normalized = |line: &[u8]| {
        line.iter()
            .copied()
            .filter(|byte| !matches!(byte, b' ' | b'\t' | b'\r' | b'\n'))
            .map(|byte| byte.to_ascii_lowercase())
            .collect::<Vec<_>>()
    };

    if let Some(section) = lines
        .iter()
        .position(|line| normalized(line) == b"[launchapps]")
    {
        if lines
            .iter()
            .skip(section + 1)
            .take_while(|line| !normalized(line).starts_with(b"["))
            .any(|line| normalized(line) == b"x:\\easydeploymesh\\easydeploymesh-shell.exe")
        {
            return Ok(PatchedWinpeshl {
                ini: input.to_vec(),
                original_shell_script: None,
            });
        }
        let mut ini = Vec::with_capacity(input.len() + EASYDEPLOYMESH_SHELL_LAUNCHER.len());
        for (index, line) in lines.iter().enumerate() {
            ini.extend_from_slice(line);
            if index == section {
                ini.extend_from_slice(EASYDEPLOYMESH_SHELL_LAUNCHER);
            }
        }
        return Ok(PatchedWinpeshl {
            ini,
            original_shell_script: None,
        });
    }

    let section = lines
        .iter()
        .position(|line| normalized(line) == b"[launchapp]")
        .ok_or_else(|| {
            PxeServiceError::InvalidConfig(
                "WinPE winpeshl.ini has neither [LaunchApp] nor [LaunchApps]".into(),
            )
        })?;
    let app_path_line = lines
        .iter()
        .enumerate()
        .skip(section + 1)
        .take_while(|(_, line)| !normalized(line).starts_with(b"["))
        .find(|(_, line)| {
            let normalized = normalized(line);
            normalized.starts_with(b"apppath=")
        })
        .map(|(index, _)| index)
        .ok_or_else(|| PxeServiceError::InvalidConfig("WinPE [LaunchApp] has no AppPath".into()))?;
    let line = lines[app_path_line];
    let equals = line.iter().position(|byte| *byte == b'=').unwrap();
    let command = line[equals + 1..]
        .iter()
        .copied()
        .skip_while(|byte| matches!(byte, b' ' | b'\t'))
        .collect::<Vec<_>>();
    let command_end = command
        .iter()
        .rposition(|byte| !matches!(byte, b'\r' | b'\n' | b' ' | b'\t'))
        .map(|index| index + 1)
        .unwrap_or(0);
    if command_end == 0 {
        return Err(PxeServiceError::InvalidConfig(
            "WinPE [LaunchApp] AppPath is empty".into(),
        ));
    }
    if normalized(&command[..command_end]) == b"x:\\easydeploymesh\\easydeploymesh-shell.exe" {
        return Ok(PatchedWinpeshl {
            ini: input.to_vec(),
            original_shell_script: None,
        });
    }
    let mut original_shell_script = b"@echo off\r\n".to_vec();
    original_shell_script.extend_from_slice(&command[..command_end]);
    original_shell_script.extend_from_slice(b"\r\n");

    let mut ini = Vec::with_capacity(input.len());
    for (index, line) in lines.iter().enumerate() {
        if index == app_path_line {
            ini.extend_from_slice(b"AppPath = X:\\EasyDeployMesh\\easydeploymesh-shell.exe\r\n");
        } else {
            ini.extend_from_slice(line);
        }
    }
    Ok(PatchedWinpeshl {
        ini,
        original_shell_script: Some(original_shell_script),
    })
}

#[cfg(all(target_os = "windows", not(test)))]
fn inject_bootstrap_into_winpe(wim: &Path, bootstrap: &[u8]) -> Result<(), PxeServiceError> {
    if !wim.is_file() {
        return Err(PxeServiceError::MissingBootFile(
            wim.to_string_lossy().into_owned(),
        ));
    }
    let mut header = [0_u8; WIM_BOOT_INDEX_OFFSET + 4];
    File::open(wim)?.read_exact(&mut header)?;
    let boot_index = wim_boot_index_from_header(&header).ok_or_else(|| {
        PxeServiceError::InvalidConfig(format!("{} has no valid boot image index", wim.display()))
    })?;
    let mount = wim.parent().unwrap_or_else(|| Path::new(".")).join(format!(
        ".easydeploymesh-bootstrap-mount-{}",
        uuid::Uuid::new_v4()
    ));
    fs::create_dir_all(&mount)?;

    let result = (|| -> Result<(), PxeServiceError> {
        run_dism(&[
            "/English".into(),
            "/Mount-Image".into(),
            os_argument("/ImageFile:", wim),
            format!("/Index:{boot_index}").into(),
            os_argument("/MountDir:", &mount),
        ])?;
        let easydeploymesh = mount.join("EasyDeployMesh");
        fs::create_dir_all(&easydeploymesh)?;
        fs::write(
            easydeploymesh.join("easydeploymesh-bootstrap.json"),
            bootstrap,
        )?;
        fs::write(
            easydeploymesh.join("collect-winpe-runtime.cmd"),
            EASYDEPLOYMESH_RUNTIME_COLLECTOR,
        )?;
        run_dism(&[
            "/English".into(),
            "/Unmount-Image".into(),
            os_argument("/MountDir:", &mount),
            "/Commit".into(),
        ])?;
        Ok(())
    })();

    if result.is_err() {
        let _ = run_dism(&[
            "/English".into(),
            "/Unmount-Image".into(),
            os_argument("/MountDir:", &mount),
            "/Discard".into(),
        ]);
    }
    let _ = fs::remove_dir_all(&mount);
    result
}

#[cfg(any(not(target_os = "windows"), test))]
fn inject_bootstrap_into_winpe(_wim: &Path, _bootstrap: &[u8]) -> Result<(), PxeServiceError> {
    Err(PxeServiceError::InvalidConfig(
        "injecting the Agent bootstrap into WinPE requires Windows DISM".into(),
    ))
}

#[cfg(all(target_os = "windows", not(test)))]
fn refresh_native_iso_bootstrap(
    package_root: &Path,
    bootstrap: &[u8],
) -> Result<(), PxeServiceError> {
    let iso = package_root.join(NATIVE_ISO_PATH);
    let boot_images = read_el_torito_boot_images(&iso)?;
    let workspace = package_root.join(format!(
        ".easydeploymesh-native-remaster-{}",
        uuid::Uuid::new_v4()
    ));
    let extracted = workspace.join("tree");
    fs::create_dir_all(&extracted)?;
    let result = (|| -> Result<(), PxeServiceError> {
        extract_iso(&iso, &extracted)?;
        let native_wim = find_path_case_insensitive(&extracted, &["wepe", "wepe64.wim"])
            .ok_or_else(|| PxeServiceError::MissingBootFile("WEPE/WEPE64.WIM".into()))?;
        inject_bootstrap_into_winpe(&native_wim, bootstrap)?;
        let replacement = workspace.join("native.iso");
        remaster_wepe_iso(&extracted, &boot_images, &replacement)?;
        let backup = workspace.join("previous.iso");
        fs::rename(&iso, &backup)?;
        if let Err(error) = fs::rename(&replacement, &iso) {
            let _ = fs::rename(&backup, &iso);
            return Err(error.into());
        }
        Ok(())
    })();
    let _ = fs::remove_dir_all(&workspace);
    result
}

#[cfg(any(not(target_os = "windows"), test))]
fn refresh_native_iso_bootstrap(
    _package_root: &Path,
    _bootstrap: &[u8],
) -> Result<(), PxeServiceError> {
    Err(PxeServiceError::InvalidConfig(
        "remastering the managed native ISO requires Windows DISM".into(),
    ))
}

#[cfg(all(target_os = "windows", not(test)))]
fn inject_agent_into_winpe(wim: &Path, agent: &Path) -> Result<(), PxeServiceError> {
    if !agent.is_file() {
        return Err(PxeServiceError::MissingBootFile(
            agent.to_string_lossy().into_owned(),
        ));
    }
    let mut header = [0_u8; WIM_BOOT_INDEX_OFFSET + 4];
    File::open(wim)?.read_exact(&mut header)?;
    let boot_index = wim_boot_index_from_header(&header).ok_or_else(|| {
        PxeServiceError::InvalidConfig(format!("{} has no valid boot image index", wim.display()))
    })?;
    let mount = wim
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!(".easydeploymesh-mount-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&mount)?;

    let result = (|| -> Result<(), PxeServiceError> {
        run_dism(&[
            "/English".into(),
            "/Mount-Image".into(),
            os_argument("/ImageFile:", wim),
            format!("/Index:{boot_index}").into(),
            os_argument("/MountDir:", &mount),
        ])?;
        let easydeploymesh = mount.join("EasyDeployMesh");
        fs::create_dir_all(&easydeploymesh)?;
        fs::copy(agent, easydeploymesh.join("easydeploymesh-agent.exe"))?;
        fs::copy(agent, easydeploymesh.join("easydeploymesh-shell.exe"))?;
        fs::write(
            easydeploymesh.join("collect-winpe-runtime.cmd"),
            EASYDEPLOYMESH_RUNTIME_COLLECTOR,
        )?;

        let system32 = mount.join("Windows/System32");
        let winpeshl = system32.join("winpeshl.ini");
        let mut shell_hook_installed = false;
        if winpeshl.is_file() {
            let patched = patch_winpeshl_for_agent(&fs::read(&winpeshl)?)?;
            fs::write(&winpeshl, patched.ini)?;
            shell_hook_installed = true;
            if let Some(script) = patched.original_shell_script {
                fs::write(
                    easydeploymesh.join("easydeploymesh-original-shell.cmd"),
                    script,
                )?;
            }
        }
        // EasyU can expose both winpeshl.ini and Setup\CmdLine. Hooking both
        // launches two vendor shells and overwrites the command preserved from
        // winpeshl.ini; EXLOAD can then run before PECMD establishes its PE
        // environment. Setup\CmdLine is only a fallback for images without a
        // usable winpeshl.ini hook.
        if should_patch_setup_cmdline(shell_hook_installed)
            && patch_setup_cmdline_for_agent(&mount, &easydeploymesh)?
        {
            shell_hook_installed = true;
        }
        if shell_hook_installed {
            fs::write(easydeploymesh.join("shell-hook.enabled"), b"enabled\r\n")?;
        }
        let startnet = system32.join("startnet.cmd");
        let original = system32.join("startnet.easydeploymesh-original.cmd");
        if startnet.is_file() && !original.exists() {
            fs::rename(&startnet, &original)?;
        }
        fs::write(&startnet, EASYDEPLOYMESH_STARTNET)?;
        run_dism(&[
            "/English".into(),
            "/Unmount-Image".into(),
            os_argument("/MountDir:", &mount),
            "/Commit".into(),
        ])?;
        Ok(())
    })();

    if result.is_err() {
        let _ = run_dism(&[
            "/English".into(),
            "/Unmount-Image".into(),
            os_argument("/MountDir:", &mount),
            "/Discard".into(),
        ]);
    }
    let _ = fs::remove_dir_all(&mount);
    result
}

#[cfg(any(target_os = "windows", test))]
fn should_patch_setup_cmdline(winpeshl_hook_installed: bool) -> bool {
    !winpeshl_hook_installed
}

#[cfg(all(target_os = "windows", not(test)))]
fn patch_setup_cmdline_for_agent(
    mount: &Path,
    easydeploymesh: &Path,
) -> Result<bool, PxeServiceError> {
    let hive = mount.join("Windows/System32/config/SYSTEM");
    if !hive.is_file() {
        return Ok(false);
    }
    let hive_name = format!("EDM_{}", uuid::Uuid::new_v4().simple());
    let hive_key = format!(r"HKLM\{hive_name}");
    run_reg(&[
        "load".into(),
        hive_key.clone().into(),
        hive.into_os_string(),
    ])?;

    let result = (|| -> Result<bool, PxeServiceError> {
        let setup_key = format!(r"{hive_key}\Setup");
        let query = background_command("reg.exe")
            .args(["query", &setup_key, "/v", "CmdLine"])
            .output()?;
        if !query.status.success() {
            return Ok(false);
        }
        let Some(command) = reg_sz_value(&query.stdout, "CmdLine") else {
            return Ok(false);
        };
        if command.eq_ignore_ascii_case(r"X:\EasyDeployMesh\easydeploymesh-shell.exe") {
            return Ok(true);
        }
        let mut script = b"@echo off\r\n".to_vec();
        script.extend_from_slice(command.as_bytes());
        script.extend_from_slice(b"\r\n");
        fs::write(
            easydeploymesh.join("easydeploymesh-original-shell.cmd"),
            script,
        )?;
        run_reg(&[
            "add".into(),
            setup_key.into(),
            "/v".into(),
            "CmdLine".into(),
            "/t".into(),
            "REG_SZ".into(),
            "/d".into(),
            r"X:\EasyDeployMesh\easydeploymesh-shell.exe".into(),
            "/f".into(),
        ])?;
        Ok(true)
    })();

    let unload = run_reg(&["unload".into(), hive_key.into()]);
    match (result, unload) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Ok(installed), Ok(_)) => Ok(installed),
    }
}

#[cfg(any(target_os = "windows", test))]
fn reg_sz_value(output: &[u8], name: &str) -> Option<String> {
    let text = String::from_utf8_lossy(output);
    text.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        if !fields.next()?.eq_ignore_ascii_case(name)
            || !fields.next()?.eq_ignore_ascii_case("REG_SZ")
        {
            return None;
        }
        let value = fields.collect::<Vec<_>>().join(" ");
        (!value.is_empty()).then_some(value)
    })
}

#[cfg(any(not(target_os = "windows"), test))]
fn inject_agent_into_winpe(_wim: &Path, _agent: &Path) -> Result<(), PxeServiceError> {
    Err(PxeServiceError::InvalidConfig(
        "injecting EasyDeployMesh Agent into WinPE requires Windows DISM".into(),
    ))
}

#[cfg(all(target_os = "windows", not(test)))]
fn os_argument(prefix: &str, path: &Path) -> std::ffi::OsString {
    let mut argument = std::ffi::OsString::from(prefix);
    argument.push(path.as_os_str());
    argument
}

#[cfg(all(target_os = "windows", not(test)))]
fn run_dism(arguments: &[std::ffi::OsString]) -> Result<std::process::Output, PxeServiceError> {
    let output = background_command("dism.exe").args(arguments).output()?;
    if output.status.success() {
        return Ok(output);
    }
    Err(PxeServiceError::InvalidConfig(format!(
        "DISM command failed ({}): {} {}",
        arguments
            .iter()
            .map(|argument| argument.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" "),
        command_output_text(&output.stdout),
        command_output_text(&output.stderr)
    )))
}

#[cfg(all(target_os = "windows", not(test)))]
fn run_reg(arguments: &[std::ffi::OsString]) -> Result<std::process::Output, PxeServiceError> {
    let output = background_command("reg.exe").args(arguments).output()?;
    if output.status.success() {
        return Ok(output);
    }
    Err(PxeServiceError::InvalidConfig(format!(
        "offline WinPE registry command failed: {} {}",
        command_output_text(&output.stdout),
        command_output_text(&output.stderr)
    )))
}

#[cfg(any(target_os = "windows", test))]
fn winpe_loader_path_from_listing(listing: &[u8]) -> Option<&'static str> {
    if output_has_ascii_line_case_insensitive(listing, WINPE_STANDARD_LOADER_PATH) {
        Some(WINPE_STANDARD_LOADER_PATH)
    } else if output_has_ascii_line_case_insensitive(listing, WINPE_SYSTEM32_BOOT_LOADER_PATH) {
        Some(WINPE_SYSTEM32_BOOT_LOADER_PATH)
    } else {
        None
    }
}

fn wim_boot_index_from_header(header: &[u8]) -> Option<u32> {
    if header.len() < WIM_BOOT_INDEX_OFFSET + 4 || !header.starts_with(WIM_SIGNATURE) {
        return None;
    }
    let header_size = u32::from_le_bytes(
        header[WIM_HEADER_SIZE_OFFSET..WIM_HEADER_SIZE_OFFSET + 4]
            .try_into()
            .ok()?,
    ) as usize;
    let part_number = u16::from_le_bytes(
        header[WIM_PART_NUMBER_OFFSET..WIM_PART_NUMBER_OFFSET + 2]
            .try_into()
            .ok()?,
    );
    let total_parts = u16::from_le_bytes(
        header[WIM_TOTAL_PARTS_OFFSET..WIM_TOTAL_PARTS_OFFSET + 2]
            .try_into()
            .ok()?,
    );
    let image_count = u32::from_le_bytes(
        header[WIM_IMAGE_COUNT_OFFSET..WIM_IMAGE_COUNT_OFFSET + 4]
            .try_into()
            .ok()?,
    );
    let boot_index = u32::from_le_bytes(
        header[WIM_BOOT_INDEX_OFFSET..WIM_BOOT_INDEX_OFFSET + 4]
            .try_into()
            .ok()?,
    );
    (header_size >= WIM_BOOT_INDEX_OFFSET + 4
        && part_number == 1
        && total_parts == 1
        && boot_index > 0
        && boot_index <= image_count)
        .then_some(boot_index)
}

fn wim_has_boot_image(wim: &Path) -> bool {
    let mut header = [0_u8; WIM_BOOT_INDEX_OFFSET + 4];
    File::open(wim)
        .and_then(|mut file| file.read_exact(&mut header))
        .is_ok()
        && wim_boot_index_from_header(&header).is_some()
}

#[cfg(any(target_os = "windows", test))]
fn dism_list_image_args(wim: &Path, boot_index: u32) -> Vec<std::ffi::OsString> {
    let mut image_argument = std::ffi::OsString::from("/ImageFile:");
    image_argument.push(wim.as_os_str());
    vec![
        "/English".into(),
        "/List-Image".into(),
        image_argument,
        format!("/Index:{boot_index}").into(),
    ]
}

#[cfg(all(target_os = "windows", not(test)))]
fn detect_winpe_loader_path(wim: &Path) -> Result<&'static str, PxeServiceError> {
    let mut header = [0_u8; WIM_BOOT_INDEX_OFFSET + 4];
    File::open(wim)?.read_exact(&mut header)?;
    let boot_index = wim_boot_index_from_header(&header).ok_or_else(|| {
        PxeServiceError::InvalidConfig(format!("{} has no valid boot image index", wim.display()))
    })?;

    let output = background_command("dism.exe")
        .args(dism_list_image_args(wim, boot_index))
        .output()?;
    if !output.status.success() {
        return Err(PxeServiceError::InvalidConfig(format!(
            "DISM could not inspect WinPE boot image {boot_index} ({}): {} {}",
            output.status,
            command_output_text(&output.stdout),
            command_output_text(&output.stderr)
        )));
    }
    winpe_loader_path_from_listing(&output.stdout).ok_or_else(|| {
        PxeServiceError::InvalidConfig(format!(
            "WinPE boot image {boot_index} contains neither {} nor {}",
            WINPE_STANDARD_LOADER_PATH, WINPE_SYSTEM32_BOOT_LOADER_PATH
        ))
    })
}

#[cfg(test)]
fn detect_winpe_loader_path(wim: &Path) -> Result<&'static str, PxeServiceError> {
    let listing = fs::read(wim)?;
    Ok(winpe_loader_path_from_listing(&listing).unwrap_or(WINPE_STANDARD_LOADER_PATH))
}

#[cfg(all(not(target_os = "windows"), not(test)))]
fn detect_winpe_loader_path(_wim: &Path) -> Result<&'static str, PxeServiceError> {
    Err(PxeServiceError::InvalidConfig(
        "inspecting a WinPE boot image requires Windows dism.exe".into(),
    ))
}

#[cfg(any(target_os = "windows", test))]
fn winpe_bcd_commands(loader_path: &'static str) -> Vec<Vec<&'static str>> {
    vec![
        vec![
            "/create",
            "{ramdiskoptions}",
            "/d",
            "EasyDeployMesh RAM disk",
        ],
        vec!["/set", "{ramdiskoptions}", "ramdisksdidevice", "boot"],
        vec![
            "/set",
            "{ramdiskoptions}",
            "ramdisksdipath",
            r"\Boot\boot.sdi",
        ],
        vec![
            "/create",
            WINPE_LOADER_ID,
            "/d",
            "EasyDeployMesh Windows PE",
            "/application",
            "osloader",
        ],
        vec!["/set", WINPE_LOADER_ID, "device", WINPE_RAMDISK_DEVICE],
        vec!["/set", WINPE_LOADER_ID, "path", loader_path],
        vec!["/set", WINPE_LOADER_ID, "osdevice", WINPE_RAMDISK_DEVICE],
        vec!["/set", WINPE_LOADER_ID, "systemroot", r"\windows"],
        vec!["/set", WINPE_LOADER_ID, "detecthal", "Yes"],
        vec!["/set", WINPE_LOADER_ID, "winpe", "Yes"],
        vec!["/create", "{bootmgr}", "/d", "EasyDeployMesh Boot Manager"],
        vec!["/set", "{bootmgr}", "timeout", "0"],
        vec!["/set", "{bootmgr}", "default", WINPE_LOADER_ID],
        vec!["/displayorder", WINPE_LOADER_ID, "/addlast"],
    ]
}

#[cfg(all(target_os = "windows", not(test)))]
fn write_clean_winpe_bcd(path: &Path, loader_path: &'static str) -> Result<(), PxeServiceError> {
    run_bcdedit(path, &["/createstore"])?;
    for command in winpe_bcd_commands(loader_path) {
        run_bcdedit(path, &command)?;
    }
    if fs::metadata(path)
        .map(|metadata| metadata.len())
        .unwrap_or(0)
        == 0
    {
        return Err(PxeServiceError::InvalidConfig(
            "bcdedit created an empty WinPE BCD store".into(),
        ));
    }
    let enumeration = run_bcdedit(path, &["/enum", "all", "/v"])?;
    for expected in [
        WINPE_LOADER_ID,
        r"\Boot\boot.wim",
        r"\Boot\boot.sdi",
        loader_path,
    ] {
        if !output_contains_ascii_case_insensitive(&enumeration.stdout, expected) {
            return Err(PxeServiceError::InvalidConfig(format!(
                "the generated WinPE BCD store is missing {expected}"
            )));
        }
    }
    if output_contains_ascii_case_insensitive(&enumeration.stdout, r"\WEPE\B64") {
        return Err(PxeServiceError::InvalidConfig(
            "the generated WinPE BCD store still references \\WEPE\\B64".into(),
        ));
    }
    Ok(())
}

#[cfg(all(target_os = "windows", not(test)))]
fn run_bcdedit(path: &Path, command: &[&str]) -> Result<std::process::Output, PxeServiceError> {
    let mut process = background_command("bcdedit.exe");
    if command == ["/createstore"] {
        process.arg("/createstore").arg(path);
    } else {
        process.arg("/store").arg(path).args(command);
    }
    let output = process.output()?;
    if output.status.success() {
        return Ok(output);
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(PxeServiceError::InvalidConfig(format!(
        "BCDEdit command failed ({}): {} {}",
        command.join(" "),
        stdout.trim(),
        stderr.trim()
    )))
}

#[cfg(any(target_os = "windows", test))]
fn output_contains_ascii_case_insensitive(output: &[u8], expected: &str) -> bool {
    let normalized = ascii_lowercase_without_nuls(output);
    let expected = expected
        .bytes()
        .map(|byte| byte.to_ascii_lowercase())
        .collect::<Vec<_>>();
    normalized
        .windows(expected.len())
        .any(|window| window == expected)
}

#[cfg(any(target_os = "windows", test))]
fn output_has_ascii_line_case_insensitive(output: &[u8], expected: &str) -> bool {
    let normalized = ascii_lowercase_without_nuls(output);
    let expected = expected
        .bytes()
        .map(|byte| byte.to_ascii_lowercase())
        .collect::<Vec<_>>();
    normalized
        .split(|byte| *byte == b'\n')
        .map(|line| line.trim_ascii())
        .any(|line| line == expected)
}

#[cfg(any(target_os = "windows", test))]
fn ascii_lowercase_without_nuls(output: &[u8]) -> Vec<u8> {
    output
        .iter()
        .copied()
        .filter(|byte| *byte != 0)
        .map(|byte| byte.to_ascii_lowercase())
        .collect()
}

#[cfg(all(target_os = "windows", not(test)))]
fn command_output_text(output: &[u8]) -> String {
    let normalized = output
        .iter()
        .copied()
        .filter(|byte| *byte != 0)
        .collect::<Vec<_>>();
    String::from_utf8_lossy(&normalized).trim().to_owned()
}

#[cfg(test)]
fn write_clean_winpe_bcd(path: &Path, loader_path: &'static str) -> Result<(), PxeServiceError> {
    let commands = winpe_bcd_commands(loader_path)
        .into_iter()
        .map(|command| command.join(" "))
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(path, commands)?;
    Ok(())
}

#[cfg(all(not(target_os = "windows"), not(test)))]
fn write_clean_winpe_bcd(_path: &Path, _loader_path: &'static str) -> Result<(), PxeServiceError> {
    Err(PxeServiceError::InvalidConfig(
        "creating a Legacy BIOS WinPE package requires Windows bcdedit.exe".into(),
    ))
}
fn walk_files(root: &Path) -> Result<Vec<PathBuf>, io::Error> {
    fn visit(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), io::Error> {
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            if path.is_dir() {
                visit(&path, out)?
            } else if path.is_file() {
                out.push(path)
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    visit(root, &mut files)?;
    Ok(files)
}
fn path_components_eq_ascii_case_insensitive(path: &Path, expected: &[&str]) -> bool {
    let mut components = path.components();
    expected.iter().all(|expected| {
        matches!(
            components.next(),
            Some(Component::Normal(actual))
                if actual
                    .to_str()
                    .is_some_and(|actual| actual.eq_ignore_ascii_case(expected))
        )
    }) && components.next().is_none()
}
fn find_named<'a>(files: &'a [PathBuf], name: &str) -> Option<&'a PathBuf> {
    files
        .iter()
        .filter(|path| {
            path.file_name()
                .and_then(|value| value.to_str())
                .is_some_and(|value| value.eq_ignore_ascii_case(name))
        })
        .min_by_key(|path| path.components().count())
}

fn is_private_wepe_layout(extracted: &Path) -> bool {
    let bootmgr = find_path_case_insensitive(extracted, &["bootmgr"]);
    let private_loader = find_path_case_insensitive(extracted, &["wepe", "wepe64"]);
    let required_private_files = [
        ["wepe", "b64"],
        ["wepe", "wepe.sdi"],
        ["wepe", "wepe64.wim"],
    ];
    let (Some(bootmgr), Some(private_loader)) = (bootmgr, private_loader) else {
        return false;
    };
    if !required_private_files
        .iter()
        .all(|path| find_path_case_insensitive(extracted, path).is_some())
    {
        return false;
    }
    files_have_same_contents(&bootmgr, &private_loader).unwrap_or(false)
}

fn ensure_supported_pe_layout(extracted: &Path) -> Result<(), PxeServiceError> {
    if is_private_wepe_layout(extracted) {
        return Err(PxeServiceError::InvalidConfig(
            "WePE media is unsupported because its private boot chain cannot reliably start an Agent-injected deployment image"
                .into(),
        ));
    }
    Ok(())
}

fn find_path_case_insensitive(root: &Path, components: &[&str]) -> Option<PathBuf> {
    let mut current = root.to_path_buf();
    for expected in components {
        let entry = fs::read_dir(&current).ok()?.find_map(|entry| {
            let entry = entry.ok()?;
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.eq_ignore_ascii_case(expected))
                .then_some(entry.path())
        })?;
        current = entry;
    }
    current.is_file().then_some(current)
}

fn files_have_same_contents(left: &Path, right: &Path) -> Result<bool, io::Error> {
    if fs::metadata(left)?.len() != fs::metadata(right)?.len() {
        return Ok(false);
    }
    let mut left = File::open(left)?;
    let mut right = File::open(right)?;
    let mut left_buffer = [0u8; 64 * 1024];
    let mut right_buffer = [0u8; 64 * 1024];
    loop {
        let left_read = left.read(&mut left_buffer)?;
        let right_read = right.read(&mut right_buffer)?;
        if left_read != right_read || left_buffer[..left_read] != right_buffer[..right_read] {
            return Ok(false);
        }
        if left_read == 0 {
            return Ok(true);
        }
    }
}

fn copy_tree(source: &Path, target: &Path) -> Result<(), std::io::Error> {
    fs::create_dir_all(target)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let destination = target.join(entry.file_name());
        if ty.is_symlink() {
            continue;
        }
        if ty.is_dir() {
            copy_tree(&entry.path(), &destination)?;
        } else if ty.is_file() {
            fs::copy(entry.path(), destination)?;
        }
    }
    Ok(())
}

fn validate_relative_path(value: &str) -> Result<(), PxeServiceError> {
    let path = Path::new(value);
    if value.is_empty()
        || path.is_absolute()
        || path.components().any(|c| {
            matches!(
                c,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(PxeServiceError::UnsafeBootFile(value.to_owned()));
    }
    Ok(())
}

pub fn validate_pxe_config(config: &PxeConfig) -> Result<(), PxeServiceError> {
    let server = parse_ipv4(&config.bind_address, "bind address")?;
    let mask = parse_ipv4(&config.subnet_mask, "subnet mask")?;
    let start = parse_ipv4(&config.pool_start, "pool start")?;
    let end = parse_ipv4(&config.pool_end, "pool end")?;
    let mask_num = u32::from(mask);
    if mask_num == 0 || (mask_num | mask_num.wrapping_sub(1)) != u32::MAX {
        return Err(PxeServiceError::InvalidConfig(
            "subnet mask must be contiguous".into(),
        ));
    }
    let network = u32::from(server) & mask_num;
    let broadcast = network | !mask_num;
    let start_num = u32::from(start);
    let end_num = u32::from(end);
    if start_num > end_num
        || start_num <= network
        || end_num >= broadcast
        || (start_num & mask_num) != network
        || (end_num & mask_num) != network
    {
        return Err(PxeServiceError::InvalidConfig(
            "DHCP pool must be ordered and inside the selected subnet".into(),
        ));
    }
    let server_num = u32::from(server);
    if start_num <= server_num && server_num <= end_num {
        return Err(PxeServiceError::PoolContainsServer);
    }
    if !(60..=604_800).contains(&config.lease_seconds) {
        return Err(PxeServiceError::InvalidConfig(
            "lease must be between 60 and 604800 seconds".into(),
        ));
    }
    if let Some(gateway) = &config.gateway {
        parse_ipv4(gateway, "gateway")?;
    }
    for dns in &config.dns_servers {
        parse_ipv4(dns, "DNS server")?;
    }
    if !config.bios_boot_file.is_empty() {
        validate_relative_path(&config.bios_boot_file)?;
    }
    validate_relative_path(&config.uefi_x64_boot_file)?;
    let root = Path::new(&config.tftp_root);
    if !root.is_dir() {
        return Err(PxeServiceError::InvalidConfig(
            "TFTP root is not a directory".into(),
        ));
    }
    for file in [&config.bios_boot_file, &config.uefi_x64_boot_file] {
        if file.is_empty() {
            continue;
        }
        if !root.join(file).is_file() {
            return Err(PxeServiceError::MissingBootFile(file.clone()));
        }
    }
    Ok(())
}

fn parse_ipv4(value: &str, label: &str) -> Result<Ipv4Addr, PxeServiceError> {
    value
        .parse()
        .map_err(|_| PxeServiceError::InvalidConfig(format!("{label} must be an IPv4 address")))
}

#[derive(Clone, Serialize, Deserialize)]
struct Lease {
    ip: Ipv4Addr,
    expires_at: DateTime<Utc>,
}

struct RunningPxe {
    status: PxeServiceStatus,
    shutdown: Vec<oneshot::Sender<()>>,
    tasks: Vec<JoinHandle<()>>,
}

#[derive(Clone)]
struct NativeIsoHttpState {
    iso: PathBuf,
    clients: Arc<RwLock<HashMap<String, PxeDiscoveredClient>>>,
}

async fn serve_native_iso(
    State(state): State<NativeIsoHttpState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    let Ok(mut file) = tokio::fs::File::open(&state.iso).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Ok(metadata) = file.metadata().await else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let total = metadata.len();
    let range = match parse_http_byte_range(headers.get(header::RANGE), total) {
        Ok(range) => range,
        Err(()) => {
            return Response::builder()
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header(header::CONTENT_RANGE, format!("bytes */{total}"))
                .body(Body::empty())
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
        }
    };
    update_client_stage_by_ip(&state.clients, peer.ip(), PxeClientStage::WaitingForAgent).await;
    let (start, end, status) = range
        .map(|(start, end)| (start, end, StatusCode::PARTIAL_CONTENT))
        .unwrap_or((0, total.saturating_sub(1), StatusCode::OK));
    if file.seek(SeekFrom::Start(start)).await.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    let length = end.saturating_sub(start).saturating_add(1);
    let stream = ReaderStream::new(file.take(length));
    let mut response = Response::builder()
        .status(status)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_LENGTH, length.to_string())
        .header(header::CONTENT_TYPE, "application/octet-stream");
    if status == StatusCode::PARTIAL_CONTENT {
        response = response.header(
            header::CONTENT_RANGE,
            format!("bytes {start}-{end}/{total}"),
        );
    }
    response
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

fn parse_http_byte_range(
    header_value: Option<&axum::http::HeaderValue>,
    total: u64,
) -> Result<Option<(u64, u64)>, ()> {
    let Some(value) = header_value else {
        return Ok(None);
    };
    let value = value.to_str().map_err(|_| ())?;
    let value = value.strip_prefix("bytes=").ok_or(())?;
    if value.contains(',') || total == 0 {
        return Err(());
    }
    let (start, end) = value.split_once('-').ok_or(())?;
    let (start, end) = if start.is_empty() {
        let suffix = end.parse::<u64>().map_err(|_| ())?;
        if suffix == 0 {
            return Err(());
        }
        (total.saturating_sub(suffix.min(total)), total - 1)
    } else {
        let start = start.parse::<u64>().map_err(|_| ())?;
        if start >= total {
            return Err(());
        }
        let end = if end.is_empty() {
            total - 1
        } else {
            end.parse::<u64>().map_err(|_| ())?.min(total - 1)
        };
        if end < start {
            return Err(());
        }
        (start, end)
    };
    Ok(Some((start, end)))
}

fn native_iso_ipxe_script(bind: Ipv4Addr, port: u16) -> String {
    format!("#!ipxe\nsanboot http://{bind}:{port}/boot/native.iso || shell\n")
}

pub struct PxeService {
    running: Mutex<Option<RunningPxe>>,
    leases: Arc<RwLock<HashMap<String, Lease>>>,
    clients: Arc<RwLock<HashMap<String, PxeDiscoveredClient>>>,
    lease_path: Option<PathBuf>,
    activities: Option<Arc<ActivityRepository>>,
}

impl Default for PxeService {
    fn default() -> Self {
        Self::new()
    }
}

impl PxeService {
    pub fn new() -> Self {
        Self {
            running: Mutex::new(None),
            leases: Arc::new(RwLock::new(HashMap::new())),
            clients: Arc::new(RwLock::new(HashMap::new())),
            lease_path: None,
            activities: None,
        }
    }

    pub fn open(lease_path: impl Into<PathBuf>) -> Result<Self, PxeServiceError> {
        let lease_path = lease_path.into();
        let leases = if lease_path.is_file() {
            serde_json::from_slice(&fs::read(&lease_path)?).unwrap_or_default()
        } else {
            HashMap::new()
        };
        Ok(Self {
            running: Mutex::new(None),
            leases: Arc::new(RwLock::new(leases)),
            clients: Arc::new(RwLock::new(HashMap::new())),
            lease_path: Some(lease_path),
            activities: None,
        })
    }

    pub fn open_with_activity(
        lease_path: impl Into<PathBuf>,
        activities: Arc<ActivityRepository>,
    ) -> Result<Self, PxeServiceError> {
        let mut service = Self::open(lease_path)?;
        service.activities = Some(activities);
        Ok(service)
    }

    pub async fn start(&self, config: PxeConfig) -> Result<PxeServiceStatus, PxeServiceError> {
        validate_pxe_config(&config)?;
        let mut running = self.running.lock().await;
        if running.is_some() {
            return Err(PxeServiceError::AlreadyRunning);
        }
        let bind = parse_ipv4(&config.bind_address, "bind address")?;
        if config.mode == PxeMode::StandaloneDhcp && detect_dhcp_server(bind).await? {
            return Err(PxeServiceError::DhcpConflict);
        }
        let tftp = bind_udp(bind, TFTP_PORT).await?;
        let dhcp_port = if config.mode == PxeMode::StandaloneDhcp {
            DHCP_SERVER_PORT
        } else {
            PROXY_DHCP_PORT
        };
        let dhcp = match bind_udp(bind, dhcp_port).await {
            Ok(s) => s,
            Err(e) => {
                drop(tftp);
                return Err(e);
            }
        };
        let proxy_discovery = if config.mode == PxeMode::ProxyDhcp {
            match bind_reusable_udp(bind, DHCP_SERVER_PORT) {
                Ok(s) => Some(s),
                Err(e) => {
                    drop(dhcp);
                    drop(tftp);
                    return Err(e);
                }
            }
        } else {
            None
        };
        dhcp.set_broadcast(true)?;
        let root = PathBuf::from(&config.tftp_root);
        let native_iso_http = if root.join(NATIVE_ISO_LAYOUT_MARKER).is_file() {
            let iso = root.join(NATIVE_ISO_PATH);
            if !iso.is_file() {
                return Err(PxeServiceError::MissingBootFile(NATIVE_ISO_PATH.into()));
            }
            let requested = SocketAddr::new(IpAddr::V4(bind), 0);
            let listener =
                TcpListener::bind(requested)
                    .await
                    .map_err(|source| PxeServiceError::HttpBind {
                        address: requested.to_string(),
                        source,
                    })?;
            let port = listener.local_addr()?.port();
            let script = native_iso_ipxe_script(bind, port);
            fs::write(root.join("boot.ipxe"), script)?;
            Some((listener, iso))
        } else {
            None
        };
        let (tftp_tx, tftp_rx) = oneshot::channel();
        let (dhcp_tx, dhcp_rx) = oneshot::channel();
        let clients_for_tftp = Arc::clone(&self.clients);
        let tftp_task = tokio::spawn(run_tftp(
            tftp,
            root.clone(),
            clients_for_tftp,
            self.activities.clone(),
            tftp_rx,
        ));
        let leases = Arc::clone(&self.leases);
        let clients = Arc::clone(&self.clients);
        let mode = config.mode;
        let lease_path = self.lease_path.clone();
        let dhcp_config = config.clone();
        let dhcp_task = tokio::spawn(run_dhcp(
            dhcp,
            dhcp_config,
            leases,
            clients,
            self.activities.clone(),
            lease_path,
            dhcp_rx,
        ));
        let mut shutdown = vec![tftp_tx, dhcp_tx];
        let mut tasks = vec![tftp_task, dhcp_task];
        if let Some((listener, iso)) = native_iso_http {
            let (tx, rx) = oneshot::channel();
            let router = Router::new()
                .route("/boot/native.iso", get(serve_native_iso))
                .with_state(NativeIsoHttpState {
                    iso,
                    clients: Arc::clone(&self.clients),
                });
            tasks.push(tokio::spawn(async move {
                let _ = axum::serve(
                    listener,
                    router.into_make_service_with_connect_info::<SocketAddr>(),
                )
                .with_graceful_shutdown(async {
                    let _ = rx.await;
                })
                .await;
            }));
            shutdown.push(tx);
        }
        if let Some(proxy_socket) = proxy_discovery {
            let (tx, rx) = oneshot::channel();
            tasks.push(tokio::spawn(run_dhcp(
                proxy_socket,
                config,
                Arc::clone(&self.leases),
                Arc::clone(&self.clients),
                self.activities.clone(),
                self.lease_path.clone(),
                rx,
            )));
            shutdown.push(tx);
        }
        let status = PxeServiceStatus {
            state: "running".into(),
            mode: Some(mode),
            bind_address: Some(bind.to_string()),
            dhcp_port: (mode == PxeMode::StandaloneDhcp).then_some(DHCP_SERVER_PORT),
            proxy_dhcp_port: (mode == PxeMode::ProxyDhcp).then_some(PROXY_DHCP_PORT),
            tftp_port: Some(TFTP_PORT),
            active_leases: 0,
            last_error: None,
        };
        *running = Some(RunningPxe {
            status: status.clone(),
            shutdown,
            tasks,
        });
        Ok(status)
    }

    pub async fn stop(&self) -> Result<PxeServiceStatus, PxeServiceError> {
        let mut server = self
            .running
            .lock()
            .await
            .take()
            .ok_or(PxeServiceError::NotRunning)?;
        for tx in server.shutdown.drain(..) {
            let _ = tx.send(());
        }
        for task in server.tasks {
            task.await?;
        }
        Ok(PxeServiceStatus {
            state: "idle".into(),
            mode: None,
            bind_address: None,
            dhcp_port: None,
            proxy_dhcp_port: None,
            tftp_port: None,
            active_leases: 0,
            last_error: None,
        })
    }

    pub async fn status(&self) -> PxeServiceStatus {
        let running = self.running.lock().await;
        let mut status = running
            .as_ref()
            .map(|r| r.status.clone())
            .unwrap_or(PxeServiceStatus {
                state: "idle".into(),
                mode: None,
                bind_address: None,
                dhcp_port: None,
                proxy_dhcp_port: None,
                tftp_port: None,
                active_leases: 0,
                last_error: None,
            });
        let now = Utc::now();
        status.active_leases = self
            .leases
            .read()
            .await
            .values()
            .filter(|l| l.expires_at > now)
            .count() as u32;
        status
    }

    pub async fn discovered_clients(&self) -> Vec<PxeDiscoveredClient> {
        let now = Utc::now();
        let mut clients = self.clients.write().await;
        clients.retain(|_, client| {
            let ttl = if client.stage == PxeClientStage::Discovered {
                PXE_DISCOVERY_TTL
            } else {
                PXE_BOOT_PROGRESS_TTL
            };
            now - client.last_seen_at <= ttl
        });
        let mut items: Vec<_> = clients.values().cloned().collect();
        items.sort_by_key(|client| std::cmp::Reverse(client.last_seen_at));
        items
    }
}

async fn bind_udp(address: Ipv4Addr, port: u16) -> Result<UdpSocket, PxeServiceError> {
    UdpSocket::bind(SocketAddr::new(IpAddr::V4(address), port))
        .await
        .map_err(|source| PxeServiceError::Bind {
            address: address.to_string(),
            port,
            source,
        })
}

fn bind_reusable_udp(address: Ipv4Addr, port: u16) -> Result<UdpSocket, PxeServiceError> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP)).map_err(|source| {
        PxeServiceError::Bind {
            address: address.to_string(),
            port,
            source,
        }
    })?;
    socket
        .set_reuse_address(true)
        .map_err(|source| PxeServiceError::Bind {
            address: address.to_string(),
            port,
            source,
        })?;
    socket
        .set_nonblocking(true)
        .map_err(|source| PxeServiceError::Bind {
            address: address.to_string(),
            port,
            source,
        })?;
    socket
        .bind(&SocketAddr::new(IpAddr::V4(address), port).into())
        .map_err(|source| PxeServiceError::Bind {
            address: address.to_string(),
            port,
            source,
        })?;
    UdpSocket::from_std(socket.into()).map_err(|source| PxeServiceError::Bind {
        address: address.to_string(),
        port,
        source,
    })
}

async fn detect_dhcp_server(bind: Ipv4Addr) -> Result<bool, PxeServiceError> {
    let socket = bind_reusable_udp(bind, DHCP_CLIENT_PORT)?;
    socket.set_broadcast(true)?;
    let mut discover = vec![0u8; 240];
    discover[0] = 1;
    discover[1] = 1;
    discover[2] = 6;
    rand::rng().fill_bytes(&mut discover[4..8]);
    discover[10] = 0x80;
    discover[11] = 0;
    rand::rng().fill_bytes(&mut discover[28..34]);
    discover[236..240].copy_from_slice(DHCP_MAGIC_COOKIE);
    push_option(&mut discover, 53, &[1]);
    push_option(&mut discover, 60, DHCP_PROBE_VENDOR_CLASS);
    discover.push(255);
    let _ = socket
        .send_to(
            &discover,
            SocketAddr::new(IpAddr::V4(Ipv4Addr::BROADCAST), DHCP_SERVER_PORT),
        )
        .await;
    let mut buf = [0u8; 1500];
    Ok(
        matches!(timeout(TokioDuration::from_millis(700), socket.recv_from(&mut buf)).await, Ok(Ok((size,_))) if dhcp_option(&buf[..size],53)==Some(&[2][..])),
    )
}

async fn run_dhcp(
    socket: UdpSocket,
    config: PxeConfig,
    leases: Arc<RwLock<HashMap<String, Lease>>>,
    clients: Arc<RwLock<HashMap<String, PxeDiscoveredClient>>>,
    activities: Option<Arc<ActivityRepository>>,
    lease_path: Option<PathBuf>,
    mut shutdown: oneshot::Receiver<()>,
) {
    let mut buf = [0u8; 1600];
    loop {
        tokio::select! { _=&mut shutdown => break, received=socket.recv_from(&mut buf) => {
            let Ok((size, peer))=received else { continue }; let packet=&buf[..size];
            if let Some((reply, target, mac, arch, offered))=process_dhcp(packet, peer, &config, &leases).await {
                record_client(&clients, &mac, offered.map(|v|v.to_string()), arch, PxeClientStage::Discovered).await;
                let mut details = serde_json::Map::new();
                details.insert("macAddress".into(), mac.clone().into());
                details.insert("architecture".into(), format!("{arch:?}").to_lowercase().into());
                if let Some(ip) = offered { details.insert("ipAddress".into(), ip.to_string().into()); }
                record_activity(&activities, ActivitySource::Device, "pxe_request_accepted", ActivitySeverity::Info, Some(ActivitySubject { id: mac.clone(), name: mac.clone() }), details, None);
                let _=socket.send_to(&reply,target).await;
                if let Some(path)=&lease_path { let _=persist_leases(path, &leases).await; }
            }
        }}
    }
}

async fn process_dhcp(
    packet: &[u8],
    peer: SocketAddr,
    config: &PxeConfig,
    leases: &Arc<RwLock<HashMap<String, Lease>>>,
) -> Option<(Vec<u8>, SocketAddr, String, Architecture, Option<Ipv4Addr>)> {
    if packet.len() < 240 || packet[0] != 1 || &packet[236..240] != DHCP_MAGIC_COOKIE {
        return None;
    }
    let message = *dhcp_option(packet, 53)?.first()?;
    // The conflict detector broadcasts a synthetic DHCP request. A running
    // EasyDeployMesh PXE service may receive it, but it is not a real client and
    // must never consume a lease or appear in the discovered-client list.
    if dhcp_option(packet, 60) == Some(DHCP_PROBE_VENDOR_CLASS) {
        return None;
    }
    if !matches!(message, 1 | 3 | 4 | 7) {
        return None;
    }
    let hlen = usize::from(packet[2]).min(16);
    if hlen == 0 {
        return None;
    }
    let mac = packet[28..28 + hlen]
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":");
    let architecture = dhcp_option(packet, 93)
        .and_then(|v| v.get(..2))
        .map(|v| u16::from_be_bytes([v[0], v[1]]))
        .unwrap_or(0);
    let is_ipxe = dhcp_option(packet, 77).is_some_and(|value| {
        String::from_utf8_lossy(value)
            .to_ascii_lowercase()
            .contains("ipxe")
    });
    let arch = if matches!(architecture, 0 | 7 | 9) {
        Architecture::X86_64
    } else {
        Architecture::Unknown
    };
    if matches!(message, 4 | 7) {
        leases.write().await.remove(&mac);
        return None;
    }
    let (offered, reply_type) = if config.mode == PxeMode::StandaloneDhcp {
        if message == 1 {
            (Some(allocate_lease(config, leases, &mac).await?), 2)
        } else {
            let requested = dhcp_option(packet, 50)
                .and_then(|value| value.get(..4))
                .map(|value| Ipv4Addr::new(value[0], value[1], value[2], value[3]))
                .or_else(|| {
                    let ip = Ipv4Addr::new(packet[12], packet[13], packet[14], packet[15]);
                    (!ip.is_unspecified()).then_some(ip)
                });
            match requested {
                Some(ip) => match assign_requested_lease(config, leases, &mac, ip).await {
                    Some(ip) => (Some(ip), 5),
                    None => (None, 6),
                },
                None => (None, 6),
            }
        }
    } else {
        (None, if message == 1 { 2 } else { 5 })
    };
    let ciaddr = Ipv4Addr::new(packet[12], packet[13], packet[14], packet[15]);
    let observed_ip = offered
        .or_else(|| (!ciaddr.is_unspecified()).then_some(ciaddr))
        .or_else(|| match peer.ip() {
            IpAddr::V4(ip) if !ip.is_unspecified() => Some(ip),
            _ => None,
        });
    let reply = build_dhcp_reply(packet, config, offered, reply_type, architecture, is_ipxe);
    let target = if peer.ip().is_unspecified() || peer.port() == DHCP_CLIENT_PORT {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::BROADCAST), DHCP_CLIENT_PORT)
    } else {
        peer
    };
    Some((reply, target, mac, arch, observed_ip))
}

async fn assign_requested_lease(
    config: &PxeConfig,
    leases: &Arc<RwLock<HashMap<String, Lease>>>,
    mac: &str,
    ip: Ipv4Addr,
) -> Option<Ipv4Addr> {
    let start = u32::from(parse_ipv4(&config.pool_start, "pool").ok()?);
    let end = u32::from(parse_ipv4(&config.pool_end, "pool").ok()?);
    let candidate = u32::from(ip);
    if candidate < start || candidate > end {
        return None;
    }
    let now = Utc::now();
    let mut map = leases.write().await;
    map.retain(|_, lease| lease.expires_at > now);
    if map
        .iter()
        .any(|(owner, lease)| owner != mac && lease.ip == ip)
    {
        return None;
    }
    map.insert(
        mac.into(),
        Lease {
            ip,
            expires_at: now + Duration::seconds(i64::from(config.lease_seconds)),
        },
    );
    Some(ip)
}

async fn allocate_lease(
    config: &PxeConfig,
    leases: &Arc<RwLock<HashMap<String, Lease>>>,
    mac: &str,
) -> Option<Ipv4Addr> {
    let now = Utc::now();
    let mut map = leases.write().await;
    map.retain(|_, l| l.expires_at > now);
    let start = u32::from(parse_ipv4(&config.pool_start, "pool").ok()?);
    let end = u32::from(parse_ipv4(&config.pool_end, "pool").ok()?);
    if let Some(lease) = map.get(mac).filter(|lease| {
        let ip = u32::from(lease.ip);
        start <= ip && ip <= end
    }) {
        return Some(lease.ip);
    }
    let ip = (start..=end)
        .map(Ipv4Addr::from)
        .find(|candidate| !map.values().any(|l| l.ip == *candidate))?;
    map.insert(
        mac.into(),
        Lease {
            ip,
            expires_at: now + Duration::seconds(i64::from(config.lease_seconds)),
        },
    );
    Some(ip)
}
async fn persist_leases(
    path: &Path,
    leases: &Arc<RwLock<HashMap<String, Lease>>>,
) -> Result<(), std::io::Error> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?
    }
    let bytes = serde_json::to_vec_pretty(&*leases.read().await).map_err(std::io::Error::other)?;
    let temp = path.with_extension("json.tmp");
    fs::write(&temp, bytes)?;
    fs::rename(temp, path)?;
    Ok(())
}

fn build_dhcp_reply(
    request: &[u8],
    config: &PxeConfig,
    yiaddr: Option<Ipv4Addr>,
    message: u8,
    architecture: u16,
    is_ipxe: bool,
) -> Vec<u8> {
    let mut out = vec![0u8; 240];
    out[0] = 2;
    out[1..4].copy_from_slice(&request[1..4]);
    out[4..8].copy_from_slice(&request[4..8]);
    out[10..12].copy_from_slice(&request[10..12]);
    if let Some(ip) = yiaddr {
        out[16..20].copy_from_slice(&ip.octets());
    }
    let server = parse_ipv4(&config.bind_address, "bind").unwrap();
    out[20..24].copy_from_slice(&server.octets());
    out[28..44].copy_from_slice(&request[28..44]);
    out[236..240].copy_from_slice(DHCP_MAGIC_COOKIE);
    push_option(&mut out, 53, &[message]);
    push_option(&mut out, 54, &server.octets());
    if config.mode == PxeMode::StandaloneDhcp {
        let mask = parse_ipv4(&config.subnet_mask, "mask").unwrap();
        push_option(&mut out, 1, &mask.octets());
        push_option(&mut out, 51, &config.lease_seconds.to_be_bytes());
        if let Some(g) = &config.gateway {
            push_option(&mut out, 3, &parse_ipv4(g, "gateway").unwrap().octets())
        }
        if !config.dns_servers.is_empty() {
            let dns: Vec<_> = config
                .dns_servers
                .iter()
                .flat_map(|v| parse_ipv4(v, "dns").unwrap().octets())
                .collect();
            push_option(&mut out, 6, &dns)
        }
    }
    let boot = if is_ipxe {
        Some("boot.ipxe")
    } else if architecture == 0 && !config.bios_boot_file.is_empty() {
        Some(config.bios_boot_file.as_str())
    } else if architecture == 7 || architecture == 9 {
        Some(config.uefi_x64_boot_file.as_str())
    } else {
        None
    };
    if let Some(boot) = boot {
        push_option(&mut out, 66, config.bind_address.as_bytes());
        push_option(&mut out, 67, boot.as_bytes());
    }
    out.push(255);
    out
}

fn push_option(out: &mut Vec<u8>, code: u8, value: &[u8]) {
    if value.len() <= 255 {
        out.push(code);
        out.push(value.len() as u8);
        out.extend_from_slice(value)
    }
}
fn dhcp_option(packet: &[u8], wanted: u8) -> Option<&[u8]> {
    if packet.len() < 240 {
        return None;
    }
    let mut i = 240;
    while i < packet.len() {
        let code = packet[i];
        i += 1;
        if code == 255 {
            break;
        }
        if code == 0 {
            continue;
        }
        if i >= packet.len() {
            break;
        }
        let len = packet[i] as usize;
        i += 1;
        if i + len > packet.len() {
            break;
        }
        if code == wanted {
            return Some(&packet[i..i + len]);
        }
        i += len
    }
    None
}

async fn run_tftp(
    socket: UdpSocket,
    root: PathBuf,
    clients: Arc<RwLock<HashMap<String, PxeDiscoveredClient>>>,
    activities: Option<Arc<ActivityRepository>>,
    mut shutdown: oneshot::Receiver<()>,
) {
    let mut buf = [0u8; 2048];
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            received = socket.recv_from(&mut buf) => {
                let Ok((size, peer)) = received else { continue };
                let request = buf[..size].to_vec();
                let requested_file = tftp_request_filename(&request)
                    .unwrap_or_else(|| "<invalid>".into());
                let root = root.clone();
                let clients = Arc::clone(&clients);
                let activities = activities.clone();
                tokio::spawn(async move {
                    let (event, severity, error) = match serve_tftp_request(&request, peer, &root, &clients).await {
                        Ok(()) => ("boot_file_sent", ActivitySeverity::Success, None),
                        Err(error) => ("tftp_failed", ActivitySeverity::Error, Some(error.to_string())),
                    };
                    let mut details = serde_json::Map::new();
                    details.insert("fileName".into(), requested_file.into());
                    details.insert("ipAddress".into(), peer.ip().to_string().into());
                    record_activity(&activities, ActivitySource::Device, event, severity, None, details, error);
                });
            }
        }
    }
}

fn tftp_request_filename(request: &[u8]) -> Option<String> {
    if request.len() < 4 || u16::from_be_bytes([request[0], request[1]]) != 1 {
        return None;
    }
    let end = request[2..].iter().position(|byte| *byte == 0)? + 2;
    let name = std::str::from_utf8(&request[2..end]).ok()?;
    Some(name.escape_default().to_string())
}

async fn serve_tftp_request(
    request: &[u8],
    peer: SocketAddr,
    root: &Path,
    clients: &Arc<RwLock<HashMap<String, PxeDiscoveredClient>>>,
) -> Result<(), std::io::Error> {
    if request.len() < 2 {
        return Ok(());
    }
    let opcode = u16::from_be_bytes([request[0], request[1]]);
    if opcode == 2 {
        return send_tftp_error(peer, 2, "write requests are not allowed").await;
    }
    if opcode != 1 || request.len() < 4 {
        return Ok(());
    }
    let fields: Vec<&[u8]> = request[2..].split(|b| *b == 0).collect();
    let Some(name) = fields.first().and_then(|v| std::str::from_utf8(v).ok()) else {
        return Ok(());
    };
    // PXE loaders commonly spell TFTP-root-relative paths with a leading slash.
    let relative_name = name.trim_start_matches('/');
    if validate_relative_path(relative_name).is_err() {
        return send_tftp_error(peer, 2, "access violation").await;
    }
    let path = root.join(relative_name);
    let canonical_root = fs::canonicalize(root)?;
    let canonical = match fs::canonicalize(&path) {
        Ok(v) => v,
        Err(_) => return send_tftp_error(peer, 1, "file not found").await,
    };
    if !canonical.starts_with(canonical_root) || !canonical.is_file() {
        return send_tftp_error(peer, 2, "access violation").await;
    }
    update_client_stage_by_ip(clients, peer.ip(), PxeClientStage::Downloading).await;
    let mut file = tokio::fs::File::open(canonical).await?;
    let file_size = file.metadata().await?.len();
    let mut block_size = 512usize;
    let mut timeout_secs = 3u64;
    let mut requested_options = Vec::new();
    let mut i = 2;
    while i + 1 < fields.len() {
        let key = String::from_utf8_lossy(fields[i]).to_ascii_lowercase();
        let val = String::from_utf8_lossy(fields[i + 1]).to_string();
        match key.as_str() {
            "blksize" => {
                if let Ok(v) = val.parse::<usize>() {
                    block_size = v.clamp(8, 1468);
                    requested_options.push(("blksize", block_size.to_string()))
                }
            }
            "timeout" => {
                if let Ok(v) = val.parse::<u64>() {
                    timeout_secs = v.clamp(1, 10);
                    requested_options.push(("timeout", timeout_secs.to_string()))
                }
            }
            "tsize" => requested_options.push(("tsize", file_size.to_string())),
            _ => {}
        }
        i += 2
    }
    let socket = UdpSocket::bind("0.0.0.0:0").await?;
    socket.connect(peer).await?;
    let mut block = 1u16;
    if !requested_options.is_empty() {
        let mut oack = vec![0, 6];
        for (k, v) in requested_options {
            oack.extend_from_slice(k.as_bytes());
            oack.push(0);
            oack.extend_from_slice(v.as_bytes());
            oack.push(0)
        }
        if !send_tftp_with_ack(&socket, &oack, 0, timeout_secs).await? {
            return Err(tftp_ack_timeout(relative_name, peer.ip(), "OACK"));
        }
    }
    let mut remaining = file_size;
    while remaining > 0 {
        let chunk_size = usize::try_from(remaining.min(block_size as u64)).unwrap_or(block_size);
        let mut packet = Vec::with_capacity(4 + chunk_size);
        packet.extend_from_slice(&[0, 3]);
        packet.extend_from_slice(&block.to_be_bytes());
        let payload_start = packet.len();
        packet.resize(payload_start + chunk_size, 0);
        file.read_exact(&mut packet[payload_start..]).await?;
        if !send_tftp_with_ack(&socket, &packet, block, timeout_secs).await? {
            return Err(tftp_ack_timeout(
                relative_name,
                peer.ip(),
                &format!("data block {block}"),
            ));
        }
        remaining -= chunk_size as u64;
        block = block.wrapping_add(1)
    }
    if file_size % block_size as u64 == 0 {
        let mut packet = vec![0, 3];
        packet.extend_from_slice(&block.to_be_bytes());
        if !send_tftp_with_ack(&socket, &packet, block, timeout_secs).await? {
            return Err(tftp_ack_timeout(
                relative_name,
                peer.ip(),
                &format!("final data block {block}"),
            ));
        }
    }
    if relative_name.eq_ignore_ascii_case("boot/boot.wim") {
        update_client_stage_by_ip(clients, peer.ip(), PxeClientStage::WaitingForAgent).await;
    }
    Ok(())
}

fn tftp_ack_timeout(file_name: &str, client_ip: IpAddr, stage: &str) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        format!("TFTP transfer of {file_name} to {client_ip} timed out awaiting ACK for {stage}"),
    )
}

async fn send_tftp_with_ack(
    socket: &UdpSocket,
    packet: &[u8],
    block: u16,
    seconds: u64,
) -> Result<bool, std::io::Error> {
    let mut ack = [0u8; 516];
    for _ in 0..4 {
        socket.send(packet).await?;
        if let Ok(Ok(size)) =
            timeout(TokioDuration::from_secs(seconds), socket.recv(&mut ack)).await
            && size >= 4
            && u16::from_be_bytes([ack[0], ack[1]]) == 4
            && u16::from_be_bytes([ack[2], ack[3]]) == block
        {
            return Ok(true);
        }
    }
    Ok(false)
}
async fn send_tftp_error(peer: SocketAddr, code: u16, message: &str) -> Result<(), std::io::Error> {
    let socket = UdpSocket::bind("0.0.0.0:0").await?;
    let mut packet = vec![0, 5];
    packet.extend_from_slice(&code.to_be_bytes());
    packet.extend_from_slice(message.as_bytes());
    packet.push(0);
    socket.send_to(&packet, peer).await?;
    Ok(())
}
async fn record_client(
    clients: &Arc<RwLock<HashMap<String, PxeDiscoveredClient>>>,
    mac: &str,
    ip: Option<String>,
    architecture: Architecture,
    stage: PxeClientStage,
) {
    let now = Utc::now();
    let mut map = clients.write().await;
    map.entry(mac.into())
        .and_modify(|c| {
            c.ip_address = ip.clone().or_else(|| c.ip_address.clone());
            c.architecture = architecture;
            if !(stage == PxeClientStage::Discovered
                && matches!(
                    c.stage,
                    PxeClientStage::Downloading | PxeClientStage::WaitingForAgent
                ))
            {
                c.stage = stage;
            }
            c.last_seen_at = now;
        })
        .or_insert(PxeDiscoveredClient {
            mac_address: mac.into(),
            ip_address: ip,
            architecture,
            stage,
            first_seen_at: now,
            last_seen_at: now,
        });
}
async fn update_client_stage_by_ip(
    clients: &Arc<RwLock<HashMap<String, PxeDiscoveredClient>>>,
    ip: IpAddr,
    stage: PxeClientStage,
) {
    let now = Utc::now();
    for client in clients.write().await.values_mut() {
        if client.ip_address.as_deref() == Some(&ip.to_string()) {
            client.stage = stage;
            client.last_seen_at = now;
        }
    }
}
fn record_activity(
    activities: &Option<Arc<ActivityRepository>>,
    source: ActivitySource,
    kind: &str,
    severity: ActivitySeverity,
    subject: Option<ActivitySubject>,
    details: serde_json::Map<String, serde_json::Value>,
    raw_message: Option<String>,
) {
    if let Some(activities) = activities {
        let _ = activities.record(source, kind, severity, subject, details, raw_message);
    }
}
