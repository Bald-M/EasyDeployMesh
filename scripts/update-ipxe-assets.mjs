import { createHash } from "node:crypto";
import { writeFile } from "node:fs/promises";
import { resolve } from "node:path";

const assets = [
  {
    url: "https://boot.ipxe.org/x86_64-efi/ipxe.efi",
    destination: "crates/service/assets/ipxe/ipxe.efi",
    sha256: "f4296f8f373c6a8a86808d251a55f8b4b411efba7d90ec2f5b0ea7c72074f4df",
  },
];

for (const asset of assets) {
  const response = await fetch(asset.url);
  if (!response.ok) {
    throw new Error(`Could not download ${asset.url}: HTTP ${response.status}`);
  }
  const bytes = Buffer.from(await response.arrayBuffer());
  const digest = createHash("sha256").update(bytes).digest("hex");
  if (digest !== asset.sha256) {
    throw new Error(
      `Refusing to update ${asset.destination}: expected SHA-256 ${asset.sha256}, received ${digest}`,
    );
  }
  const destination = resolve(asset.destination);
  await writeFile(destination, bytes);
  console.log(`Updated ${destination} (${bytes.length} bytes, SHA-256 ${digest})`);
}
