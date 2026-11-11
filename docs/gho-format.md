# Ghost GHO partition image format (as implemented)

This note describes the parts of the Norton Ghost 11.x `.gho` format that
`crates/gho` reads and restores. It was written for interoperability from
observed image bytes and black-box behaviour, and was checked against a real
Ghost 11.5 Windows XP NTFS partition image (2.0 GB file, 3.4 GB logical stream,
15,199 MFT records). It contains no Ghost code. Do not commit decompiled Ghost
binaries or code to this repository.

Anything not described here is unsupported, and the reader fails closed on it.

## File layout

| Offset | Content |
| --- | --- |
| `0` | 512-byte file header |
| `512` | Area not needed for restore (zero padding and an index table). Skipped. |
| descriptor | Uncompressed partition descriptor segment (see below) |
| after descriptor | Chain of compressed packets |
| terminator | `00 00`: a zero-length packet that ends the chain |
| after terminator | Index used by Ghost Explorer. Ignored. |
| `len - 34` | End record |

### File header (512 bytes)

| Bytes | Meaning |
| --- | --- |
| `0..2` | Magic `FE EF` (`0xEFFE` little-endian) |
| `2` | File type: `1` = primary image, `9` = GHS continuation span |
| `3` | Compression tag: `0` none, `2` fast (`-z1`), `3..10` zlib levels `-z2..-z9` (tag = level + 1) |
| `4..8` | Image identifier, repeated in every span header |
| `12` | Flags; bit `0x02` = password-protected or encrypted |

GHS continuation files start with their own 512-byte header (type `9`, same
compression tag and identifier) that is skipped when joining spans. The joined
byte stream is otherwise one continuous file.

### End record

The last 34 bytes are a record: `u16 kind = 0x0023`, 2 bytes, `u32 magic =
0x012F18D8`, `u16 body length = 24`, then a 24-byte body. Body bytes `8..16`
hold the distance, in bytes, from the start of the end record back to the
terminator packet. The terminator must contain `00 00`.

### Packet chain

Each packet is `u16 stored length` (including these two bytes) followed by a
complete zlib stream. Each packet expands to at most 32 KiB, and the reader
rejects anything over 64 KiB. Each zlib stream carries its own Adler-32
checksum, which the reader verifies. The chain must reach the terminator
exactly.

Only zlib packets (tags `3..10`) have been verified against a real image.
Uncompressed (`0`) and fast (`2`) images are rejected until verified samples
exist. Their framing differs: fast packets use a different LZ codec, and
uncompressed images have no packet framing.

## Logical partition stream

The logical stream is the descriptor segment, read as stored, followed by
the expanded bytes of every packet. It is a sequence of three fixed-size
packet types:

| Bytes | Packet |
| --- | --- |
| `0E kind flags 00 00 00 00 <u64 index> 0F` (16 bytes) | Header |
| `0F <u64 length> 0E` (10 bytes) | Data follows: `length` bytes, or `length` clusters inside a run |
| `0A <u32 checksum> 00 00 00 00 0B` (10 bytes) | Per-item checksum. Its algorithm is unidentified, so the value is ignored. |

Header kinds:

| Kind | Flags | Meaning |
| --- | --- | --- |
| `0x01` | `0x00` | Partition descriptor. A `0F` byte-length packet and the payload follow. |
| `0x04` | `0x80` | In-use MFT record `index`. A `0F` byte-length packet (the record size) and the record follow. |
| `0x08` | `0x80` | Free MFT record that still has contents. Same layout as `0x04`. |
| `0x10` | `0x80` | Empty MFT record slot. Same layout as `0x04`. |
| `0x20` | `0x80` | Data for the next non-resident attribute of the current record follows |
| `0x40` | `0x80` | The next non-resident attribute's data is omitted |
| `0x02` | `0x00` | Run `index` of the current attribute (0-based, consecutive) |

### Grammar

```
stream     := descriptor record+
descriptor := H(0x01) D(n) <n bytes> C
record     := H(0x04) D(r) <r bytes> section* C
            | H(0x08) D(r) <r bytes>
            | H(0x10) D(r) <r bytes>
section    := H(0x20) run*      ; one per non-resident attribute, in record order
            | H(0x40)
run        := H(0x02, i) [ D(clusters) <clusters * cluster size bytes> ]
                                ; the D part is absent if and only if run i is sparse
```

The descriptor payload begins with `u16`, then `GHPR`, and ends with `RPHG`. It
embeds exactly one copy of the partition's 512-byte NTFS boot sector. That copy
supplies the volume geometry before any data arrives.

### Records and attributes

* Records are stored with the NTFS update-sequence fixups already removed.
  The raw `$MFT` data stream (record 0's `$DATA` runs) holds the on-disk form,
  and is identical apart from the update sequence array.
* Record 0 (`$MFT`) comes first, and its `$DATA` runs carry the whole MFT.
* For every in-use record, the number of `0x20`/`0x40` sections equals the
  number of non-resident attributes in the record, taken in record order.
  Extension records carry their own attributes' sections.
* Every run of an attribute appears in order. The data length must equal the
  run length from the record's mapping pairs.
* In the verified image, Ghost omitted only `$MFTMirr` (record 1, `$DATA`) and
  `$BadClus:$Bad` (record 8). The restorer accepts no other omitted
  attributes.

## Restore algorithm

1. Read the boot sector from the descriptor. Require 512-byte sectors and a
   target at least `(total_sectors + 1) * 512` bytes.
2. Write every stored run at `LCN * cluster_size`. Reject a run that overlaps
   another run or lies beyond `total_sectors`.
3. The stored `$Boot` sector must equal the descriptor copy. Patch BPB hidden
   sectors (`0x1C`) with the target partition's starting LBA.
4. Regenerate `$MFTMirr` from the first `$MFT` clusters.
5. Write the backup boot sector at sector `total_sectors`.
6. Leave free clusters untouched. The restored file system keeps its original
   size. On a larger partition, the Agent runs `diskpart extend filesystem`.

The dry run also records whether any in-use record has a `$FILE_NAME` of
`winload.exe`. Without one, the service blocks the image, because the Agent
configures boot with BCDBoot, which needs Windows Vista or later.

The service runs the same algorithm as a dry run at import and before every
lease. That run records the logical stream's size and SHA-256 and the minimum
volume size. The Agent recomputes those values while it writes.
