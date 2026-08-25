# Third-party notices

## wimlib

The macOS desktop application bundles `wimlib-imagex` 1.14.5, copyright Eric
Biggers and contributors, under GPL-3.0-or-later. The corresponding source is
available from <https://wimlib.net/downloads/wimlib-1.14.5.tar.gz> with SHA-256
`84221a3abd5b91228f15f8e6065c335a336237b5738197b75bf419eea561a194`.

The complete GPLv3 license is bundled beside the executable. Release artifacts
must provide the exact corresponding source archive or a GPL-compliant written
offer. EasyDeployMesh's build script only disables optional FUSE and NTFS-3G
integrations; it does not patch wimlib.
