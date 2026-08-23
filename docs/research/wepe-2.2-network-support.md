# WePE 2.2 network support research

Date: 2026-08-23

## Conclusion

WePE is intentionally built without its network module. The absence of
`ipconfig.exe`, the `wpeinit` failure, and the Agent's inability to find a MAC
address are therefore consistent with the media's documented design; they are
not evidence that the user selected the wrong VMware adapter or performed the
PXE workflow incorrectly.

The official WePE technical specification lists **Network module: not
supported** for both 32-bit and 64-bit editions and answers **Never!!!** to
whether networking will be added. This is the strongest first-party evidence
about the product's intended capabilities:

- [WePE official technical specification](https://www.wepe.com.cn/learnmore.html)

The official WePE 2.2 changelog says that version 2.2 uses Windows 10 21H1
build `10.19043.1165` and mentions added VMD and touchpad support, but it does
not announce network support or network drivers:

- [WePE official version 2.2 changelog](https://www.wepe.com.cn/update/update2.2.html)

Consequently, merely changing VMware's virtual NIC model is not a sufficient
fix for this WePE image. The product deliberately omits network functionality,
rather than merely lacking a driver for one particular NIC.

## Why PXE succeeds before WePE but the Agent cannot connect afterward

iPXE and Windows PE are separate execution environments. iPXE can download and
SAN-boot the ISO using its own network driver. After control transfers to
Windows PE, Windows needs its own TCP/IP networking implementation and a
compatible Windows NIC driver. Successful iPXE traffic therefore does not prove
that the booted PE has Windows networking.

Microsoft documents LAN/TCP-IP networking as a normal WinPE capability and
documents `Wpeinit`/`Startnet.cmd` as the startup mechanism. Microsoft also
states that WinPE contains generic network drivers but that additional drivers
may sometimes be needed:

- [Microsoft: Windows PE overview](https://learn.microsoft.com/en-us/windows-hardware/manufacture/desktop/winpe-intro?view=windows-11)
- [Microsoft: OEM deployment of Windows desktop editions](https://learn.microsoft.com/en-us/windows-hardware/manufacture/desktop/oem-deployment-of-windows-desktop-editions?view=windows-10)

That guidance applies to normal/customized Microsoft WinPE images. It does not
override WePE's explicit decision to omit its network module.

## Why injecting only a NIC driver is insufficient here

For an ordinary WinPE image whose network stack is present, Microsoft supports
offline driver injection with `DISM /Add-Driver`. Broadcom's WinPE diagnostic
procedure similarly loads the correct NIC INF with `DRVLOAD`, initializes the
WinPE network stack with `NETCFG -WINPE`, and verifies it with `IPCONFIG`:

- [Microsoft: Mount and customize Windows PE](https://learn.microsoft.com/en-us/windows-hardware/manufacture/desktop/winpe-mount-and-customize?view=windows-11)
- [Broadcom: Testing NIC drivers with WinPE 10/11](https://knowledge.broadcom.com/external/article/178552/testing-nic-drivers-with-winpe1011-witho.html)

In the observed WePE 2.2 environment, `ipconfig` itself is absent and `wpeinit`
returns `0x80004005`. Those observations, combined with WePE's official
specification, indicate that the prerequisite Windows PE networking payload was
removed. Adding an E1000/E1000E/VMXNET3 `.inf` alone cannot restore executables,
services, registry configuration, and other networking payload that the image
does not contain.

VMware/Broadcom documentation does confirm that VMXNET3 normally requires its
driver and that E1000E can be used as a temporary adapter recognized by some
Windows guests. This distinction matters only after a usable Windows networking
stack exists:

- [Broadcom: VMXNET3 adapter missing and temporary E1000E workaround](https://knowledge.broadcom.com/external/article/443192/vmxnet3-network-adapter-missing-and-vmwa.html)

## Implications for EasyDeployMesh

1. Do not diagnose this particular result as only a missing VMware NIC driver.
2. Do not claim that injecting a NIC INF alone makes WePE 2.2 Agent-capable.
3. Keep the native ISO/SAN boot path for boot compatibility, but treat the
   original WePE runtime as **offline-only** unless a complete compatible
   networking payload is added and validated.
4. The safer supported implementation is to boot a project-managed, standard
   network-capable WinPE runtime for the EasyDeployMesh Agent and expose or
   chain the WePE tools separately. Reconstructing Microsoft's networking stack
   inside a heavily stripped third-party WIM is a broader compatibility and
   servicing project, not a normal driver-injection operation.
5. If full network enablement of the WePE WIM is pursued, it must be validated
   on disposable BIOS and UEFI VMs with at least E1000E and VMXNET3, and must
   prove DHCP, MAC enumeration, Agent registration/heartbeat, and preservation
   of the original WePE desktop/tool startup flow.

## Evidence from the reported run

The reported runtime behavior agrees with the first-party documentation:

- `X:\EasyDeployMesh` exists, proving managed-ISO and WIM file injection worked.
- `easydeploymesh-shell.exe` starts, proving the injected startup hook executes.
- `wpeinit` exits with `0x80004005`.
- the Agent repeatedly reports `no usable MAC address was found`.
- `ipconfig` is not present in the running image.

Together, these facts locate the failure after ISO boot and Agent injection, at
the missing/disabled Windows networking layer inside WePE itself.
