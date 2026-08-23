# Bundled iPXE assets

- `undionly.kpxe` provides the BIOS PXE chainloader.
- `ipxe.efi` provides the x86-64 UEFI PXE chainloader.
- Both binaries are distributed under the GNU GPL v2; see `COPYING.GPLv2`.
- The UEFI binary is iPXE v2.0.0+ (`g0ab22`) from
  <https://boot.ipxe.org/x86_64-efi/ipxe.efi>.
- Its SHA-256 is
  `f4296f8f373c6a8a86808d251a55f8b4b411efba7d90ec2f5b0ea7c72074f4df`.

Run `pnpm assets:update:ipxe` to reproduce the checked-in UEFI asset. The update
script refuses content that does not match the pinned digest.
